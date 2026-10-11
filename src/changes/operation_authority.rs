//! One durable operation slot and acknowledged publication floor.
//!
//! Root must require this file whenever any normalized-catalog activation
//! evidence exists, and durably initialize it before publishing that evidence.
//! This module never scans history and never proves a canonical receipt. Under
//! the held writer, callers prove terminal publication before acknowledge,
//! and no applied canonical changes (or completed rollback) before cancel.
use super::PreparedChange;
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    vault::{DirectorySync, DurableIo, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use crate::vault::paths::descriptor;
#[cfg(unix)]
#[path = "operation_authority_legacy.rs"]
mod legacy;
const NAME: &str = "operations.json";
const MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Presence {
    Required,
    LegacyMayBeAbsent,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Publication {
    pub(crate) file_id: String,
    pub(crate) epoch: u64,
}
impl Publication {
    fn validate(&self) -> Result<()> {
        if self.file_id.len() != 32
            || !self
                .file_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.epoch == 0
            || self.epoch > i64::MAX as u64
        {
            return Err(recovery(
                "operation authority has invalid publication identity or epoch",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActiveOperation {
    pub(crate) change: PreparedChange,
    pub(crate) starting: Publication,
    pub(crate) intended: Publication,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    vault_id: RecordId,
    revision: u64,
    publication: Publication,
    active: Option<ActiveOperation>,
}
impl State {
    fn validate(&self, vault: &RecordId) -> Result<()> {
        if self.version != 1 || &self.vault_id != vault || self.revision == 0 {
            return Err(recovery(
                "operation authority version, vault or revision is invalid",
            ));
        }
        self.publication.validate()?;
        if let Some(active) = &self.active {
            if self.revision < 2 {
                return Err(recovery("active operation has no reserved state revision"));
            }
            active.starting.validate()?;
            active.intended.validate()?;
            if active.starting != self.publication {
                return Err(recovery(
                    "active operation starting publication differs from floor",
                ));
            }
            require_successor(&active.starting, &active.intended)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Authority {
    state: State,
    hash: Blake3Hash,
}
impl Authority {
    pub(crate) fn revision(&self) -> u64 {
        self.state.revision
    }
    pub(crate) fn publication(&self) -> &Publication {
        &self.state.publication
    }
    pub(crate) fn active(&self) -> Option<&ActiveOperation> {
        self.state.active.as_ref()
    }
    /// Exact state equality is useful for mutation CAS. Ordinary evidence reads
    /// retain their starting publication floor across unrelated operations.
    pub(crate) fn same_revision(&self, other: &Self) -> bool {
        self == other
    }
    pub(crate) fn require_publication(&self, actual: &Publication) -> Result<()> {
        self.require_idle()?;
        actual.validate()?;
        if actual.file_id != self.state.publication.file_id
            || actual.epoch < self.state.publication.epoch
        {
            return Err(recovery(
                "selected publication precedes or differs from acknowledged authority",
            ));
        }
        Ok(())
    }
    /// A read begun before a later update may keep its coherent older snapshot.
    /// A new read cannot use an epoch preceding the floor it observed at start.
    pub(crate) fn require_read_publication(&self, actual: &Publication) -> Result<()> {
        actual.validate()?;
        if actual.epoch < self.state.publication.epoch
            || (actual.epoch == self.state.publication.epoch
                && actual.file_id != self.state.publication.file_id)
        {
            return Err(recovery(
                "selected publication precedes the query's starting floor",
            ));
        }
        Ok(())
    }
    fn require_idle(&self) -> Result<()> {
        if self.active().is_some() {
            return Err(recovery(
                "an active canonical operation requires completion or recovery",
            ));
        }
        Ok(())
    }
    fn next(&self) -> Result<State> {
        let mut state = self.state.clone();
        state.revision = state.revision.checked_add(1).ok_or_else(|| {
            WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "operation authority revision exhausted",
            )
        })?;
        Ok(state)
    }
    fn require_change(&self, change: &PreparedChange) -> Result<&ActiveOperation> {
        self.active()
            .filter(|a| &a.change == change)
            .ok_or_else(|| conflict("active operation differs from exact prepared change"))
    }
}
fn recovery(message: &str) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn io(action: &str, error: std::io::Error) -> WikiError {
    recovery(&format!("{action}: {error}"))
}
fn durable(value: DirectorySync) -> Result<()> {
    if value == DirectorySync::Unsupported {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "operation authority requires supported directory durability",
        ));
    }
    Ok(())
}
fn require_successor(old: &Publication, new: &Publication) -> Result<()> {
    new.validate()?;
    if new.file_id != old.file_id || old.epoch.checked_add(1) != Some(new.epoch) {
        return Err(recovery(
            "ordinary operation must retain file identity and advance epoch exactly once",
        ));
    }
    Ok(())
}

/// Missing is permitted only while root has proved no normalized activation.
/// An existing authority is always parsed and validated, regardless of Presence.
pub(crate) fn load(
    fs: &VaultFs,
    vault: &RecordId,
    presence: Presence,
) -> Result<Option<Authority>> {
    let Some(bytes) = read_bytes(fs)? else {
        return match presence {
            Presence::Required => Err(recovery("required operation authority is missing")),
            Presence::LegacyMayBeAbsent => Ok(None),
        };
    };
    let state: State = serde_json::from_slice(&bytes)
        .map_err(|_| recovery("operation authority JSON is malformed"))?;
    state.validate(vault)?;
    Ok(Some(Authority {
        state,
        hash: Blake3Hash::digest(bytes),
    }))
}

/// Caller has reconciled legacy pending changes and holds the selected initial
/// publication proof. Required+missing is never healed by activation. Existing
/// matching idle state is retained, including its revision, and re-synced.
pub(crate) fn activate(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    publication: Publication,
    presence: Presence,
) -> Result<Authority> {
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    publication.validate()?;
    if let Some(existing) = load(fs, vault, presence)? {
        existing.require_idle()?;
        if existing.publication() != &publication {
            return Err(conflict(
                "activation cannot reset the acknowledged publication",
            ));
        }
        sync_existing(fs)?;
        if load(fs, vault, Presence::Required)?.as_ref() != Some(&existing) {
            return Err(conflict(
                "authority changed while reestablishing activation durability",
            ));
        }
        return Ok(existing);
    }
    persist(
        fs,
        writer,
        None,
        State {
            version: 1,
            vault_id: vault.clone(),
            revision: 1,
            publication,
            active: None,
        },
    )
}

pub(crate) fn begin(
    fs: &VaultFs,
    writer: &WriterPermit,
    expected: &Authority,
    change: PreparedChange,
    intended: Publication,
) -> Result<Authority> {
    expected.require_idle()?;
    require_successor(expected.publication(), &intended)?;
    let mut next = expected.next()?;
    next.active = Some(ActiveOperation {
        change,
        starting: expected.publication().clone(),
        intended,
    });
    persist(fs, writer, Some(expected), next)
}
/// Caller proves exact canonical terminal receipt and committed publication
/// immediately before this call under the same writer permit.
pub(crate) fn acknowledge(
    fs: &VaultFs,
    writer: &WriterPermit,
    expected: &Authority,
    change: &PreparedChange,
    committed: Publication,
) -> Result<Authority> {
    if expected.require_change(change)?.intended != committed {
        return Err(conflict(
            "acknowledged publication differs from reserved intent",
        ));
    }
    let mut next = expected.next()?;
    next.publication = committed;
    next.active = None;
    persist(fs, writer, Some(expected), next)
}
/// Caller proves no canonical mutation occurred, or rollback restored all
/// canonical state and left the starting catalog publication intact.
pub(crate) fn cancel(
    fs: &VaultFs,
    writer: &WriterPermit,
    expected: &Authority,
    change: &PreparedChange,
) -> Result<Authority> {
    expected.require_change(change)?;
    let mut next = expected.next()?;
    next.active = None;
    persist(fs, writer, Some(expected), next)
}
/// Explicit rebuild publication only. Root supplies a completed replacement
/// proof and coordinates selector publication; ordinary refresh uses begin/ack.
pub(crate) fn publish_rebuild(
    fs: &VaultFs,
    writer: &WriterPermit,
    expected: &Authority,
    publication: Publication,
) -> Result<Authority> {
    expected.require_idle()?;
    publication.validate()?;
    if publication.file_id == expected.publication().file_id
        || publication.epoch <= expected.publication().epoch
    {
        return Err(conflict(
            "rebuild must advance epoch and replace file identity",
        ));
    }
    let mut next = expected.next()?;
    next.publication = publication;
    persist(fs, writer, Some(expected), next)
}

#[cfg(test)]
thread_local! { static INSPECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
fn inspect(path: &Path) -> std::io::Result<fs::Metadata> {
    #[cfg(test)]
    INSPECTIONS.with(|n| n.set(n.get() + 1));
    fs::symlink_metadata(path)
}
/// Fixed, reserved state names: no directory enumeration or portable sibling
/// scans. WriterPermit already creates .wiki/state before writes begin.
fn authority_path(fs: &VaultFs) -> Result<PathBuf> {
    let mut path = fs.root().path().to_path_buf();
    for component in [".wiki", "state"] {
        path.push(component);
        match inspect(&path) {
            Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                return Err(recovery(
                    "operation authority ancestor is not a real directory",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io("inspect authority ancestor", e)),
        }
        match inspect(&path.join("WIKI.md")) {
            Ok(_) => {
                return Err(recovery(
                    "operation authority namespace contains a vault marker",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io("inspect authority boundary", e)),
        }
    }
    path.push(NAME);
    match inspect(&path) {
        Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
            return Err(recovery("operation authority must be a regular file"));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io("inspect operation authority", e)),
    }
    Ok(path)
}
struct Parents {
    #[cfg(unix)]
    fs: VaultFs,
    #[cfg(unix)]
    route: descriptor::Route,
    #[cfg(unix)]
    legacy_handles: Option<Vec<(PathBuf, File)>>,
    #[cfg(windows)]
    guard: crate::vault::windows_security::DirectoryGuard,
}
impl Parents {
    fn open(fs: &VaultFs, path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            if !fs.uses_descriptor_relative() {
                let result = Self {
                    fs: fs.clone(),
                    route: descriptor::Route::legacy(path.to_owned()),
                    legacy_handles: Some(legacy::parents(path)?),
                };
                result.verify()?;
                return Ok(result);
            }
            let relative = path
                .strip_prefix(fs.root().path())
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| recovery("authority path escapes vault"))?;
            let route = fs.bind(&VaultRelativePath::new(relative)?, true)?;
            let result = Self {
                fs: fs.clone(),
                route,
                legacy_handles: None,
            };
            result.verify()?;
            Ok(result)
        }
        #[cfg(windows)]
        {
            let _ = fs;
            let guard = crate::vault::windows_security::open_pinned_directory(
                path.parent().expect("state parent"),
                crate::vault::acl_policy::Protection::Private,
            )
            .map_err(|e| io("pin authority ancestor", e))?;
            Ok(Self { guard })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (fs, path);
            Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "operation authority file protection unavailable",
            ))
        }
    }
    fn verify(&self) -> Result<()> {
        #[cfg(unix)]
        {
            if let Some(handles) = &self.legacy_handles {
                for (path, file) in handles {
                    legacy::same_file(path, file, true)?;
                }
                return Ok(());
            }
            let path = authority_path(&self.fs)?;
            let relative = path
                .strip_prefix(self.fs.root().path())
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| recovery("authority path escapes vault"))?;
            let fresh = self.fs.bind(&VaultRelativePath::new(relative)?, true)?;
            self.route.same_parent(&fresh)?;
        }
        #[cfg(windows)]
        self.guard
            .verify_binding()
            .map_err(|e| io("verify authority ancestor", e))?;
        Ok(())
    }
    #[cfg(unix)]
    fn open_file(&self, path: &Path, writable: bool) -> Result<File> {
        if self.legacy_handles.is_some() {
            return legacy::open_file(path, writable);
        }
        self.verify()?;
        if path.parent() != self.route.path.parent() {
            return Err(recovery("authority leaf has another parent"));
        }
        descriptor::open_at(
            self.route
                .parent()
                .map_err(|e| io("open bound authority parent", e))?,
            path.file_name().expect("authority leaf"),
            if writable {
                libc::O_RDWR
            } else {
                libc::O_RDONLY
            },
            0,
        )
        .map_err(|e| io("open operation authority", e))
    }
    fn same_file(&self, path: &Path, file: &File) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if self.legacy_handles.is_some() {
                return legacy::same_file(path, file, false);
            }
            self.verify()?;
            let relative = path
                .strip_prefix(self.fs.root().path())
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| recovery("authority leaf escapes vault"))?;
            let route = self.fs.bind(&VaultRelativePath::new(relative)?, true)?;
            route.same_held_parent(
                self.route
                    .parent()
                    .map_err(|e| io("open held authority parent", e))?,
            )?;
            route.require_file_identity(file)?;
            if file
                .metadata()
                .map_err(|e| io("inspect authority hardlinks", e))?
                .nlink()
                != 1
            {
                return Err(recovery(
                    "authority handle binding changed or has unsafe type",
                ));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            same_file(path, file, false)
        }
    }
}
#[cfg(not(unix))]
fn same_file(path: &Path, file: &File, directory: bool) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = directory;
        crate::vault::windows_security::validate_same_file(
            file,
            path,
            crate::vault::acl_policy::Protection::Private,
        )
        .map_err(|e| io("verify protected authority", e))?;
    }
    Ok(())
}
fn read_bytes(fs: &VaultFs) -> Result<Option<Vec<u8>>> {
    let path = authority_path(fs)?;
    match inspect(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            if fs.uses_descriptor_relative() {
                fs.bind(
                    &VaultRelativePath::new(".wiki/state/operations.json")?,
                    true,
                )?;
            }
            return Ok(None);
        }
        Err(e) => return Err(io("inspect authority size", e)),
        Ok(m) if m.len() > MAX_BYTES as u64 => {
            return Err(recovery("operation authority exceeds 4096 bytes"));
        }
        Ok(_) => {}
    }
    let parents = Parents::open(fs, &path)?;
    #[cfg(test)]
    crate::vault::maintenance_read_tests::before_open(&path);
    #[cfg(unix)]
    let mut file = parents.open_file(&path, false)?;
    #[cfg(windows)]
    let mut file = open_file(&path, false)?;
    #[cfg(not(any(unix, windows)))]
    return Err(WikiError::new(
        ErrorCode::CapabilityUnavailable,
        "operation authority file protection unavailable",
    ));
    #[cfg(any(unix, windows))]
    {
        parents.same_file(&path, &file)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io("read operation authority", e))?;
        #[cfg(test)]
        crate::catalog::query_diagnostics::read("operation-authority", &path, bytes.len());
        parents.verify()?;
        parents.same_file(&path, &file)?;
        if bytes.len() > MAX_BYTES {
            return Err(recovery("operation authority exceeds 4096 bytes"));
        }
        Ok(Some(bytes))
    }
}
#[cfg(windows)]
fn open_file(path: &Path, writable: bool) -> Result<File> {
    #[cfg(windows)]
    {
        crate::vault::windows_security::open_checked_file(
            path,
            crate::vault::acl_policy::Protection::Private,
            if writable {
                crate::vault::windows_security::Sharing::Lock
            } else {
                crate::vault::windows_security::Sharing::ReadOnly
            },
            writable,
        )
        .map(|checked| checked.into_file())
        .map_err(|e| io("open operation authority", e))
    }
}
fn sync_existing(fs: &VaultFs) -> Result<()> {
    let path = authority_path(fs)?;
    let parents = Parents::open(fs, &path)?;
    #[cfg(unix)]
    let file = parents.open_file(&path, true)?;
    #[cfg(windows)]
    let file = open_file(&path, true)?;
    #[cfg(not(any(unix, windows)))]
    let file = File::open(&path).map_err(|e| io("open authority for durability", e))?;
    parents.same_file(&path, &file)?;
    fs.durable_io()
        .sync_file(&file)
        .map_err(|e| io("sync retained authority", e))?;
    parents.verify()?;
    sync_activation_directories(fs)
}
// Activation can encounter ancestors left by an interrupted directory
// creation. Reestablish their entries before acknowledging the first state.
fn sync_activation_directories(fs: &VaultFs) -> Result<()> {
    #[cfg(unix)]
    if fs.uses_descriptor_relative() {
        // Each parent corresponds to the original root/.wiki/.wiki/state sync,
        // without introducing a reserved synthetic leaf or pathname open.
        for child in [".wiki", ".wiki/state", ".wiki/state/operations.json"] {
            let route = fs.bind(&VaultRelativePath::new(child)?, true)?;
            durable(
                fs.durable_io()
                    .sync_directory_file(
                        route
                            .parent()
                            .map_err(|e| io("open activation directory", e))?,
                    )
                    .map_err(|e| io("sync authority activation ancestry", e))?,
            )?;
        }
        return Ok(());
    }
    for path in [
        fs.root().path().to_path_buf(),
        fs.root().path().join(".wiki"),
        fs.root().path().join(".wiki/state"),
    ] {
        durable(
            fs.durable_io()
                .sync_directory(&path)
                .map_err(|e| io("sync authority activation ancestry", e))?,
        )?;
    }
    Ok(())
}
struct Stage {
    path: PathBuf,
    io: Arc<dyn DurableIo>,
    #[cfg(unix)]
    parent: Option<File>,
}
impl Drop for Stage {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(parent) = self.parent.as_ref() {
            let _ = self
                .io
                .remove_at(parent, self.path.file_name().expect("authority stage leaf"));
            return;
        }
        let _ = self.io.remove(&self.path);
    }
}
fn persist(
    fs: &VaultFs,
    writer: &WriterPermit,
    expected: Option<&Authority>,
    next: State,
) -> Result<Authority> {
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    next.validate(&next.vault_id)?;
    let path = authority_path(fs)?;
    let parents = Parents::open(fs, &path)?;
    let check = || -> Result<()> {
        let found = load(fs, &next.vault_id, Presence::LegacyMayBeAbsent)?;
        if found.as_ref() != expected {
            return Err(conflict("operation authority compare-and-swap failed"));
        }
        Ok(())
    };
    check()?;
    let bytes =
        serde_json::to_vec(&next).map_err(|_| recovery("cannot serialize operation authority"))?;
    if bytes.len() > MAX_BYTES {
        return Err(recovery("operation authority exceeds 4096 bytes"));
    }
    let io = fs.durable_io();
    let parent = path.parent().expect("state parent");
    // Refuse unsupported durability before publishing any new authority.
    if expected.is_none() {
        sync_activation_directories(fs)?;
    }
    #[cfg(unix)]
    let synced = if fs.uses_descriptor_relative() {
        io.sync_directory_file(
            parents
                .route
                .parent()
                .map_err(|e| io_error("open authority parent", e))?,
        )
    } else {
        io.sync_directory(parent)
    };
    #[cfg(not(unix))]
    let synced = io.sync_directory(parent);
    durable(synced.map_err(|e| io_error("check authority directory durability", e))?)?;
    let stage_path = parent.join(format!(".operations-{}.tmp", uuid::Uuid::now_v7()));
    // Only own cleanup after successful exclusive creation. An injected error
    // after creation may leave a harmless unselected stage, never an idle proof.
    #[cfg(unix)]
    let stage_parent = if fs.uses_descriptor_relative() {
        Some(
            parents
                .route
                .parent()
                .map_err(|e| io_error("open authority stage parent", e))?
                .try_clone()
                .map_err(|e| io_error("retain authority stage parent", e))?,
        )
    } else {
        None
    };
    #[cfg(unix)]
    let created = if let Some(parent) = stage_parent.as_ref() {
        io.create_private_stage_at(
            parent,
            stage_path.file_name().expect("authority stage leaf"),
        )
    } else {
        io.create_private_stage(&stage_path)
    };
    #[cfg(not(unix))]
    let created = io.create_private_stage(&stage_path);
    let mut file = created.map_err(|e| io_error("create authority stage", e))?;
    let stage = Stage {
        path: stage_path,
        io: io.clone(),
        #[cfg(unix)]
        parent: stage_parent,
    };
    parents.same_file(&stage.path, &file)?;
    io.write_stage(&mut file, &bytes)
        .map_err(|e| io_error("write authority stage", e))?;
    io.sync_file(&file)
        .map_err(|e| io_error("sync authority stage", e))?;
    parents.verify()?;
    parents.same_file(&stage.path, &file)?;
    // Close the stage for Windows replacement while retaining directory guards.
    drop(file);
    #[cfg(any(unix, windows))]
    {
        #[cfg(unix)]
        let mut staged = parents.open_file(&stage.path, false)?;
        #[cfg(windows)]
        let mut staged = open_file(&stage.path, false)?;
        parents.same_file(&stage.path, &staged)?;
        let mut stored = Vec::new();
        (&mut staged)
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut stored)
            .map_err(|e| io_error("verify authority stage", e))?;
        if stored != bytes {
            return Err(conflict("authority stage payload changed"));
        }
        parents.verify()?;
    }
    check()?;
    #[cfg(unix)]
    let replaced = if fs.uses_descriptor_relative() {
        parents.verify()?;
        let parent = parents
            .route
            .parent()
            .map_err(|e| io_error("open authority replacement parent", e))?;
        io.replace_at(
            stage
                .parent
                .as_ref()
                .expect("relative private stage parent"),
            stage.path.file_name().expect("authority stage leaf"),
            parent,
            path.file_name().expect("authority leaf"),
        )
    } else {
        io.replace(&stage.path, &path)
    };
    #[cfg(not(unix))]
    let replaced = io.replace(&stage.path, &path);
    replaced.map_err(|e| io_error("replace operation authority", e))?;
    parents.verify()?;
    #[cfg(unix)]
    let synced = if fs.uses_descriptor_relative() {
        io.sync_directory_file(
            parents
                .route
                .parent()
                .map_err(|e| io_error("open authority replacement directory", e))?,
        )
    } else {
        io.sync_directory(parent)
    };
    #[cfg(not(unix))]
    let synced = io.sync_directory(parent);
    durable(synced.map_err(|e| io_error("sync authority replacement", e))?)?;
    let result = Authority {
        state: next,
        hash: Blake3Hash::digest(bytes),
    };
    if load(fs, &result.state.vault_id, Presence::Required)?.as_ref() != Some(&result) {
        return Err(conflict("operation authority changed after replacement"));
    }
    Ok(result)
}
fn io_error(action: &str, error: std::io::Error) -> WikiError {
    io(action, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{NativeIo, VaultRoot};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    fn vault() -> RecordId {
        RecordId::new("vault_authority").unwrap()
    }
    fn publication(epoch: u64) -> Publication {
        Publication {
            file_id: "a".repeat(32),
            epoch,
        }
    }
    fn change() -> PreparedChange {
        PreparedChange {
            change_id: RecordId::new("change_one").unwrap(),
            manifest_hash: Blake3Hash::digest("manifest one"),
        }
    }
    fn fixture() -> (tempfile::TempDir, VaultFs, WriterPermit) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"),"---\nwiki_schema: \"1\"\nwiki_id: vault_authority\nwiki_kind: vault\ntitle: Authority\n---\nWiki\n").unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(vault.root(), Duration::from_secs(1)).unwrap();
        (temp, vault, writer)
    }
    fn initial(fs: &VaultFs, writer: &WriterPermit) -> Authority {
        activate(
            fs,
            writer,
            &vault(),
            publication(1),
            Presence::LegacyMayBeAbsent,
        )
        .unwrap()
    }
    fn current(fs: &VaultFs) -> Authority {
        load(fs, &vault(), Presence::Required).unwrap().unwrap()
    }
    fn path(fs: &VaultFs) -> PathBuf {
        fs.root().path().join(".wiki/state/operations.json")
    }
    #[test]
    fn exact_transitions_floor_and_old_tokens() {
        let (_temp, fs, writer) = fixture();
        let idle = initial(&fs, &writer);
        assert_eq!(idle.revision(), 1);
        idle.require_publication(&publication(1)).unwrap();
        idle.require_publication(&publication(2)).unwrap();
        let active = begin(&fs, &writer, &idle, change(), publication(2)).unwrap();
        assert_eq!(active.revision(), 2);
        assert_eq!(active.active().unwrap().starting, publication(1));
        assert!(active.require_publication(&publication(1)).is_err());
        assert!(begin(&fs, &writer, &idle, change(), publication(2)).is_err());
        assert!(begin(&fs, &writer, &active, change(), publication(2)).is_err());
        let mut other = change();
        other.manifest_hash = Blake3Hash::digest("different manifest");
        assert!(cancel(&fs, &writer, &active, &other).is_err());
        assert!(acknowledge(&fs, &writer, &active, &other, publication(2)).is_err());
        assert!(acknowledge(&fs, &writer, &active, &change(), publication(3)).is_err());
        let acknowledged = acknowledge(&fs, &writer, &active, &change(), publication(2)).unwrap();
        assert_eq!(acknowledged.revision(), 3);
        assert_eq!(acknowledged.publication(), &publication(2));
        assert!(acknowledged.active().is_none());
        assert!(acknowledged.require_publication(&publication(1)).is_err());
        assert!(cancel(&fs, &writer, &active, &change()).is_err());
        assert!(!idle.same_revision(&acknowledged));
        assert!(acknowledged.same_revision(&current(&fs)));
        let active = begin(&fs, &writer, &acknowledged, change(), publication(3)).unwrap();
        let canceled = cancel(&fs, &writer, &active, &change()).unwrap();
        assert_eq!(canceled.publication(), &publication(2));
        assert_eq!(canceled.revision(), 5);
        let replacement = Publication {
            file_id: "b".repeat(32),
            epoch: 3,
        };
        let rebuilt = publish_rebuild(&fs, &writer, &canceled, replacement.clone()).unwrap();
        assert_eq!(rebuilt.publication(), &replacement);
        assert!(rebuilt.require_publication(&publication(3)).is_err());
        rebuilt.require_publication(&replacement).unwrap();
    }
    #[test]
    fn activation_never_resets_or_heals_required_absence() {
        let (_temp, fs, writer) = fixture();
        assert!(
            load(&fs, &vault(), Presence::LegacyMayBeAbsent)
                .unwrap()
                .is_none()
        );
        assert!(load(&fs, &vault(), Presence::Required).is_err());
        assert!(activate(&fs, &writer, &vault(), publication(1), Presence::Required).is_err());
        let idle = initial(&fs, &writer);
        let again = activate(&fs, &writer, &vault(), publication(1), Presence::Required).unwrap();
        assert_eq!(idle, again);
        assert!(
            activate(
                &fs,
                &writer,
                &vault(),
                publication(2),
                Presence::LegacyMayBeAbsent
            )
            .is_err()
        );
        assert!(
            load(
                &fs,
                &RecordId::new("vault_foreign").unwrap(),
                Presence::LegacyMayBeAbsent
            )
            .is_err()
        );
        let active = begin(&fs, &writer, &idle, change(), publication(2)).unwrap();
        assert!(
            activate(
                &fs,
                &writer,
                &vault(),
                publication(1),
                Presence::LegacyMayBeAbsent
            )
            .is_err()
        );
        assert_eq!(current(&fs), active);
        fs::remove_file(path(&fs)).unwrap();
        assert!(load(&fs, &vault(), Presence::Required).is_err());
        assert!(activate(&fs, &writer, &vault(), publication(1), Presence::Required).is_err());
    }
    #[test]
    fn invalid_ids_epochs_revisions_and_json_fail_closed() {
        let (_temp, fs, writer) = fixture();
        let idle = initial(&fs, &writer);
        for epoch in [0, i64::MAX as u64 + 1] {
            assert!(begin(&fs, &writer, &idle, change(), publication(epoch)).is_err());
        }
        for id in ["../escape".to_string(), "A".repeat(32), "a".repeat(31)] {
            assert!(
                publish_rebuild(
                    &fs,
                    &writer,
                    &idle,
                    Publication {
                        file_id: id,
                        epoch: 2
                    }
                )
                .is_err()
            );
        }
        assert!(begin(&fs, &writer, &idle, change(), publication(3)).is_err());
        assert!(
            begin(
                &fs,
                &writer,
                &idle,
                change(),
                Publication {
                    file_id: "b".repeat(32),
                    epoch: 2
                }
            )
            .is_err()
        );
        assert!(publish_rebuild(&fs, &writer, &idle, publication(2)).is_err());
        let mut value = serde_json::to_value(&idle.state).unwrap();
        let original = value.clone();
        for (key, bad) in [
            ("revision", serde_json::json!(0)),
            ("version", serde_json::json!(9)),
            ("vault_id", serde_json::json!("vault_foreign")),
        ] {
            value = original.clone();
            value[key] = bad;
            fs::write(path(&fs), serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(load(&fs, &vault(), Presence::LegacyMayBeAbsent).is_err());
        }
        for bytes in [
            b"{".to_vec(),
            vec![b' '; MAX_BYTES + 1],
            b"{\"version\":1,\"version\":1}".to_vec(),
        ] {
            fs::write(path(&fs), bytes).unwrap();
            assert!(load(&fs, &vault(), Presence::LegacyMayBeAbsent).is_err());
        }
        let mut max = idle.state.clone();
        max.revision = u64::MAX;
        fs::write(path(&fs), serde_json::to_vec(&max).unwrap()).unwrap();
        assert!(begin(&fs, &writer, &current(&fs), change(), publication(2)).is_err());
        max.revision = 1;
        max.publication.epoch = i64::MAX as u64;
        fs::write(path(&fs), serde_json::to_vec(&max).unwrap()).unwrap();
        assert!(
            begin(
                &fs,
                &writer,
                &current(&fs),
                change(),
                publication(i64::MAX as u64 + 1)
            )
            .is_err()
        );
    }
    #[test]
    fn byte_hash_cas_rejects_semantically_equal_rewrites_and_foreign_writer() {
        let (_temp, fs, writer) = fixture();
        let idle = initial(&fs, &writer);
        fs::write(
            path(&fs),
            serde_json::to_string_pretty(&idle.state).unwrap(),
        )
        .unwrap();
        assert_eq!(current(&fs).revision(), idle.revision());
        assert!(!current(&fs).same_revision(&idle));
        assert_eq!(
            begin(&fs, &writer, &idle, change(), publication(2))
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        let (_other, other_fs, other_writer) = fixture();
        assert!(begin(&fs, &other_writer, &current(&fs), change(), publication(2)).is_err());
        assert!(
            activate(
                &other_fs,
                &writer,
                &vault(),
                publication(1),
                Presence::LegacyMayBeAbsent
            )
            .is_err()
        );
    }
    #[test]
    #[cfg(unix)]
    fn symlinks_hardlinks_and_nested_markers_are_refused() {
        use std::os::unix::fs::symlink;
        for component in [".wiki", ".wiki/state", ".wiki/state/operations.json"] {
            let (_temp, fs, writer) = fixture();
            initial(&fs, &writer);
            let target = fs.root().path().join(component);
            let displaced = target.with_extension("retained");
            fs::rename(&target, &displaced).unwrap();
            symlink(&displaced, &target).unwrap();
            assert!(load(&fs, &vault(), Presence::LegacyMayBeAbsent).is_err());
        }
        let (_temp, fs, writer) = fixture();
        initial(&fs, &writer);
        fs::hard_link(path(&fs), fs.root().path().join("second.json")).unwrap();
        assert!(load(&fs, &vault(), Presence::Required).is_err());
        fs::remove_file(fs.root().path().join("second.json")).unwrap();
        for directory in [".wiki", ".wiki/state"] {
            let marker = fs.root().path().join(directory).join("WIKI.md");
            fs::write(&marker, b"unexpected").unwrap();
            assert!(load(&fs, &vault(), Presence::Required).is_err());
            fs::remove_file(marker).unwrap();
        }
    }
    #[test]
    fn authority_load_is_independent_of_retained_history_and_unrelated_names() {
        let (_temp, fs, writer) = fixture();
        initial(&fs, &writer);
        let count = || {
            INSPECTIONS.with(|n| n.set(0));
            let value = current(&fs);
            let n = INSPECTIONS.with(|n| n.get());
            (value, n)
        };
        let before = count();
        let history = fs.root().path().join(".wiki/retained/changes");
        fs::create_dir_all(&history).unwrap();
        for index in 0..512 {
            fs::write(
                history.join(format!("old-{index}")),
                b"unreadable history is irrelevant",
            )
            .unwrap();
            fs::write(
                fs.root()
                    .path()
                    .join(".wiki/state")
                    .join(format!("other-{index}")),
                b"unrelated",
            )
            .unwrap();
        }
        assert_eq!(count(), before);
    }
    #[test]
    fn load_has_no_filesystem_writes_or_creation() {
        let temp = tempfile::tempdir().unwrap();
        let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
        assert!(
            load(&fs, &vault(), Presence::LegacyMayBeAbsent)
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
        let (_temp, fs, writer) = fixture();
        initial(&fs, &writer);
        let snapshot = || {
            let directory = fs.root().path().join(".wiki/state");
            let mut values = std::fs::read_dir(directory)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (
                        entry.path(),
                        std::fs::read(entry.path()).unwrap(),
                        entry.metadata().unwrap().modified().unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            values.sort_by(|a, b| a.0.cmp(&b.0));
            values
        };
        let before = snapshot();
        current(&fs);
        assert_eq!(snapshot(), before);
    }
    #[test]
    fn persisted_active_binding_cannot_violate_start_and_intent_invariants() {
        let (_temp, fs, writer) = fixture();
        let idle = initial(&fs, &writer);
        let active = begin(&fs, &writer, &idle, change(), publication(2)).unwrap();
        for which in 0..5 {
            let mut altered = active.state.clone();
            match which {
                0 => altered.active.as_mut().unwrap().starting.epoch = 2,
                1 => altered.active.as_mut().unwrap().intended.epoch = 1,
                2 => altered.active.as_mut().unwrap().intended.file_id = "b".repeat(32),
                3 => altered.revision = 1,
                _ => altered.active.as_mut().unwrap().intended.epoch = i64::MAX as u64 + 1,
            }
            std::fs::write(path(&fs), serde_json::to_vec(&altered).unwrap()).unwrap();
            assert!(load(&fs, &vault(), Presence::Required).is_err());
        }
    }
    struct Fault {
        relative: bool,
        cleanup_fault: Option<bool>,
        cleanup_count: AtomicUsize,
        at: usize,
        after: bool,
        count: AtomicUsize,
        unsupported: bool,
    }
    impl Fault {
        fn call<T>(&self, f: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
            let hit = self.count.fetch_add(1, Ordering::SeqCst) + 1 == self.at;
            if hit && !self.after {
                return Err(std::io::Error::other("injected before"));
            }
            let value = f()?;
            if hit {
                return Err(std::io::Error::other("injected after"));
            }
            Ok(value)
        }
    }
    impl DurableIo for Fault {
        #[cfg(unix)]
        fn descriptor_relative(&self) -> bool {
            self.relative && NativeIo.descriptor_relative()
        }
        #[cfg(unix)]
        fn create_stage_at(&self, parent: &File, leaf: &std::ffi::OsStr) -> std::io::Result<File> {
            self.call(|| NativeIo.create_stage_at(parent, leaf))
        }
        #[cfg(unix)]
        fn create_private_stage_at(
            &self,
            parent: &File,
            leaf: &std::ffi::OsStr,
        ) -> std::io::Result<File> {
            self.call(|| NativeIo.create_private_stage_at(parent, leaf))
        }
        #[cfg(unix)]
        fn create_private_directory_at(
            &self,
            parent: &File,
            leaf: &std::ffi::OsStr,
        ) -> std::io::Result<()> {
            NativeIo.create_private_directory_at(parent, leaf)
        }
        #[cfg(unix)]
        fn open_append_at(&self, parent: &File, leaf: &std::ffi::OsStr) -> std::io::Result<File> {
            NativeIo.open_append_at(parent, leaf)
        }
        #[cfg(unix)]
        fn replace_at(
            &self,
            a: &File,
            from: &std::ffi::OsStr,
            b: &File,
            to: &std::ffi::OsStr,
        ) -> std::io::Result<()> {
            self.call(|| NativeIo.replace_at(a, from, b, to))
        }
        #[cfg(unix)]
        fn remove_at(&self, parent: &File, leaf: &std::ffi::OsStr) -> std::io::Result<()> {
            self.cleanup_count.fetch_add(1, Ordering::SeqCst);
            if self.cleanup_fault == Some(false) {
                return Err(std::io::Error::other("cleanup before"));
            }
            NativeIo.remove_at(parent, leaf)?;
            if self.cleanup_fault == Some(true) {
                return Err(std::io::Error::other("cleanup after"));
            }
            Ok(())
        }
        #[cfg(unix)]
        fn create_directory_at(
            &self,
            parent: &File,
            leaf: &std::ffi::OsStr,
        ) -> std::io::Result<()> {
            NativeIo.create_directory_at(parent, leaf)
        }
        #[cfg(unix)]
        fn sync_directory_file(&self, directory: &File) -> std::io::Result<DirectorySync> {
            if self.unsupported {
                Ok(DirectorySync::Unsupported)
            } else {
                self.call(|| NativeIo.sync_directory_file(directory))
            }
        }
        fn create_stage(&self, p: &Path) -> std::io::Result<File> {
            assert!(
                !self.relative,
                "relative authority adapter reached pathname mutation"
            );
            self.call(|| NativeIo.create_stage(p))
        }
        fn create_private_stage(&self, p: &Path) -> std::io::Result<File> {
            assert!(
                !self.relative,
                "relative authority adapter reached pathname mutation"
            );
            self.call(|| NativeIo.create_private_stage(p))
        }
        fn open_append(&self, p: &Path) -> std::io::Result<File> {
            NativeIo.open_append(p)
        }
        fn truncate_file(&self, f: &File, n: u64) -> std::io::Result<()> {
            NativeIo.truncate_file(f, n)
        }
        fn write_stage(&self, f: &mut File, b: &[u8]) -> std::io::Result<()> {
            if self.cleanup_fault.is_some() {
                return Err(std::io::Error::other("force staged cleanup"));
            }
            self.call(|| NativeIo.write_stage(f, b))
        }
        fn sync_file(&self, f: &File) -> std::io::Result<()> {
            self.call(|| NativeIo.sync_file(f))
        }
        fn replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
            assert!(
                !self.relative,
                "relative authority adapter reached pathname mutation"
            );
            self.call(|| NativeIo.replace(a, b))
        }
        fn remove(&self, p: &Path) -> std::io::Result<()> {
            assert!(
                !self.relative,
                "relative authority adapter reached pathname mutation"
            );
            NativeIo.remove(p)
        }
        fn create_directory(&self, p: &Path) -> std::io::Result<()> {
            NativeIo.create_directory(p)
        }
        fn sync_directory(&self, p: &Path) -> std::io::Result<DirectorySync> {
            assert!(
                !self.relative,
                "relative authority adapter reached pathname mutation"
            );
            if self.unsupported {
                Ok(DirectorySync::Unsupported)
            } else {
                self.call(|| NativeIo.sync_directory(p))
            }
        }
    }
    #[test]
    fn every_replacement_fault_leaves_old_or_new_complete_authority() {
        for at in 1..=6 {
            for after in [false, true] {
                let (_temp, native, writer) = fixture();
                let idle = initial(&native, &writer);
                let faulty = VaultFs::with_io(
                    native.root().clone(),
                    Arc::new(Fault {
                        relative: false,
                        cleanup_fault: None,
                        cleanup_count: AtomicUsize::new(0),
                        at,
                        after,
                        count: AtomicUsize::new(0),
                        unsupported: false,
                    }),
                );
                assert!(
                    begin(&faulty, &writer, &idle, change(), publication(2)).is_err(),
                    "{at} {after}"
                );
                let observed = current(&native);
                assert_eq!(observed.publication(), &publication(1));
                if let Some(active) = observed.active() {
                    assert_eq!(observed.revision(), 2);
                    assert_eq!(active.change, change());
                    assert_eq!(active.intended, publication(2));
                    // No canonical write occurred in this fixture; cancel proof is known.
                    cancel(&native, &writer, &observed, &change()).unwrap();
                } else {
                    assert_eq!(observed, idle);
                }
            }
        }
    }
    #[test]
    fn interrupted_activation_is_absent_or_complete_and_retry_preserves_state() {
        for at in 1..=9 {
            for after in [false, true] {
                let (_temp, native, writer) = fixture();
                let faulty = VaultFs::with_io(
                    native.root().clone(),
                    Arc::new(Fault {
                        relative: false,
                        cleanup_fault: None,
                        cleanup_count: AtomicUsize::new(0),
                        at,
                        after,
                        count: AtomicUsize::new(0),
                        unsupported: false,
                    }),
                );
                assert!(
                    activate(
                        &faulty,
                        &writer,
                        &vault(),
                        publication(1),
                        Presence::LegacyMayBeAbsent
                    )
                    .is_err(),
                    "{at} {after}"
                );
                if let Some(observed) =
                    load(&native, &vault(), Presence::LegacyMayBeAbsent).unwrap()
                {
                    assert_eq!(observed.revision(), 1);
                    assert_eq!(observed.publication(), &publication(1));
                    assert!(observed.active().is_none());
                    assert_eq!(
                        activate(
                            &native,
                            &writer,
                            &vault(),
                            publication(1),
                            Presence::LegacyMayBeAbsent
                        )
                        .unwrap(),
                        observed
                    );
                } else {
                    assert!(load(&native, &vault(), Presence::Required).is_err());
                    initial(&native, &writer);
                }
            }
        }
    }
    #[test]
    fn terminal_transition_faults_preserve_old_or_new_floor() {
        for operation in 0..3 {
            for at in 1..=6 {
                for after in [false, true] {
                    let (_temp, native, writer) = fixture();
                    let idle = initial(&native, &writer);
                    let expected = if operation < 2 {
                        begin(&native, &writer, &idle, change(), publication(2)).unwrap()
                    } else {
                        idle
                    };
                    let replacement = Publication {
                        file_id: "b".repeat(32),
                        epoch: 2,
                    };
                    let faulty = VaultFs::with_io(
                        native.root().clone(),
                        Arc::new(Fault {
                            relative: false,
                            cleanup_fault: None,
                            cleanup_count: AtomicUsize::new(0),
                            at,
                            after,
                            count: AtomicUsize::new(0),
                            unsupported: false,
                        }),
                    );
                    let result = match operation {
                        0 => acknowledge(&faulty, &writer, &expected, &change(), publication(2)),
                        1 => cancel(&faulty, &writer, &expected, &change()),
                        _ => publish_rebuild(&faulty, &writer, &expected, replacement.clone()),
                    };
                    assert!(result.is_err(), "{operation} {at} {after}");
                    let found = current(&native);
                    if found != expected {
                        assert_eq!(found.revision(), expected.revision() + 1);
                        assert!(found.active().is_none());
                        assert_eq!(
                            found.publication(),
                            &match operation {
                                0 => publication(2),
                                1 => publication(1),
                                _ => replacement,
                            }
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn unsupported_directory_durability_publishes_nothing() {
        let (_temp, native, writer) = fixture();
        let faulty = VaultFs::with_io(
            native.root().clone(),
            Arc::new(Fault {
                relative: false,
                cleanup_fault: None,
                cleanup_count: AtomicUsize::new(0),
                at: 0,
                after: false,
                count: AtomicUsize::new(0),
                unsupported: true,
            }),
        );
        let error = activate(
            &faulty,
            &writer,
            &vault(),
            publication(1),
            Presence::LegacyMayBeAbsent,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
        assert!(
            load(&native, &vault(), Presence::LegacyMayBeAbsent)
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_every_replacement_fault_leaves_old_or_new_complete_authority() {
        if !NativeIo.descriptor_relative() {
            return;
        }
        for at in 1..=6 {
            for after in [false, true] {
                let (_temp, native, writer) = fixture();
                let idle = initial(&native, &writer);
                let fault = Arc::new(Fault {
                    relative: true,
                    cleanup_fault: None,
                    cleanup_count: AtomicUsize::new(0),
                    at,
                    after,
                    count: AtomicUsize::new(0),
                    unsupported: false,
                });
                let faulty = VaultFs::with_io(native.root().clone(), fault.clone())
                    .for_maintenance_execution();
                assert!(
                    begin(&faulty, &writer, &idle, change(), publication(2)).is_err(),
                    "{at} {after}"
                );
                assert!(
                    fault.count.load(Ordering::SeqCst) >= at,
                    "unreached relative cut {at}"
                );
                let observed = current(&native);
                assert_eq!(observed.publication(), &publication(1));
                if let Some(active) = observed.active() {
                    assert_eq!(observed.revision(), 2);
                    assert_eq!(active.change, change());
                    assert_eq!(active.intended, publication(2));
                    // No canonical write occurred in this fixture; cancel proof is known.
                    cancel(&native, &writer, &observed, &change()).unwrap();
                } else {
                    assert_eq!(observed, idle);
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn relative_interrupted_activation_is_absent_or_complete_and_retry_preserves_state() {
        if !NativeIo.descriptor_relative() {
            return;
        }
        for at in 1..=9 {
            for after in [false, true] {
                let (_temp, native, writer) = fixture();
                let fault = Arc::new(Fault {
                    relative: true,
                    cleanup_fault: None,
                    cleanup_count: AtomicUsize::new(0),
                    at,
                    after,
                    count: AtomicUsize::new(0),
                    unsupported: false,
                });
                let faulty = VaultFs::with_io(native.root().clone(), fault.clone())
                    .for_maintenance_execution();
                assert!(
                    activate(
                        &faulty,
                        &writer,
                        &vault(),
                        publication(1),
                        Presence::LegacyMayBeAbsent
                    )
                    .is_err(),
                    "{at} {after}"
                );
                assert!(
                    fault.count.load(Ordering::SeqCst) >= at,
                    "unreached relative cut {at}"
                );
                if let Some(observed) =
                    load(&native, &vault(), Presence::LegacyMayBeAbsent).unwrap()
                {
                    assert_eq!(observed.revision(), 1);
                    assert_eq!(observed.publication(), &publication(1));
                    assert!(observed.active().is_none());
                    assert_eq!(
                        activate(
                            &native,
                            &writer,
                            &vault(),
                            publication(1),
                            Presence::LegacyMayBeAbsent
                        )
                        .unwrap(),
                        observed
                    );
                } else {
                    assert!(load(&native, &vault(), Presence::Required).is_err());
                    initial(&native, &writer);
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn relative_terminal_transition_faults_preserve_old_or_new_floor() {
        if !NativeIo.descriptor_relative() {
            return;
        }
        for operation in 0..3 {
            for at in 1..=6 {
                for after in [false, true] {
                    let (_temp, native, writer) = fixture();
                    let idle = initial(&native, &writer);
                    let expected = if operation < 2 {
                        begin(&native, &writer, &idle, change(), publication(2)).unwrap()
                    } else {
                        idle
                    };
                    let replacement = Publication {
                        file_id: "b".repeat(32),
                        epoch: 2,
                    };
                    let fault = Arc::new(Fault {
                        relative: true,
                        cleanup_fault: None,
                        cleanup_count: AtomicUsize::new(0),
                        at,
                        after,
                        count: AtomicUsize::new(0),
                        unsupported: false,
                    });
                    let faulty = VaultFs::with_io(native.root().clone(), fault.clone())
                        .for_maintenance_execution();
                    let result = match operation {
                        0 => acknowledge(&faulty, &writer, &expected, &change(), publication(2)),
                        1 => cancel(&faulty, &writer, &expected, &change()),
                        _ => publish_rebuild(&faulty, &writer, &expected, replacement.clone()),
                    };
                    assert!(result.is_err(), "{operation} {at} {after}");
                    assert!(
                        fault.count.load(Ordering::SeqCst) >= at,
                        "unreached relative cut {at}"
                    );
                    let found = current(&native);
                    if found != expected {
                        assert_eq!(found.revision(), expected.revision() + 1);
                        assert!(found.active().is_none());
                        assert_eq!(
                            found.publication(),
                            &match operation {
                                0 => publication(2),
                                1 => publication(1),
                                _ => replacement,
                            }
                        );
                    }
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn relative_private_cleanup_fault_preserves_authority_and_retry() {
        if !NativeIo.descriptor_relative() {
            return;
        }
        for after in [false, true] {
            let (_temp, native, writer) = fixture();
            let idle = initial(&native, &writer);
            let fault = Arc::new(Fault {
                relative: true,
                cleanup_fault: Some(after),
                cleanup_count: AtomicUsize::new(0),
                at: usize::MAX,
                after: false,
                count: AtomicUsize::new(0),
                unsupported: false,
            });
            let faulty =
                VaultFs::with_io(native.root().clone(), fault.clone()).for_maintenance_execution();
            assert!(begin(&faulty, &writer, &idle, change(), publication(2)).is_err());
            assert_eq!(
                fault.cleanup_count.load(Ordering::SeqCst),
                1,
                "private cleanup unlink cut was not reached"
            );
            assert_eq!(current(&native), idle);
            let unselected = fs::read_dir(native.root().path().join(".wiki/state"))
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_str()
                        .unwrap()
                        .starts_with(".operations-")
                })
                .count();
            assert_eq!(unselected, usize::from(!after));
            let active = begin(&native, &writer, &idle, change(), publication(2)).unwrap();
            assert_eq!(active.revision(), 2);
            assert_eq!(active.active().unwrap().change, change());
            cancel(&native, &writer, &active, &change()).unwrap();
            assert!(current(&native).active().is_none());
        }
    }
}

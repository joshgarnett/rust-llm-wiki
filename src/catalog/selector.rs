//! Durable sibling selection. Lock order: writer permit -> gate -> file lease.
//! Lease inodes are permanent; a read connection must drop before its lease.
use super::file_types::CatalogSelection;
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    vault::{DirectorySync, ExpectedState, VaultFs, WriterPermit},
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const CURRENT: CachePath<'static> = CachePath::Current;
const ACTIVE: CachePath<'static> = CachePath::Active;
const GATE: CachePath<'static> = CachePath::Gate;
const DIRECTORY: CachePath<'static> = CachePath::Catalogs;
const JSON_LIMIT: usize = 4096;

#[derive(Clone, Copy)]
enum FileKind {
    Database,
    Lease,
    Wal,
    Shm,
    Journal,
}
#[derive(Clone, Copy)]
enum CachePath<'a> {
    Current,
    Active,
    Gate,
    Catalogs,
    File(&'a str, FileKind),
}
impl CachePath<'_> {
    fn relative(self) -> Result<String> {
        Ok(match self {
            Self::Current => ".wiki/cache/catalog-current.json".into(),
            Self::Active => ".wiki/cache/catalog-v2-active.json".into(),
            Self::Gate => ".wiki/cache/catalog-acquisition.lock".into(),
            Self::Catalogs => ".wiki/cache/catalogs".into(),
            Self::File(id, kind) => {
                if id.len() != 32
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(corrupt(
                        "catalog file path requires a 32-character lower-case hex identity",
                    ));
                }
                let suffix = match kind {
                    FileKind::Database => ".sqlite",
                    FileKind::Lease => ".lock",
                    FileKind::Wal => ".sqlite-wal",
                    FileKind::Shm => ".sqlite-shm",
                    FileKind::Journal => ".sqlite-journal",
                };
                format!(".wiki/cache/catalogs/{id}{suffix}")
            }
        })
    }
}

#[cfg(test)]
thread_local! { static PATH_INSPECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
fn inspect_path(path: &Path) -> std::io::Result<fs::Metadata> {
    #[cfg(test)]
    PATH_INSPECTIONS.with(|count| count.set(count.get() + 1));
    fs::symlink_metadata(path)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    version: u32,
    vault_id: RecordId,
}

/// Declaration order is significant: T (including all statements/connections)
/// is destroyed before the OS lease. Never expose an operation taking T out.
pub(crate) struct Selected<T> {
    value: T,
    selection: CatalogSelection,
    _lease: HeldLock,
}
impl<T> Selected<T> {
    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn selection(&self) -> &CatalogSelection {
        &self.selection
    }
}
// Prevent moving a value out; expose shared access only so replacing the
// connection cannot accidentally release its lease while it remains open.
impl<T> Drop for Selected<T> {
    fn drop(&mut self) {}
}

fn corrupt(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn io(action: &str, error: std::io::Error) -> WikiError {
    corrupt(format!("{action}: {error}"))
}
fn durable(result: DirectorySync) -> Result<()> {
    if result == DirectorySync::Unsupported {
        Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "catalog selection requires directory durability",
        ))
    } else {
        Ok(())
    }
}

/// Only this reserved cache namespace uses direct generated-name lookups.
/// Authority comes from checked handles and the vault/file/header identity,
/// not from proving that unrelated case-folded siblings are absent.
fn path(fs: &VaultFs, entry: CachePath<'_>) -> Result<PathBuf> {
    let relative = entry.relative()?;
    let components = relative.split('/');
    let count = components.clone().count(); // at most four allowlisted components
    let mut path = fs.root().path().to_path_buf();
    for (index, component) in components.enumerate() {
        path.push(component);
        let directory = index + 1 < count || matches!(entry, CachePath::Catalogs);
        match inspect_path(&path) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || (directory && !metadata.is_dir())
                    || (!directory && !metadata.is_file()) =>
            {
                return Err(corrupt("owned catalog path has unsafe object type"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io("inspect owned catalog path", e)),
        }
        if directory {
            // A marker has no legitimate meaning inside this reserved cache.
            // Any object at this name refuses; no exact-case directory scan.
            match inspect_path(&path.join("WIKI.md")) {
                Ok(_) => return Err(corrupt("owned catalog ancestry contains a vault marker")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io("inspect owned catalog marker", e)),
            }
        }
    }
    Ok(path)
}

struct Checked {
    file: File,
    path: PathBuf,
    #[cfg(unix)]
    parents: Vec<(PathBuf, File)>,
    #[cfg(windows)]
    parents: Vec<crate::vault::windows_security::DirectoryGuard>,
}
impl Checked {
    fn open(path: &Path, create: bool, lock: bool) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut parents = Vec::new();
            for parent in path
                .ancestors()
                .skip(1)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                let file = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
                    .open(parent)
                    .map_err(|e| io("pin catalog ancestor", e))?;
                parents.push((parent.to_path_buf(), file));
            }
            let file = fs::OpenOptions::new()
                .read(true)
                .write(create)
                .create_new(create)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(path)
                .map_err(|e| io("open catalog handle", e))?;
            let checked = Self {
                file,
                path: path.to_path_buf(),
                parents,
            };
            let _ = lock;
            checked.verify()?;
            Ok(checked)
        }
        #[cfg(windows)]
        {
            use crate::vault::{
                acl_policy::Protection,
                windows_security::{self, Sharing},
            };
            let checked = if create {
                windows_security::create_private_file(path, Sharing::Lock)
            } else {
                windows_security::open_checked_file(
                    path,
                    Protection::IntegrityProtected,
                    if lock {
                        Sharing::Lock
                    } else {
                        Sharing::ReadOnly
                    },
                    false,
                )
            }
            .map_err(|e| io("open protected catalog handle", e))?;
            let (file, parents) = checked.into_parts();
            Ok(Self {
                file,
                path: path.to_path_buf(),
                parents,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (path, create, lock);
            Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "catalog file identity unavailable",
            ))
        }
    }
    fn verify(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            fn same(path: &Path, file: &File, directory: bool) -> Result<()> {
                let held = file
                    .metadata()
                    .map_err(|e| io("inspect catalog handle", e))?;
                let named = inspect_path(path).map_err(|e| io("inspect catalog binding", e))?;
                if named.file_type().is_symlink()
                    || held.dev() != named.dev()
                    || held.ino() != named.ino()
                    || if directory {
                        !held.is_dir()
                    } else {
                        !held.is_file() || held.nlink() != 1
                    }
                {
                    return Err(corrupt(
                        "catalog handle/path binding changed or unsafe type",
                    ));
                }
                Ok(())
            }
            for (path, file) in &self.parents {
                same(path, file, true)?;
            }
            same(&self.path, &self.file, false)?;
        }
        #[cfg(windows)]
        {
            for parent in &self.parents {
                parent
                    .verify_binding()
                    .map_err(|e| io("verify catalog ancestor", e))?;
            }
            crate::vault::windows_security::validate_same_file(
                &self.file,
                &self.path,
                crate::vault::acl_policy::Protection::IntegrityProtected,
            )
            .map_err(|e| io("verify catalog binding", e))?;
        }
        Ok(())
    }
}
struct HeldLock {
    checked: Checked,
}
impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = self.checked.file.unlock();
    }
}
fn lock(checked: Checked, exclusive: bool, timeout: Duration) -> Result<Option<HeldLock>> {
    let started = Instant::now();
    loop {
        let result = if exclusive {
            checked.file.try_lock()
        } else {
            checked.file.try_lock_shared()
        };
        match result {
            Ok(()) => {
                checked.verify()?;
                return Ok(Some(HeldLock { checked }));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Ok(None);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(5)));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(io("acquire catalog lease", e)),
        }
    }
}
fn required_lock(checked: Checked, exclusive: bool, timeout: Duration) -> Result<HeldLock> {
    lock(checked, exclusive, timeout)?.ok_or_else(|| {
        let mut e = WikiError::new(
            ErrorCode::LockTimeout,
            "catalog acquisition or file lease timeout",
        );
        e.retryable = true;
        e
    })
}
fn exists(path: &Path) -> Result<bool> {
    match inspect_path(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io("inspect catalog path", e)),
    }
}
fn read(fs: &VaultFs, name: CachePath<'_>) -> Result<Option<Vec<u8>>> {
    let name = path(fs, name)?;
    if !exists(&name)? {
        return Ok(None);
    }
    let mut checked = Checked::open(&name, false, false)?;
    if checked
        .file
        .metadata()
        .map_err(|e| io("inspect catalog JSON size", e))?
        .len()
        > JSON_LIMIT as u64
    {
        return Err(corrupt("catalog JSON exceeds 4096 bytes"));
    }
    let mut bytes = Vec::new();
    (&mut checked.file)
        .take(JSON_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io("read catalog JSON", e))?;
    checked.verify()?;
    if bytes.len() > JSON_LIMIT {
        return Err(corrupt("catalog JSON exceeds 4096 bytes"));
    }
    Ok(Some(bytes))
}
fn marker(fs: &VaultFs, vault: &RecordId) -> Result<bool> {
    let Some(bytes) = read(fs, ACTIVE)? else {
        return Ok(false);
    };
    let marker: Activation = serde_json::from_slice(&bytes)
        .map_err(|_| corrupt("catalog activation marker malformed"))?;
    if marker.version != 2 || &marker.vault_id != vault {
        return Err(corrupt(
            "catalog activation marker belongs to another vault or version",
        ));
    }
    Ok(true)
}
fn selection(fs: &VaultFs, vault: &RecordId) -> Result<Option<CatalogSelection>> {
    let active = marker(fs, vault)?;
    let Some(bytes) = read(fs, CURRENT)? else {
        return if active {
            Err(corrupt(
                "activated catalog selector is missing; explicit rebuild required",
            ))
        } else {
            Ok(None)
        };
    };
    let selected: CatalogSelection = serde_json::from_slice(&bytes)
        .map_err(|_| corrupt("catalog selector malformed; explicit rebuild required"))?;
    selected.validate(vault)?;
    Ok(Some(selected))
}
fn database_name(selection: &CatalogSelection) -> CachePath<'_> {
    CachePath::File(&selection.file_id, FileKind::Database)
}
fn lease_name(selection: &CatalogSelection) -> CachePath<'_> {
    CachePath::File(&selection.file_id, FileKind::Lease)
}
fn no_sidecars(fs: &VaultFs, selection: &CatalogSelection) -> Result<()> {
    for kind in [FileKind::Wal, FileKind::Shm, FileKind::Journal] {
        if exists(&path(fs, CachePath::File(&selection.file_id, kind))?)? {
            return Err(corrupt("sealed catalog has unexpected SQLite sidecar"));
        }
    }
    Ok(())
}

/// Caller must open READ_ONLY, BEGIN and validate the selected header inside
/// `open`. The shared gate remains held through the entire closure.
pub(crate) fn acquire<T>(
    fs: &VaultFs,
    vault: &RecordId,
    timeout: Duration,
    open: impl FnOnce(&Path, &CatalogSelection) -> Result<T>,
) -> Result<Option<Selected<T>>> {
    let gate_path = path(fs, GATE)?;
    if !exists(&gate_path)? {
        // A lost gate after any v2 evidence cannot reactivate a stale v1 file.
        if read(fs, CURRENT)?.is_some() || read(fs, ACTIVE)?.is_some() {
            return Err(corrupt("activated catalog acquisition gate is missing"));
        }
        return Ok(None);
    }
    let started = Instant::now();
    let gate = required_lock(Checked::open(&gate_path, false, true)?, false, timeout)?;
    let Some(selection) = selection(fs, vault)? else {
        return Ok(None);
    };
    let lease = required_lock(
        Checked::open(&path(fs, lease_name(&selection))?, false, true)?,
        false,
        timeout.saturating_sub(started.elapsed()),
    )?;
    no_sidecars(fs, &selection)?;
    let db = Checked::open(&path(fs, database_name(&selection))?, false, false)?;
    rollback_header(&db)?;
    let value = open(&db.path, &selection)?;
    db.verify()?;
    gate.checked.verify()?;
    lease.checked.verify()?;
    drop(gate);
    Ok(Some(Selected {
        value,
        selection,
        _lease: lease,
    }))
}

fn ensure_lock(fs: &VaultFs, name: CachePath<'_>) -> Result<()> {
    let path = path(fs, name)?;
    let checked = Checked::open(&path, !exists(&path)?, true)?;
    checked
        .file
        .sync_all()
        .map_err(|e| io("sync catalog lock", e))?;
    checked.verify()?;
    durable(
        fs.durable_io()
            .sync_directory(path.parent().unwrap())
            .map_err(|e| io("sync catalog lock parent", e))?,
    )
}
/// Reserve a new, never reused sibling identity before construction starts.
pub(crate) fn prepare(
    fs: &VaultFs,
    writer: &WriterPermit,
    selection: &CatalogSelection,
) -> Result<PathBuf> {
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    selection.validate(&selection.vault_id)?;
    durable(fs.ensure_directory(&VaultRelativePath::new(DIRECTORY.relative()?)?, writer)?)?;
    if !exists(&path(fs, GATE)?)? && (read(fs, CURRENT)?.is_some() || read(fs, ACTIVE)?.is_some()) {
        return Err(corrupt(
            "activated catalog gate is missing; refusing to recycle its inode",
        ));
    }
    ensure_lock(fs, GATE)?;
    let gate = required_lock(
        Checked::open(&path(fs, GATE)?, false, true)?,
        true,
        Duration::from_secs(30),
    )?;
    marker(fs, &selection.vault_id)?;
    let db = path(fs, database_name(selection))?;
    let lease = path(fs, lease_name(selection))?;
    if exists(&db)? || exists(&lease)? {
        return Err(corrupt(
            "catalog sibling identity has already been reserved",
        ));
    }
    no_sidecars(fs, selection)?;
    ensure_lock(fs, lease_name(selection))?;
    gate.checked.verify()?;
    Ok(db)
}

/// Inspect the fixed SQLite header before SQLite sees the path. READ_ONLY can
/// create sidecars for a WAL database when the directory is writable.
fn rollback_header(checked: &Checked) -> Result<()> {
    let mut bytes = [0u8; 100];
    let mut file = &checked.file;
    file.read_exact(&mut bytes)
        .map_err(|e| io("read sealed SQLite header", e))?;
    checked.verify()?;
    if &bytes[..16] != b"SQLite format 3\0" || bytes[18] != 1 || bytes[19] != 1 {
        return Err(corrupt("catalog SQLite header is not sealed rollback mode"));
    }
    Ok(())
}

/// Verify publication/retirement ownership, never create SQL or recover WAL.
fn verify_sealed(fs: &VaultFs, selection: &CatalogSelection) -> Result<Checked> {
    no_sidecars(fs, selection)?;
    let checked = Checked::open(&path(fs, database_name(selection))?, false, false)?;
    rollback_header(&checked)?;
    let connection = Connection::open_with_flags(
        &checked.path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(super::sql::sql_error)?;
    let started = Instant::now();
    let mut ticks = 0usize;
    connection
        .progress_handler(
            1000,
            Some(move || {
                ticks += 1;
                ticks > 1000 || started.elapsed() > Duration::from_secs(2)
            }),
        )
        .map_err(super::sql::sql_error)?;
    connection
        .set_limit(
            rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
            8 * 1024 * 1024,
        )
        .map_err(super::sql::sql_error)?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(super::sql::sql_error)?;
    connection
        .execute_batch("PRAGMA query_only=ON; BEGIN;")
        .map_err(super::sql::sql_error)?;
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .map_err(super::sql::sql_error)?;
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(super::sql::sql_error)?;
    let valid:i64=connection.query_row("SELECT count(*) FROM catalog_meta WHERE singleton=1 AND schema_version=2 AND vault_id=?1 AND file_id=?2 AND creation_epoch=?3 AND creation_header_hash=?4 AND epoch>=creation_epoch AND state='complete' AND parser_hash IS NOT NULL AND control_hash IS NOT NULL AND dependency_hash IS NOT NULL",
        rusqlite::params![selection.vault_id.as_str(),selection.file_id,selection.creation_epoch as i64,selection.creation_header_hash.as_str()],|r|r.get(0)).map_err(super::sql::sql_error)?;
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM catalog_meta", [], |r| r.get(0))
        .map_err(super::sql::sql_error)?;
    if mode != "delete" || version != 2 || valid != 1 || rows != 1 {
        return Err(corrupt(
            "catalog is unsealed or selected identity does not match header",
        ));
    }
    checked.verify()?;
    drop(connection);
    Ok(checked)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublishPoint {
    BeforeMarker,
    MarkerStaged,
    MarkerDurable,
    SelectorStaged,
    SelectorDurable,
}

pub(crate) fn publish(
    fs: &VaultFs,
    writer: &WriterPermit,
    selection: &CatalogSelection,
    timeout: Duration,
) -> Result<()> {
    publish_with_faults(fs, writer, selection, timeout, &mut |_| Ok(()))
}
/// Exact rename-before-directory-sync faults use the VaultFs DurableIo adapter:
/// replace intentionally exposes no gap between those durability operations.
pub(crate) fn publish_with_faults(
    fs: &VaultFs,
    writer: &WriterPermit,
    selected: &CatalogSelection,
    timeout: Duration,
    fault: &mut dyn FnMut(PublishPoint) -> Result<()>,
) -> Result<()> {
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    selected.validate(&selected.vault_id)?;
    let started = Instant::now();
    let gate = required_lock(Checked::open(&path(fs, GATE)?, false, true)?, true, timeout)?;
    let lease = required_lock(
        Checked::open(&path(fs, lease_name(selected))?, false, true)?,
        true,
        timeout.saturating_sub(started.elapsed()),
    )?;
    let db = verify_sealed(fs, selected)?;
    // Re-sync a complete file observed after an interrupted build/seal.
    db.file
        .sync_all()
        .map_err(|e| io("sync sealed catalog", e))?;
    durable(
        fs.durable_io()
            .sync_directory(db.path.parent().unwrap())
            .map_err(|e| io("sync sealed catalog directory", e))?,
    )?;
    let active = marker(fs, &selected.vault_id)?; // foreign/corrupt marker never implicitly repaired
    fault(PublishPoint::BeforeMarker)?;
    if !active {
        let bytes = serde_json::to_vec(&Activation {
            version: 2,
            vault_id: selected.vault_id.clone(),
        })
        .map_err(|e| corrupt(e.to_string()))?;
        let staged = fs.stage(&VaultRelativePath::new(ACTIVE.relative()?)?, &bytes, writer)?;
        fault(PublishPoint::MarkerStaged)?;
        durable(fs.replace(staged, &ExpectedState::Absent, writer)?)?;
    } else {
        durable(fs.sync_target(&VaultRelativePath::new(ACTIVE.relative()?)?, writer)?)?;
    }
    fault(PublishPoint::MarkerDurable)?;
    let expected = read(fs, CURRENT)?.map_or(ExpectedState::Absent, |bytes| {
        ExpectedState::Hash(Blake3Hash::digest(bytes))
    });
    let bytes = serde_json::to_vec(selected).map_err(|e| corrupt(e.to_string()))?;
    if bytes.len() > JSON_LIMIT {
        return Err(corrupt("catalog selector exceeds limit"));
    }
    let staged = fs.stage(
        &VaultRelativePath::new(CURRENT.relative()?)?,
        &bytes,
        writer,
    )?;
    fault(PublishPoint::SelectorStaged)?;
    db.verify()?;
    gate.checked.verify()?;
    lease.checked.verify()?;
    durable(fs.replace(staged, &expected, writer)?)?;
    fault(PublishPoint::SelectorDurable)
}

pub(crate) fn retire(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    candidate: &CatalogSelection,
    timeout: Duration,
) -> Result<bool> {
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    candidate.validate(vault)?;
    let started = Instant::now();
    let gate = required_lock(Checked::open(&path(fs, GATE)?, false, true)?, true, timeout)?;
    if selection(fs, vault)?
        .as_ref()
        .is_some_and(|s| s.file_id == candidate.file_id)
    {
        return Ok(false);
    }
    let Some(lease) = lock(
        Checked::open(&path(fs, lease_name(candidate))?, false, true)?,
        true,
        timeout.saturating_sub(started.elapsed()),
    )?
    else {
        return Ok(false);
    };
    let db = verify_sealed(fs, candidate)?;
    gate.checked.verify()?;
    lease.checked.verify()?;
    db.verify()?;
    let name = db.path.clone();
    // Windows checked handles deliberately forbid deletion while held.
    drop(db);
    fs.durable_io()
        .remove(&name)
        .map_err(|e| io("retire catalog file", e))?;
    durable(
        fs.durable_io()
            .sync_directory(name.parent().unwrap())
            .map_err(|e| io("sync retired catalog directory", e))?,
    )?;
    Ok(true)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::vault::{DurableIo, NativeIo, VaultRoot};
    use std::sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    };

    fn fixture() -> (tempfile::TempDir, VaultFs, WriterPermit, RecordId) {
        let temp = tempfile::tempdir().unwrap();
        let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
        (
            temp,
            fs,
            writer,
            RecordId::new("vault_selector_test").unwrap(),
        )
    }
    fn build(
        fs: &VaultFs,
        writer: &WriterPermit,
        vault: &RecordId,
        text: &str,
    ) -> CatalogSelection {
        let selected = CatalogSelection::new(vault.clone(), 1).unwrap();
        let name = prepare(fs, writer, &selected).unwrap();
        let connection = Connection::open(name).unwrap();
        connection
            .execute_batch(super::super::normalized_schema::SCHEMA)
            .unwrap();
        let hash = Blake3Hash::digest(b"fixture");
        connection.execute("INSERT INTO catalog_meta(singleton,schema_version,vault_id,file_id,creation_epoch,creation_header_hash,epoch,parser_hash,control_hash,dependency_hash,state,vector_cache_lost,vector_loss_unknown) VALUES(1,2,?1,?2,?3,?4,?3,?5,?5,?5,'complete',0,0)",
            rusqlite::params![vault.as_str(),selected.file_id,selected.creation_epoch as i64,selected.creation_header_hash.as_str(),hash.as_str()]).unwrap();
        connection.execute_batch("CREATE TABLE test_content(text TEXT); CREATE VIRTUAL TABLE test_fts USING fts5(text);").unwrap();
        connection
            .execute("INSERT INTO test_content VALUES(?1)", [text])
            .unwrap();
        connection
            .execute("INSERT INTO test_fts VALUES(?1)", [text])
            .unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=DELETE;")
            .unwrap();
        drop(connection);
        selected
    }
    fn open(name: &Path, selected: &CatalogSelection) -> Result<Connection> {
        let c = Connection::open_with_flags(
            name,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(super::super::sql::sql_error)?;
        c.execute_batch("BEGIN;")
            .map_err(super::super::sql::sql_error)?;
        let id: String = c
            .query_row(
                "SELECT file_id FROM catalog_meta WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(super::super::sql::sql_error)?;
        if id != selected.file_id {
            return Err(corrupt("test header mismatch"));
        }
        Ok(c)
    }
    fn text(reader: &Selected<Connection>) -> String {
        reader
            .value
            .query_row("SELECT text FROM test_content", [], |r| r.get(0))
            .unwrap()
    }
    fn get(fs: &VaultFs, vault: &RecordId) -> Result<Option<Selected<Connection>>> {
        acquire(fs, vault, Duration::ZERO, open)
    }
    fn put(fs: &VaultFs, writer: &WriterPermit, selected: &CatalogSelection) {
        publish(fs, writer, selected, Duration::ZERO).unwrap();
    }

    #[test]
    fn absent_reader_has_no_filesystem_effects() {
        let temp = tempfile::tempdir().unwrap();
        let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
        assert!(
            get(&fs, &RecordId::new("vault_empty").unwrap())
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }
    #[test]
    fn marker_only_corruption_and_lost_selector_never_fall_back_to_legacy() {
        let (_temp, fs, writer, vault) = fixture();
        let selected = build(&fs, &writer, &vault, "new");
        fs::write(
            fs.root().path().join(".wiki/cache/index.sqlite"),
            b"stale legacy",
        )
        .unwrap();
        fs::write(
            path(&fs, ACTIVE).unwrap(),
            serde_json::to_vec(&Activation {
                version: 2,
                vault_id: vault.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(get(&fs, &vault).is_err());
        put(&fs, &writer, &selected);
        fs::remove_file(path(&fs, CURRENT).unwrap()).unwrap();
        assert!(get(&fs, &vault).is_err());
        fs::write(path(&fs, CURRENT).unwrap(), b"{").unwrap();
        assert!(get(&fs, &vault).is_err());
        put(&fs, &writer, &selected); // explicit publication repairs malformed selector
        assert_eq!(text(&get(&fs, &vault).unwrap().unwrap()), "new");
        fs::remove_file(path(&fs, ACTIVE).unwrap()).unwrap();
        assert_eq!(text(&get(&fs, &vault).unwrap().unwrap()), "new");
        fs::remove_file(path(&fs, GATE).unwrap()).unwrap();
        assert!(get(&fs, &vault).is_err());
        let next = CatalogSelection::new(vault.clone(), 1).unwrap();
        assert!(prepare(&fs, &writer, &next).is_err());
        assert!(!path(&fs, GATE).unwrap().exists());
    }
    #[test]
    fn foreign_marker_or_selector_is_refused_and_marker_is_not_repaired() {
        let (_temp, fs, writer, vault) = fixture();
        let selected = build(&fs, &writer, &vault, "ours");
        put(&fs, &writer, &selected);
        let foreign = RecordId::new("vault_foreign").unwrap();
        fs::write(
            path(&fs, ACTIVE).unwrap(),
            serde_json::to_vec(&Activation {
                version: 2,
                vault_id: foreign.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(get(&fs, &vault).is_err());
        assert!(publish(&fs, &writer, &selected, Duration::ZERO).is_err());
        fs::write(path(&fs, ACTIVE).unwrap(), b"not JSON").unwrap();
        assert!(publish(&fs, &writer, &selected, Duration::ZERO).is_err());
        fs::remove_file(path(&fs, ACTIVE).unwrap()).unwrap();
        let copied = CatalogSelection::new(foreign, 1).unwrap();
        fs::write(
            path(&fs, CURRENT).unwrap(),
            serde_json::to_vec(&copied).unwrap(),
        )
        .unwrap();
        assert!(get(&fs, &vault).is_err());
    }
    #[test]
    fn old_reader_new_reader_and_pinned_retirement() {
        let (_temp, fs, writer, vault) = fixture();
        let first = build(&fs, &writer, &vault, "old");
        put(&fs, &writer, &first);
        let old = get(&fs, &vault).unwrap().unwrap();
        let second = build(&fs, &writer, &vault, "new");
        put(&fs, &writer, &second);
        let new = get(&fs, &vault).unwrap().unwrap();
        assert_eq!(old.selection(), &first);
        assert_eq!(text(&old), "old");
        assert_eq!(text(&new), "new");
        let found: String = old
            .value
            .query_row(
                "SELECT text FROM test_fts WHERE test_fts MATCH 'old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, "old");
        assert!(!retire(&fs, &writer, &vault, &first, Duration::ZERO).unwrap());
        assert!(!retire(&fs, &writer, &vault, &second, Duration::ZERO).unwrap());
        drop(old);
        assert!(retire(&fs, &writer, &vault, &first, Duration::ZERO).unwrap());
        assert!(!exists(&path(&fs, database_name(&first)).unwrap()).unwrap());
        assert!(exists(&path(&fs, lease_name(&first)).unwrap()).unwrap());
        assert!(prepare(&fs, &writer, &first).is_err()); // no inode recycling
    }
    #[test]
    fn gate_covers_reader_handshake_before_publisher_can_switch_or_retire() {
        let (_temp, fs, writer, vault) = fixture();
        let first = build(&fs, &writer, &vault, "old");
        put(&fs, &writer, &first);
        let second = build(&fs, &writer, &vault, "new");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let reader_fs = &fs;
            let reader_vault = &vault;
            let reader = scope.spawn(move || {
                acquire(
                    reader_fs,
                    reader_vault,
                    Duration::from_secs(2),
                    |name, selected| {
                        entered_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        open(name, selected)
                    },
                )
            });
            entered_rx.recv().unwrap();
            let error = publish(&fs, &writer, &second, Duration::ZERO).unwrap_err();
            assert_eq!(error.code, ErrorCode::LockTimeout);
            assert!(retire(&fs, &writer, &vault, &first, Duration::ZERO).is_err());
            resume_tx.send(()).unwrap();
            let held = reader.join().unwrap().unwrap().unwrap();
            put(&fs, &writer, &second);
            assert_eq!(text(&held), "old");
            assert!(!retire(&fs, &writer, &vault, &first, Duration::ZERO).unwrap());
        });
        assert!(retire(&fs, &writer, &vault, &first, Duration::ZERO).unwrap());
    }
    #[test]
    fn value_is_dropped_while_lease_is_still_held() {
        struct Probe {
            lease: PathBuf,
            observed: Arc<AtomicU8>,
            connection: Option<Connection>,
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                drop(self.connection.take());
                let file = File::open(&self.lease).unwrap();
                assert!(matches!(
                    file.try_lock(),
                    Err(std::fs::TryLockError::WouldBlock)
                ));
                self.observed.store(1, Ordering::SeqCst);
            }
        }
        let (_temp, fs, writer, vault) = fixture();
        let selected = build(&fs, &writer, &vault, "one");
        put(&fs, &writer, &selected);
        let observed = Arc::new(AtomicU8::new(0));
        let held = acquire(&fs, &vault, Duration::ZERO, |name, s| {
            Ok(Probe {
                lease: path(&fs, lease_name(s))?,
                observed: observed.clone(),
                connection: Some(open(name, s)?),
            })
        })
        .unwrap()
        .unwrap();
        drop(held);
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        let file = File::open(path(&fs, lease_name(&selected)).unwrap()).unwrap();
        file.try_lock().unwrap();
    }
    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
        fn walk(root: &Path, out: &mut Vec<(PathBuf, Vec<u8>, std::time::SystemTime)>) {
            for e in fs::read_dir(root).unwrap() {
                let e = e.unwrap();
                if e.file_type().unwrap().is_dir() {
                    walk(&e.path(), out);
                } else {
                    out.push((
                        e.path(),
                        fs::read(e.path()).unwrap(),
                        e.metadata().unwrap().modified().unwrap(),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, &mut out);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    #[test]
    fn sealed_read_does_not_write_or_create_sidecars() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, fs, writer, vault) = fixture();
        let selected = build(&fs, &writer, &vault, "sealed");
        put(&fs, &writer, &selected);
        let before = snapshot(fs.root().path());
        let directory = path(&fs, DIRECTORY).unwrap();
        let permissions = fs::metadata(&directory).unwrap().permissions();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
        let read = get(&fs, &vault).unwrap().unwrap();
        assert_eq!(text(&read), "sealed");
        drop(read);
        fs::set_permissions(directory, permissions).unwrap();
        assert_eq!(before, snapshot(fs.root().path()));
    }
    #[test]
    fn symlink_gate_lease_database_and_parent_are_refused() {
        use std::os::unix::fs::symlink;
        for which in 0..4 {
            let (_temp, fs, writer, vault) = fixture();
            let selected = build(&fs, &writer, &vault, "safe");
            put(&fs, &writer, &selected);
            let target = match which {
                0 => path(&fs, GATE).unwrap(),
                1 => path(&fs, lease_name(&selected)).unwrap(),
                2 => path(&fs, database_name(&selected)).unwrap(),
                _ => path(&fs, DIRECTORY).unwrap(),
            };
            let displaced = target.with_extension("displaced");
            fs::rename(&target, &displaced).unwrap();
            symlink(&displaced, &target).unwrap();
            assert!(get(&fs, &vault).is_err(), "case {which}");
        }
    }
    #[test]
    fn owned_cache_lookup_work_is_independent_of_siblings_and_lease_history() {
        #[cfg(not(target_os = "macos"))]
        use std::os::unix::ffi::OsStringExt;
        let (_temp, fs, writer, vault) = fixture();
        let selected = build(&fs, &writer, &vault, "bounded");
        put(&fs, &writer, &selected);
        let measure = || {
            PATH_INSPECTIONS.with(|count| count.set(0));
            let reader = get(&fs, &vault).unwrap().unwrap();
            assert_eq!(text(&reader), "bounded");
            drop(reader);
            PATH_INSPECTIONS.with(|count| count.get())
        };
        let initial = measure();
        assert!(initial > 0);
        let cache = fs.root().path().join(".wiki/cache");
        let catalogs = cache.join("catalogs");
        for i in 0..2048 {
            fs::write(catalogs.join(format!("{i:032x}.lock")), b"").unwrap();
            fs::write(cache.join(format!("unrelated-{i}")), b"").unwrap();
        }
        // Unrelated names are irrelevant to the owned lookup's proof. Keep
        // ordinary and Unicode controls on every platform; APFS refuses the
        // invalid UTF-8 fixture (same boundary as tests/vault_fs.rs).
        for parent in [
            fs.root().path().to_path_buf(),
            fs.root().path().join(".wiki"),
            cache,
            catalogs,
        ] {
            fs::write(parent.join("ordinary-name-control"), b"").unwrap();
            fs::write(parent.join("unrelated-Ångström-東京"), b"").unwrap();
            #[cfg(not(target_os = "macos"))]
            fs::write(
                parent.join(std::ffi::OsString::from_vec(vec![0xff, 0xfe])),
                b"",
            )
            .unwrap();
        }
        let with_history = measure();
        assert_eq!(
            initial, with_history,
            "metadata probe count must not grow with history"
        );
    }

    #[test]
    fn any_marker_object_in_reserved_cache_ancestry_is_refused() {
        use std::os::unix::fs::symlink;
        let (_temp, fs, writer, vault) = fixture();
        let selected = build(&fs, &writer, &vault, "safe");
        put(&fs, &writer, &selected);
        for directory in [".wiki", ".wiki/cache", ".wiki/cache/catalogs"] {
            let marker = fs.root().path().join(directory).join("WIKI.md");
            fs::write(&marker, b"unexpected nested marker").unwrap();
            assert!(get(&fs, &vault).is_err());
            fs::remove_file(&marker).unwrap();
            fs::create_dir(&marker).unwrap();
            assert!(get(&fs, &vault).is_err());
            fs::remove_dir(&marker).unwrap();
            symlink("absent-marker-target", &marker).unwrap();
            assert!(get(&fs, &vault).is_err());
            fs::remove_file(marker).unwrap();
        }
        assert_eq!(text(&get(&fs, &vault).unwrap().unwrap()), "safe");
    }

    #[test]
    fn owned_file_names_refuse_traversal_and_noncanonical_identifiers() {
        let (_temp, fs, _writer, _vault) = fixture();
        for id in [
            "../index",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "0000000000000000000000000000000",
            "index.sqlite",
        ] {
            PATH_INSPECTIONS.with(|count| count.set(0));
            assert!(path(&fs, CachePath::File(id, FileKind::Database)).is_err());
            assert_eq!(PATH_INSPECTIONS.with(|count| count.get()), 0);
        }
    }

    #[test]
    fn publication_fault_boundaries_preserve_activation_evidence() {
        for point in [
            PublishPoint::BeforeMarker,
            PublishPoint::MarkerStaged,
            PublishPoint::MarkerDurable,
            PublishPoint::SelectorStaged,
            PublishPoint::SelectorDurable,
        ] {
            let (_temp, fs, writer, vault) = fixture();
            let selected = build(&fs, &writer, &vault, "safe");
            let error = publish_with_faults(&fs, &writer, &selected, Duration::ZERO, &mut |p| {
                if p == point {
                    Err(corrupt("injected"))
                } else {
                    Ok(())
                }
            });
            assert!(error.is_err());
            let result = get(&fs, &vault);
            match point {
                PublishPoint::BeforeMarker | PublishPoint::MarkerStaged => {
                    assert!(result.unwrap().is_none())
                }
                PublishPoint::MarkerDurable | PublishPoint::SelectorStaged => {
                    assert!(result.is_err())
                }
                PublishPoint::SelectorDurable => {
                    assert_eq!(text(&result.unwrap().unwrap()), "safe")
                }
            }
            put(&fs, &writer, &selected);
            assert_eq!(text(&get(&fs, &vault).unwrap().unwrap()), "safe");
        }
    }
    struct RenameFault {
        point: u8,
        after: AtomicU8,
    }
    impl DurableIo for RenameFault {
        fn create_stage(&self, p: &Path) -> std::io::Result<File> {
            NativeIo.create_stage(p)
        }
        fn open_append(&self, p: &Path) -> std::io::Result<File> {
            NativeIo.open_append(p)
        }
        fn truncate_file(&self, f: &File, n: u64) -> std::io::Result<()> {
            NativeIo.truncate_file(f, n)
        }
        fn write_stage(&self, f: &mut File, b: &[u8]) -> std::io::Result<()> {
            NativeIo.write_stage(f, b)
        }
        fn sync_file(&self, f: &File) -> std::io::Result<()> {
            NativeIo.sync_file(f)
        }
        fn replace(&self, s: &Path, t: &Path) -> std::io::Result<()> {
            let kind = if t.ends_with("catalog-v2-active.json") {
                1
            } else {
                2
            };
            if self.point == kind * 2 - 1 {
                return Err(std::io::Error::other("injected before rename"));
            }
            NativeIo.replace(s, t)?;
            self.after.store(kind, Ordering::SeqCst);
            Ok(())
        }
        fn remove(&self, p: &Path) -> std::io::Result<()> {
            NativeIo.remove(p)
        }
        fn create_directory(&self, p: &Path) -> std::io::Result<()> {
            NativeIo.create_directory(p)
        }
        fn sync_directory(&self, p: &Path) -> std::io::Result<DirectorySync> {
            let kind = self.after.swap(0, Ordering::SeqCst);
            if kind != 0 && self.point == kind * 2 {
                Err(std::io::Error::other("injected before directory sync"))
            } else {
                NativeIo.sync_directory(p)
            }
        }
    }
    #[test]
    fn faults_at_marker_and_selector_rename_and_directory_sync() {
        for point in 1..=4 {
            let (_temp, fs, writer, vault) = fixture();
            let selected = build(&fs, &writer, &vault, "safe");
            fs::write(
                fs.root().path().join(".wiki/cache/index.sqlite"),
                b"stale legacy",
            )
            .unwrap();
            let faulty = VaultFs::with_io(
                fs.root().clone(),
                Arc::new(RenameFault {
                    point,
                    after: AtomicU8::new(0),
                }),
            );
            assert!(publish(&faulty, &writer, &selected, Duration::ZERO).is_err());
            let result = get(&fs, &vault);
            match point {
                1 => assert!(result.unwrap().is_none()),
                2 | 3 => assert!(result.is_err()),
                4 => assert_eq!(text(&result.unwrap().unwrap()), "safe"),
                _ => unreachable!(),
            }
            put(&fs, &writer, &selected);
        }
    }
    #[test]
    fn unsealed_foreign_header_and_unexpected_sidecars_cannot_publish_or_retire() {
        for which in 0..3 {
            let (_temp, fs, writer, vault) = fixture();
            let selected = build(&fs, &writer, &vault, "safe");
            if which == 0 {
                let c = Connection::open(path(&fs, database_name(&selected)).unwrap()).unwrap();
                c.execute("UPDATE catalog_meta SET file_id='wrong'", [])
                    .unwrap();
            }
            if which == 1 {
                let c = Connection::open(path(&fs, database_name(&selected)).unwrap()).unwrap();
                c.execute_batch("PRAGMA journal_mode=WAL").unwrap();
            }
            if which == 2 {
                fs::write(
                    path(&fs, CachePath::File(&selected.file_id, FileKind::Wal)).unwrap(),
                    b"unexpected",
                )
                .unwrap();
            }
            let before = snapshot(fs.root().path());
            assert!(publish(&fs, &writer, &selected, Duration::ZERO).is_err());
            assert!(retire(&fs, &writer, &vault, &selected, Duration::ZERO).is_err());
            assert_eq!(before, snapshot(fs.root().path()));
            assert!(exists(&path(&fs, database_name(&selected)).unwrap()).unwrap());
        }
    }
}

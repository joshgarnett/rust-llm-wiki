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
    io::{Read, Seek, SeekFrom},
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
    StateWriter,
    RebuildState,
    CacheRoot,
    File(&'a str, FileKind),
    Legacy(FileKind),
}
impl CachePath<'_> {
    fn relative(self) -> Result<String> {
        Ok(match self {
            Self::Current => ".wiki/cache/catalog-current.json".into(),
            Self::Active => ".wiki/cache/catalog-v2-active.json".into(),
            Self::Gate => ".wiki/cache/catalog-acquisition.lock".into(),
            Self::Catalogs => ".wiki/cache/catalogs".into(),
            Self::StateWriter => ".wiki/state/writer.lock".into(),
            Self::RebuildState => ".wiki/state/catalog-rebuild.json".into(),
            Self::CacheRoot => ".wiki/cache".into(),
            Self::Legacy(kind) => {
                let suffix = match kind {
                    FileKind::Database => "",
                    FileKind::Wal => "-wal",
                    FileKind::Shm => "-shm",
                    FileKind::Journal => "-journal",
                    FileKind::Lease => return Err(corrupt("legacy doctor has no file lease")),
                };
                format!(".wiki/cache/index.sqlite{suffix}")
            }
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
        let directory =
            index + 1 < count || matches!(entry, CachePath::Catalogs | CachePath::CacheRoot);
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
    let read = (&mut checked.file)
        .take(JSON_LIMIT as u64 + 1)
        .read_to_end(&mut bytes);
    #[cfg(test)]
    super::query_diagnostics::read("catalog-selector", &name, bytes.len());
    read.map_err(|e| io("read catalog JSON", e))?;
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
/// Any retained activation evidence requires operational authority, including
/// an interrupted selector replacement. Never interpret corruption as legacy.
pub(crate) fn has_activation_evidence(fs: &VaultFs) -> Result<bool> {
    Ok(read(fs, ACTIVE)?.is_some()
        || read(fs, CURRENT)?.is_some()
        || read_rebuild_record(fs)?.is_some())
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
    no_file_sidecars(fs, &selection.file_id)
}
fn no_file_sidecars(fs: &VaultFs, file_id: &str) -> Result<()> {
    for kind in [FileKind::Wal, FileKind::Shm, FileKind::Journal] {
        if exists(&path(fs, CachePath::File(file_id, kind))?)? {
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
    let db = Checked::open(&path(fs, database_name(&selection))?, false, true)?;
    let sidecars = checked_sidecars(fs, &selection, &db, false)?;
    let value = open(&db.path, &selection)?;
    verify_sidecars(&sidecars)?;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum JournalMode {
    Delete,
    Wal,
}

/// Inspect before SQLite sees the path: a read-only WAL open can otherwise
/// create sidecars when its directory is writable.
fn journal_header(checked: &Checked) -> Result<JournalMode> {
    let mut bytes = [0u8; 100];
    let mut file = &checked.file;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| io("seek SQLite header", e))?;
    file.read_exact(&mut bytes)
        .map_err(|e| io("read SQLite header", e))?;
    #[cfg(test)]
    super::query_diagnostics::read("catalog-db-header", &checked.path, bytes.len());
    checked.verify()?;
    if &bytes[..16] != b"SQLite format 3\0" {
        return Err(corrupt("catalog SQLite header is invalid"));
    }
    match (bytes[18], bytes[19]) {
        (1, 1) => Ok(JournalMode::Delete),
        (2, 2) => Ok(JournalMode::Wal),
        _ => Err(corrupt("catalog SQLite journal mode is unsupported")),
    }
}

/// Sidecars are owned SQLite state, not arbitrary cache paths. Missing WAL
/// SHM is reconstructible only from a retained WAL under an exclusive lifetime
/// lease. A missing WAL always refuses: the main DB may be a predecessor.
/// Native SQLite alone interprets or removes their contents.
fn checked_sidecars(
    fs: &VaultFs,
    selected: &CatalogSelection,
    db: &Checked,
    allow_missing: bool,
) -> Result<Vec<Checked>> {
    checked_file_sidecars(fs, &selected.file_id, db, allow_missing)
}
fn checked_file_sidecars(
    fs: &VaultFs,
    file_id: &str,
    db: &Checked,
    allow_missing: bool,
) -> Result<Vec<Checked>> {
    checked_file_sidecars_with_missing(fs, file_id, db, allow_missing, false)
}
fn checked_file_sidecars_with_missing(
    fs: &VaultFs,
    file_id: &str,
    db: &Checked,
    allow_missing_shm: bool,
    allow_missing_wal: bool,
) -> Result<Vec<Checked>> {
    if journal_header(db)? == JournalMode::Delete {
        no_file_sidecars(fs, file_id)?;
        return Ok(Vec::new());
    }
    if exists(&path(fs, CachePath::File(file_id, FileKind::Journal))?)? {
        return Err(corrupt("WAL catalog has unexpected rollback journal"));
    }
    let mut held = Vec::new();
    for kind in [FileKind::Wal, FileKind::Shm] {
        let name = path(fs, CachePath::File(file_id, kind))?;
        if !exists(&name)? {
            if (allow_missing_shm && matches!(kind, FileKind::Shm))
                || (allow_missing_wal && matches!(kind, FileKind::Wal))
            {
                continue;
            }
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                if matches!(kind, FileKind::Wal) {
                    "catalog WAL is missing; restore the exact WAL or explicitly rebuild"
                } else {
                    "catalog SHM is missing; exclusive writer recovery is required"
                },
            ));
        }
        let checked = Checked::open(&name, false, true)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owner = db
                .file
                .metadata()
                .map_err(|e| io("inspect database owner", e))?
                .uid();
            let metadata = checked
                .file
                .metadata()
                .map_err(|e| io("inspect sidecar owner", e))?;
            if metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
                return Err(corrupt(
                    "SQLite sidecar ownership or write permissions are unsafe",
                ));
            }
        }
        held.push(checked);
    }
    Ok(held)
}
fn verify_sidecars(sidecars: &[Checked]) -> Result<()> {
    for checked in sidecars {
        checked.verify()?;
    }
    Ok(())
}

fn persist_wal(connection: &Connection, enabled: bool) -> Result<()> {
    let mut flag: std::ffi::c_int = i32::from(enabled);
    // SAFETY: the borrowed connection owns a live SQLite handle; SQLite uses
    // this writable integer synchronously and does not retain its pointer.
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&mut flag as *mut std::ffi::c_int).cast(),
        )
    };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(corrupt(format!(
            "SQLite persistent WAL is unavailable ({result})"
        )));
    }
    Ok(())
}

/// Initial construction or exclusive migration only. A real page-one write
/// materializes WAL/SHM even when the preceding database is fully checkpointed.
/// Callers own the writer permit and an unpublished file or exclusive lease.
pub(crate) fn configure_wal(connection: &Connection) -> Result<()> {
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(super::sql::sql_error)?;
    persist_wal(connection, true)?;
    let mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))
        .map_err(super::sql::sql_error)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(corrupt("SQLite refused WAL mode"));
    }
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(super::sql::sql_error)?;
    connection
        .pragma_update(None, "user_version", version)
        .map_err(super::sql::sql_error)?;
    Ok(())
}

/// Read only the selected physical identity under the acquisition gate so WAL
/// preflight can run before a missing SHM would prevent opening a query reader.
/// This grants no header or canonical publication authority.
pub(crate) fn delta_selection(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    timeout: Duration,
) -> Result<CatalogSelection> {
    writer.require_root(fs.root())?;
    let gate = required_lock(
        Checked::open(&path(fs, GATE)?, false, true)?,
        false,
        timeout,
    )?;
    let selected = selection(fs, vault)?.ok_or_else(|| corrupt("delta selection is absent"))?;
    gate.checked.verify()?;
    Ok(selected)
}

/// Preflight before canonical writes. Only first migration or missing-sidecar
/// recovery of SHM needs exclusive lifetime ownership; ready WAL readers may
/// remain. A missing WAL is never recreated by this preflight.
pub(crate) fn ensure_delta_ready(
    fs: &VaultFs,
    writer: &WriterPermit,
    selected: &CatalogSelection,
    timeout: Duration,
) -> Result<()> {
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    selected.validate(&selected.vault_id)?;
    let started = Instant::now();
    let gate = required_lock(Checked::open(&path(fs, GATE)?, false, true)?, true, timeout)?;
    if selection(fs, &selected.vault_id)?.as_ref() != Some(selected) {
        return Err(corrupt("delta selection changed"));
    }
    let db = Checked::open(&path(fs, database_name(selected))?, false, true)?;
    let sidecars = checked_sidecars(fs, selected, &db, true)?;
    if journal_header(&db)? == JournalMode::Wal && sidecars.len() == 2 {
        verify_sidecars(&sidecars)?;
        return Ok(());
    }
    let lease = required_lock(
        Checked::open(&path(fs, lease_name(selected))?, false, true)?,
        true,
        timeout.saturating_sub(started.elapsed()),
    )?;
    let connection = writable(&db.path)?;
    super::normalized_read::header(&connection, selected)?;
    configure_wal(&connection)?;
    drop(connection);
    db.verify()?;
    let ready = checked_sidecars(fs, selected, &db, false)?;
    for sidecar in &ready {
        sidecar
            .file
            .sync_all()
            .map_err(|e| io("sync migrated SQLite sidecar", e))?;
    }
    db.file
        .sync_all()
        .map_err(|e| io("sync migrated SQLite database", e))?;
    durable(
        fs.durable_io()
            .sync_directory(db.path.parent().unwrap())
            .map_err(|e| io("sync migrated SQLite directory", e))?,
    )?;
    gate.checked.verify()?;
    lease.checked.verify()?;
    Ok(())
}
fn writable(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(super::sql::sql_error)?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(super::sql::sql_error)?;
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(super::sql::sql_error)?;
    persist_wal(&connection, true)?;
    Ok(connection)
}

/// The connection cannot escape either its shared lifetime lease or the writer
/// permit. The root adapter supplies the immediate row+FTS+epoch transaction.
pub(crate) struct DeltaWriter<'a> {
    selected: Selected<Connection>,
    _writer: &'a WriterPermit,
}
impl DeltaWriter<'_> {
    pub(crate) fn connection(&self) -> &Connection {
        self.selected.value()
    }
    pub(crate) fn selection(&self) -> &CatalogSelection {
        self.selected.selection()
    }
}
/// Caller must verify the durable operational authority and compare its
/// acknowledged file/epoch floor with this handle's header before applying a
/// delta. Active-operation starting/intended epochs and receipt proof belong
/// to that caller; selection identity alone does not establish publication.
pub(crate) fn open_delta<'a>(
    fs: &VaultFs,
    writer: &'a WriterPermit,
    selected: &CatalogSelection,
    timeout: Duration,
) -> Result<DeltaWriter<'a>> {
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    let pinned = acquire(fs, &selected.vault_id, timeout, |name, current| {
        if current != selected {
            return Err(corrupt("delta selection changed"));
        }
        let connection = writable(name)?;
        let mode: String = connection
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .map_err(super::sql::sql_error)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(corrupt(
                "delta requires WAL preflight before canonical writes",
            ));
        }
        super::normalized_read::header(&connection, selected)?;
        Ok(connection)
    })?
    .ok_or_else(|| corrupt("delta catalog selection is absent"))?;
    Ok(DeltaWriter {
        selected: pinned,
        _writer: writer,
    })
}

/// A physical-header read has a finite VM, elapsed and row-length ceiling.
fn maintenance_connection(name: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        name,
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
        .set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH, 256 * 1024)
        .map_err(super::sql::sql_error)?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(super::sql::sql_error)?;
    connection
        .execute_batch("PRAGMA query_only=ON; PRAGMA mmap_size=0; PRAGMA cache_size=-8192; BEGIN;")
        .map_err(super::sql::sql_error)?;
    Ok(connection)
}

/// A legacy WAL header without ordinary sidecars is ambiguous: it can be a
/// clean close or missing durable state. Never open SQLite to resolve that
/// ambiguity because a read-only connection can create a missing SHM file.
pub(crate) enum LegacyDoctorHeader {
    Absent,
    Uninspected(&'static str),
    Header(crate::domain::ReadSnapshot),
}

pub(crate) fn legacy_doctor_header(fs: &VaultFs) -> Result<LegacyDoctorHeader> {
    legacy_doctor_header_inner(
        fs,
        #[cfg(test)]
        &mut || Ok(()),
        #[cfg(test)]
        &mut || Ok(()),
    )
}

#[cfg(test)]
pub(crate) fn legacy_doctor_header_with_probe(
    fs: &VaultFs,
    before_guard: &mut dyn FnMut() -> Result<()>,
    before_open: &mut dyn FnMut() -> Result<()>,
) -> Result<LegacyDoctorHeader> {
    legacy_doctor_header_inner(fs, before_guard, before_open)
}

fn legacy_doctor_header_inner(
    fs: &VaultFs,
    #[cfg(test)] before_guard: &mut dyn FnMut() -> Result<()>,
    #[cfg(test)] before_open: &mut dyn FnMut() -> Result<()>,
) -> Result<LegacyDoctorHeader> {
    let name = path(fs, CachePath::Legacy(FileKind::Database))?;
    if !exists(&name)? {
        // Validate existing sidecars even when the main file is absent.
        for kind in [FileKind::Wal, FileKind::Shm, FileKind::Journal] {
            let sidecar = path(fs, CachePath::Legacy(kind))?;
            if exists(&sidecar)? {
                Checked::open(&sidecar, false, false)?.verify()?;
                return Err(corrupt("legacy sidecar has no main database"));
            }
        }
        return Ok(LegacyDoctorHeader::Absent);
    }
    let checked = Checked::open(&name, false, false)?;
    #[cfg(test)]
    before_guard()?;
    // A shared lock on the existing writer inode excludes ordinary writers
    // across journal/sidecar preflight, SQLite open, and SQLite close. Merely
    // holding WAL/SHM handles does not prevent their unlink on Unix.
    let writer_name = path(fs, CachePath::StateWriter)?;
    let (guard, guard_note) = if exists(&writer_name)? {
        let writer = Checked::open(&writer_name, false, true)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owner = checked
                .file
                .metadata()
                .map_err(|e| io("inspect database owner", e))?
                .uid();
            let metadata = writer
                .file
                .metadata()
                .map_err(|e| io("inspect writer lock", e))?;
            if metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
                return Err(corrupt(
                    "legacy observation writer lock ownership or permissions are unsafe",
                ));
            }
        }
        #[cfg(windows)]
        crate::vault::windows_security::validate_same_file(
            &writer.file,
            &writer_name,
            crate::vault::acl_policy::Protection::Private,
        )
        .map_err(|e| io("verify private writer lock", e))?;
        match lock(writer, false, Duration::ZERO)? {
            Some(guard) => (Some(guard), None),
            None => (
                None,
                Some(
                    "The existing writer lock is busy; no SQLite header inspection was performed. No cache or sidecar repair was attempted.",
                ),
            ),
        }
    } else {
        (
            None,
            Some(
                "The existing writer lock is absent; no SQLite header inspection was performed. No lock, cache, or sidecar files were created.",
            ),
        )
    };
    // The main handle was opened before locking for owner comparison. A
    // cooperating writer may have replaced it in that gap; reject the old
    // inode before deriving journal/sidecar admission from its bytes.
    if guard.is_some() {
        checked.verify()?;
    }
    let mut sidecars = Vec::new();
    let mut wal = false;
    let mut shm = false;
    let mut journal = false;
    for kind in [FileKind::Wal, FileKind::Shm, FileKind::Journal] {
        let sidecar = path(fs, CachePath::Legacy(kind))?;
        if !exists(&sidecar)? {
            continue;
        }
        let held = Checked::open(&sidecar, false, false)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owner = checked
                .file
                .metadata()
                .map_err(|e| io("inspect database owner", e))?
                .uid();
            let metadata = held
                .file
                .metadata()
                .map_err(|e| io("inspect sidecar owner", e))?;
            if metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
                return Err(corrupt(
                    "SQLite sidecar ownership or write permissions are unsafe",
                ));
            }
        }
        match kind {
            FileKind::Wal => wal = true,
            FileKind::Shm => shm = true,
            FileKind::Journal => journal = true,
            _ => unreachable!(),
        }
        sidecars.push(held);
    }
    // Even a missing/busy guard cannot hide an unsafe present sidecar.
    if let Some(note) = guard_note {
        verify_sidecars(&sidecars)?;
        checked.verify()?;
        return Ok(LegacyDoctorHeader::Uninspected(note));
    }
    let guard = guard.expect("successful legacy observation requires shared writer lock");
    let mode = journal_header(&checked)?;
    let note = if mode == JournalMode::Wal && (!wal || !shm) {
        Some(
            "Legacy WAL sidecars are absent or incomplete; a clean close and missing WAL state cannot be distinguished by this lightweight check. No SQLite header inspection was performed.",
        )
    } else if journal || mode == JournalMode::Delete && (wal || shm) {
        Some(
            "Legacy SQLite sidecars require interpretation beyond this lightweight check. No SQLite header inspection was performed.",
        )
    } else {
        None
    };
    if let Some(note) = note {
        verify_sidecars(&sidecars)?;
        checked.verify()?;
        return Ok(LegacyDoctorHeader::Uninspected(note));
    }
    guard.checked.verify()?;
    #[cfg(test)]
    before_open()?;
    let connection = maintenance_connection(&name)?;
    super::sql::validate_header(&connection)?;
    let snapshot = legacy_scalar_header(&connection)?;
    drop(connection);
    verify_sidecars(&sidecars)?;
    checked.verify()?;
    guard.checked.verify()?;
    Ok(LegacyDoctorHeader::Header(snapshot))
}

fn legacy_scalar_header(connection: &Connection) -> Result<crate::domain::ReadSnapshot> {
    let mut statement = connection.prepare("SELECT m.published_gen,g.gen,g.state,g.parser_hash,g.manifest_hash,m.vector_cache_lost,m.vector_loss_unknown FROM index_meta m LEFT JOIN generations g ON g.gen=m.published_gen WHERE m.singleton=1").map_err(super::sql::sql_error)?;
    let mut rows = statement.query([]).map_err(super::sql::sql_error)?;
    let row = rows
        .next()
        .map_err(super::sql::sql_error)?
        .ok_or_else(|| corrupt("legacy published header is absent"))?;
    let mut bytes = 0usize;
    for column in 0..7 {
        if let rusqlite::types::ValueRef::Text(value) =
            row.get_ref(column).map_err(super::sql::sql_error)?
        {
            bytes = bytes
                .checked_add(value.len())
                .filter(|n| *n <= 16 * 1024)
                .ok_or_else(|| corrupt("legacy published header exceeds 16 KiB"))?;
        }
    }
    let epoch: i64 = row.get(0).map_err(super::sql::sql_error)?;
    let generation: i64 = row.get(1).map_err(super::sql::sql_error)?;
    let text = |column| -> Result<&str> {
        row.get_ref(column)
            .map_err(super::sql::sql_error)?
            .as_str()
            .map_err(|e| corrupt(e.to_string()))
    };
    if epoch <= 0 || generation != epoch || text(2)? != "complete" {
        return Err(corrupt(
            "legacy published generation is invalid or incomplete",
        ));
    }
    for column in [5, 6] {
        if !matches!(
            row.get::<_, i64>(column).map_err(super::sql::sql_error)?,
            0 | 1
        ) {
            return Err(corrupt("legacy cache loss notice is not boolean"));
        }
    }
    let parser = Blake3Hash::new(text(3)?).map_err(|e| corrupt(e.message))?;
    let manifest = Blake3Hash::new(text(4)?).map_err(|e| corrupt(e.message))?;
    let snapshot = crate::domain::ReadSnapshot::canonical(epoch as u64, parser, manifest);
    if rows.next().map_err(super::sql::sql_error)?.is_some() {
        return Err(corrupt("legacy published header is not unique"));
    }
    Ok(snapshot)
}

/// Verify publication/retirement ownership using a bounded read transaction.
fn verify_sealed(fs: &VaultFs, selection: &CatalogSelection) -> Result<(Checked, u64)> {
    let checked = Checked::open(&path(fs, database_name(selection))?, false, true)?;
    let sidecars = checked_sidecars(fs, selection, &checked, false)?;
    let connection = maintenance_connection(&checked.path)?;
    let header = super::normalized_read::header(&connection, selection)?;
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM catalog_meta", [], |r| r.get(0))
        .map_err(super::sql::sql_error)?;
    if rows != 1 {
        return Err(corrupt("catalog header is not unique"));
    }
    let epoch = header.snapshot.generation;
    verify_sidecars(&sidecars)?;
    checked.verify()?;
    drop(connection);
    Ok((checked, epoch))
}

/// Read selected physical metadata without parser or general-query admission.
/// The transaction and file lease remain alive through header validation.
pub(crate) fn maintenance_header(
    fs: &VaultFs,
    vault: &RecordId,
    timeout: Duration,
) -> Result<Option<(CatalogSelection, super::normalized_read::CatalogHeader)>> {
    let mut header = None;
    let selected = acquire(fs, vault, timeout, |name, selection| {
        let connection = maintenance_connection(name)?;
        header = Some(super::normalized_read::header(&connection, selection)?);
        let mut statement = connection
            .prepare("SELECT singleton FROM catalog_meta LIMIT 2")
            .map_err(super::sql::sql_error)?;
        let mut rows = statement.query([]).map_err(super::sql::sql_error)?;
        let first = rows
            .next()
            .map_err(super::sql::sql_error)?
            .ok_or_else(|| corrupt("catalog header is absent"))?;
        if first.get::<_, i64>(0).map_err(super::sql::sql_error)? != 1
            || rows.next().map_err(super::sql::sql_error)?.is_some()
        {
            return Err(corrupt("catalog header is not unique"));
        }
        drop(rows);
        drop(statement);
        Ok(connection)
    })?;
    match selected {
        None => Ok(None),
        Some(selected) => Ok(Some((
            selected.selection().clone(),
            header.ok_or_else(|| corrupt("acquisition did not validate header"))?,
        ))),
    }
}

/// Admit canonical reconstruction of corrupt/missing cache content only after
/// validating publication control identity and every present owned file path.
/// Missing data/sidecars are repairable; unsafe bindings and lost locks are not.
pub(crate) fn validate_rebuild_predecessor(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    timeout: Duration,
) -> Result<CatalogSelection> {
    use crate::changes::operation_authority::{self as operations, Presence};
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    let authority = operations::load(fs, vault, Presence::Required)?
        .ok_or_else(|| corrupt("rebuild predecessor authority is absent"))?;
    authority.require_publication(authority.publication())?;
    let started = Instant::now();
    let gate = required_lock(
        Checked::open(&path(fs, GATE)?, false, true)?,
        false,
        timeout,
    )?;
    if !marker(fs, vault)? {
        return Err(corrupt("rebuild predecessor activation marker is absent"));
    }
    let selected =
        selection(fs, vault)?.ok_or_else(|| corrupt("rebuild predecessor selector is absent"))?;
    if selected.file_id != authority.publication().file_id {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "unfinished catalog publication requires exact acknowledged recovery",
        ));
    }
    let lease = required_lock(
        Checked::open(&path(fs, lease_name(&selected))?, false, true)?,
        false,
        timeout.saturating_sub(started.elapsed()),
    )?;
    let mut held = Vec::new();
    for kind in [
        FileKind::Database,
        FileKind::Wal,
        FileKind::Shm,
        FileKind::Journal,
    ] {
        let name = path(fs, CachePath::File(&selected.file_id, kind))?;
        if exists(&name)? {
            held.push(Checked::open(&name, false, true)?);
        }
    }
    verify_sidecars(&held)?;
    gate.checked.verify()?;
    lease.checked.verify()?;
    Ok(selected)
}

/// Complete only the exact idle-authority publication interrupted before its
/// marker/selector switch. A coherent pointer needs no physical-cache read;
/// cache reconstruction is the explicit rebuild coordinator's responsibility.
pub(crate) fn resume_acknowledged_rebuild(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    timeout: Duration,
) -> Result<bool> {
    use crate::changes::operation_authority::{self as operations, Presence};
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    let authority = operations::load(
        fs,
        vault,
        if has_activation_evidence(fs)? {
            Presence::Required
        } else {
            Presence::LegacyMayBeAbsent
        },
    )?;
    let Some(authority) = authority else {
        return Ok(false);
    };
    authority.require_publication(authority.publication())?; // requires idle
    let started = Instant::now();
    let gate = required_lock(
        Checked::open(&path(fs, GATE)?, false, true)?,
        false,
        timeout,
    )?;
    let active = marker(fs, vault)?;
    // Missing CURRENT is legitimate after the first marker became durable.
    // Malformed/foreign CURRENT remains a refusal, never an implicit repair.
    let current = read(fs, CURRENT)?
        .map(|bytes| -> Result<CatalogSelection> {
            let selected: CatalogSelection = serde_json::from_slice(&bytes)
                .map_err(|_| corrupt("catalog selector malformed; explicit rebuild required"))?;
            selected.validate(vault)?;
            Ok(selected)
        })
        .transpose()?;
    if active && current.is_some_and(|s| s.file_id == authority.publication().file_id) {
        gate.checked.verify()?;
        return Ok(false);
    }
    let file_id = &authority.publication().file_id;
    let lease = required_lock(
        Checked::open(
            &path(fs, CachePath::File(file_id, FileKind::Lease))?,
            false,
            true,
        )?,
        false,
        timeout.saturating_sub(started.elapsed()),
    )?;
    let db = Checked::open(
        &path(fs, CachePath::File(file_id, FileKind::Database))?,
        false,
        true,
    )?;
    let sidecars = checked_file_sidecars(fs, file_id, &db, false)?;
    let connection = maintenance_connection(&db.path)?;
    // Read immutable identity from this one authority-named physical file;
    // its mutable epoch must not be substituted for creation_epoch.
    let candidate = {
        let mut statement = connection
            .prepare("SELECT vault_id,file_id,creation_epoch,creation_header_hash FROM catalog_meta WHERE singleton=1")
            .map_err(super::sql::sql_error)?;
        let mut rows = statement.query([]).map_err(super::sql::sql_error)?;
        let row = rows
            .next()
            .map_err(super::sql::sql_error)?
            .ok_or_else(|| corrupt("acknowledged candidate header is absent"))?;
        let mut bytes = 0usize;
        for index in [0, 1, 3] {
            let value = row.get_ref(index).map_err(super::sql::sql_error)?;
            let text = value.as_str().map_err(|e| corrupt(e.to_string()))?;
            bytes = bytes
                .checked_add(text.len())
                .ok_or_else(|| corrupt("candidate identity byte count overflow"))?;
        }
        if bytes > 16 * 1024 {
            return Err(corrupt("candidate identity exceeds header limit"));
        }
        let epoch: i64 = row.get(2).map_err(super::sql::sql_error)?;
        CatalogSelection {
            version: super::file_types::CATALOG_FILE_VERSION,
            vault_id: RecordId::new(row.get::<_, String>(0).map_err(super::sql::sql_error)?)
                .map_err(|e| corrupt(e.message))?,
            file_id: row.get(1).map_err(super::sql::sql_error)?,
            creation_epoch: u64::try_from(epoch)
                .map_err(|_| corrupt("invalid candidate creation epoch"))?,
            creation_header_hash: Blake3Hash::new(
                row.get::<_, String>(3).map_err(super::sql::sql_error)?,
            )
            .map_err(|e| corrupt(e.message))?,
        }
    };
    candidate.validate(vault)?;
    let header = super::normalized_read::header(&connection, &candidate)?;
    if candidate.file_id != *file_id
        || header.snapshot.generation != authority.publication().epoch
        || header.origin.is_some()
    {
        return Err(corrupt(
            "acknowledged rebuild candidate differs from its authority",
        ));
    }
    verify_sidecars(&sidecars)?;
    db.verify()?;
    gate.checked.verify()?;
    lease.checked.verify()?;
    drop(connection);
    drop(lease);
    drop(gate);
    // Existing publication revalidates the exact sealed file and unchanged
    // authority under exclusive locks before restoring marker/CURRENT.
    publish(
        fs,
        writer,
        &candidate,
        timeout.saturating_sub(started.elapsed()),
    )?;
    Ok(true)
}

/// A durable complete candidate precedes the publication floor, which precedes
/// selection. A failed selector switch may resume only this exact candidate.
fn acknowledge_candidate(
    fs: &VaultFs,
    writer: &WriterPermit,
    candidate: &CatalogSelection,
    epoch: u64,
) -> Result<()> {
    use crate::changes::operation_authority::{self as operations, Presence, Publication};
    let presence = if has_activation_evidence(fs)? {
        Presence::Required
    } else {
        Presence::LegacyMayBeAbsent
    };
    let publication = Publication {
        file_id: candidate.file_id.clone(),
        epoch,
    };
    match operations::load(fs, &candidate.vault_id, presence)? {
        None => {
            // One explicit activation reconciliation, never a per-query scan.
            super::Catalog::new(fs.clone(), candidate.vault_id.clone()).guard_current(None)?;
            operations::activate(fs, writer, &candidate.vault_id, publication, presence)?;
        }
        Some(authority) if authority.publication() == &publication => {
            operations::activate(fs, writer, &candidate.vault_id, publication, presence)?;
        }
        Some(authority) => {
            let current = selection(fs, &candidate.vault_id)?;
            let reserved_reconstruction = super::missing_cache::authorize_publication(
                fs,
                &candidate.vault_id,
                candidate,
                &authority,
            )?;
            if !reserved_reconstruction
                && !current
                    .is_some_and(|selected| selected.file_id == authority.publication().file_id)
            {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "unfinished catalog publication must resume its acknowledged candidate",
                ));
            }
            operations::publish_rebuild(fs, writer, &authority, publication)?;
        }
    }
    Ok(())
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
    let (db, epoch) = verify_sealed(fs, selected)?;
    // WAL frames are persistent state; sync them before acknowledging a floor.
    for sidecar in checked_sidecars(fs, selected, &db, false)? {
        sidecar
            .file
            .sync_all()
            .map_err(|e| io("sync published SQLite sidecar", e))?;
    }
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
    acknowledge_candidate(fs, writer, selected, epoch)?;
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

/// Authenticate only immutable ownership and a recognized build state; a
/// failed provisional catalog need not contain complete serving fingerprints.
fn verify_unpublished_identity(
    connection: &Connection,
    candidate: &CatalogSelection,
) -> Result<()> {
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
    if super::sql::version(connection)? != 3 {
        return Err(corrupt("unpublished catalog schema version is not 3"));
    }
    let mut statement = connection.prepare(
        "SELECT schema_version,vault_id,file_id,creation_epoch,creation_header_hash,epoch,state FROM catalog_meta WHERE singleton=1"
    ).map_err(super::sql::sql_error)?;
    let mut rows = statement.query([]).map_err(super::sql::sql_error)?;
    let row = rows
        .next()
        .map_err(super::sql::sql_error)?
        .ok_or_else(|| corrupt("unpublished catalog header is absent"))?;
    let text = |column| -> Result<&str> {
        row.get_ref(column)
            .map_err(super::sql::sql_error)?
            .as_str()
            .map_err(|e| corrupt(e.to_string()))
    };
    let mut bytes = 0usize;
    for column in [1, 2, 4, 6] {
        bytes = bytes
            .checked_add(text(column)?.len())
            .ok_or_else(|| corrupt("unpublished header byte count overflow"))?;
    }
    if bytes > 16 * 1024
        || row.get::<_, i64>(0).map_err(super::sql::sql_error)? != 3
        || text(1)? != candidate.vault_id.as_str()
        || text(2)? != candidate.file_id
        || row.get::<_, i64>(3).map_err(super::sql::sql_error)?
            != super::sql::integer(candidate.creation_epoch)?
        || text(4)? != candidate.creation_header_hash.as_str()
        || row.get::<_, i64>(5).map_err(super::sql::sql_error)?
            < super::sql::integer(candidate.creation_epoch)?
        || !matches!(text(6)?, "building" | "complete")
    {
        return Err(corrupt(
            "unpublished catalog immutable identity does not match owner",
        ));
    }
    drop(rows);
    drop(statement);
    let count: i64 = connection
        .query_row("SELECT count(*) FROM catalog_meta", [], |r| r.get(0))
        .map_err(super::sql::sql_error)?;
    if count != 1 {
        return Err(corrupt("unpublished catalog header is not unique"));
    }
    Ok(())
}

pub(crate) fn retire(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    candidate: &CatalogSelection,
    timeout: Duration,
) -> Result<bool> {
    retire_candidate(fs, writer, vault, candidate, timeout, false)
}

/// Caller owns a candidate whose builder successfully began and has dropped.
/// Never use this after a failed begin, which could be an identity collision.
pub(crate) fn retire_unpublished(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    candidate: &CatalogSelection,
    timeout: Duration,
) -> Result<bool> {
    retire_candidate(fs, writer, vault, candidate, timeout, true)
}

fn retire_candidate(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    candidate: &CatalogSelection,
    timeout: Duration,
    unpublished: bool,
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
    if crate::changes::operation_authority::load(
        fs,
        vault,
        if has_activation_evidence(fs)? {
            crate::changes::operation_authority::Presence::Required
        } else {
            crate::changes::operation_authority::Presence::LegacyMayBeAbsent
        },
    )?
    .is_some_and(|authority| authority.publication().file_id == candidate.file_id)
    {
        // An interrupted switch still needs its exact acknowledged candidate.
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
    let db = if unpublished {
        let db = Checked::open(&path(fs, database_name(candidate))?, false, true)?;
        // A dropped builder can checkpoint and remove nonpersistent WAL/SHM.
        // Only this exact owned-abort path may admit their absence.
        let sidecars = checked_file_sidecars_with_missing(fs, &candidate.file_id, &db, true, true)?;
        let connection = writable(&db.path)?;
        verify_unpublished_identity(&connection, candidate)?;
        verify_sidecars(&sidecars)?;
        db.verify()?;
        // Checked sidecar handles must close before native SQLite removes them
        // (Windows deliberately denies deletion while those handles are held).
        drop(sidecars);
        persist_wal(&connection, false)?;
        let mode: String = connection
            .pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))
            .map_err(super::sql::sql_error)?;
        if !mode.eq_ignore_ascii_case("delete") {
            return Err(corrupt("unpublished catalog remains in WAL mode"));
        }
        connection
            .close()
            .map_err(|(_, error)| super::sql::sql_error(error))?;
        no_sidecars(fs, candidate)?;
        db
    } else {
        verify_sealed(fs, candidate)?.0
    };
    if !unpublished && journal_header(&db)? == JournalMode::Wal {
        // SQLite checkpoints and deletes its own sidecars under native locks.
        // Never unlink a WAL that might contain committed database pages.
        let connection = writable(&db.path)?;
        persist_wal(&connection, false)?;
        let mode: String = connection
            .pragma_update_and_check(None, "journal_mode", "DELETE", |r| r.get(0))
            .map_err(super::sql::sql_error)?;
        if !mode.eq_ignore_ascii_case("delete") {
            return Err(corrupt("retired catalog remains in WAL mode"));
        }
        drop(connection);
        no_sidecars(fs, candidate)?;
    }
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
    mod maintenance_selector_tests {
        include!("maintenance_selector_tests.rs");
    }

    use super::*;
    use crate::vault::{DurableIo, NativeIo, VaultRoot};
    use std::sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc,
    };

    fn fixture() -> (tempfile::TempDir, VaultFs, WriterPermit, RecordId) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            "---\nwiki_schema: \"1\"\nwiki_id: vault_selector_test\nwiki_kind: vault\ntitle: Disposable selector fixture\n---\nFixture\n",
        ).unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
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
        // Several tests prepare both candidates before publishing either one.
        // Give every candidate a distinct increasing epoch independently of the
        // currently selected pointer, including those unselected preparations.
        static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);
        let selected =
            CatalogSelection::new(vault.clone(), NEXT_EPOCH.fetch_add(1, Ordering::Relaxed))
                .unwrap();
        let name = prepare(fs, writer, &selected).unwrap();
        let connection = Connection::open(name).unwrap();
        connection
            .execute_batch(super::super::normalized_schema::SCHEMA)
            .unwrap();
        let hash = Blake3Hash::digest(b"fixture");
        connection.execute("INSERT INTO catalog_meta(singleton,schema_version,vault_id,file_id,creation_epoch,creation_header_hash,epoch,parser_hash,publication_hash,control_hash,dependency_hash,audit_epoch,state,vector_cache_lost,vector_loss_unknown) VALUES(1,3,?1,?2,?3,?4,?3,?5,?5,?5,?5,?3,'complete',0,0)",
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

    fn make_wal(fs: &VaultFs, selected: &CatalogSelection) {
        let connection = Connection::open(path(fs, database_name(selected)).unwrap()).unwrap();
        configure_wal(&connection).unwrap();
        drop(connection);
        let db = Checked::open(&path(fs, database_name(selected)).unwrap(), false, true).unwrap();
        assert_eq!(checked_sidecars(fs, selected, &db, false).unwrap().len(), 2);
    }

    #[test]
    fn wal_delta_preserves_old_snapshot_and_atomic_fts_visibility() {
        let (_temp, fs, writer, vault) = fixture();
        let first = build(&fs, &writer, &vault, "oldneedle");
        make_wal(&fs, &first);
        put(&fs, &writer, &first);
        let old = get(&fs, &vault).unwrap().unwrap();
        // A ready preflight and ordinary writer must not request old's
        // exclusive lifetime lease, even with a zero wait budget.
        ensure_delta_ready(&fs, &writer, &first, Duration::ZERO).unwrap();
        let delta = open_delta(&fs, &writer, &first, Duration::ZERO).unwrap();
        assert_eq!(delta.selection(), &first);
        let transaction = rusqlite::Transaction::new_unchecked(
            delta.connection(),
            rusqlite::TransactionBehavior::Immediate,
        )
        .unwrap();
        transaction.execute_batch("UPDATE test_content SET text='newneedle'; DELETE FROM test_fts; INSERT INTO test_fts VALUES('newneedle'); UPDATE catalog_meta SET epoch=epoch+1,control_hash=NULL,dependency_hash=NULL,audit_epoch=NULL;").unwrap();
        let during = get(&fs, &vault).unwrap().unwrap();
        assert_eq!(text(&during), "oldneedle");
        transaction.commit().unwrap();
        drop(delta);
        let new = get(&fs, &vault).unwrap().unwrap();
        assert_eq!(text(&old), "oldneedle");
        assert_eq!(text(&during), "oldneedle");
        assert_eq!(text(&new), "newneedle");
        for (reader, token, epoch) in [
            (&old, "oldneedle", first.creation_epoch),
            (&new, "newneedle", first.creation_epoch + 1),
        ] {
            assert_eq!(
                reader
                    .value()
                    .query_row(
                        "SELECT count(*) FROM test_fts WHERE test_fts MATCH ?1",
                        [token],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
            assert_eq!(
                super::super::normalized_read::header(reader.value(), &first)
                    .unwrap()
                    .snapshot
                    .generation,
                epoch
            );
        }
        let second = build(&fs, &writer, &vault, "replacement");
        make_wal(&fs, &second);
        put(&fs, &writer, &second);
        assert!(!retire(&fs, &writer, &vault, &first, Duration::ZERO).unwrap());
        drop(old);
        drop(during);
        drop(new);
        assert!(retire(&fs, &writer, &vault, &first, Duration::ZERO).unwrap());
        no_sidecars(&fs, &first).unwrap();
        assert!(!path(&fs, database_name(&first)).unwrap().exists());
    }

    #[test]
    fn first_wal_migration_refuses_held_reader_before_any_write() {
        let (_temp, fs, writer, vault) = fixture();
        let first = build(&fs, &writer, &vault, "unchanged");
        put(&fs, &writer, &first);
        let old = get(&fs, &vault).unwrap().unwrap();
        let name = path(&fs, database_name(&first)).unwrap();
        let before = fs::read(&name).unwrap();
        let modified = fs::metadata(&name).unwrap().modified().unwrap();
        assert_eq!(
            ensure_delta_ready(&fs, &writer, &first, Duration::ZERO)
                .unwrap_err()
                .code,
            ErrorCode::LockTimeout
        );
        assert!(open_delta(&fs, &writer, &first, Duration::ZERO).is_err());
        assert_eq!(fs::read(&name).unwrap(), before);
        assert_eq!(fs::metadata(&name).unwrap().modified().unwrap(), modified);
        no_sidecars(&fs, &first).unwrap();
        drop(old);
        ensure_delta_ready(&fs, &writer, &first, Duration::ZERO).unwrap();
        assert_eq!(text(&get(&fs, &vault).unwrap().unwrap()), "unchanged");
        assert!(open_delta(&fs, &writer, &first, Duration::ZERO).is_ok());
    }

    #[test]
    fn wal_sidecars_are_required_and_unsafe_objects_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for kind in [FileKind::Wal, FileKind::Shm] {
            for variant in 0..5 {
                let (_temp, fs, writer, vault) = fixture();
                let first = build(&fs, &writer, &vault, "safe");
                make_wal(&fs, &first);
                put(&fs, &writer, &first);
                let sidecar = path(&fs, CachePath::File(&first.file_id, kind)).unwrap();
                let bytes = fs::read(&sidecar).unwrap();
                fs::remove_file(&sidecar).unwrap();
                let other = fs.root().path().join("unrelated");
                fs::write(&other, &bytes).unwrap();
                match variant {
                    0 => {}
                    1 => symlink(&other, &sidecar).unwrap(),
                    2 => fs::hard_link(&other, &sidecar).unwrap(),
                    3 => fs::create_dir(&sidecar).unwrap(),
                    4 => {
                        fs::write(&sidecar, &bytes).unwrap();
                        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o666)).unwrap();
                    }
                    _ => unreachable!(),
                }
                assert!(get(&fs, &vault).is_err());
                assert!(open_delta(&fs, &writer, &first, Duration::ZERO).is_err());
                assert_eq!(fs::read(&other).unwrap(), bytes);
                if variant == 0 && matches!(kind, FileKind::Shm) {
                    // SHM can be reconstructed from the retained checked WAL.
                    // Even a clean-looking missing WAL requires explicit rebuild.
                    ensure_delta_ready(&fs, &writer, &first, Duration::ZERO).unwrap();
                    assert_eq!(text(&get(&fs, &vault).unwrap().unwrap()), "safe");
                } else {
                    assert!(ensure_delta_ready(&fs, &writer, &first, Duration::ZERO).is_err());
                }
            }
        }
    }

    #[test]
    fn lost_uncheckpointed_wal_refuses_without_mutating_surviving_state() {
        let (_temp, fs, writer, vault) = fixture();
        let selected = build(&fs, &writer, &vault, "oldcommitted");
        make_wal(&fs, &selected);
        put(&fs, &writer, &selected);
        let name = path(&fs, database_name(&selected)).unwrap();
        let old_main = fs::read(&name).unwrap();
        let connection = writable(&name).unwrap();
        connection
            .set_db_config(
                rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                true,
            )
            .unwrap();
        connection.execute_batch("PRAGMA wal_autocheckpoint=0; BEGIN IMMEDIATE; UPDATE test_content SET text='newcommitted'; UPDATE catalog_meta SET epoch=epoch+1; COMMIT;").unwrap();
        assert_eq!(
            connection
                .query_row("SELECT text FROM test_content", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "newcommitted"
        );
        drop(connection);
        assert_eq!(
            fs::read(&name).unwrap(),
            old_main,
            "committed update exists only in WAL"
        );
        let wal = path(&fs, CachePath::File(&selected.file_id, FileKind::Wal)).unwrap();
        assert!(fs::metadata(&wal).unwrap().len() > 32);
        fs::remove_file(&wal).unwrap();
        fn state(directory: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
            let mut files = Vec::new();
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    files.extend(state(&path));
                } else {
                    files.push((
                        path.clone(),
                        fs::read(&path).unwrap(),
                        fs::metadata(&path).unwrap().modified().unwrap(),
                    ));
                }
            }
            files.sort_by(|a, b| a.0.cmp(&b.0));
            files
        }
        let before = state(fs.root().path());
        assert_eq!(
            ensure_delta_ready(&fs, &writer, &selected, Duration::ZERO)
                .unwrap_err()
                .code,
            ErrorCode::RecoveryRequired
        );
        assert!(get(&fs, &vault).is_err());
        assert!(open_delta(&fs, &writer, &selected, Duration::ZERO).is_err());
        assert_eq!(state(fs.root().path()), before);
        assert!(!wal.exists());
    }

    #[test]
    fn retained_committed_wal_reconstructs_missing_shm_at_latest_epoch() {
        let (_temp, fs, writer, vault) = fixture();
        let selected = build(&fs, &writer, &vault, "oldcommitted");
        make_wal(&fs, &selected);
        put(&fs, &writer, &selected);
        let name = path(&fs, database_name(&selected)).unwrap();
        let old_main = fs::read(&name).unwrap();
        let connection = writable(&name).unwrap();
        connection
            .set_db_config(
                rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                true,
            )
            .unwrap();
        connection.execute_batch("PRAGMA wal_autocheckpoint=0; BEGIN IMMEDIATE; UPDATE test_content SET text='newcommitted'; UPDATE catalog_meta SET epoch=epoch+1; COMMIT;").unwrap();
        drop(connection);
        assert_eq!(
            fs::read(&name).unwrap(),
            old_main,
            "committed update exists only in retained WAL"
        );
        let wal = path(&fs, CachePath::File(&selected.file_id, FileKind::Wal)).unwrap();
        let committed_wal = fs::read(&wal).unwrap();
        assert!(committed_wal.len() > 32);
        let shm = path(&fs, CachePath::File(&selected.file_id, FileKind::Shm)).unwrap();
        fs::remove_file(&shm).unwrap();
        assert_eq!(
            get(&fs, &vault).err().unwrap().code,
            ErrorCode::RecoveryRequired
        );
        assert!(!shm.exists(), "query must not reconstruct SHM");
        assert_eq!(fs::read(&name).unwrap(), old_main);
        assert_eq!(fs::read(&wal).unwrap(), committed_wal);

        ensure_delta_ready(&fs, &writer, &selected, Duration::ZERO).unwrap();
        assert!(shm.is_file());
        assert!(wal.is_file());
        let reader = get(&fs, &vault).unwrap().unwrap();
        assert_eq!(text(&reader), "newcommitted");
        assert_eq!(
            super::super::normalized_read::header(reader.value(), &selected)
                .unwrap()
                .snapshot
                .generation,
            selected.creation_epoch + 1
        );
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
        // An interrupted activation may retain only the marker, but its
        // acknowledged authority must already have been made durable.
        acknowledge_candidate(&fs, &writer, &selected, selected.creation_epoch).unwrap();
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
    #[test]
    fn interrupted_rebuild_resumes_exact_acknowledged_candidate() {
        use crate::changes::operation_authority::{self as operations, Presence};
        let (_temp, fs, writer, vault) = fixture();
        let first = build(&fs, &writer, &vault, "first");
        put(&fs, &writer, &first);
        let second = build(&fs, &writer, &vault, "second");
        assert!(
            publish_with_faults(&fs, &writer, &second, Duration::ZERO, &mut |point| {
                if point == PublishPoint::BeforeMarker {
                    Err(corrupt("interrupted switch"))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        let pending = operations::load(&fs, &vault, Presence::Required)
            .unwrap()
            .unwrap();
        assert_eq!(pending.publication().file_id, second.file_id);
        assert_eq!(
            selection(&fs, &vault).unwrap().unwrap().file_id,
            first.file_id
        );
        assert!(!retire(&fs, &writer, &vault, &second, Duration::ZERO).unwrap());
        let third = build(&fs, &writer, &vault, "unrelated replacement");
        assert_eq!(
            publish(&fs, &writer, &third, Duration::ZERO)
                .unwrap_err()
                .code,
            ErrorCode::RecoveryRequired
        );
        put(&fs, &writer, &second);
        assert_eq!(text(&get(&fs, &vault).unwrap().unwrap()), "second");
        let resumed = operations::load(&fs, &vault, Presence::Required)
            .unwrap()
            .unwrap();
        assert_eq!(resumed.revision(), pending.revision());
        assert_eq!(resumed.publication(), pending.publication());
    }
    #[test]
    fn activation_reconciles_unresolved_legacy_intent_before_establishing_idle() {
        use crate::changes::{ChangeDraft, ChangeEngine, ChangeEvent, ExpectedWrite, journal};
        let (_temp, fs, writer, vault) = fixture();
        let engine = ChangeEngine::new(fs.clone()).unwrap();
        let staged = engine
            .prepare(
                &writer,
                ChangeDraft {
                    title: "Unresolved legacy update".into(),
                    origin: None,
                    inverse_of: None,
                    allocated_ids: Default::default(),
                    read_preconditions: vec![],
                    operations: vec![ExpectedWrite {
                        target: VaultRelativePath::new("draft.md").unwrap(),
                        expected: ExpectedState::Absent,
                        proposed: Some(b"proposed".to_vec()),
                        apply_after: vec![],
                    }],
                },
            )
            .unwrap();
        journal::append_event(
            &fs,
            &writer,
            &staged.manifest,
            &staged.prepared.manifest_hash,
            ChangeEvent::Applying,
        )
        .unwrap();
        let candidate = build(&fs, &writer, &vault, "candidate");
        assert_eq!(
            publish(&fs, &writer, &candidate, Duration::ZERO)
                .unwrap_err()
                .code,
            ErrorCode::RecoveryRequired
        );
        assert!(
            !fs.root()
                .path()
                .join(".wiki/state/operations.json")
                .exists()
        );
        assert!(!has_activation_evidence(&fs).unwrap());
    }
    #[test]
    fn normalized_activation_fences_legacy_readers_sync_and_rebuild() {
        let (_temp, fs, writer, vault) = fixture();
        let catalog = super::super::Catalog::new(fs.clone(), vault.clone());
        catalog.sync(&writer).unwrap();
        drop(catalog.index_snapshot().unwrap());
        let candidate = build(&fs, &writer, &vault, "normalized");
        put(&fs, &writer, &candidate);
        let before = snapshot(fs.root().path());
        for error in [
            catalog.index_snapshot().err().unwrap(),
            catalog.verified_snapshot(Some(&writer)).err().unwrap(),
            catalog.canonical_snapshot().err().unwrap(),
            catalog.check_available().unwrap_err(),
            catalog.sync(&writer).unwrap_err(),
            catalog.rebuild(&writer).unwrap_err(),
        ] {
            assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
        }
        assert_eq!(before, snapshot(fs.root().path()));
    }
    struct RenameFault {
        point: u8,
        after: AtomicU8,
    }
    impl DurableIo for RenameFault {
        fn create_stage(&self, p: &Path) -> std::io::Result<File> {
            NativeIo.create_stage(p)
        }
        fn create_private_stage(&self, p: &Path) -> std::io::Result<File> {
            NativeIo.create_private_stage(p)
        }
        fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
            NativeIo.create_private_directory(p)
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
            } else if t.ends_with("catalog-current.json") {
                2
            } else {
                return NativeIo.replace(s, t);
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
            let error = publish(&faulty, &writer, &selected, Duration::ZERO).unwrap_err();
            assert!(
                error.message.contains("injected"),
                "fault point {point} did not reach injected failure: {error:?}"
            );
            let result = get(&fs, &vault);
            match point {
                1 => assert!(result.unwrap().is_none()),
                2 | 3 => assert!(
                    result.is_err(),
                    "fault point {point}: expected retained activation marker; present={}",
                    path(&fs, ACTIVE).unwrap().exists()
                ),
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

pub(super) fn read_rebuild_record(fs: &VaultFs) -> Result<Option<Vec<u8>>> {
    read(fs, CachePath::RebuildState)
}
pub(super) fn store_rebuild_record(
    fs: &VaultFs,
    writer: &WriterPermit,
    expected: &ExpectedState,
    bytes: Option<&[u8]>,
) -> Result<()> {
    let name = VaultRelativePath::new(CachePath::RebuildState.relative()?)?;
    // Validate the exact control ancestry and present file before CAS.
    let _ = read_rebuild_record(fs)?;
    if let Some(bytes) = bytes {
        if bytes.len() > JSON_LIMIT {
            return Err(corrupt("rebuild witness exceeds 4096 bytes"));
        }
        let staged = fs.stage(&name, bytes, writer)?;
        durable(fs.replace(staged, expected, writer)?)
    } else {
        durable(fs.delete(&name, expected, writer)?)
    }
}
pub(super) fn cache_root_absent(fs: &VaultFs) -> Result<bool> {
    Ok(!exists(&path(fs, CachePath::CacheRoot)?)?)
}
pub(super) fn require_unselected_rebuild(fs: &VaultFs, vault: &RecordId) -> Result<()> {
    let _ = vault;
    if read(fs, CURRENT)?.is_some() || read(fs, ACTIVE)?.is_some() {
        return Err(corrupt(
            "unacknowledged cache reconstruction has publication controls",
        ));
    }
    Ok(())
}
/// Measure only fixed witness-reserved artifacts, with checked no-follow handles.
/// Missing members are not recreated; caller must enforce cumulative limits.
pub(super) fn measure_abandoned_candidate(
    fs: &VaultFs,
    candidate: &CatalogSelection,
) -> Result<u64> {
    candidate.validate(&candidate.vault_id)?;
    let mut total = 0u64;
    for kind in [
        FileKind::Database,
        FileKind::Wal,
        FileKind::Shm,
        FileKind::Journal,
        FileKind::Lease,
    ] {
        let name = path(fs, CachePath::File(&candidate.file_id, kind))?;
        if !exists(&name)? {
            continue;
        }
        let checked = Checked::open(&name, false, matches!(kind, FileKind::Lease))?;
        let size = checked
            .file
            .metadata()
            .map_err(|e| io("measure rebuild artifact", e))?
            .len();
        if matches!(kind, FileKind::Lease) {
            if size != 0 {
                return Err(corrupt("rebuild lease is not an empty reservation"));
            }
        } else {
            total = total
                .checked_add(size)
                .ok_or_else(|| corrupt("rebuild artifact bytes overflow"))?;
        }
        checked.verify()?;
    }
    let _ = checkpointed_rebuild_identity(fs, candidate)?;
    Ok(total)
}
fn checkpointed_rebuild_identity(fs: &VaultFs, candidate: &CatalogSelection) -> Result<bool> {
    let name = path(fs, database_name(candidate))?;
    if !exists(&name)? {
        return Ok(false);
    }
    let db = Checked::open(&name, false, false)?;
    let authenticated = match rebuild_identity_connection(&name) {
        Ok(connection) => {
            let header = connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='catalog_meta')", [], |row| row.get::<_, bool>(0));
            match header {
                Ok(true) => {
                    verify_unpublished_identity(&connection, candidate)?;
                    true
                }
                Ok(false) | Err(_) => false,
            }
        }
        Err(_) => false,
    };
    db.verify()?;
    Ok(authenticated)
}
/// Immutable SQLite inspection cannot create sidecars or recover/edit a candidate.
/// It intentionally sees only the main file's checkpointed header; an unfinished
/// initialization remains preserved under the witness's small-file ceiling.
fn rebuild_identity_connection(name: &Path) -> Result<Connection> {
    let name = name
        .to_str()
        .ok_or_else(|| corrupt("catalog path is not UTF-8"))?;
    let mut uri = String::from("file:");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(super::sql::sql_error)?;
    connection
        .set_limit(
            rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
            8 * 1024 * 1024,
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
    Ok(connection)
}
pub(super) fn inspect_unacknowledged_candidate(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    candidate: &CatalogSelection,
    timeout: Duration,
) -> Result<super::missing_cache::CandidateDisposition> {
    use super::missing_cache::CandidateDisposition;
    writer.require_root(fs.root())?;
    require_unselected_rebuild(fs, vault)?;
    let bytes = measure_abandoned_candidate(fs, candidate)?;
    let db_name = path(fs, database_name(candidate))?;
    let has_db = exists(&db_name)?;
    let any_sidecar = [FileKind::Wal, FileKind::Shm, FileKind::Journal]
        .into_iter()
        .map(|kind| {
            path(fs, CachePath::File(&candidate.file_id, kind)).and_then(|name| exists(&name))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .any(|exists| exists);
    if !has_db && !any_sidecar {
        return Ok(CandidateDisposition::Retired);
    }
    // Every database creation follows durable gate/lease reservation. Never
    // classify an independently supplied file without those exact reservations.
    let gate = required_lock(Checked::open(&path(fs, GATE)?, false, true)?, true, timeout)?;
    let lease = required_lock(
        Checked::open(&path(fs, lease_name(candidate))?, false, true)?,
        true,
        timeout,
    )?;
    let authenticated = checkpointed_rebuild_identity(fs, candidate)?;
    gate.checked.verify()?;
    lease.checked.verify()?;
    drop(lease);
    drop(gate);
    if authenticated {
        if !retire_unpublished(fs, writer, vault, candidate, timeout)? {
            return Err(corrupt(
                "unacknowledged rebuild candidate could not be retired",
            ));
        }
        Ok(CandidateDisposition::Retired)
    } else {
        Ok(CandidateDisposition::Preserved { bytes })
    }
}

/// This census runs only during explicit reconstruction, never ordinary queries.
/// Its finite inventory authorizes no deletion and never adopts unrelated files.
pub(super) fn validate_unselected_rebuild_namespace(
    fs: &VaultFs,
    candidates: &[CatalogSelection],
) -> Result<()> {
    require_unselected_rebuild(
        fs,
        &candidates
            .first()
            .ok_or_else(|| corrupt("rebuild reservation is absent"))?
            .vault_id,
    )?;
    if cache_root_absent(fs)? {
        return Ok(());
    }
    let cache = path(fs, CachePath::CacheRoot)?;
    let mut steps = 0usize;
    let mut inspect = |directory: &Path| -> Result<Vec<(String, PathBuf)>> {
        let mut entries = vec![];
        for entry in
            std::fs::read_dir(directory).map_err(|e| io("read reserved rebuild namespace", e))?
        {
            steps += 1;
            if steps > 4096 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "rebuild namespace exceeds 4096 entries",
                ));
            }
            let entry = entry.map_err(|e| io("read rebuild member", e))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| corrupt("rebuild namespace contains a non-UTF-8 name"))?;
            entries.push((name, entry.path()));
        }
        Ok(entries)
    };
    for (name, entry) in inspect(&cache)? {
        match name.as_str() {
            "catalogs" => {
                let _ = path(fs, DIRECTORY)?;
                let allowed = candidates
                    .iter()
                    .flat_map(|candidate| {
                        [
                            ".sqlite",
                            ".sqlite-wal",
                            ".sqlite-shm",
                            ".sqlite-journal",
                            ".lock",
                        ]
                        .map(|suffix| format!("{}{suffix}", candidate.file_id))
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                for (name, entry) in inspect(&entry)? {
                    let tombstone = name.strip_suffix(".lock").is_some_and(|id| {
                        id.len() == 32
                            && id
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    });
                    if !allowed.contains(&name) && !tombstone {
                        return Err(corrupt("rebuild namespace contains an unreserved member"));
                    }
                    let checked = Checked::open(&entry, false, tombstone)?;
                    if tombstone
                        && checked
                            .file
                            .metadata()
                            .map_err(|e| io("inspect rebuild tombstone", e))?
                            .len()
                            != 0
                    {
                        return Err(corrupt("rebuild tombstone is not empty"));
                    }
                    checked.verify()?;
                }
            }
            "catalog-acquisition.lock" => {
                let checked = Checked::open(&entry, false, true)?;
                if checked
                    .file
                    .metadata()
                    .map_err(|e| io("inspect rebuild gate", e))?
                    .len()
                    != 0
                {
                    return Err(corrupt("rebuild gate is not empty"));
                }
                checked.verify()?;
            }
            _ => return Err(corrupt("rebuild cache root contains an unreserved member")),
        }
    }
    Ok(())
}

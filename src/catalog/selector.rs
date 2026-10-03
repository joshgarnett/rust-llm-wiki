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
/// Any retained activation evidence requires operational authority, including
/// an interrupted selector replacement. Never interpret corruption as legacy.
pub(crate) fn has_activation_evidence(fs: &VaultFs) -> Result<bool> {
    Ok(read(fs, ACTIVE)?.is_some() || read(fs, CURRENT)?.is_some())
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
    if journal_header(db)? == JournalMode::Delete {
        no_sidecars(fs, selected)?;
        return Ok(Vec::new());
    }
    if exists(&path(
        fs,
        CachePath::File(&selected.file_id, FileKind::Journal),
    )?)? {
        return Err(corrupt("WAL catalog has unexpected rollback journal"));
    }
    let mut held = Vec::new();
    for kind in [FileKind::Wal, FileKind::Shm] {
        let name = path(fs, CachePath::File(&selected.file_id, kind))?;
        if !exists(&name)? {
            if allow_missing && matches!(kind, FileKind::Shm) {
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

/// Verify publication/retirement ownership using a bounded read transaction.
fn verify_sealed(fs: &VaultFs, selection: &CatalogSelection) -> Result<(Checked, u64)> {
    let checked = Checked::open(&path(fs, database_name(selection))?, false, true)?;
    let sidecars = checked_sidecars(fs, selection, &checked, false)?;
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
            if !current.is_some_and(|selected| selected.file_id == authority.publication().file_id)
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
    let (db, _) = verify_sealed(fs, candidate)?;
    if journal_header(&db)? == JournalMode::Wal {
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

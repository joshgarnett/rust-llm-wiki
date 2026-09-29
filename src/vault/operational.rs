//! Private, bounded job persistence. This authority cannot write canonical records.
use super::{BeforeImage, DirectorySync, DurableIo, ExpectedState, VaultFs, WriterPermit};
use crate::{domain::*, jobs::AttemptRef, records::parse_note};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::io::Seek;
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const JOURNAL_LIMIT: u64 = 64 * 1024 * 1024;
const FRAME_LIMIT: usize = 256 * 1024;
const OWNER_LIMIT: u64 = 4096;
#[derive(Debug, Clone, Copy)]
pub(crate) enum SpoolPart {
    Body,
    Metadata,
}
pub(crate) enum RunFile<'a> {
    Spool {
        attempt: &'a AttemptRef,
        part: SpoolPart,
    },
    Checkpoint,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Owner {
    version: u8,
    vault_id: RecordId,
    attempt: AttemptRef,
}
pub(crate) struct RunStore {
    fs: VaultFs,
    vault_id: RecordId,
    run_id: RecordId,
    io: Arc<dyn DurableIo>,
    #[cfg(windows)]
    pinned_dirs: std::sync::Mutex<Vec<super::windows_security::DirectoryGuard>>,
}
pub(crate) struct RunLedgerGuard<'a> {
    store: &'a RunStore,
    file: File,
    lock_path: PathBuf,
    #[cfg(windows)]
    pinned_dirs: Vec<super::windows_security::DirectoryGuard>,
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn io_error(action: &str, _: std::io::Error) -> WikiError {
    WikiError::new(ErrorCode::Internal, action)
}
fn private_file(file: &File) -> Result<()> {
    #[cfg(windows)]
    super::windows_security::validate_file_security(file, super::acl_policy::Protection::Private)
        .map_err(|e| io_error("validate private job file protection", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| io_error("restrict private job file", e))?;
    }
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    #[cfg(windows)]
    super::windows_security::open_pinned_directory(path, super::acl_policy::Protection::Private)
        .map_err(|e| io_error("validate private job directory protection", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|e| io_error("restrict private job directory", e))?;
    }
    Ok(())
}
fn same_inode(file: &File, path: &Path) -> Result<()> {
    let a = file
        .metadata()
        .map_err(|e| io_error("inspect held job inode", e))?;
    let b = fs::symlink_metadata(path).map_err(|e| io_error("inspect current job inode", e))?;
    if !b.is_file() || b.file_type().is_symlink() {
        return Err(conflict("job lock must remain a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return Err(conflict("job lock inode changed"));
        }
    }
    #[cfg(windows)]
    {
        let _ = a;
        super::windows_security::validate_same_file(
            file,
            path,
            super::acl_policy::Protection::Private,
        )
        .map_err(|e| io_error("validate held job file identity", e))?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = a;
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "protected file identity unavailable",
        ));
    }
    Ok(())
}
#[cfg(not(windows))]
fn read(path: &Path, limit: u64) -> Result<Option<BeforeImage>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error("inspect private job file", e)),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(conflict("private job payload must be a regular file"));
    }
    if meta.len() > limit {
        return Err(budget("private job payload exceeds read ceiling"));
    }
    let file = File::open(path).map_err(|e| io_error("open private job payload", e))?;
    same_inode(&file, path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("read private job payload", e))?;
    if bytes.len() as u64 > limit {
        return Err(budget("private job payload exceeds read ceiling"));
    }
    Ok(Some(BeforeImage {
        hash: Blake3Hash::digest(&bytes),
        bytes,
    }))
}
#[cfg(windows)]
fn read_windows(
    path: &Path,
    limit: u64,
    protection: super::acl_policy::Protection,
) -> Result<Option<BeforeImage>> {
    let _parent = if protection == super::acl_policy::Protection::Private {
        Some(
            super::windows_security::open_pinned_directory(
                path.parent()
                    .ok_or_else(|| conflict("private file has no parent"))?,
                super::acl_policy::Protection::Private,
            )
            .map_err(|error| io_error("pin private payload directory", error))?,
        )
    } else {
        None
    };
    let meta = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("inspect protected job payload", error)),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(conflict("job payload must be a regular file"));
    }
    if meta.len() > limit {
        return Err(budget("job payload exceeds read ceiling"));
    }
    let limit =
        usize::try_from(limit).map_err(|_| budget("job payload ceiling exceeds platform"))?;
    let bytes = super::windows_security::read_protected(path, limit, protection)
        .map_err(|error| io_error("read protected held job payload", error))?;
    Ok(Some(BeforeImage {
        hash: Blake3Hash::digest(&bytes),
        bytes,
    }))
}
#[cfg(windows)]
fn read(path: &Path, limit: u64) -> Result<Option<BeforeImage>> {
    read_windows(path, limit, super::acl_policy::Protection::Private)
}
fn require_vault(fs: &VaultFs, vault_id: &RecordId) -> Result<()> {
    let path = fs.root().resolve(&VaultRelativePath::new("WIKI.md")?)?;
    #[cfg(windows)]
    let image = read_windows(
        &path,
        256 * 1024,
        super::acl_policy::Protection::IntegrityProtected,
    )?;
    #[cfg(not(windows))]
    let image = read(&path, 256 * 1024)?;
    let bytes = image
        .ok_or_else(|| conflict("vault identity missing"))?
        .bytes;
    let note = parse_note(&bytes);
    if !note
        .canonical
        .as_ref()
        .is_some_and(|r| r.kind() == RecordKind::Vault && r.id() == vault_id)
    {
        return Err(conflict("job storage vault identity changed"));
    }
    Ok(())
}
impl RunStore {
    /// Include incomplete namespaces; the caller validates each genesis/head.
    /// Discovery grants no accounting, canonical, or dispatch authority.
    pub(crate) fn discover_existing(
        fs: &VaultFs,
        vault_id: &RecordId,
        max_entries: usize,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<RecordId>> {
        if max_entries == 0 || max_entries > 4096 {
            return Err(budget("invalid private run discovery ceiling"));
        }
        check()?;
        require_vault(fs, vault_id)?;
        let directory = fs
            .root()
            .resolve(&VaultRelativePath::new(".wiki/state/jobs")?)?;
        #[cfg(windows)]
        let _directory = match super::windows_security::open_pinned_directory(
            &directory,
            super::acl_policy::Protection::Private,
        ) {
            Ok(guard) => guard,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(io_error("pin private run discovery directory", error)),
        };
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(io_error("enumerate private run namespaces", error)),
        };
        let mut ids = std::collections::BTreeSet::new();
        for (index, entry) in entries.enumerate() {
            check()?;
            if index >= max_entries {
                return Err(budget("private run discovery ceiling exceeded"));
            }
            let entry = entry.map_err(|error| io_error("read private run entry", error))?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| conflict("non-UTF-8 private run namespace"))?;
            let id = RecordId::new(name)
                .map_err(|_| conflict("invalid private run namespace identity"))?;
            let child = fs
                .root()
                .resolve(&VaultRelativePath::new(format!(".wiki/state/jobs/{id}"))?)?;
            let metadata = fs::symlink_metadata(&child)
                .map_err(|error| io_error("inspect private run namespace", error))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(conflict("private run namespace must be a directory"));
            }
            #[cfg(windows)]
            let _child = super::windows_security::open_pinned_directory(
                &child,
                super::acl_policy::Protection::Private,
            )
            .map_err(|error| io_error("pin discovered private run", error))?;
            ids.insert(id);
        }
        require_vault(fs, vault_id)?;
        Ok(ids.into_iter().collect())
    }
    fn new(fs: &VaultFs, vault_id: &RecordId, run_id: &RecordId) -> Result<Self> {
        let store = Self {
            fs: fs.clone(),
            vault_id: vault_id.clone(),
            run_id: run_id.clone(),
            io: fs.durable_io(),
            #[cfg(windows)]
            pinned_dirs: std::sync::Mutex::new(vec![
                super::windows_security::open_pinned_directory(
                    fs.root().path(),
                    super::acl_policy::Protection::IntegrityProtected,
                )
                .map_err(|error| io_error("pin private job root", error))?,
            ]),
        };
        #[cfg(windows)]
        for relative in [
            ".wiki".to_owned(),
            ".wiki/state".to_owned(),
            ".wiki/state/jobs".to_owned(),
            store.run_dir(),
            ".wiki/state/requests".to_owned(),
        ] {
            let path = store.path(&relative)?;
            match fs::symlink_metadata(&path) {
                Ok(_) => store
                    .pinned_dirs
                    .lock()
                    .map_err(|_| conflict("private directory guard unavailable"))?
                    .push(
                        super::windows_security::open_pinned_directory(
                            &path,
                            if relative == ".wiki" || relative == ".wiki/state" {
                                super::acl_policy::Protection::IntegrityProtected
                            } else {
                                super::acl_policy::Protection::Private
                            },
                        )
                        .map_err(|error| io_error("pin existing job directory", error))?,
                    ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error("inspect existing job directory", error)),
            }
        }
        store.binding()?;
        Ok(store)
    }
    pub(crate) fn bootstrap(
        fs: &VaultFs,
        writer: &WriterPermit,
        vault_id: &RecordId,
        run_id: &RecordId,
    ) -> Result<Self> {
        writer.require_root(fs.root())?;
        let store = Self::new(fs, vault_id, run_id)?;
        for relative in [
            ".wiki".to_owned(),
            ".wiki/state".to_owned(),
            ".wiki/state/jobs".to_owned(),
            store.run_dir(),
            ".wiki/state/requests".to_owned(),
        ] {
            let path = store.path(&relative)?;
            match store.io.create_private_directory(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
                Err(e) => return Err(io_error("create private job directory", e)),
            }
            if relative.starts_with(".wiki/state/jobs") || relative == ".wiki/state/requests" {
                private_dir(&path)?;
            }
            #[cfg(windows)]
            store
                .pinned_dirs
                .lock()
                .map_err(|_| conflict("private directory guard unavailable"))?
                .push(
                    super::windows_security::open_pinned_directory(
                        &path,
                        if relative.starts_with(".wiki/state/jobs")
                            || relative == ".wiki/state/requests"
                        {
                            super::acl_policy::Protection::Private
                        } else {
                            super::acl_policy::Protection::IntegrityProtected
                        },
                    )
                    .map_err(|error| io_error("pin created private directory", error))?,
                );
            store.sync_dir(&path)?;
            store.sync_dir(
                path.parent()
                    .ok_or_else(|| conflict("private directory has no parent"))?,
            )?;
        }
        // Preserve existing bytes on bootstrap retry; never truncate or replace authority.
        for name in ["ledger.lock", "journal.bin"] {
            let path = store.path(&format!("{}/{}", store.run_dir(), name))?;
            match store.io.create_private_stage(&path) {
                Ok(file) => {
                    private_file(&file)?;
                    store
                        .io
                        .sync_file(&file)
                        .map_err(|e| io_error("sync new job authority", e))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&path)
                        .map_err(|e| io_error("open existing job authority", e))?;
                    same_inode(&file, &path)?;
                    private_file(&file)?;
                    store
                        .io
                        .sync_file(&file)
                        .map_err(|e| io_error("resync existing job authority", e))?;
                }
                Err(e) => return Err(io_error("create job authority", e)),
            }
            store.sync_dir(path.parent().expect("job parent"))?;
        }
        Ok(store)
    }
    pub(crate) fn open_existing(
        fs: &VaultFs,
        vault_id: &RecordId,
        run_id: &RecordId,
    ) -> Result<Self> {
        Self::new(fs, vault_id, run_id)
    }
    fn binding(&self) -> Result<()> {
        #[cfg(windows)]
        for directory in self
            .pinned_dirs
            .lock()
            .map_err(|_| conflict("private directory guard unavailable"))?
            .iter()
        {
            directory
                .verify_binding()
                .map_err(|error| io_error("verify private directory binding", error))?;
        }
        require_vault(&self.fs, &self.vault_id)
    }
    fn run_dir(&self) -> String {
        format!(".wiki/state/jobs/{}", self.run_id)
    }
    fn path(&self, relative: &str) -> Result<PathBuf> {
        self.fs.root().resolve(&VaultRelativePath::new(relative)?)
    }
    fn sync_dir(&self, path: &Path) -> Result<()> {
        match self
            .io
            .sync_directory(path)
            .map_err(|e| io_error("sync private job directory", e))?
        {
            DirectorySync::Supported => Ok(()),
            DirectorySync::Unsupported => Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "private job directory durability unavailable on this platform",
            )),
        }
    }
    pub(crate) fn lock(
        &self,
        timeout: Duration,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<RunLedgerGuard<'_>> {
        if timeout > Duration::from_secs(30) {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "job lock timeout exceeds 30 seconds",
            ));
        }
        self.binding()?;
        let path = self.path(&format!("{}/ledger.lock", self.run_dir()))?;
        #[cfg(windows)]
        let (file, pinned_dirs) = super::windows_security::open_checked_file(
            &path,
            super::acl_policy::Protection::Private,
            super::windows_security::Sharing::Lock,
            true,
        )
        .map_err(|error| io_error("open held private job lock", error))?
        .into_parts();
        #[cfg(not(windows))]
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| io_error("open fixed job lock", e))?;
        same_inode(&file, &path)?;
        let start = Instant::now();
        loop {
            check()?;
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    let remaining = timeout.saturating_sub(start.elapsed());
                    if remaining.is_zero() {
                        let mut error = conflict("job ledger lock contention timeout");
                        error.retryable = true;
                        return Err(error);
                    }
                    thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(io_error("acquire job lock", e));
                }
            }
        }
        let guard = RunLedgerGuard {
            store: self,
            file,
            lock_path: path,
            #[cfg(windows)]
            pinned_dirs,
        };
        guard.check()?;
        check()?;
        Ok(guard)
    }
}
impl Drop for RunLedgerGuard<'_> {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
impl RunLedgerGuard<'_> {
    fn check(&self) -> Result<()> {
        #[cfg(windows)]
        for directory in &self.pinned_dirs {
            directory
                .verify_binding()
                .map_err(|error| io_error("verify held job lock ancestors", error))?;
        }
        same_inode(&self.file, &self.lock_path)?;
        self.store.binding()
    }
    fn journal_path(&self) -> Result<PathBuf> {
        self.store
            .path(&format!("{}/journal.bin", self.store.run_dir()))
    }
    pub(crate) fn read_journal(&self) -> Result<Option<BeforeImage>> {
        self.check()?;
        read(&self.journal_path()?, JOURNAL_LIMIT)
    }
    pub(crate) fn read_checkpoint(&self, max_bytes: u64) -> Result<Option<BeforeImage>> {
        self.check()?;
        if max_bytes == 0 || max_bytes > 64 * 1024 {
            return Err(budget("job checkpoint read bound invalid"));
        }
        read(
            &self
                .store
                .path(&format!("{}/checkpoint.json", self.store.run_dir()))?,
            max_bytes,
        )
    }
    pub(crate) fn append_journal(&self, expected_length: u64, frame: &[u8]) -> Result<u64> {
        self.check()?;
        if frame.is_empty() || frame.len() > FRAME_LIMIT {
            return Err(budget("job frame exceeds ceiling"));
        }
        let length = expected_length
            .checked_add(frame.len() as u64)
            .ok_or_else(|| budget("job journal overflow"))?;
        if length > JOURNAL_LIMIT {
            return Err(budget("job journal exceeds ceiling"));
        }
        let path = self.journal_path()?;
        // A missing authority file is a conflict, never an append-created new history.
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|e| io_error("open existing journal", e))?;
        same_inode(&file, &path)?;
        if file
            .metadata()
            .map_err(|e| io_error("inspect journal length", e))?
            .len()
            != expected_length
        {
            return Err(conflict("journal length changed"));
        }
        let mut append = self
            .store
            .io
            .open_append(&path)
            .map_err(|e| io_error("open journal append", e))?;
        same_inode(&append, &path)?;
        self.store
            .io
            .write_stage(&mut append, frame)
            .map_err(|e| io_error("append job journal", e))?;
        self.store
            .io
            .sync_file(&append)
            .map_err(|e| io_error("sync job journal", e))?;
        self.store
            .sync_dir(path.parent().expect("journal parent"))?;
        Ok(length)
    }
    pub(crate) fn truncate_torn_tail(
        &self,
        expected_length: u64,
        verified_offset: u64,
    ) -> Result<()> {
        self.check()?;
        if verified_offset > expected_length {
            return Err(conflict("journal truncation cannot extend history"));
        }
        let path = self.journal_path()?;
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|e| io_error("open journal truncation", e))?;
        same_inode(&file, &path)?;
        if file
            .metadata()
            .map_err(|e| io_error("inspect journal truncation", e))?
            .len()
            != expected_length
        {
            return Err(conflict("journal length changed before truncation"));
        }
        self.store
            .io
            .truncate_file(&file, verified_offset)
            .map_err(|e| io_error("truncate torn job tail", e))?;
        self.store
            .io
            .sync_file(&file)
            .map_err(|e| io_error("sync truncated job journal", e))?;
        self.store.sync_dir(path.parent().expect("journal parent"))
    }
    fn attempt_dir(&self, attempt: &AttemptRef) -> Result<PathBuf> {
        if attempt.run_id != self.store.run_id {
            return Err(conflict("spool attempt belongs to another run"));
        }
        self.store
            .path(&format!(".wiki/state/requests/{}", attempt.attempt_id))
    }
    fn owner_bytes(&self, attempt: &AttemptRef) -> Result<Vec<u8>> {
        serde_json::to_vec(&Owner {
            version: 1,
            vault_id: self.store.vault_id.clone(),
            attempt: attempt.clone(),
        })
        .map_err(|_| WikiError::invalid("invalid spool owner"))
    }
    fn owner(&self, attempt: &AttemptRef) -> Result<()> {
        let bytes = read(&self.attempt_dir(attempt)?.join("owner.json"), OWNER_LIMIT)?
            .ok_or_else(|| conflict("spool owner missing"))?;
        if bytes.bytes != self.owner_bytes(attempt)? {
            return Err(conflict("spool owner binding changed"));
        }
        Ok(())
    }
    pub(crate) fn ensure_attempt_dir(&self, attempt: &AttemptRef) -> Result<()> {
        self.check()?;
        let path = self.attempt_dir(attempt)?;
        match self.store.io.create_private_directory(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
            Err(e) => return Err(io_error("create attempt spool", e)),
        }
        private_dir(&path)?;
        self.store.sync_dir(&path)?;
        self.store.sync_dir(path.parent().expect("spool parent"))?;
        let owner = path.join("owner.json");
        let bytes = self.owner_bytes(attempt)?;
        match read(&owner, OWNER_LIMIT)? {
            Some(image) if image.bytes == bytes => {
                let file = File::open(&owner).map_err(|e| io_error("open spool owner", e))?;
                self.store
                    .io
                    .sync_file(&file)
                    .map_err(|e| io_error("resync spool owner", e))?;
                self.store.sync_dir(&path)?;
            }
            Some(_) => return Err(conflict("attempt spool owned by another request")),
            None => {
                if fs::read_dir(&path)
                    .map_err(|e| io_error("inspect unowned spool", e))?
                    .next()
                    .is_some()
                {
                    return Err(conflict("unbound nonempty spool retained"));
                }
                self.replace_path(&owner, &ExpectedState::Absent, &bytes, OWNER_LIMIT)?;
            }
        }
        self.owner(attempt)
    }
    fn spool_path(&self, attempt: &AttemptRef, part: SpoolPart) -> Result<PathBuf> {
        Ok(self.attempt_dir(attempt)?.join(match part {
            SpoolPart::Body => "response.bin",
            SpoolPart::Metadata => "metadata.json",
        }))
    }
    fn limit(part: SpoolPart) -> u64 {
        match part {
            SpoolPart::Body => 8 * 1024 * 1024,
            SpoolPart::Metadata => 64 * 1024,
        }
    }
    pub(crate) fn read_spool(
        &self,
        attempt: &AttemptRef,
        part: SpoolPart,
        max_bytes: u64,
    ) -> Result<Option<BeforeImage>> {
        self.check()?;
        if max_bytes == 0 || max_bytes > Self::limit(part) {
            return Err(budget("spool read bound invalid"));
        }
        let directory = self.attempt_dir(attempt)?;
        if !directory.exists() {
            return Ok(None);
        }
        #[cfg(windows)]
        let _directory = super::windows_security::open_pinned_directory(
            &directory,
            super::acl_policy::Protection::Private,
        )
        .map_err(|error| io_error("pin spool read directory", error))?;
        if read(&directory.join("owner.json"), OWNER_LIMIT)?.is_none() {
            if fs::read_dir(&directory)
                .map_err(|e| io_error("inspect cleaned spool", e))?
                .next()
                .is_none()
            {
                return Ok(None);
            }
            return Err(conflict("unbound nonempty spool retained"));
        }
        self.owner(attempt)?;
        read(&self.spool_path(attempt, part)?, max_bytes)
    }
    fn guarded(path: &Path, expected: &ExpectedState, limit: u64) -> Result<()> {
        let actual =
            read(path, limit)?.map_or(ExpectedState::Absent, |b| ExpectedState::Hash(b.hash));
        if &actual != expected {
            return Err(conflict("private payload hash or absence changed"));
        }
        Ok(())
    }
    fn replace_path(
        &self,
        path: &Path,
        expected: &ExpectedState,
        bytes: &[u8],
        limit: u64,
    ) -> Result<Blake3Hash> {
        if bytes.len() as u64 > limit {
            return Err(budget("private payload exceeds write ceiling"));
        }
        Self::guarded(path, expected, limit)?;
        let parent = path.parent().expect("private payload parent");
        #[cfg(windows)]
        let private_parent = super::windows_security::open_pinned_directory(
            parent,
            super::acl_policy::Protection::Private,
        )
        .map_err(|error| io_error("pin private replacement parent", error))?;

        let stage = parent.join(format!(".lwiki-private-{}.tmp", uuid::Uuid::now_v7()));
        let result = (|| {
            #[cfg(windows)]
            private_parent
                .verify_binding()
                .map_err(|error| io_error("verify private replacement parent", error))?;
            let mut file = self
                .store
                .io
                .create_private_stage(&stage)
                .map_err(|e| io_error("create private stage", e))?;
            private_file(&file)?;
            self.store
                .io
                .write_stage(&mut file, bytes)
                .map_err(|e| io_error("write private stage", e))?;
            self.store
                .io
                .sync_file(&file)
                .map_err(|e| io_error("sync private stage", e))?;
            #[cfg(not(windows))]
            let staged = read(&stage, limit)?.ok_or_else(|| conflict("private stage missing"))?;
            #[cfg(windows)]
            let staged = {
                same_inode(&file, &stage)?;
                file.rewind()
                    .map_err(|error| io_error("rewind held private stage", error))?;
                let mut retained = Vec::new();
                (&mut file)
                    .take(limit.saturating_add(1))
                    .read_to_end(&mut retained)
                    .map_err(|error| io_error("read held private stage", error))?;
                if retained.len() as u64 > limit {
                    return Err(budget("private stage exceeds read ceiling"));
                }
                same_inode(&file, &stage)?;
                if file
                    .metadata()
                    .map_err(|error| io_error("inspect held private stage", error))?
                    .len()
                    != retained.len() as u64
                {
                    return Err(conflict("private stage size changed while reading"));
                }
                BeforeImage {
                    hash: Blake3Hash::digest(&retained),
                    bytes: retained,
                }
            };
            let hash = Blake3Hash::digest(bytes);
            if staged.hash != hash {
                return Err(conflict("private stage changed"));
            }
            Self::guarded(path, expected, limit)?;
            self.store
                .io
                .replace(&stage, path)
                .map_err(|e| io_error("publish private stage", e))?;
            self.store.sync_dir(parent)?;
            Ok(hash)
        })();
        let _ = self.store.io.remove(&stage);
        result
    }
    pub(crate) fn secure_replace(
        &self,
        file: RunFile<'_>,
        expected: ExpectedState,
        bytes: &[u8],
    ) -> Result<Blake3Hash> {
        self.check()?;
        match file {
            RunFile::Spool { attempt, part } => {
                self.owner(attempt)?;
                self.replace_path(
                    &self.spool_path(attempt, part)?,
                    &expected,
                    bytes,
                    Self::limit(part),
                )
            }
            RunFile::Checkpoint => self.replace_path(
                &self
                    .store
                    .path(&format!("{}/checkpoint.json", self.store.run_dir()))?,
                &expected,
                bytes,
                FRAME_LIMIT as u64,
            ),
        }
    }
    pub(crate) fn remove_spool_file(
        &self,
        attempt: &AttemptRef,
        part: SpoolPart,
        expected: &Blake3Hash,
    ) -> Result<()> {
        self.check()?;
        if self.read_spool(attempt, part, Self::limit(part))?.is_none() {
            let directory = self.attempt_dir(attempt)?;
            if directory.exists() {
                self.store.sync_dir(&directory)?;
            } else {
                self.store
                    .sync_dir(directory.parent().expect("spool parent"))?;
            }
            return Ok(());
        }
        self.owner(attempt)?;
        let path = self.spool_path(attempt, part)?;
        Self::guarded(
            &path,
            &ExpectedState::Hash(expected.clone()),
            Self::limit(part),
        )?;
        self.store
            .io
            .remove(&path)
            .map_err(|e| io_error("remove verified spool", e))?;
        self.store.sync_dir(path.parent().expect("spool parent"))
    }
    pub(crate) fn remove_empty_attempt_dir(&self, attempt: &AttemptRef) -> Result<()> {
        self.check()?;
        let path = self.attempt_dir(attempt)?;
        if !path.exists() {
            return Ok(());
        }
        #[cfg(windows)]
        let _parent = super::windows_security::open_pinned_directory(
            path.parent().expect("spool parent"),
            super::acl_policy::Protection::Private,
        )
        .map_err(|error| io_error("pin spool cleanup parent", error))?;
        #[cfg(windows)]
        let directory = super::windows_security::open_pinned_directory(
            &path,
            super::acl_policy::Protection::Private,
        )
        .map_err(|error| io_error("pin spool cleanup directory", error))?;
        let owner_path = path.join("owner.json");
        let owner_present = read(&owner_path, OWNER_LIMIT)?.is_some();
        if owner_present {
            self.owner(attempt)?;
        }
        for part in [SpoolPart::Body, SpoolPart::Metadata] {
            if read(&self.spool_path(attempt, part)?, Self::limit(part))?.is_some() {
                return Err(conflict("cannot remove nonempty response spool"));
            }
        }
        // No unfamiliar staging/other payload may be discarded during cleanup.
        for entry in fs::read_dir(&path).map_err(|e| io_error("inspect empty spool", e))? {
            let entry = entry.map_err(|e| io_error("inspect spool entry", e))?;
            if entry.file_name() != "owner.json" {
                return Err(conflict("unfamiliar spool entry retained"));
            }
        }
        if owner_present {
            self.store
                .io
                .remove(&owner_path)
                .map_err(|e| io_error("remove verified spool owner", e))?;
        }
        self.store.sync_dir(&path)?;
        #[cfg(windows)]
        drop(directory); // Release this directory's no-delete handle before removing it.
        self.store
            .io
            .remove_directory(&path)
            .map_err(|e| io_error("remove empty spool directory", e))?;
        self.store.sync_dir(path.parent().expect("spool parent"))
    }
    pub(crate) fn enumerate_spool_ids(
        &self,
        max_entries: usize,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<AttemptRef>> {
        self.check()?;
        if max_entries == 0 || max_entries > 65536 {
            return Err(budget("spool enumeration ceiling invalid"));
        }
        let path = self.store.path(".wiki/state/requests")?;
        #[cfg(windows)]
        let _requests = super::windows_security::open_pinned_directory(
            &path,
            super::acl_policy::Protection::Private,
        )
        .map_err(|error| io_error("pin request enumeration directory", error))?;
        let mut out = Vec::new();
        for (index, entry) in fs::read_dir(&path)
            .map_err(|e| io_error("enumerate request spools", e))?
            .enumerate()
        {
            check()?;
            if index >= max_entries {
                return Err(budget("spool enumeration ceiling exceeded"));
            }
            let entry = entry.map_err(|e| io_error("inspect request spool", e))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| conflict("non UTF-8 spool directory"))?;
            let id = RecordId::new(name)?;
            let relative = format!(".wiki/state/requests/{id}/owner.json");
            let owner_path = self.store.path(&relative)?;
            #[cfg(windows)]
            let _attempt = super::windows_security::open_pinned_directory(
                owner_path.parent().expect("owner parent"),
                super::acl_policy::Protection::Private,
            )
            .map_err(|error| io_error("pin enumerated private attempt", error))?;
            let Some(owner) = read(&owner_path, OWNER_LIMIT)? else {
                // A crash after verified cleanup removes owner before its empty dir.
                // It carries no response/dispatch authority and is safe to report absent.
                if fs::read_dir(owner_path.parent().expect("owner parent"))
                    .map_err(|e| io_error("inspect unbound spool", e))?
                    .next()
                    .is_none()
                {
                    continue;
                }
                return Err(conflict("unbound response spool retained"));
            };
            let owner: Owner = serde_json::from_slice(&owner.bytes)
                .map_err(|_| conflict("invalid response spool owner"))?;
            if owner.version != 1
                || owner.vault_id != self.store.vault_id
                || owner.attempt.attempt_id != id
            {
                return Err(conflict("response spool owner identity differs"));
            }
            if owner.attempt.run_id == self.store.run_id {
                out.push(owner.attempt);
            }
        }
        out.sort_by(|a, b| a.attempt_id.cmp(&b.attempt_id));
        Ok(out)
    }
}

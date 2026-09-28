//! Guarded per-file durability primitives. P03 owns retained payloads and journal ordering.
use super::{
    lock::WriterPermit,
    paths::{VaultRoot, io_error},
};
use crate::domain::{Blake3Hash, ErrorCode, Result, VaultRelativePath, WikiError};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "state",
    content = "hash",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ExpectedState {
    Absent,
    Hash(Blake3Hash),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeforeImage {
    pub bytes: Vec<u8>,
    pub hash: Blake3Hash,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectorySync {
    Supported,
    Unsupported,
}

/// A production adapter, also suitable for injecting failures around actual I/O calls.
/// Methods must perform exactly the requested operation, without hidden retries.
pub trait DurableIo: Send + Sync {
    fn create_stage(&self, path: &Path) -> std::io::Result<File>;
    fn open_append(&self, path: &Path) -> std::io::Result<File>;
    fn truncate_file(&self, file: &File, length: u64) -> std::io::Result<()>;
    fn write_stage(&self, file: &mut File, bytes: &[u8]) -> std::io::Result<()>;
    fn sync_file(&self, file: &File) -> std::io::Result<()>;
    fn replace(&self, staged: &Path, target: &Path) -> std::io::Result<()>;
    fn remove(&self, target: &Path) -> std::io::Result<()>;
    fn create_directory(&self, path: &Path) -> std::io::Result<()>;
    fn sync_directory(&self, directory: &Path) -> std::io::Result<DirectorySync>;
}
#[derive(Debug, Default)]
pub struct NativeIo;
impl DurableIo for NativeIo {
    fn create_stage(&self, path: &Path) -> std::io::Result<File> {
        OpenOptions::new().write(true).create_new(true).open(path)
    }
    fn write_stage(&self, file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
        file.write_all(bytes)
    }
    fn open_append(&self, path: &Path) -> std::io::Result<File> {
        OpenOptions::new().create(true).append(true).open(path)
    }
    fn truncate_file(&self, file: &File, length: u64) -> std::io::Result<()> {
        file.set_len(length)
    }
    fn sync_file(&self, file: &File) -> std::io::Result<()> {
        file.sync_all()
    }
    fn replace(&self, staged: &Path, target: &Path) -> std::io::Result<()> {
        fs::rename(staged, target)
    }
    fn remove(&self, target: &Path) -> std::io::Result<()> {
        fs::remove_file(target)
    }
    fn create_directory(&self, path: &Path) -> std::io::Result<()> {
        fs::create_dir(path)
    }
    fn sync_directory(&self, directory: &Path) -> std::io::Result<DirectorySync> {
        #[cfg(unix)]
        {
            File::open(directory)?.sync_all()?;
            Ok(DirectorySync::Supported)
        }
        #[cfg(not(unix))]
        {
            let _ = directory;
            Ok(DirectorySync::Unsupported)
        }
    }
}

pub struct VaultFs {
    root: VaultRoot,
    io: Arc<dyn DurableIo>,
}
pub struct StagedFile {
    path: PathBuf,
    target: VaultRelativePath,
    root: VaultRoot,
    io: Arc<dyn DurableIo>,
    proposed_hash: Blake3Hash,
}
impl StagedFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn proposed_hash(&self) -> &Blake3Hash {
        &self.proposed_hash
    }
}
impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = self.io.remove(&self.path);
    }
}
impl VaultFs {
    pub fn new(root: VaultRoot) -> Self {
        Self::with_io(root, Arc::new(NativeIo))
    }
    pub fn with_io(root: VaultRoot, io: Arc<dyn DurableIo>) -> Self {
        Self { root, io }
    }
    pub fn root(&self) -> &VaultRoot {
        &self.root
    }
    fn require_operational(target: &VaultRelativePath) -> Result<()> {
        if !target.as_str().starts_with(".wiki/state/") {
            return Err(WikiError::invalid(
                "operational append/truncate requires .wiki/state path",
            ));
        }
        Ok(())
    }
    /// Operational append primitive, not canonical record publication.
    pub fn append_synced(
        &self,
        target: &VaultRelativePath,
        bytes: &[u8],
        permit: &WriterPermit,
    ) -> Result<DirectorySync> {
        permit.require_root(&self.root)?;
        Self::require_operational(target)?;
        self.root
            .validate_portable_paths(std::slice::from_ref(target))?;
        let path = self.root.resolve(target)?;
        let mut file = self
            .io
            .open_append(&path)
            .map_err(|e| io_error("open operational append", e))?;
        self.io
            .write_stage(&mut file, bytes)
            .map_err(|e| io_error("append operational bytes", e))?;
        self.io
            .sync_file(&file)
            .map_err(|e| io_error("sync operational append", e))?;
        self.io
            .sync_directory(path.parent().expect("managed file has parent"))
            .map_err(|e| io_error("sync append directory", e))
    }
    pub fn truncate_synced(
        &self,
        target: &VaultRelativePath,
        length: u64,
        permit: &WriterPermit,
    ) -> Result<()> {
        permit.require_root(&self.root)?;
        Self::require_operational(target)?;
        let path = self.root.resolve(target)?;
        // Truncation never creates a missing operational file.
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| io_error("open operational truncation", e))?;
        if length
            > file
                .metadata()
                .map_err(|e| io_error("inspect operational length", e))?
                .len()
        {
            return Err(WikiError::invalid(
                "operational truncation cannot extend a file",
            ));
        }
        self.io
            .truncate_file(&file, length)
            .map_err(|e| io_error("truncate operational tail", e))?;
        self.io
            .sync_file(&file)
            .map_err(|e| io_error("sync operational truncation", e))
    }
    /// Recovery must sync observed proposed bytes/absence before journaling completion.
    pub fn sync_target(
        &self,
        target: &VaultRelativePath,
        permit: &WriterPermit,
    ) -> Result<DirectorySync> {
        permit.require_root(&self.root)?;
        let path = self.root.resolve(target)?;
        match File::open(&path) {
            Ok(file) => self
                .io
                .sync_file(&file)
                .map_err(|e| io_error("sync observed target", e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error("open observed target", e)),
        }
        self.io
            .sync_directory(path.parent().expect("managed file has parent"))
            .map_err(|e| io_error("sync observed target directory", e))
    }
    pub fn read_before(&self, target: &VaultRelativePath) -> Result<Option<BeforeImage>> {
        let path = self.root.resolve(target)?;
        match fs::read(path) {
            Ok(bytes) => Ok(Some(BeforeImage {
                hash: Blake3Hash::digest(&bytes),
                bytes,
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error("read before-image", e)),
        }
    }
    pub fn ensure_directory(
        &self,
        directory: &VaultRelativePath,
        permit: &WriterPermit,
    ) -> Result<DirectorySync> {
        permit.require_root(&self.root)?;
        self.root
            .validate_portable_paths(std::slice::from_ref(directory))?;
        let mut relative = String::new();
        let mut support = DirectorySync::Supported;
        for part in directory.as_str().split('/') {
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(part);
            let path = self.root.resolve(&VaultRelativePath::new(&relative)?)?;
            match self.io.create_directory(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
                Err(e) => return Err(io_error("create managed directory", e)),
            }
            // Existing directories may be the result of an interrupted mkdir/sync.
            // Reestablish every traversed parent-entry durability before success.
            for dir in [
                path.as_path(),
                path.parent().expect("managed directory has parent"),
            ] {
                let result = self
                    .io
                    .sync_directory(dir)
                    .map_err(|e| io_error("sync managed directory", e))?;
                if result == DirectorySync::Unsupported {
                    support = result;
                }
            }
        }
        Ok(support)
    }
    pub fn stage(
        &self,
        target: &VaultRelativePath,
        bytes: &[u8],
        permit: &WriterPermit,
    ) -> Result<StagedFile> {
        permit.require_root(&self.root)?;
        self.root
            .validate_portable_paths(std::slice::from_ref(target))?;
        let destination = self.root.resolve(target)?;
        let parent = destination.parent().expect("managed file has parent");
        let path = parent.join(format!(".lwiki-stage-{}.tmp", uuid::Uuid::now_v7()));
        let mut file = match self.io.create_stage(&path) {
            Ok(file) => file,
            Err(e) => {
                // Adapter failures after create can leave an empty private stage.
                if e.kind() != std::io::ErrorKind::AlreadyExists {
                    let _ = self.io.remove(&path);
                }
                return Err(io_error("create same-directory stage", e));
            }
        };
        let staged = StagedFile {
            path,
            target: target.clone(),
            root: self.root.clone(),
            io: Arc::clone(&self.io),
            proposed_hash: Blake3Hash::digest(bytes),
        };
        self.io
            .write_stage(&mut file, bytes)
            .map_err(|e| io_error("write stage", e))?;
        self.io
            .sync_file(&file)
            .map_err(|e| io_error("sync stage", e))?;
        Ok(staged)
    }
    fn guard(&self, target: &VaultRelativePath, expected: &ExpectedState) -> Result<()> {
        let actual = self
            .read_before(target)?
            .map_or(ExpectedState::Absent, |before| {
                ExpectedState::Hash(before.hash)
            });
        if &actual != expected {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "destination no longer matches expected hash or absence",
            ));
        }
        Ok(())
    }
    /// Recheck immediately before rename. External editors can still race this check.
    pub fn replace(
        &self,
        staged: StagedFile,
        expected: &ExpectedState,
        permit: &WriterPermit,
    ) -> Result<DirectorySync> {
        permit.require_root(&self.root)?;
        if staged.root != self.root {
            return Err(WikiError::invalid("stage belongs to another vault"));
        }
        self.root
            .validate_portable_paths(std::slice::from_ref(&staged.target))?;
        let destination = self.root.resolve(&staged.target)?;
        // Stage is public by path for journal intent; detect accidental tampering.
        let metadata =
            fs::symlink_metadata(&staged.path).map_err(|e| io_error("inspect stage", e))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "stage must remain a regular file",
            ));
        }
        if Blake3Hash::digest(fs::read(&staged.path).map_err(|e| io_error("verify stage", e))?)
            != staged.proposed_hash
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "stage payload changed",
            ));
        }
        self.guard(&staged.target, expected)?;
        self.io
            .replace(&staged.path, &destination)
            .map_err(|e| io_error("replace staged file", e))?;
        self.io
            .sync_directory(destination.parent().expect("managed file has parent"))
            .map_err(|e| io_error("sync replacement directory", e))
    }
    pub fn delete(
        &self,
        target: &VaultRelativePath,
        expected: &ExpectedState,
        permit: &WriterPermit,
    ) -> Result<DirectorySync> {
        permit.require_root(&self.root)?;
        self.root
            .validate_portable_paths(std::slice::from_ref(target))?;
        let destination = self.root.resolve(target)?;
        self.guard(target, expected)?;
        if expected == &ExpectedState::Absent {
            return Ok(DirectorySync::Supported);
        }
        self.io
            .remove(&destination)
            .map_err(|e| io_error("delete guarded file", e))?;
        self.io
            .sync_directory(destination.parent().expect("managed file has parent"))
            .map_err(|e| io_error("sync deletion directory", e))
    }
}

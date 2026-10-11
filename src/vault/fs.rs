//! Guarded per-file durability primitives. P03 owns retained payloads and journal ordering.
use super::{
    lock::WriterPermit,
    paths::{VaultRoot, io_error},
};
use crate::domain::{Blake3Hash, ErrorCode, Result, VaultRelativePath, WikiError};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use super::paths::descriptor;
#[cfg(unix)]
type BoundMetadata = descriptor::Metadata;
#[cfg(not(unix))]
type BoundMetadata = fs::Metadata;
fn bound_metadata(value: fs::Metadata) -> BoundMetadata {
    #[cfg(unix)]
    {
        value.into()
    }
    #[cfg(not(unix))]
    {
        value
    }
}

/// A bounded regular-file observation with fresh namespace checks on both sides.
/// Unix nonblocking opens reject raced special files without stranding joined jobs.
pub(crate) fn read_regular_bounded(
    root: &VaultRoot,
    relative: &VaultRelativePath,
    raw: bool,
    limit: usize,
    layer: &'static str,
) -> Result<Option<Vec<u8>>> {
    read_regular_bounded_policy(root, relative, raw, limit, layer, false, None)
}
fn read_regular_bounded_policy(
    root: &VaultRoot,
    relative: &VaultRelativePath,
    raw: bool,
    limit: usize,
    layer: &'static str,
    descriptor_relative: bool,
    scoped_fs: Option<&VaultFs>,
) -> Result<Option<Vec<u8>>> {
    observe_regular_bounded(
        root,
        relative,
        raw,
        limit,
        layer,
        descriptor_relative,
        scoped_fs,
        |length| {
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(length).map_err(|_| {
                WikiError::new(ErrorCode::BudgetExceeded, "bounded input allocation failed")
            })?;
            if bytes.capacity() > limit {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "bounded input capacity exceeds allowance",
                ));
            }
            Ok(bytes)
        },
        |bytes, chunk| {
            bytes.try_reserve_exact(chunk.len()).map_err(|_| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "bounded input growth allocation failed",
                )
            })?;
            if bytes.capacity() > limit {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "bounded input capacity exceeds allowance",
                ));
            }
            bytes.extend_from_slice(chunk);
            Ok(())
        },
    )
    .map(|value| value.map(|(bytes, _)| bytes))
}

/// Stream an authority observation without allocating its full byte contents.
/// The 64KiB read buffer and at most 8KiB hash state fit the activation reservation.
pub(crate) fn hash_regular_raw_bounded(
    root: &VaultRoot,
    relative: &VaultRelativePath,
    limit: usize,
    layer: &'static str,
) -> Result<Option<(u64, [u8; 32])>> {
    hash_regular_raw_bounded_policy(root, relative, limit, layer, false, None)
}
fn hash_regular_raw_bounded_policy(
    root: &VaultRoot,
    relative: &VaultRelativePath,
    limit: usize,
    layer: &'static str,
    descriptor_relative: bool,
    scoped_fs: Option<&VaultFs>,
) -> Result<Option<(u64, [u8; 32])>> {
    const {
        assert!(std::mem::size_of::<blake3::Hasher>() <= 8 * 1024);
    }
    observe_regular_bounded(
        root,
        relative,
        true,
        limit,
        layer,
        descriptor_relative,
        scoped_fs,
        |_| Ok(blake3::Hasher::new()),
        |hasher, chunk| {
            hasher.update(chunk);
            Ok(())
        },
    )
    .map(|value| value.map(|(hasher, bytes)| (bytes, *hasher.finalize().as_bytes())))
}

fn observe_regular_bounded<T>(
    root: &VaultRoot,
    relative: &VaultRelativePath,
    raw: bool,
    limit: usize,
    layer: &'static str,
    descriptor_relative: bool,
    scoped_fs: Option<&VaultFs>,
    initialize: impl FnOnce(usize) -> Result<T>,
    mut consume: impl FnMut(&mut T, &[u8]) -> Result<()>,
) -> Result<Option<(T, u64)>> {
    crate::maintenance_diagnostic::observe(
        crate::maintenance_diagnostic::Phase::FileBinding,
        || {
            let owner_activation = crate::storage::maintenance_activation::owner_capture_active();
            if owner_activation {
                crate::maintenance_parallel::record_owner_activation_read();
            }
            #[cfg(unix)]
            let resolve = || {
                if descriptor_relative {
                    match scoped_fs {
                        Some(fs) => fs.bind(relative, raw),
                        None => root.bind(relative, raw),
                    }
                } else {
                    (if raw {
                        root.resolve_raw(relative)
                    } else {
                        root.resolve(relative)
                    })
                    .map(descriptor::Route::legacy)
                }
            };
            #[cfg(not(unix))]
            let resolve = || {
                if raw {
                    root.resolve_raw(relative)
                } else {
                    root.resolve(relative)
                }
            };
            #[cfg(unix)]
            let route = resolve()?;
            #[cfg(all(unix, test))]
            let path = &route.path;
            #[cfg(not(unix))]
            let path = resolve()?;
            let before = match crate::maintenance_diagnostic::observe(
                crate::maintenance_diagnostic::Phase::MetadataObservation,
                || {
                    crate::maintenance_diagnostic::bump(
                        crate::maintenance_diagnostic::Counter::MetadataCall,
                        1,
                    );
                    #[cfg(unix)]
                    {
                        route.metadata()
                    }
                    #[cfg(not(unix))]
                    {
                        fs::symlink_metadata(&path)
                    }
                },
            ) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    crate::maintenance_diagnostic::bump(
                        crate::maintenance_diagnostic::Counter::ExpectedMissing,
                        1,
                    );
                    return Ok(None);
                }
                Err(e) => return Err(io_error("inspect bounded input", e)),
            };
            if !before.is_file() || before.file_type().is_symlink() {
                return Err(WikiError::invalid("bounded read requires regular file"));
            }
            if before.len() > limit as u64 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "bounded input exceeds byte allowance",
                ));
            }
            #[cfg(test)]
            super::maintenance_read_tests::before_open(&path);
            #[cfg(unix)]
            let mut file = {
                crate::maintenance_diagnostic::observe(
                    crate::maintenance_diagnostic::Phase::FileOpen,
                    || {
                        crate::maintenance_diagnostic::bump(
                            crate::maintenance_diagnostic::Counter::FileOpenCall,
                            1,
                        );
                        route.open_read()
                    },
                )
            }
            .map_err(|e| io_error("open bounded input", e))?;
            #[cfg(not(unix))]
            let mut file = crate::maintenance_diagnostic::observe(
                crate::maintenance_diagnostic::Phase::FileOpen,
                || {
                    crate::maintenance_diagnostic::bump(
                        crate::maintenance_diagnostic::Counter::FileOpenCall,
                        1,
                    );
                    File::open(&path)
                },
            )
            .map_err(|e| io_error("open bounded input", e))?;
            let same = |a: &BoundMetadata, b: &BoundMetadata| -> Result<()> {
                if !b.is_file() || b.file_type().is_symlink() {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "bounded input type changed",
                    ));
                }
                #[cfg(unix)]
                {
                    if a.dev() != b.dev() || a.ino() != b.ino() {
                        return Err(WikiError::new(
                            ErrorCode::ContentConflict,
                            "bounded input identity changed",
                        ));
                    }
                }
                #[cfg(not(unix))]
                let _ = a;
                Ok(())
            };
            let opened = crate::maintenance_diagnostic::observe(
                crate::maintenance_diagnostic::Phase::MetadataObservation,
                || {
                    crate::maintenance_diagnostic::bump(
                        crate::maintenance_diagnostic::Counter::MetadataCall,
                        1,
                    );
                    file.metadata().map(bound_metadata)
                },
            )
            .map_err(|e| io_error("inspect opened input", e))?;
            crate::maintenance_diagnostic::bump(
                crate::maintenance_diagnostic::Counter::FileBindingCheck,
                1,
            );
            same(&before, &opened)?;
            if opened.len() > limit as u64 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "opened input exceeds byte allowance",
                ));
            }
            let mut state = initialize(opened.len() as usize)?;
            let mut observed = 0usize;
            let result = (|| {
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let remaining = limit.saturating_sub(observed);
                    if remaining == 0 {
                        let mut sentinel = [0u8; 1];
                        let n = crate::maintenance_diagnostic::read(|| file.read(&mut sentinel))
                            .map_err(|e| io_error("read input boundary", e))?;
                        observed += n;
                        if n != 0 {
                            return Err(WikiError::new(
                                ErrorCode::BudgetExceeded,
                                "bounded input grew beyond allowance",
                            ));
                        }
                        break;
                    }
                    let cap = remaining.min(buffer.len());
                    let n = crate::maintenance_diagnostic::read(|| file.read(&mut buffer[..cap]))
                        .map_err(|e| io_error("read bounded input", e))?;
                    observed += n;
                    if n == 0 {
                        break;
                    }
                    consume(&mut state, &buffer[..n])?;
                }
                let after = crate::maintenance_diagnostic::observe(
                    crate::maintenance_diagnostic::Phase::MetadataObservation,
                    || {
                        crate::maintenance_diagnostic::bump(
                            crate::maintenance_diagnostic::Counter::MetadataCall,
                            1,
                        );
                        file.metadata().map(bound_metadata)
                    },
                )
                .map_err(|e| io_error("recheck opened input", e))?;
                crate::maintenance_diagnostic::bump(
                    crate::maintenance_diagnostic::Counter::FileBindingCheck,
                    1,
                );
                same(&opened, &after)?;
                #[cfg(unix)]
                let fresh = resolve()?;
                #[cfg(unix)]
                route.same_parent(&fresh)?;
                #[cfg(not(unix))]
                if resolve()? != path {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "bounded input route changed",
                    ));
                }
                let named = crate::maintenance_diagnostic::observe(
                    crate::maintenance_diagnostic::Phase::MetadataObservation,
                    || {
                        crate::maintenance_diagnostic::bump(
                            crate::maintenance_diagnostic::Counter::MetadataCall,
                            1,
                        );
                        #[cfg(unix)]
                        {
                            fresh.metadata()
                        }
                        #[cfg(not(unix))]
                        {
                            fs::symlink_metadata(&path)
                        }
                    },
                )
                .map_err(|e| io_error("recheck named input", e))?;
                crate::maintenance_diagnostic::bump(
                    crate::maintenance_diagnostic::Counter::FileBindingCheck,
                    1,
                );
                same(&opened, &named)?;
                if opened.len() != observed as u64 || after.len() != observed as u64 {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "bounded input length changed",
                    ));
                }
                Ok(())
            })();
            if owner_activation {
                crate::maintenance_parallel::record_owner_activation_bytes(observed as u64);
            }
            #[cfg(test)]
            crate::catalog::query_diagnostics::read(layer, &path, observed);
            #[cfg(not(test))]
            let _ = (layer, observed);
            result?;
            Ok(Some((state, observed as u64)))
        },
    )
}

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
    /// Opt in only when every relative operation is implemented. Legacy/fault
    /// adapters retain their original callbacks and path semantics.
    #[cfg(unix)]
    fn descriptor_relative(&self) -> bool {
        false
    }
    #[cfg(unix)]
    fn create_stage_at(&self, _parent: &File, _leaf: &std::ffi::OsStr) -> std::io::Result<File> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    #[cfg(unix)]
    fn create_private_stage_at(
        &self,
        _parent: &File,
        _leaf: &std::ffi::OsStr,
    ) -> std::io::Result<File> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    #[cfg(unix)]
    fn create_private_directory_at(
        &self,
        _parent: &File,
        _leaf: &std::ffi::OsStr,
    ) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    #[cfg(unix)]
    fn open_append_at(&self, _parent: &File, _leaf: &std::ffi::OsStr) -> std::io::Result<File> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    #[cfg(unix)]
    fn replace_at(
        &self,
        _from_parent: &File,
        _from: &std::ffi::OsStr,
        _to_parent: &File,
        _to: &std::ffi::OsStr,
    ) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    #[cfg(unix)]
    fn remove_at(&self, _parent: &File, _leaf: &std::ffi::OsStr) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    #[cfg(unix)]
    fn create_directory_at(&self, _parent: &File, _leaf: &std::ffi::OsStr) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    #[cfg(unix)]
    fn sync_directory_file(&self, _directory: &File) -> std::io::Result<DirectorySync> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    fn create_stage(&self, path: &Path) -> std::io::Result<File>;
    /// Adapters must explicitly implement protected creation before accepting private payloads.
    fn create_private_stage(&self, _path: &Path) -> std::io::Result<File> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    fn create_private_directory(&self, _path: &Path) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    fn open_append(&self, path: &Path) -> std::io::Result<File>;
    fn truncate_file(&self, file: &File, length: u64) -> std::io::Result<()>;
    fn write_stage(&self, file: &mut File, bytes: &[u8]) -> std::io::Result<()>;
    fn sync_file(&self, file: &File) -> std::io::Result<()>;
    fn replace(&self, staged: &Path, target: &Path) -> std::io::Result<()>;
    fn remove(&self, target: &Path) -> std::io::Result<()>;
    fn remove_directory(&self, target: &Path) -> std::io::Result<()> {
        fs::remove_dir(target)
    }
    fn create_directory(&self, path: &Path) -> std::io::Result<()>;
    fn sync_directory(&self, directory: &Path) -> std::io::Result<DirectorySync>;
}
#[derive(Debug, Default)]
pub struct NativeIo;
impl DurableIo for NativeIo {
    #[cfg(unix)]
    fn descriptor_relative(&self) -> bool {
        descriptor::supported()
    }
    #[cfg(unix)]
    fn create_stage_at(&self, parent: &File, leaf: &std::ffi::OsStr) -> std::io::Result<File> {
        descriptor::open_at(
            parent,
            leaf,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o666,
        )
    }
    #[cfg(unix)]
    fn create_private_stage_at(
        &self,
        parent: &File,
        leaf: &std::ffi::OsStr,
    ) -> std::io::Result<File> {
        descriptor::open_at(
            parent,
            leaf,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
    }
    #[cfg(unix)]
    fn create_private_directory_at(
        &self,
        parent: &File,
        leaf: &std::ffi::OsStr,
    ) -> std::io::Result<()> {
        descriptor::mkdir_private_at(parent, leaf)
    }
    #[cfg(unix)]
    fn open_append_at(&self, parent: &File, leaf: &std::ffi::OsStr) -> std::io::Result<File> {
        descriptor::open_at(
            parent,
            leaf,
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            0o666,
        )
    }
    #[cfg(unix)]
    fn replace_at(
        &self,
        from_parent: &File,
        from: &std::ffi::OsStr,
        to_parent: &File,
        to: &std::ffi::OsStr,
    ) -> std::io::Result<()> {
        descriptor::rename_at(from_parent, from, to_parent, to)
    }
    #[cfg(unix)]
    fn remove_at(&self, parent: &File, leaf: &std::ffi::OsStr) -> std::io::Result<()> {
        descriptor::unlink_at(parent, leaf)
    }
    #[cfg(unix)]
    fn create_directory_at(&self, parent: &File, leaf: &std::ffi::OsStr) -> std::io::Result<()> {
        descriptor::mkdir_at(parent, leaf)
    }
    #[cfg(unix)]
    fn sync_directory_file(&self, directory: &File) -> std::io::Result<DirectorySync> {
        // Route descriptors may carry search permission only. Open the held
        // directory itself for durability, without resolving its old pathname.
        descriptor::open_at(
            directory,
            std::ffi::OsStr::new("."),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?
        .sync_all()?;
        Ok(DirectorySync::Supported)
    }
    fn create_stage(&self, path: &Path) -> std::io::Result<File> {
        OpenOptions::new().write(true).create_new(true).open(path)
    }
    fn create_private_stage(&self, path: &Path) -> std::io::Result<File> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
        }
        #[cfg(windows)]
        {
            // Operational callers retain a pinned private parent through replacement.
            Ok(super::windows_security::create_private_file(
                path,
                super::windows_security::Sharing::Stage,
            )?
            .into_file())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
    fn create_private_directory(&self, path: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(path)
        }
        #[cfg(windows)]
        {
            super::windows_security::create_private_directory(path).map(|_| ())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(std::io::ErrorKind::Unsupported.into())
        }
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
        #[cfg(windows)]
        {
            super::windows_security::inspect_directory(directory)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = directory;
            Ok(DirectorySync::Unsupported)
        }
    }
}

#[derive(Clone)]
pub struct VaultFs {
    root: VaultRoot,
    io: Arc<dyn DurableIo>,
    storage_recovery: bool,
    maintenance_execution: bool,
    published_refresh_paths: Option<Arc<crate::catalog::source_refresh::PublishedRefreshPaths>>,
}
/// Owner-admitted temporary-file work. It grants no replacement authority.
#[derive(Clone)]
pub(crate) struct StagePreparation {
    fs: VaultFs,
    target: VaultRelativePath,
    destination: PathBuf,
}
impl StagePreparation {
    pub(crate) fn result_owned_bound(&self) -> u64 {
        (std::mem::size_of::<StagedFile>()
            + self.fs.root.owned_capacity()
            + self.target.owned_capacity()
            + 8192usize.max(
                self.destination
                    .capacity()
                    .saturating_add(96)
                    .saturating_mul(2),
            )
            + 128) as u64
    }
    pub(crate) fn owned_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.fs.root.owned_capacity()
            + self.target.owned_capacity()
            + self.destination.capacity()
    }
    pub(crate) fn stage(&self, bytes: &[u8]) -> Result<StagedFile> {
        crate::maintenance_diagnostic::observe(crate::maintenance_diagnostic::Phase::Stage, || {
            self.fs.require_storage_ready()?;
            self.fs.validate_paths(std::slice::from_ref(&self.target))?;
            #[cfg(unix)]
            let (destination, parent_descriptor) = if self.fs.uses_descriptor_relative() {
                self.fs.bind(&self.target, false)?.into_parts()
            } else {
                (self.fs.resolve(&self.target)?, None)
            };
            #[cfg(not(unix))]
            let destination = self.fs.resolve(&self.target)?;
            if destination != self.destination {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "stage route changed after admission",
                ));
            }
            let parent = destination.parent().expect("managed file has parent");
            let path = parent.join(format!(".lwiki-stage-{}.tmp", uuid::Uuid::now_v7()));
            #[cfg(unix)]
            let created = if self.fs.uses_descriptor_relative() {
                self.fs.io.create_stage_at(
                    parent_descriptor.as_ref().ok_or_else(|| {
                        io_error("open stage parent", std::io::ErrorKind::NotFound.into())
                    })?,
                    path.file_name().expect("stage leaf"),
                )
            } else {
                self.fs.io.create_stage(&path)
            };
            #[cfg(not(unix))]
            let created = self.fs.io.create_stage(&path);
            let mut file = match created {
                Ok(file) => file,
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::AlreadyExists {
                        #[cfg(unix)]
                        if let Some(parent) = parent_descriptor.as_ref() {
                            let _ = self
                                .fs
                                .io
                                .remove_at(parent, path.file_name().expect("stage leaf"));
                        } else {
                            let _ = self.fs.io.remove(&path);
                        }
                        #[cfg(not(unix))]
                        let _ = self.fs.io.remove(&path);
                    }
                    return Err(io_error("create same-directory stage", e));
                }
            };
            let mut staged = StagedFile {
                path,
                #[cfg(unix)]
                parent_descriptor,
                #[cfg(unix)]
                prepared_parent_identity: (0, 0),
                target: self.target.clone(),
                root: self.fs.root.clone(),
                io: Arc::clone(&self.fs.io),
                proposed_hash: Blake3Hash::digest(bytes),
                proposed_len: bytes.len(),
            };
            #[cfg(unix)]
            {
                staged.prepared_parent_identity = {
                    use std::os::unix::fs::MetadataExt;
                    #[cfg(test)]
                    if descriptor::fail_parent_observation() {
                        return Err(io_error(
                            "inspect prepared stage parent",
                            std::io::Error::from_raw_os_error(libc::EIO),
                        ));
                    }
                    let meta = match staged.parent_descriptor.as_ref() {
                        Some(parent) => parent.metadata(),
                        None => fs::symlink_metadata(parent),
                    }
                    .map_err(|e| io_error("inspect prepared stage parent", e))?;
                    (meta.dev(), meta.ino())
                };
            }
            if staged.owned_bytes() > self.result_owned_bound() {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "stage result exceeds admitted ownership",
                ));
            }
            self.fs
                .io
                .write_stage(&mut file, bytes)
                .map_err(|e| io_error("write stage", e))?;
            self.fs
                .io
                .sync_file(&file)
                .map_err(|e| io_error("sync stage", e))?;
            #[cfg(unix)]
            if let Some(parent) = staged.parent_descriptor.as_ref() {
                let fresh = self.fs.bind(&self.target, false)?;
                fresh.same_held_parent(parent)?;
                // A fresh stage observation authenticates the descriptor-created
                // bytes in the currently named parent before returning a receipt.
                let stage_route = self.fs.bind(
                    &VaultRelativePath::new(
                        staged
                            .path
                            .strip_prefix(self.fs.root.path())
                            .ok()
                            .and_then(Path::to_str)
                            .ok_or_else(|| WikiError::invalid("stage escapes vault"))?,
                    )?,
                    true,
                )?;
                stage_route.same_held_parent(parent)?;
                stage_route.require_file_identity(&file)?;
            }
            Ok(staged)
        })
    }
}
pub struct StagedFile {
    path: PathBuf,
    #[cfg(unix)]
    parent_descriptor: Option<File>,
    #[cfg(unix)]
    prepared_parent_identity: (u64, u64),
    target: VaultRelativePath,
    root: VaultRoot,
    io: Arc<dyn DurableIo>,
    proposed_hash: Blake3Hash,
    proposed_len: usize,
}
impl StagedFile {
    pub(crate) fn owned_bytes(&self) -> u64 {
        (std::mem::size_of::<Self>()
            + self.path.capacity()
            + self.target.owned_capacity()
            + self.root.owned_capacity()
            + self.proposed_hash.owned_capacity()) as u64
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn proposed_hash(&self) -> &Blake3Hash {
        &self.proposed_hash
    }
}
impl Drop for StagedFile {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(parent) = self.parent_descriptor.as_ref() {
            let _ = self
                .io
                .remove_at(parent, self.path.file_name().expect("stage leaf"));
            return;
        }
        let _ = self.io.remove(&self.path);
    }
}
impl VaultFs {
    /// Inline descriptor and owned root bytes; I/O and published scope are shared.
    pub(crate) fn owned_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.root.owned_capacity()
    }
    pub fn new(root: VaultRoot) -> Self {
        Self::with_io(root, Arc::new(NativeIo))
    }
    pub fn with_io(root: VaultRoot, io: Arc<dyn DurableIo>) -> Self {
        Self {
            root,
            io,
            storage_recovery: false,
            maintenance_execution: false,
            published_refresh_paths: None,
        }
    }
    #[doc(hidden)]
    pub fn for_maintenance_execution(&self) -> Self {
        let mut scoped = self.clone();
        scoped.maintenance_execution = true;
        scoped
    }
    pub(crate) fn maintenance_execution_requested(&self) -> bool {
        self.maintenance_execution
    }
    pub(crate) fn uses_descriptor_relative(&self) -> bool {
        #[cfg(unix)]
        {
            self.maintenance_execution && descriptor::supported() && self.io.descriptor_relative()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
    #[cfg(unix)]
    pub(crate) fn bind(&self, path: &VaultRelativePath, raw: bool) -> Result<descriptor::Route> {
        if raw {
            return self.root.bind(path, true);
        }
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::ResolveValidation,
            || {
                crate::maintenance_diagnostic::bump(
                    crate::maintenance_diagnostic::Counter::LogicalResolve,
                    1,
                );
                let physical = crate::storage::layout::physical_relative_fs(self, path)?;
                self.root.bind(&physical, true)
            },
        )
    }
    #[cfg(unix)]
    pub(crate) fn bind_raw_budgeted(
        &self,
        path: &VaultRelativePath,
        on_step: &mut dyn FnMut() -> Result<()>,
    ) -> Result<descriptor::Route> {
        descriptor::Route::resolve(&self.root, path, on_step)
    }
    pub(crate) fn raw_exists(&self, path: &VaultRelativePath) -> Result<bool> {
        #[cfg(unix)]
        if self.uses_descriptor_relative() {
            return self.bind(path, true).map(|route| route.metadata().is_ok());
        }
        Ok(self.root.resolve_raw(path)?.exists())
    }
    fn resolve(&self, path: &VaultRelativePath) -> Result<PathBuf> {
        #[cfg(unix)]
        if self.uses_descriptor_relative() {
            return self.bind(path, false).map(|route| route.path);
        }
        self.root.resolve(path)
    }
    pub(crate) fn read_regular_bounded(
        &self,
        path: &VaultRelativePath,
        raw: bool,
        limit: usize,
        layer: &'static str,
    ) -> Result<Option<Vec<u8>>> {
        read_regular_bounded_policy(
            &self.root,
            path,
            raw,
            limit,
            layer,
            self.uses_descriptor_relative(),
            Some(self),
        )
    }
    pub(crate) fn hash_regular_raw_bounded(
        &self,
        path: &VaultRelativePath,
        limit: usize,
        layer: &'static str,
    ) -> Result<Option<(u64, [u8; 32])>> {
        hash_regular_raw_bounded_policy(
            &self.root,
            path,
            limit,
            layer,
            self.uses_descriptor_relative(),
            Some(self),
        )
    }
    pub(crate) fn with_published_refresh_paths(
        &self,
        paths: crate::catalog::source_refresh::PublishedRefreshPaths,
    ) -> Result<Self> {
        paths.require_root(&self.root)?;
        let mut scoped = self.clone();
        scoped.published_refresh_paths = Some(Arc::new(paths));
        Ok(scoped)
    }
    pub(crate) fn validate_paths(&self, paths: &[VaultRelativePath]) -> Result<()> {
        self.root
            .validate_refresh_paths(paths, self.published_refresh_paths.as_deref())
    }
    pub fn root(&self) -> &VaultRoot {
        &self.root
    }
    pub(crate) fn durable_io(&self) -> Arc<dyn DurableIo> {
        Arc::clone(&self.io)
    }
    pub(crate) fn is_storage_recovery(&self) -> bool {
        self.storage_recovery
    }
    /// Only the storage coordinator may continue an immutable pending cleanup.
    pub(crate) fn for_storage_recovery(&self) -> Self {
        Self {
            root: self.root.clone(),
            io: Arc::clone(&self.io),
            storage_recovery: true,
            maintenance_execution: self.maintenance_execution,
            published_refresh_paths: None,
        }
    }
    pub(crate) fn require_storage_ready(&self) -> Result<()> {
        if !self.storage_recovery
            && self
                .hash_regular_raw_bounded(
                    &VaultRelativePath::new(".wiki/state/storage/cleanup.json")?,
                    64 * 1024 * 1024,
                    "storage-read",
                )?
                .is_some()
        {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "storage cleanup is interrupted; run `lwiki storage cleanup` before ordinary writes",
            ));
        }
        Ok(())
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
        self.require_storage_ready()?;
        Self::require_operational(target)?;
        self.validate_paths(std::slice::from_ref(target))?;
        let path = self.resolve(target)?;
        #[cfg(unix)]
        if self.uses_descriptor_relative() {
            let route = self.bind(target, false)?;
            if route.path != path {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "append route changed",
                ));
            }
            let parent = route
                .parent()
                .map_err(|e| io_error("open append parent", e))?;
            let mut file = self
                .io
                .open_append_at(parent, route.leaf())
                .map_err(|e| io_error("open operational append", e))?;
            let fresh = self.bind(target, false)?;
            route.same_parent(&fresh)?;
            fresh.require_file_identity(&file)?;
            self.io
                .write_stage(&mut file, bytes)
                .map_err(|e| io_error("append operational bytes", e))?;
            self.io
                .sync_file(&file)
                .map_err(|e| io_error("sync operational append", e))?;
            return self
                .io
                .sync_directory_file(parent)
                .map_err(|e| io_error("sync append directory", e));
        }
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
        self.require_storage_ready()?;
        Self::require_operational(target)?;
        let path = self.resolve(target)?;
        // Truncation never creates a missing operational file.
        #[cfg(unix)]
        let route = if self.uses_descriptor_relative() {
            Some(self.bind(target, false)?)
        } else {
            None
        };
        #[cfg(unix)]
        let file = if let Some(route) = route.as_ref() {
            if route.path != path {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "truncation route changed",
                ));
            }
            descriptor::open_at(
                route
                    .parent()
                    .map_err(|e| io_error("open truncation parent", e))?,
                route.leaf(),
                libc::O_WRONLY,
                0,
            )
        } else {
            OpenOptions::new().write(true).open(&path)
        }
        .map_err(|e| io_error("open operational truncation", e))?;
        #[cfg(not(unix))]
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|e| io_error("open operational truncation", e))?;
        #[cfg(unix)]
        if let Some(route) = route.as_ref() {
            let fresh = self.bind(target, false)?;
            route.same_parent(&fresh)?;
            fresh.require_file_identity(&file)?;
        }
        if length
            > crate::maintenance_diagnostic::observe(
                crate::maintenance_diagnostic::Phase::MetadataObservation,
                || {
                    crate::maintenance_diagnostic::bump(
                        crate::maintenance_diagnostic::Counter::MetadataCall,
                        1,
                    );
                    file.metadata()
                },
            )
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
        self.require_storage_ready()?;
        let path = self.resolve(target)?;
        #[cfg(unix)]
        if self.uses_descriptor_relative() {
            let route = self.bind(target, false)?;
            if route.path != path {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "sync route changed",
                ));
            }
            match route.open_read() {
                Ok(file) => {
                    let fresh = self.bind(target, false)?;
                    route.same_parent(&fresh)?;
                    fresh.require_file_identity(&file)?;
                    self.io
                        .sync_file(&file)
                        .map_err(|e| io_error("sync observed target", e))?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    crate::maintenance_diagnostic::bump(
                        crate::maintenance_diagnostic::Counter::ExpectedMissing,
                        1,
                    );
                    let fresh = self.bind(target, false)?;
                    route.same_parent(&fresh)?;
                    match fresh.metadata() {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(io_error("recheck absent sync target", error)),
                        Ok(_) => {
                            return Err(WikiError::new(
                                ErrorCode::ContentConflict,
                                "absent sync target appeared",
                            ));
                        }
                    }
                }
                Err(error) => return Err(io_error("open observed target", error)),
            }
            return self
                .io
                .sync_directory_file(
                    route
                        .parent()
                        .map_err(|e| io_error("open sync parent", e))?,
                )
                .map_err(|e| io_error("sync observed target directory", e));
        }
        #[cfg(unix)]
        let opened = {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)
        };
        #[cfg(not(unix))]
        let opened = File::open(&path);
        match opened {
            Ok(file) => {
                let held = crate::maintenance_diagnostic::observe(
                    crate::maintenance_diagnostic::Phase::MetadataObservation,
                    || {
                        crate::maintenance_diagnostic::bump(
                            crate::maintenance_diagnostic::Counter::MetadataCall,
                            1,
                        );
                        file.metadata()
                    },
                )
                .map_err(|e| io_error("inspect sync target", e))?;
                let named = crate::maintenance_diagnostic::observe(
                    crate::maintenance_diagnostic::Phase::MetadataObservation,
                    || {
                        crate::maintenance_diagnostic::bump(
                            crate::maintenance_diagnostic::Counter::MetadataCall,
                            1,
                        );
                        fs::symlink_metadata(&path)
                    },
                )
                .map_err(|e| io_error("recheck sync target", e))?;
                if !held.is_file() || !named.is_file() || named.file_type().is_symlink() {
                    return Err(WikiError::invalid("sync target must remain a regular file"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if held.dev() != named.dev() || held.ino() != named.ino() {
                        return Err(WikiError::new(
                            ErrorCode::ContentConflict,
                            "sync target identity changed",
                        ));
                    }
                }
                if self.resolve(target)? != path {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "sync target route changed",
                    ));
                }
                self.io
                    .sync_file(&file)
                    .map_err(|e| io_error("sync observed target", e))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                crate::maintenance_diagnostic::bump(
                    crate::maintenance_diagnostic::Counter::ExpectedMissing,
                    1,
                );
            }
            Err(e) => return Err(io_error("open observed target", e)),
        }
        self.io
            .sync_directory(path.parent().expect("managed file has parent"))
            .map_err(|e| io_error("sync observed target directory", e))
    }
    pub fn read_before(&self, target: &VaultRelativePath) -> Result<Option<BeforeImage>> {
        Ok(self
            .read_regular_bounded(target, false, usize::MAX, "vault-read")?
            .map(|bytes| BeforeImage {
                hash: Blake3Hash::digest(&bytes),
                bytes,
            }))
    }
    pub fn ensure_directory(
        &self,
        directory: &VaultRelativePath,
        permit: &WriterPermit,
    ) -> Result<DirectorySync> {
        permit.require_root(&self.root)?;
        self.require_storage_ready()?;
        self.validate_paths(std::slice::from_ref(directory))?;
        let directory = crate::storage::layout::physical_relative_fs(self, directory)?;
        let mut relative = String::new();
        let mut support = DirectorySync::Supported;
        for part in directory.as_str().split('/') {
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(part);
            let path = self.resolve(&VaultRelativePath::new(&relative)?)?;
            #[cfg(unix)]
            if self.uses_descriptor_relative() {
                let relative = VaultRelativePath::new(&relative)?;
                let route = self.bind(&relative, false)?;
                if route.path != path {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "directory route changed",
                    ));
                }
                let parent = route
                    .parent()
                    .map_err(|e| io_error("open managed directory parent", e))?;
                match self.io.create_directory_at(parent, route.leaf()) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let meta = route
                            .metadata()
                            .map_err(|e| io_error("inspect existing directory", e))?;
                        if !meta.is_dir() || meta.file_type().is_symlink() {
                            return Err(WikiError::invalid(
                                "managed directory must remain a directory",
                            ));
                        }
                    }
                    Err(error) => return Err(io_error("create managed directory", error)),
                }
                let fresh = self.bind(&relative, false)?;
                route.same_parent(&fresh)?;
                let child = descriptor::directory_at(parent, route.leaf())?;
                for directory in [&child, parent] {
                    let result = self
                        .io
                        .sync_directory_file(directory)
                        .map_err(|e| io_error("sync managed directory", e))?;
                    if result == DirectorySync::Unsupported {
                        support = result;
                    }
                }
                continue;
            }
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
        self.prepare_stage(target, permit)?.stage(bytes)
    }
    pub(crate) fn prepare_stage(
        &self,
        target: &VaultRelativePath,
        permit: &WriterPermit,
    ) -> Result<StagePreparation> {
        permit.require_root(&self.root)?;
        self.require_storage_ready()?;
        self.validate_paths(std::slice::from_ref(target))?;
        let destination = self.resolve(target)?;
        let parent = destination.parent().expect("managed file has parent");
        // Logical operational files can have a managed physical parent different
        // from their visible siblings. Ensure only that contained parent.
        if crate::storage::layout::managed_path(target).is_some()
            && crate::storage::layout::active_fs(self)?
        {
            let relative = parent
                .strip_prefix(self.root.path())
                .map_err(|_| WikiError::invalid("mapped stage escapes vault"))?
                .to_str()
                .ok_or_else(|| WikiError::invalid("mapped parent UTF-8 invalid"))?;
            if !relative.is_empty()
                && self.ensure_directory(&VaultRelativePath::new(relative)?, permit)?
                    == DirectorySync::Unsupported
            {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "mapped parent durability unavailable",
                ));
            }
        }
        Ok(StagePreparation {
            fs: self.clone(),
            target: target.clone(),
            destination,
        })
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
        #[cfg(unix)]
        if staged.parent_descriptor.is_some() && !self.maintenance_execution_requested() {
            return self
                .for_maintenance_execution()
                .replace(staged, expected, permit);
        }
        #[cfg(unix)]
        if staged.parent_descriptor.is_some() && !self.uses_descriptor_relative() {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "bound stage requires descriptor-relative publisher",
            ));
        }
        permit.require_root(&self.root)?;
        self.require_storage_ready()?;
        if staged.root != self.root {
            return Err(WikiError::invalid("stage belongs to another vault"));
        }
        self.validate_paths(std::slice::from_ref(&staged.target))?;
        let destination = self.resolve(&staged.target)?;
        // Stage is public by path for journal intent; detect accidental tampering.
        let metadata = crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::MetadataObservation,
            || {
                crate::maintenance_diagnostic::bump(
                    crate::maintenance_diagnostic::Counter::MetadataCall,
                    1,
                );
                fs::symlink_metadata(&staged.path)
            },
        )
        .map_err(|e| io_error("inspect stage", e))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "stage must remain a regular file",
            ));
        }
        let stage_relative = staged
            .path
            .strip_prefix(self.root.path())
            .ok()
            .and_then(Path::to_str)
            .ok_or_else(|| WikiError::invalid("stage escapes vault"))?;
        let stage_relative = VaultRelativePath::new(stage_relative)?;
        let (_, stage_hash) = self
            .hash_regular_raw_bounded(&stage_relative, staged.proposed_len, "stage-read")
            .map_err(|error| {
                if error.code == ErrorCode::BudgetExceeded {
                    WikiError::new(
                        ErrorCode::ContentConflict,
                        "stage payload grew after preparation",
                    )
                } else {
                    error
                }
            })?
            .ok_or_else(|| WikiError::new(ErrorCode::ContentConflict, "stage disappeared"))?;
        if blake3::Hash::from(stage_hash).to_hex().as_str() != staged.proposed_hash.hex() {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "stage payload changed",
            ));
        }
        self.guard(&staged.target, expected)?;
        #[cfg(unix)]
        if self.uses_descriptor_relative() {
            // Rewalk both complete named routes after the byte guard. A held
            // stage parent is never sufficient authority after ancestor rename.
            let target_route = self.bind(&staged.target, false)?;
            if target_route.path != destination {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "replacement layout route changed",
                ));
            }
            let stage_route = self.bind(&stage_relative, true)?;
            let stage_parent = match staged.parent_descriptor.as_ref() {
                Some(parent) => {
                    stage_route.same_held_parent(parent)?;
                    parent
                }
                None => stage_route
                    .parent()
                    .map_err(|e| io_error("open prepared stage parent", e))?,
            };
            {
                use std::os::unix::fs::MetadataExt;
                let held = stage_parent
                    .metadata()
                    .map_err(|e| io_error("inspect prepared parent", e))?;
                if (held.dev(), held.ino()) != staged.prepared_parent_identity {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "prepared stage parent changed",
                    ));
                }
            }
            let target_parent = target_route
                .parent()
                .map_err(|e| io_error("open replacement parent", e))?;
            self.io
                .replace_at(
                    stage_parent,
                    staged.path.file_name().expect("stage leaf"),
                    target_parent,
                    target_route.leaf(),
                )
                .map_err(|e| io_error("replace staged file", e))?;
            return self
                .io
                .sync_directory_file(target_parent)
                .map_err(|e| io_error("sync replacement directory", e));
        }
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
        self.require_storage_ready()?;
        self.validate_paths(std::slice::from_ref(target))?;
        let destination = self.resolve(target)?;
        self.guard(target, expected)?;
        if expected == &ExpectedState::Absent {
            return Ok(DirectorySync::Supported);
        }
        #[cfg(unix)]
        if self.uses_descriptor_relative() {
            let route = self.bind(target, false)?;
            if route.path != destination {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "deletion layout route changed",
                ));
            }
            let parent = route
                .parent()
                .map_err(|e| io_error("open deletion parent", e))?;
            self.io
                .remove_at(parent, route.leaf())
                .map_err(|e| io_error("delete guarded file", e))?;
            return self
                .io
                .sync_directory_file(parent)
                .map_err(|e| io_error("sync deletion directory", e));
        }
        self.io
            .remove(&destination)
            .map_err(|e| io_error("delete guarded file", e))?;
        self.io
            .sync_directory(destination.parent().expect("managed file has parent"))
            .map_err(|e| io_error("sync deletion directory", e))
    }
}

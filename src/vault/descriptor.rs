//! Short-lived Unix route binding. Descriptors contain I/O; fresh walks establish
//! authority in the current named namespace. Neither is a reusable validation.
use super::{VaultRoot, io_error, worker_path};
use crate::domain::{ErrorCode, Result, VaultRelativePath, WikiError};
use std::{
    ffi::{CStr, CString, OsStr},
    fs::{File, Metadata as FsMetadata},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd, RawFd},
    },
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy)]
pub(crate) struct Metadata {
    mode: libc::mode_t,
    length: u64,
    device: u64,
    inode: u64,
}
pub(crate) struct FileType(libc::mode_t);
impl FileType {
    pub(crate) fn is_symlink(&self) -> bool {
        self.0 & libc::S_IFMT == libc::S_IFLNK
    }
}
impl Metadata {
    pub(crate) fn is_file(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFREG
    }
    pub(crate) fn is_dir(&self) -> bool {
        self.mode & libc::S_IFMT == libc::S_IFDIR
    }
    pub(crate) fn file_type(&self) -> FileType {
        FileType(self.mode)
    }
    pub(crate) fn len(&self) -> u64 {
        self.length
    }
    pub(crate) fn dev(&self) -> u64 {
        self.device
    }
    pub(crate) fn ino(&self) -> u64 {
        self.inode
    }
    fn same_identity(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}
impl From<FsMetadata> for Metadata {
    fn from(value: FsMetadata) -> Self {
        Self {
            mode: value.mode() as libc::mode_t,
            length: value.len(),
            device: value.dev(),
            inode: value.ino(),
        }
    }
}
fn name(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}
pub(crate) fn open_at(
    parent: &File,
    leaf: &OsStr,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<File> {
    open_fd(parent.as_raw_fd(), &name(leaf)?, flags, mode)
}
fn open_fd(parent: RawFd, leaf: &CStr, flags: libc::c_int, mode: libc::mode_t) -> io::Result<File> {
    #[cfg(test)]
    if tests::fail_open() {
        return Err(io::Error::from_raw_os_error(libc::EMFILE));
    }
    let fd = unsafe {
        libc::openat(
            parent,
            leaf.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        #[cfg(test)]
        tests::record_open(fd);
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
fn metadata_at(parent: &File, leaf: &OsStr) -> io::Result<Metadata> {
    crate::maintenance_diagnostic::observe(
        crate::maintenance_diagnostic::Phase::MetadataObservation,
        || {
            crate::maintenance_diagnostic::bump(
                crate::maintenance_diagnostic::Counter::MetadataCall,
                1,
            );
            metadata_at_unobserved(parent, leaf)
        },
    )
}
fn metadata_at_unobserved(parent: &File, leaf: &OsStr) -> io::Result<Metadata> {
    let mut value = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name(leaf)?.as_ptr(),
            value.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    Ok(Metadata {
        mode: value.st_mode,
        length: value.st_size.max(0) as u64,
        device: value.st_dev as u64,
        inode: value.st_ino as u64,
    })
}
fn held_metadata(file: &File) -> io::Result<Metadata> {
    crate::maintenance_diagnostic::observe(
        crate::maintenance_diagnostic::Phase::MetadataObservation,
        || {
            crate::maintenance_diagnostic::bump(
                crate::maintenance_diagnostic::Counter::MetadataCall,
                1,
            );
            file.metadata().map(Metadata::from)
        },
    )
}
/// One predicate selects supported platforms for candidate actual I/O helpers.
/// Existing validation and semantic scanners retain their baseline checks.
/// Unsupported Unix retains its original pathname implementation.
pub(crate) const fn supported() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ))
}
fn directory_flags() -> libc::c_int {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        return libc::O_PATH | libc::O_DIRECTORY;
    }
    #[cfg(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ))]
    {
        return libc::O_SEARCH | libc::O_DIRECTORY;
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    )))]
    {
        libc::O_RDONLY | libc::O_DIRECTORY
    }
}
pub(crate) fn directory_at(parent: &File, component: &OsStr) -> Result<File> {
    let named =
        metadata_at(parent, component).map_err(|e| io_error("inspect named directory", e))?;
    if !named.is_dir() || named.file_type().is_symlink() {
        return Err(WikiError::invalid(
            "managed path has a non-directory or symlink component",
        ));
    }
    let held = open_at(parent, component, directory_flags(), 0)
        .map_err(|e| io_error("open named directory", e))?;
    let opened = held_metadata(&held).map_err(|e| io_error("inspect held directory", e))?;
    let after =
        metadata_at(parent, component).map_err(|e| io_error("recheck named directory", e))?;
    if !opened.is_dir()
        || !after.is_dir()
        || !named.same_identity(&opened)
        || !opened.same_identity(&after)
    {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "directory identity changed during route binding",
        ));
    }
    Ok(held)
}
fn clear_errno() -> io::Result<()> {
    #[cfg(any(
        target_os = "linux",
        target_os = "emscripten",
        target_os = "hurd",
        target_os = "redox",
        target_os = "dragonfly"
    ))]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    unsafe {
        *libc::__error() = 0;
    }
    #[cfg(any(
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "cygwin",
        target_os = "nuttx"
    ))]
    unsafe {
        *libc::__errno() = 0;
    }
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    unsafe {
        *libc::___errno() = 0;
    }
    #[cfg(target_os = "haiku")]
    unsafe {
        *libc::_errnop() = 0;
    }
    #[cfg(target_os = "nto")]
    unsafe {
        *libc::__get_errno_ptr() = 0;
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "emscripten",
        target_os = "hurd",
        target_os = "redox",
        target_os = "dragonfly",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "cygwin",
        target_os = "nuttx",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "haiku",
        target_os = "nto"
    )))]
    {
        return Err(io::ErrorKind::Unsupported.into());
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "emscripten",
        target_os = "hurd",
        target_os = "redox",
        target_os = "dragonfly",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "cygwin",
        target_os = "nuttx",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "haiku",
        target_os = "nto"
    ))]
    Ok(())
}
fn marker(directory: &File, path: &Path, on_entry: &mut dyn FnMut() -> Result<()>) -> Result<bool> {
    if clear_errno().is_err() {
        // Unknown Unix enumeration ABI keeps the original marker policy. The
        // complete named chain is still revalidated before descriptor I/O.
        return super::exact_marker_budgeted(path, on_entry);
    }
    crate::maintenance_diagnostic::observe(
        crate::maintenance_diagnostic::Phase::MarkerObservation,
        || {
            crate::maintenance_diagnostic::bump(
                crate::maintenance_diagnostic::Counter::MarkerProbe,
                1,
            );
            on_entry()?;
            match metadata_at(directory, OsStr::new("WIKI.md")) {
                Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => return Ok(false),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    crate::maintenance_diagnostic::bump(
                        crate::maintenance_diagnostic::Counter::ExpectedMissing,
                        1,
                    );
                    return Ok(false);
                }
                Err(error) => return Err(io_error("inspect vault marker", error)),
            }
            on_entry()?;
            #[cfg(test)]
            let mut profile =
                super::profile::enumeration(path, Path::new(""), super::profile::Kind::Marker);
            #[cfg(not(test))]
            let _ = path;
            // A new open description avoids sharing enumeration offsets with the held
            // route. fdopendir takes ownership only after a successful conversion.
            let enumeration = open_at(
                directory,
                OsStr::new("."),
                libc::O_RDONLY | libc::O_DIRECTORY,
                0,
            )
            .map_err(|e| io_error("open marker enumeration", e))?;
            use std::os::unix::io::IntoRawFd;
            let fd = enumeration.into_raw_fd();
            let stream = crate::maintenance_diagnostic::observe(
                crate::maintenance_diagnostic::Phase::DirectoryObservation,
                || {
                    crate::maintenance_diagnostic::bump(
                        crate::maintenance_diagnostic::Counter::DirectoryEnumeration,
                        1,
                    );
                    let stream = unsafe { libc::fdopendir(fd) };
                    if stream.is_null() {
                        let error = io::Error::last_os_error();
                        // Failure leaves ownership with the caller; success transfers
                        // it to DIR, whose guard below closes it on every exit.
                        unsafe { libc::close(fd) };
                        Err(error)
                    } else {
                        Ok(stream)
                    }
                },
            )
            .map_err(|error| io_error("read vault directory", error))?;
            struct Stream(*mut libc::DIR);
            impl Drop for Stream {
                fn drop(&mut self) {
                    unsafe {
                        libc::closedir(self.0);
                    }
                }
            }
            let stream = Stream(stream);
            loop {
                // This DIR is owned by this call alone. Clear errno before each
                // readdir to distinguish end-of-directory from an actual error.
                clear_errno().map_err(|e| io_error("reset enumeration errno", e))?;
                let returned = unsafe { libc::readdir(stream.0) };
                if returned.is_null() {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(0) {
                        return Err(io_error("read vault entry", error));
                    }
                    break;
                }
                let entry_name = unsafe { CStr::from_ptr((*returned).d_name.as_ptr()) };
                if entry_name.to_bytes() == b"." || entry_name.to_bytes() == b".." {
                    continue;
                }
                crate::maintenance_diagnostic::bump(
                    crate::maintenance_diagnostic::Counter::EnumerationEntry,
                    1,
                );
                #[cfg(test)]
                profile.entry();
                on_entry()?;
                if entry_name.to_bytes() == b"WIKI.md" {
                    let meta = metadata_at(directory, OsStr::new("WIKI.md"))
                        .map_err(|e| io_error("inspect WIKI.md", e))?;
                    return Ok(meta.is_file() && !meta.file_type().is_symlink());
                }
            }
            Ok(false)
        },
    )
}

#[derive(Debug, PartialEq, Eq)]
enum LeafWitness {
    Missing(usize),
    Present(u64, u64, libc::mode_t),
}
struct Walk {
    path: PathBuf,
    parent: Option<File>,
    identities: Vec<(u64, u64)>,
    witness: LeafWitness,
}
fn walk(
    root: &VaultRoot,
    relative: &VaultRelativePath,
    on_entry: &mut dyn FnMut() -> Result<()>,
) -> Result<Walk> {
    let mut directory = open_fd(libc::AT_FDCWD, c"/", directory_flags(), 0)
        .map_err(|e| io_error("open filesystem root", e))?;
    let filesystem =
        held_metadata(&directory).map_err(|e| io_error("inspect filesystem root", e))?;
    let mut identities = vec![(filesystem.dev(), filesystem.ino())];
    // Assignment closes each previous ancestor as soon as its child has been
    // authenticated. Names come from the immutable input path; only identities
    // are retained between the two independent complete named walks.
    for component in root.path().components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                directory = directory_at(&directory, part)?;
                let held =
                    held_metadata(&directory).map_err(|e| io_error("inspect root ancestry", e))?;
                identities.push((held.dev(), held.ino()));
            }
            _ => {
                return Err(WikiError::invalid(
                    "vault root is not canonical absolute path",
                ));
            }
        }
    }
    let bound = identities.last().expect("bound root");
    if *bound != root.identity {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "vault root identity changed",
        ));
    }
    let mut path = crate::maintenance_diagnostic::measure(
        crate::maintenance_diagnostic::Phase::PathBufferConstruction,
        || root.path().to_owned(),
    );
    worker_path(&path, path.capacity())?;
    let mut missing = None;
    let mut witness = None;
    let mut parts = relative.as_str().split('/').enumerate().peekable();
    while let Some((index, component)) = parts.next() {
        on_entry()?;
        crate::maintenance_diagnostic::bump(
            crate::maintenance_diagnostic::Counter::PathComponent,
            1,
        );
        crate::maintenance_diagnostic::measure(
            crate::maintenance_diagnostic::Phase::PathWorkspaceConstruction,
            || path.push(component),
        );
        worker_path(&path, path.capacity())?;
        if missing.is_some() {
            continue;
        }
        match metadata_at(&directory, OsStr::new(component)) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(WikiError::invalid("managed path has a symlink component"));
            }
            Ok(meta) if meta.is_dir() => {
                let child = directory_at(&directory, OsStr::new(component))?;
                if marker(&child, &path, on_entry)? {
                    return Err(WikiError::invalid(
                        "managed path crosses a nested vault boundary",
                    ));
                }
                let held =
                    held_metadata(&child).map_err(|e| io_error("inspect managed directory", e))?;
                if parts.peek().is_some() {
                    identities.push((held.dev(), held.ino()));
                    directory = child;
                } else {
                    witness = Some(LeafWitness::Present(
                        held.dev(),
                        held.ino(),
                        held.mode & libc::S_IFMT,
                    ));
                }
            }
            Ok(_) if parts.peek().is_some() => {
                return Err(WikiError::invalid(
                    "managed path has a non-directory component",
                ));
            }
            Ok(meta) => {
                witness = Some(LeafWitness::Present(
                    meta.dev(),
                    meta.ino(),
                    meta.mode & libc::S_IFMT,
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                crate::maintenance_diagnostic::bump(
                    crate::maintenance_diagnostic::Counter::ExpectedMissing,
                    1,
                );
                missing = Some((OsStr::new(component), parts.peek().is_some()));
                witness = Some(LeafWitness::Missing(index));
            }
            Err(error) => return Err(io_error("inspect managed path", error)),
        }
    }
    if let Some((name, _)) = missing {
        match metadata_at(&directory, name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("recheck absent path component", error)),
            Ok(_) => {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "absent path component appeared",
                ));
            }
        }
    }
    let parent = if missing.is_some_and(|(_, ancestor)| ancestor) {
        None
    } else {
        Some(directory)
    };
    Ok(Walk {
        path,
        parent,
        identities,
        witness: witness.expect("relative path has a leaf"),
    })
}

pub(crate) struct Route {
    pub(crate) path: PathBuf,
    parent: Option<File>,
    identities: Vec<(u64, u64)>,
    legacy: bool,
}
impl Route {
    pub(crate) fn legacy(path: PathBuf) -> Self {
        Self {
            path,
            parent: None,
            identities: Vec::new(),
            legacy: true,
        }
    }
    pub(crate) fn resolve(
        root: &VaultRoot,
        relative: &VaultRelativePath,
        on_entry: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::ResolveValidation,
            || {
                crate::maintenance_diagnostic::bump(
                    crate::maintenance_diagnostic::Counter::RawResolve,
                    1,
                );
                let original = walk(root, relative, on_entry)?;
                // Reopen from '/' rather than treating held fds or identity vectors
                // as namespace authority. Every current ancestor, root binding,
                // marker, type and absence is observed by this second full walk.
                let fresh = walk(root, relative, on_entry)?;
                if original.path != fresh.path
                    || original.identities != fresh.identities
                    || original.witness != fresh.witness
                {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "complete named ancestry changed",
                    ));
                }
                if let (Some(a), Some(b)) = (&original.parent, &fresh.parent) {
                    if !held_metadata(a)
                        .map_err(|e| io_error("inspect first parent", e))?
                        .same_identity(
                            &held_metadata(b).map_err(|e| io_error("inspect current parent", e))?,
                        )
                    {
                        return Err(WikiError::new(
                            ErrorCode::ContentConflict,
                            "named parent identity changed",
                        ));
                    }
                }
                Ok(Self {
                    path: fresh.path,
                    parent: fresh.parent,
                    identities: fresh.identities,
                    legacy: false,
                })
            },
        )
    }
    pub(crate) fn parent(&self) -> io::Result<&File> {
        self.parent
            .as_ref()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }
    pub(crate) fn leaf(&self) -> &OsStr {
        self.path.file_name().expect("relative route has a leaf")
    }
    pub(crate) fn metadata(&self) -> io::Result<Metadata> {
        if self.legacy {
            return std::fs::symlink_metadata(&self.path).map(Metadata::from);
        }
        metadata_at_unobserved(self.parent()?, self.leaf())
    }
    pub(crate) fn open_read(&self) -> io::Result<File> {
        if self.legacy {
            use std::os::unix::fs::OpenOptionsExt;
            return std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&self.path);
        }
        open_at(self.parent()?, self.leaf(), libc::O_RDONLY, 0)
    }
    pub(crate) fn same_parent(&self, fresh: &Self) -> Result<()> {
        if self.path != fresh.path || self.identities != fresh.identities {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "managed route changed",
            ));
        }
        match (&self.parent, &fresh.parent) {
            (Some(a), Some(b))
                if held_metadata(a)
                    .map_err(|e| io_error("inspect original route", e))?
                    .same_identity(
                        &held_metadata(b).map_err(|e| io_error("inspect fresh route", e))?,
                    ) =>
            {
                Ok(())
            }
            (None, None) => Ok(()),
            _ => Err(WikiError::new(
                ErrorCode::ContentConflict,
                "named parent identity changed",
            )),
        }
    }
    pub(crate) fn same_held_parent(&self, held: &File) -> Result<()> {
        let current = held_metadata(
            self.parent()
                .map_err(|e| io_error("open fresh parent", e))?,
        )
        .map_err(|e| io_error("inspect fresh parent", e))?;
        let original = held_metadata(held).map_err(|e| io_error("inspect held parent", e))?;
        if !current.same_identity(&original) {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "held parent lost named authority",
            ));
        }
        Ok(())
    }
    pub(crate) fn require_file_identity(&self, held: &File) -> Result<()> {
        let named = self
            .metadata()
            .map_err(|e| io_error("inspect named target", e))?;
        let opened = held_metadata(held).map_err(|e| io_error("inspect opened target", e))?;
        if !named.is_file() || !opened.is_file() || !named.same_identity(&opened) {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "target identity or type changed",
            ));
        }
        Ok(())
    }
    pub(crate) fn require_directory_identity(&self, held: &File) -> Result<()> {
        let named = self
            .metadata()
            .map_err(|e| io_error("inspect named directory", e))?;
        let opened = held_metadata(held).map_err(|e| io_error("inspect opened directory", e))?;
        if !named.is_dir() || !opened.is_dir() || !named.same_identity(&opened) {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "directory identity or type changed",
            ));
        }
        Ok(())
    }
    pub(crate) fn into_parts(self) -> (PathBuf, Option<File>) {
        (self.path, self.parent)
    }
}
pub(crate) fn unlink_at(parent: &File, leaf: &OsStr) -> io::Result<()> {
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name(leaf)?.as_ptr(), 0) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub(crate) fn mkdir_at(parent: &File, leaf: &OsStr) -> io::Result<()> {
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name(leaf)?.as_ptr(), 0o777) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub(crate) fn mkdir_private_at(parent: &File, leaf: &OsStr) -> io::Result<()> {
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name(leaf)?.as_ptr(), 0o700) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub(crate) fn rename_at(
    from_parent: &File,
    from: &OsStr,
    to_parent: &File,
    to: &OsStr,
) -> io::Result<()> {
    if unsafe {
        libc::renameat(
            from_parent.as_raw_fd(),
            name(from)?.as_ptr(),
            to_parent.as_raw_fd(),
            name(to)?.as_ptr(),
        )
    } < 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "descriptor_tests.rs"]
mod tests;

pub(crate) fn directory_entries(
    directory: &File,
    visit: &mut dyn FnMut(&OsStr, &File) -> Result<()>,
) -> Result<()> {
    use std::os::unix::io::IntoRawFd;
    let owned = open_at(
        directory,
        OsStr::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    )
    .map_err(|e| io_error("open initialization directory", e))?;
    let fd = owned.into_raw_fd();
    let raw = crate::maintenance_diagnostic::observe(
        crate::maintenance_diagnostic::Phase::DirectoryObservation,
        || {
            crate::maintenance_diagnostic::bump(
                crate::maintenance_diagnostic::Counter::DirectoryEnumeration,
                1,
            );
            let raw = unsafe { libc::fdopendir(fd) };
            if raw.is_null() {
                let error = io::Error::last_os_error();
                unsafe {
                    libc::close(fd);
                }
                Err(error)
            } else {
                Ok(raw)
            }
        },
    )
    .map_err(|e| io_error("read initialization directory", e))?;
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = Stream(raw);
    loop {
        clear_errno().map_err(|e| io_error("reset enumeration errno", e))?;
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(io_error("read initialization entry", error));
            }
            return Ok(());
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        crate::maintenance_diagnostic::bump(
            crate::maintenance_diagnostic::Counter::EnumerationEntry,
            1,
        );
        let name = OsStr::from_bytes(name.to_bytes());
        visit(name, directory)?;
    }
}

#[cfg(test)]
pub(crate) fn fail_parent_observation() -> bool {
    tests::fail_parent_observation()
}

pub(crate) fn entry_is_regular(directory: &File, leaf: &OsStr) -> Result<bool> {
    metadata_at(directory, leaf)
        .map(|meta| meta.is_file())
        .map_err(|e| io_error("inspect initialization entry", e))
}

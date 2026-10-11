//! Descriptor-relative private configuration I/O on the supported Unix hosts.
use crate::domain::{ErrorCode, Result, WikiError};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub(super) fn error(path: &Path, action: &str, error: impl std::fmt::Display) -> WikiError {
    let mut result = WikiError::new(
        ErrorCode::ConfigInvalid,
        format!("{action} {}: {error}", path.display()),
    );
    result.hint = Some("Use --no-user-config to bypass ambient preferences, or --preferences FILE to select another file".into());
    result
}

/// Resolve existing ancestor aliases once, but never alias the config directory.
pub(super) fn effective_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path.to_str().is_none()
        || path.as_os_str().len() > 4096
        || path.components().count() > 64
    {
        return Err(error(
            path,
            "resolve configuration",
            "path must be absolute UTF-8 and bounded to 4096 bytes/64 components",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| error(path, "resolve configuration", "file name required"))?;
    let parent = path
        .parent()
        .ok_or_else(|| error(path, "resolve configuration", "parent required"))?;
    match fs::symlink_metadata(parent) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(error(
                path,
                "inspect configuration directory",
                "immediate parent must be a regular directory, not a symlink",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error(path, "inspect configuration directory", e)),
    }
    let mut existing = parent.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(&existing) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                resolved.push(name);
                if resolved
                    .components()
                    .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
                {
                    return Err(error(
                        path,
                        "resolve configuration",
                        "unresolved path component",
                    ));
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let component = existing
                    .file_name()
                    .ok_or_else(|| error(path, "resolve configuration directory", &e))?
                    .to_owned();
                if component == "." || component == ".." {
                    return Err(error(
                        path,
                        "resolve configuration directory",
                        "missing parent cannot contain . or ..",
                    ));
                }
                missing.push(component);
                if !existing.pop() {
                    return Err(error(path, "resolve configuration directory", e));
                }
            }
            Err(e) => return Err(error(path, "resolve configuration directory", e)),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ParentSnapshot {
    #[cfg(unix)]
    bindings: Vec<(PathBuf, u64, u64)>,
}
impl ParentSnapshot {
    pub(super) fn capture(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mut result = Self::default();
            let mut current = PathBuf::from("/");
            for component in path.parent().expect("validated parent").components() {
                let Component::Normal(component) = component else {
                    continue;
                };
                current.push(component);
                match fs::symlink_metadata(&current) {
                    Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => result
                        .bindings
                        .push((current.clone(), meta.dev(), meta.ino())),
                    Ok(_) => {
                        return Err(error(
                            path,
                            "inspect configuration parent",
                            "directory or symlink changed",
                        ));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                    Err(e) => return Err(error(path, "inspect configuration parent", e)),
                }
            }
            Ok(result)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(Self::default())
        }
    }
    pub(super) fn verify(&self, config_path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            for (path, dev, ino) in &self.bindings {
                let meta = fs::symlink_metadata(path)
                    .map_err(|e| error(config_path, "verify loaded configuration parent", e))?;
                if !meta.is_dir()
                    || meta.file_type().is_symlink()
                    || meta.dev() != *dev
                    || meta.ino() != *ino
                {
                    return Err(error(
                        config_path,
                        "verify loaded configuration parent",
                        "parent directory changed since configuration was loaded",
                    ));
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = config_path;
        }
        Ok(())
    }
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::{
        ffi::CString,
        fs::File,
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::{MetadataExt, OpenOptionsExt},
        },
        time::{Duration, Instant},
    };

    pub(in crate::config) struct Directory {
        file: File,
        path: PathBuf,
        // Every existing directory is checked again before any publication.
        bindings: Vec<(PathBuf, u64, u64)>,
    }
    fn name(value: &std::ffi::OsStr) -> std::io::Result<CString> {
        use std::os::unix::ffi::OsStrExt;
        CString::new(value.as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput.into())
    }
    fn opened(
        parent: &File,
        component: &std::ffi::OsStr,
        flags: i32,
        mode: libc::mode_t,
    ) -> std::io::Result<File> {
        let name = name(component)?;
        // The descriptor owns the returned fd; no path-based follow-up opens.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    fn same(a: &fs::Metadata, b: &fs::Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    fn protect(meta: &fs::Metadata) -> bool {
        meta.is_file()
            && meta.nlink() == 1
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0
    }
    impl Directory {
        pub(in crate::config) fn open(path: &Path, create: bool) -> Result<Option<Self>> {
            let parent = path
                .parent()
                .ok_or_else(|| error(path, "open configuration directory", "parent required"))?;
            let mut file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open("/")
                .map_err(|e| error(path, "open configuration directory", e))?;
            let mut current = PathBuf::from("/");
            let mut bindings = Vec::new();
            for component in parent.components() {
                let Component::Normal(component) = component else {
                    continue;
                };
                let next = match opened(&file, component, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
                    Ok(next) => next,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => {
                        return Ok(None);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        let component_name = name(component)
                            .map_err(|e| error(path, "create configuration directory", e))?;
                        let status = unsafe {
                            libc::mkdirat(file.as_raw_fd(), component_name.as_ptr(), 0o700)
                        };
                        if status < 0 {
                            let e = std::io::Error::last_os_error();
                            if e.kind() != std::io::ErrorKind::AlreadyExists {
                                return Err(error(path, "create configuration directory", e));
                            }
                        } else {
                            file.sync_all().map_err(|e| {
                                error(path, "sync configuration directory parent", e)
                            })?;
                        }
                        opened(&file, component, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                            .map_err(|e| error(path, "open created configuration directory", e))?
                    }
                    Err(e) => return Err(error(path, "open configuration directory", e)),
                };
                current.push(component);
                let metadata = next
                    .metadata()
                    .map_err(|e| error(path, "inspect configuration directory", e))?;
                bindings.push((current.clone(), metadata.dev(), metadata.ino()));
                file = next;
            }
            let result = Self {
                file,
                path: path.to_path_buf(),
                bindings,
            };
            result.verify()?;
            Ok(Some(result))
        }
        pub(in crate::config) fn verify(&self) -> Result<()> {
            for (path, dev, ino) in &self.bindings {
                let meta = fs::symlink_metadata(path)
                    .map_err(|e| error(&self.path, "verify configuration directory binding", e))?;
                if !meta.is_dir()
                    || meta.file_type().is_symlink()
                    || meta.dev() != *dev
                    || meta.ino() != *ino
                {
                    return Err(error(
                        &self.path,
                        "verify configuration directory binding",
                        "directory changed",
                    ));
                }
            }
            Ok(())
        }
        fn open_file(
            &self,
            name: &std::ffi::OsStr,
            flags: i32,
            mode: libc::mode_t,
        ) -> Result<File> {
            opened(&self.file, name, flags, mode)
                .map_err(|e| error(&self.path, "open configuration file", e))
        }
        pub(in crate::config) fn read(
            &self,
            name: &std::ffi::OsStr,
            max: usize,
        ) -> Result<Option<Vec<u8>>> {
            self.verify()?;
            let mut file = match opened(&self.file, name, libc::O_RDONLY, 0) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(error(&self.path, "read configuration file", e)),
            };
            let before = file
                .metadata()
                .map_err(|e| error(&self.path, "inspect configuration file", e))?;
            if !before.is_file() || before.len() > max as u64 {
                return Err(error(
                    &self.path,
                    "read configuration file",
                    "bounded regular JSON file required (maximum 64 KiB)",
                ));
            }
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(max as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| error(&self.path, "read configuration file", e))?;
            let after = file
                .metadata()
                .map_err(|e| error(&self.path, "verify configuration file", e))?;
            let current = self
                .open_file(name, libc::O_RDONLY, 0)?
                .metadata()
                .map_err(|e| error(&self.path, "verify configuration file", e))?;
            if bytes.len() > max
                || before.len() != bytes.len() as u64
                || !same(&before, &after)
                || !same(&before, &current)
                || before.len() != after.len()
                || before.mtime() != after.mtime()
                || before.mtime_nsec() != after.mtime_nsec()
                || before.ctime() != after.ctime()
                || before.ctime_nsec() != after.ctime_nsec()
            {
                return Err(error(
                    &self.path,
                    "verify configuration file",
                    "file changed while reading",
                ));
            }
            self.verify()?;
            Ok(Some(bytes))
        }
        pub(in crate::config) fn lock(&self, timeout_ms: u64) -> Result<Guard> {
            self.verify_writable()?;
            let name = suffixed_name(&self.path, ".lock")?;
            let file = self.open_file(&name, libc::O_RDWR | libc::O_CREAT, 0o600)?;
            let metadata = file
                .metadata()
                .map_err(|e| error(&self.path, "inspect configuration lock", e))?;
            if !protect(&metadata) {
                return Err(error(
                    &self.path,
                    "inspect configuration lock",
                    "lock must be private, owned, regular and unlinked elsewhere",
                ));
            }
            let start = Instant::now();
            loop {
                match file.try_lock() {
                    Ok(()) => break,
                    Err(std::fs::TryLockError::WouldBlock) => {
                        let remaining =
                            Duration::from_millis(timeout_ms).saturating_sub(start.elapsed());
                        if remaining.is_zero() {
                            return Err(WikiError::new(
                                ErrorCode::LockTimeout,
                                format!(
                                    "configuration writer lock timeout: {}",
                                    self.path.display()
                                ),
                            ));
                        }
                        std::thread::sleep(remaining.min(Duration::from_millis(10)));
                    }
                    Err(std::fs::TryLockError::Error(e)) => {
                        return Err(error(&self.path, "lock configuration", e));
                    }
                }
            }
            self.verify_lock(&file, &name)?;
            Ok(Guard { file, name })
        }
        pub(in crate::config) fn verify_lock(
            &self,
            file: &File,
            name: &std::ffi::OsStr,
        ) -> Result<()> {
            self.verify()?;
            let opened = file
                .metadata()
                .map_err(|e| error(&self.path, "verify configuration lock", e))?;
            let current = self
                .open_file(name, libc::O_RDONLY, 0)?
                .metadata()
                .map_err(|e| error(&self.path, "verify configuration lock", e))?;
            if !protect(&current) || !same(&opened, &current) {
                return Err(error(
                    &self.path,
                    "verify configuration lock",
                    "lock binding changed",
                ));
            }
            Ok(())
        }
        pub(in crate::config) fn stage(&self, bytes: &[u8]) -> Result<Stage<'_>> {
            self.verify_writable()?;
            let name = suffixed_name(&self.path, &format!(".stage-{}", uuid::Uuid::now_v7()))?;
            let mut file =
                self.open_file(&name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)?;
            let metadata = file
                .metadata()
                .map_err(|e| error(&self.path, "inspect staged configuration", e))?;
            let stage = Stage {
                directory: self,
                name,
                identity: (metadata.dev(), metadata.ino()),
                _file: file
                    .try_clone()
                    .map_err(|e| error(&self.path, "retain staged configuration", e))?,
                expected: bytes.to_vec(),
                published: false,
            };
            file.write_all(bytes)
                .map_err(|e| error(&self.path, "write staged configuration", e))?;
            file.sync_all()
                .map_err(|e| error(&self.path, "sync staged configuration", e))?;
            Ok(stage)
        }
        fn verify_writable(&self) -> Result<()> {
            self.verify()?;
            let metadata = self
                .file
                .metadata()
                .map_err(|e| error(&self.path, "inspect configuration directory ownership", e))?;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
                return Err(error(
                    &self.path,
                    "write configuration",
                    "configuration directory must be owned by this user and not writable by group or others",
                ));
            }
            Ok(())
        }
        pub(in crate::config) fn preserve_v1(&self, bytes: &[u8], max: usize) -> Result<PathBuf> {
            let backup = suffixed_name(&self.path, ".v1.bak")?;
            if let Some(existing) = self.read(&backup, max)? {
                if existing != bytes {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        format!(
                            "v1 configuration preservation file differs: {}",
                            self.path.with_file_name(&backup).display()
                        ),
                    ));
                }
                return Ok(self.path.with_file_name(backup));
            }
            let stage = self.stage(bytes)?;
            let source =
                name(&stage.name).map_err(|e| error(&self.path, "preserve v1 configuration", e))?;
            let target =
                name(&backup).map_err(|e| error(&self.path, "preserve v1 configuration", e))?;
            stage.verify()?;
            if unsafe {
                libc::linkat(
                    self.file.as_raw_fd(),
                    source.as_ptr(),
                    self.file.as_raw_fd(),
                    target.as_ptr(),
                    0,
                )
            } < 0
            {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::AlreadyExists
                    || self.read(&backup, max)?.as_deref() != Some(bytes)
                {
                    return Err(error(
                        &self.path,
                        "preserve v1 configuration without overwrite",
                        e,
                    ));
                }
            }
            self.file
                .sync_all()
                .map_err(|e| error(&self.path, "sync v1 configuration preservation", e))?;
            drop(stage);
            Ok(self.path.with_file_name(backup))
        }
        pub(in crate::config) fn publish(&self, stage: &mut Stage<'_>) -> Result<()> {
            stage.verify()?;
            let source =
                name(&stage.name).map_err(|e| error(&self.path, "replace configuration", e))?;
            let target = name(self.path.file_name().expect("validated file name"))
                .map_err(|e| error(&self.path, "replace configuration", e))?;
            if unsafe {
                libc::renameat(
                    self.file.as_raw_fd(),
                    source.as_ptr(),
                    self.file.as_raw_fd(),
                    target.as_ptr(),
                )
            } < 0
            {
                return Err(error(
                    &self.path,
                    "replace configuration",
                    std::io::Error::last_os_error(),
                ));
            }
            stage.published = true;
            self.file
                .sync_all()
                .map_err(|e| error(&self.path, "sync configuration publication", e))?;
            self.verify()?;
            Ok(())
        }
    }
    pub(in crate::config) struct Guard {
        pub(in crate::config) file: File,
        pub(in crate::config) name: std::ffi::OsString,
    }
    pub(in crate::config) struct Stage<'a> {
        directory: &'a Directory,
        name: std::ffi::OsString,
        identity: (u64, u64),
        _file: File,
        expected: Vec<u8>,
        published: bool,
    }
    impl Stage<'_> {
        fn verify(&self) -> Result<()> {
            self.directory.verify_writable()?;
            let file = self.directory.open_file(&self.name, libc::O_RDONLY, 0)?;
            let metadata = file
                .metadata()
                .map_err(|e| error(&self.directory.path, "verify staged configuration", e))?;
            if !protect(&metadata)
                || (metadata.dev(), metadata.ino()) != self.identity
                || self
                    .directory
                    .read(&self.name, self.expected.len())?
                    .as_deref()
                    != Some(self.expected.as_slice())
            {
                return Err(error(
                    &self.directory.path,
                    "verify staged configuration",
                    "staged file binding or bytes changed",
                ));
            }
            Ok(())
        }
    }
    impl Drop for Stage<'_> {
        fn drop(&mut self) {
            if !self.published {
                let owned = self
                    .directory
                    .open_file(&self.name, libc::O_RDONLY, 0)
                    .and_then(|file| {
                        file.metadata()
                            .map_err(|e| error(&self.directory.path, "inspect staged cleanup", e))
                    })
                    .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.identity);
                if !owned {
                    return;
                }
                if let Ok(name) = name(&self.name) {
                    unsafe {
                        libc::unlinkat(self.directory.file.as_raw_fd(), name.as_ptr(), 0);
                    }
                }
            }
        }
    }
    fn suffixed_name(path: &Path, suffix: &str) -> Result<std::ffi::OsString> {
        let mut result = path
            .file_name()
            .ok_or_else(|| error(path, "configuration file", "file name required"))?
            .to_owned();
        result.push(suffix);
        Ok(result)
    }
}

#[cfg(unix)]
pub(super) use native::Directory;

#[cfg(not(unix))]
pub(super) fn unsupported(path: &Path) -> WikiError {
    error(
        path,
        "private user configuration",
        "supported only on macOS and GNU/Linux",
    )
}

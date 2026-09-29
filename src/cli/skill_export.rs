//! Explicit portable copies anchored to held directory objects. No path-based cleanup.
use crate::domain::{ErrorCode, Result, WikiError};
use clap::CommandFactory;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn io_error(path: &Path, error: std::io::Error) -> WikiError {
    WikiError::new(
        ErrorCode::Internal,
        format!("skill export I/O at {}: {error}", path.display()),
    )
}
fn changed(path: &Path) -> WikiError {
    let mut error = conflict(format!(
        "held export directory/file binding changed: {}",
        path.display()
    ));
    error.details = json!({"binding_changed":true});
    error
}
fn component(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return Err(conflict("unsafe export component"));
    }
    Ok(())
}
#[derive(Clone, Copy)]
enum Event {
    DirectoryCreate,
    FileCreate,
    VerifyEnumeration,
    Return,
}
#[cfg(test)]
type EventCallback = Box<dyn FnMut(Event, &Path)>;
#[derive(Default)]
struct Events {
    #[cfg(test)]
    callback: Option<EventCallback>,
}
impl Events {
    fn fire(&mut self, event: Event, path: &Path) {
        #[cfg(test)]
        if let Some(callback) = &mut self.callback {
            callback(event, path);
        }
        #[cfg(not(test))]
        let _ = (event, path);
    }
}

/// A missing suffix is planned under its last held ancestor, never re-resolved later.
struct Location {
    held: filesystem::Directory,
    missing: Vec<String>,
}
impl Location {
    fn append(&self, relative: &str) -> Result<Self> {
        let mut held = self.held.clone();
        let mut missing = self.missing.clone();
        for name in relative.split('/') {
            component(name)?;
            if missing.is_empty() {
                match held.child(name)? {
                    Some(child) => held = child,
                    None => missing.push(name.into()),
                }
            } else {
                missing.push(name.into());
            }
        }
        held.verify_binding()?;
        Ok(Self { held, missing })
    }
    fn materialize(
        mut self,
        created: &mut Vec<String>,
        events: &mut Events,
    ) -> Result<filesystem::Directory> {
        for name in self.missing {
            self.held = self.held.create_directory(&name, created, events)?;
        }
        Ok(self.held)
    }
}
fn command_reference() -> Result<Vec<u8>> {
    let mut text = format!(
        "# Implemented lwiki {} commands\n\nGenerated from the release command registry and argument parser. Run `lwiki --json capabilities` before use.\n",
        env!("CARGO_PKG_VERSION")
    );
    let mut parser = super::arguments::Arguments::command();
    parser.build();
    for name in super::dispatch::COMMANDS {
        let mut leaf = &parser;
        for component in name.split_whitespace() {
            leaf = leaf.find_subcommand(component).ok_or_else(|| {
                WikiError::new(
                    ErrorCode::Internal,
                    format!("registered command has no parser: {name}"),
                )
            })?;
        }
        let mut help = leaf.clone();
        text.push_str(&format!(
            "\n## {name}\n\n```text\n{}\n```\n",
            help.render_long_help()
        ));
    }
    text.push_str("\n## Implemented schemas\n\n");
    for name in super::dispatch::SCHEMAS {
        text.push_str(&format!("- `lwiki --json schema {name}`\n"));
    }
    let text = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    Ok(text.into_bytes())
}
fn package(target: &str) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::from([
        (
            "SKILL.md".into(),
            include_bytes!("../../skills/llm-wiki/SKILL.md").to_vec(),
        ),
        (
            "agents/openai.yaml".into(),
            include_bytes!("../../skills/llm-wiki/agents/openai.yaml").to_vec(),
        ),
        (
            "references/workflows.md".into(),
            include_bytes!("../../skills/llm-wiki/references/workflows.md").to_vec(),
        ),
        (
            "references/examples.json".into(),
            include_bytes!("../../skills/llm-wiki/references/examples.json").to_vec(),
        ),
        ("references/commands.md".into(), command_reference()?),
    ]);
    let hashes: BTreeMap<_, _> = files
        .iter()
        .map(|(p, b)| (p.clone(), blake3::hash(b).to_hex().to_string()))
        .collect();
    let fingerprint = blake3::hash(
        &serde_json::to_vec(&hashes)
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
    )
    .to_hex()
    .to_string();
    let manifest = json!({"schema":"lwiki.skill-manifest.v1","version":env!("CARGO_PKG_VERSION"),"target":target,"commands":super::dispatch::COMMANDS,"schemas":super::dispatch::SCHEMAS,"checksum_algorithm":"blake3","package_checksum":fingerprint,"files":hashes});
    files.insert(
        "manifest.json".into(),
        serde_json::to_vec_pretty(&manifest)
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
    );
    Ok(files)
}
fn verify_tree(
    root: &filesystem::Directory,
    files: &BTreeMap<String, Vec<u8>>,
    events: &mut Events,
    retained_directories: &mut Vec<filesystem::Directory>,
    retained_files: &mut Vec<filesystem::FileGuard>,
) -> Result<()> {
    fn walk(
        dir: &filesystem::Directory,
        prefix: &str,
        files: &BTreeMap<String, Vec<u8>>,
        seen: &mut BTreeSet<String>,
        events: &mut Events,
        retained_directories: &mut Vec<filesystem::Directory>,
        retained_files: &mut Vec<filesystem::FileGuard>,
    ) -> Result<()> {
        dir.verify_binding()?;
        events.fire(Event::VerifyEnumeration, dir.path());
        for name in dir.entries(files.len() + 1)? {
            component(&name)?;
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if let Some(bytes) = files.get(&relative) {
                let (read, guard) = dir.read_file(&name, bytes.len())?;
                if read != *bytes {
                    return Err(conflict("export file differs from release package"));
                }
                retained_files.push(guard);
                if !seen.insert(relative) {
                    return Err(conflict("duplicate package path"));
                }
            } else if files.keys().any(|p| p.starts_with(&format!("{relative}/"))) {
                let child = dir
                    .child(&name)?
                    .ok_or_else(|| changed(&dir.path().join(&name)))?;
                retained_directories.push(child.clone());
                walk(
                    &child,
                    &relative,
                    files,
                    seen,
                    events,
                    retained_directories,
                    retained_files,
                )?;
            } else {
                return Err(conflict("extra export directory/file"));
            }
        }
        dir.verify_binding()
    }
    let mut seen = BTreeSet::new();
    walk(
        root,
        "",
        files,
        &mut seen,
        events,
        retained_directories,
        retained_files,
    )?;
    if seen.len() != files.len() {
        return Err(conflict("incomplete skill export"));
    }
    Ok(())
}
pub(crate) fn export_skill(target: &str, output: &Path, dry_run: bool) -> Result<Value> {
    export_with_events(target, output, dry_run, &mut Events::default())
}
fn export_with_events(
    target: &str,
    output: &Path,
    dry_run: bool,
    events: &mut Events,
) -> Result<Value> {
    let layout = match target {
        "codex" | "cursor" => ".agents/skills/llm-wiki",
        "claude-code" => ".claude/skills/llm-wiki",
        _ => {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "target must be codex, claude-code or cursor",
            ));
        }
    };
    let root_path = output.join(layout);
    let project = filesystem::acquire(output)?;
    let selected = project.append(layout)?;
    for alternate in [
        ".agents/skills/llm-wiki",
        ".claude/skills/llm-wiki",
        ".cursor/skills/llm-wiki",
        ".codex/skills/llm-wiki",
    ] {
        if alternate != layout && project.append(alternate)?.missing.is_empty() {
            return Err(conflict(
                "alternate skill discovery copy already exists in export project",
            ));
        }
    }
    let files = package(target)?;
    let reused = selected.missing.is_empty();
    let mut verified_directories = Vec::new();
    let mut verified_files = Vec::new();
    if reused {
        verify_tree(
            &selected.held,
            &files,
            events,
            &mut verified_directories,
            &mut verified_files,
        )?;
    }
    if dry_run || reused {
        events.fire(Event::Return, selected.held.path());
        selected.held.verify_binding()?;
        for dir in &verified_directories {
            dir.verify_binding()?;
        }
        for file in &verified_files {
            file.verify_binding()?;
        }
        return Ok(
            json!({"target":target,"path":root_path,"version":env!("CARGO_PKG_VERSION"),"dry_run":dry_run,"reused":reused,"files":files.keys().collect::<Vec<_>>(),"complete":reused}),
        );
    }
    let mut created = Vec::new();
    let result = (|| -> Result<()> {
        let root = selected.materialize(&mut created, events)?;
        let mut directories = BTreeMap::from([(String::new(), root.clone())]);
        let mut order: Vec<_> = files
            .keys()
            .filter(|p| p.as_str() != "SKILL.md" && p.as_str() != "manifest.json")
            .cloned()
            .collect();
        order.extend(["SKILL.md".into(), "manifest.json".into()]);
        for relative in order {
            let (parent, name) = relative.rsplit_once('/').unwrap_or(("", &relative));
            if !directories.contains_key(parent) {
                // Package subdirectories are fixed one-level names, not user paths.
                component(parent)?;
                let dir = root.create_directory(parent, &mut created, events)?;
                directories.insert(parent.into(), dir);
            }
            directories[parent].write_new(name, &files[&relative], &mut created, events)?;
        }
        verify_tree(
            &root,
            &files,
            events,
            &mut verified_directories,
            &mut verified_files,
        )?;
        events.fire(Event::Return, root.path());
        for dir in directories.values().chain(verified_directories.iter()) {
            dir.verify_binding()?;
        }
        for file in &verified_files {
            file.verify_binding()?;
        }
        root.verify_binding()
    })();
    if let Err(mut error) = result {
        let binding_changed = error
            .details
            .get("binding_changed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !created.is_empty() {
            error.message.push_str(&format!("; partial export remains in originally held directory objects ({} creations; requested path {})", created.len(), root_path.display()));
        }
        error.details = json!({"partial_export":!created.is_empty(),"created_paths":created,"created_path_semantics":"requested names under originally held directory objects; bindings may have changed","binding_changed":binding_changed,"path":root_path,"complete":false});
        return Err(error);
    }
    Ok(
        json!({"target":target,"path":root_path,"version":env!("CARGO_PKG_VERSION"),"dry_run":false,"reused":false,"files":files.keys().collect::<Vec<_>>(),"complete":true}),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod filesystem {
    use super::*;
    use std::{
        ffi::{CStr, CString},
        fs::File,
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::MetadataExt,
        },
        sync::Arc,
    };

    #[derive(Clone)]
    pub(super) struct Directory(Arc<Held>);
    pub(super) struct FileGuard {
        file: File,
        parent: Directory,
        name: CString,
        path: PathBuf,
        stamp: (u64, i64, i64, i64, i64),
    }
    fn stamp(file: &File) -> std::io::Result<(u64, i64, i64, i64, i64)> {
        let m = file.metadata()?;
        Ok((
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        ))
    }
    impl FileGuard {
        pub(super) fn verify_binding(&self) -> Result<()> {
            self.parent.verify_binding()?;
            let current = stat(&self.parent, &self.name)?.ok_or_else(|| changed(&self.path))?;
            if !current.regular()
                || !current
                    .matches(&self.file)
                    .map_err(|e| io_error(&self.path, e))?
                || stamp(&self.file).map_err(|e| io_error(&self.path, e))? != self.stamp
            {
                return Err(changed(&self.path));
            }
            Ok(())
        }
    }
    struct Held {
        file: File,
        path: PathBuf,
        parent: Option<Directory>,
        name: Option<CString>,
        dev: u64,
        ino: u64,
    }
    #[derive(Clone, Copy)]
    struct Identity {
        dev: u64,
        ino: u64,
        mode: libc::mode_t,
        size: u64,
    }
    impl Identity {
        fn directory(self) -> bool {
            self.mode & libc::S_IFMT == libc::S_IFDIR
        }
        fn regular(self) -> bool {
            self.mode & libc::S_IFMT == libc::S_IFREG
        }
        fn matches(self, file: &File) -> std::io::Result<bool> {
            let m = file.metadata()?;
            Ok(m.dev() == self.dev && m.ino() == self.ino)
        }
    }
    fn name(name: &str) -> Result<CString> {
        component(name)?;
        CString::new(name).map_err(|_| conflict("NUL in export component"))
    }
    #[allow(
        clippy::unnecessary_cast,
        reason = "libc device/inode widths differ by Unix target"
    )]
    fn stat(parent: &Directory, name: &CStr) -> Result<Option<Identity>> {
        let mut value = std::mem::MaybeUninit::<libc::stat>::uninit();
        // A valid held directory descriptor and terminated component; output initialized only on success.
        let result = unsafe {
            libc::fstatat(
                parent.0.file.as_raw_fd(),
                name.as_ptr(),
                value.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let e = std::io::Error::last_os_error();
            return if e.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(io_error(parent.path(), e))
            };
        }
        let s = unsafe { value.assume_init() };
        Ok(Some(Identity {
            dev: s.st_dev as u64,
            ino: s.st_ino as u64,
            mode: s.st_mode,
            size: s.st_size.max(0) as u64,
        }))
    }
    #[allow(
        clippy::unnecessary_cast,
        reason = "C variadic mode is promoted to unsigned int; libc::mode_t widths differ by Unix target"
    )]
    fn open_child(
        parent: &Directory,
        name: &CStr,
        flags: i32,
        mode: libc::mode_t,
    ) -> std::io::Result<File> {
        // The component is one name; openat is anchored to this retained parent object.
        let fd = unsafe {
            libc::openat(
                parent.0.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    fn held(
        file: File,
        path: PathBuf,
        parent: Option<Directory>,
        name: Option<CString>,
    ) -> Result<Directory> {
        let metadata = file.metadata().map_err(|e| io_error(&path, e))?;
        if !metadata.is_dir() {
            return Err(conflict("export directory is not a directory"));
        }
        let value = Directory(Arc::new(Held {
            dev: metadata.dev(),
            ino: metadata.ino(),
            file,
            path,
            parent,
            name,
        }));
        value.verify_binding()?;
        Ok(value)
    }
    pub(super) fn acquire(output: &Path) -> Result<Location> {
        if output
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "skill export path must not contain ..",
            ));
        }
        // Capture cwd before resolving any relative spelling. Compare the fully pinned chain to it.
        let mut options = File::options();
        options.read(true);
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let initial_cwd = if output.is_relative() {
            Some(options.open(".").map_err(|e| io_error(Path::new("."), e))?)
        } else {
            None
        };
        let cwd = if initial_cwd.is_some() {
            Some(std::env::current_dir().map_err(|e| io_error(output, e))?)
        } else {
            None
        };
        let root = held(
            options.open("/").map_err(|e| io_error(Path::new("/"), e))?,
            PathBuf::from("/"),
            None,
            None,
        )?;
        let mut location = Location {
            held: root,
            missing: vec![],
        };
        if let Some(cwd) = &cwd {
            for c in cwd.components() {
                if let Component::Normal(s) = c {
                    location = location
                        .append(s.to_str().ok_or_else(|| conflict("non-UTF8 export path"))?)?;
                }
            }
            if !location.missing.is_empty() {
                return Err(changed(cwd));
            }
            let a = initial_cwd
                .as_ref()
                .expect("captured cwd")
                .metadata()
                .map_err(|e| io_error(cwd, e))?;
            if a.dev() != location.held.0.dev || a.ino() != location.held.0.ino {
                return Err(changed(cwd));
            }
        }
        for c in output.components() {
            match c {
                Component::Normal(s) => {
                    location = location
                        .append(s.to_str().ok_or_else(|| conflict("non-UTF8 export path"))?)?
                }
                Component::RootDir | Component::CurDir => {}
                _ => return Err(conflict("unsafe export path component")),
            }
        }
        location.held.verify_binding()?;
        Ok(location)
    }
    impl Directory {
        pub(super) fn path(&self) -> &Path {
            &self.0.path
        }
        pub(super) fn verify_binding(&self) -> Result<()> {
            let mut next = Some(self);
            while let Some(dir) = next {
                let m = dir.0.file.metadata().map_err(|e| io_error(dir.path(), e))?;
                if !m.is_dir() || m.dev() != dir.0.dev || m.ino() != dir.0.ino {
                    return Err(changed(dir.path()));
                }
                if let (Some(parent), Some(name)) = (&dir.0.parent, &dir.0.name) {
                    let Some(current) = stat(parent, name)? else {
                        return Err(changed(dir.path()));
                    };
                    if !current.directory() || current.dev != dir.0.dev || current.ino != dir.0.ino
                    {
                        return Err(changed(dir.path()));
                    }
                }
                next = dir.0.parent.as_ref();
            }
            Ok(())
        }
        pub(super) fn child(&self, child: &str) -> Result<Option<Self>> {
            self.verify_binding()?;
            let name = name(child)?;
            let Some(before) = stat(self, &name)? else {
                return Ok(None);
            };
            if !before.directory() {
                return Err(conflict("symlink/special/file export directory component"));
            }
            let file = open_child(self, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                .map_err(|_| changed(&self.path().join(child)))?;
            if !before
                .matches(&file)
                .map_err(|e| io_error(self.path(), e))?
            {
                return Err(changed(&self.path().join(child)));
            }
            Ok(Some(held(
                file,
                self.path().join(child),
                Some(self.clone()),
                Some(name),
            )?))
        }
        pub(super) fn create_directory(
            &self,
            child: &str,
            created: &mut Vec<String>,
            events: &mut Events,
        ) -> Result<Self> {
            self.verify_binding()?;
            let name = name(child)?;
            let path = self.path().join(child);
            events.fire(Event::DirectoryCreate, &path);
            // Exclusive mkdir relative to the original parent, even if its name is swapped now.
            if unsafe { libc::mkdirat(self.0.file.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let e = std::io::Error::last_os_error();
                return Err(if e.kind() == std::io::ErrorKind::AlreadyExists {
                    conflict("export directory appeared during creation")
                } else {
                    io_error(&path, e)
                });
            }
            created.push(path.display().to_string());
            self.verify_binding()?;
            self.child(child)?.ok_or_else(|| changed(&path))
        }
        pub(super) fn write_new(
            &self,
            child: &str,
            bytes: &[u8],
            created: &mut Vec<String>,
            events: &mut Events,
        ) -> Result<()> {
            self.verify_binding()?;
            let name = name(child)?;
            let path = self.path().join(child);
            events.fire(Event::FileCreate, &path);
            let mut file = open_child(
                self,
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NONBLOCK,
                0o600,
            )
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    conflict("export file appeared during creation")
                } else {
                    io_error(&path, e)
                }
            })?;
            created.push(path.display().to_string());
            self.verify_binding()?;
            file.write_all(bytes).map_err(|e| io_error(&path, e))?;
            file.sync_all().map_err(|e| io_error(&path, e))?;
            let current = stat(self, &name)?.ok_or_else(|| changed(&path))?;
            if !current.regular() || !current.matches(&file).map_err(|e| io_error(&path, e))? {
                return Err(changed(&path));
            }
            self.verify_binding()
        }
        pub(super) fn read_file(&self, child: &str, size: usize) -> Result<(Vec<u8>, FileGuard)> {
            self.verify_binding()?;
            let name = name(child)?;
            let path = self.path().join(child);
            let before = stat(self, &name)?.ok_or_else(|| changed(&path))?;
            if !before.regular() || before.size != size as u64 {
                return Err(conflict(
                    "export resource is not an exact bounded regular file",
                ));
            }
            let file = open_child(self, &name, libc::O_RDONLY | libc::O_NONBLOCK, 0)
                .map_err(|_| changed(&path))?;
            let metadata = file.metadata().map_err(|e| io_error(&path, e))?;
            if !metadata.is_file()
                || metadata.len() != size as u64
                || !before.matches(&file).map_err(|e| io_error(&path, e))?
            {
                return Err(changed(&path));
            }
            let original_stamp = stamp(&file).map_err(|e| io_error(&path, e))?;
            let mut bytes = Vec::new();
            (&file)
                .take(size as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| io_error(&path, e))?;
            let after = stat(self, &name)?.ok_or_else(|| changed(&path))?;
            if !after.regular()
                || !after.matches(&file).map_err(|e| io_error(&path, e))?
                || bytes.len() != size
            {
                return Err(changed(&path));
            }
            let guard = FileGuard {
                file,
                parent: self.clone(),
                name,
                path,
                stamp: original_stamp,
            };
            guard.verify_binding()?;
            Ok((bytes, guard))
        }
        pub(super) fn entries(&self, limit: usize) -> Result<Vec<String>> {
            self.verify_binding()?;
            // Opening dot on the held fd creates an independent enumeration offset.
            let dot = CString::new(".").expect("literal dot");
            let descriptor = open_child(self, &dot, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                .map_err(|e| io_error(self.path(), e))?;
            use std::os::fd::IntoRawFd;
            let fd = descriptor.into_raw_fd();
            let stream = unsafe { libc::fdopendir(fd) };
            if stream.is_null() {
                let error = std::io::Error::last_os_error();
                unsafe {
                    libc::close(fd);
                }
                return Err(io_error(self.path(), error));
            }
            struct Stream(*mut libc::DIR);
            impl Drop for Stream {
                fn drop(&mut self) {
                    unsafe {
                        libc::closedir(self.0);
                    }
                }
            }
            let stream = Stream(stream);
            let mut names = Vec::new();
            loop {
                errno::clear();
                let entry = unsafe { libc::readdir(stream.0) };
                if entry.is_null() {
                    if errno::get() != 0 {
                        return Err(io_error(self.path(), std::io::Error::last_os_error()));
                    }
                    break;
                }
                let value = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
                    .to_str()
                    .map_err(|_| conflict("non-UTF8 export filename"))?;
                if value == "." || value == ".." {
                    continue;
                }
                component(value)?;
                if names.len() >= limit {
                    return Err(conflict("too many export directory entries"));
                }
                names.push(value.to_owned());
            }
            self.verify_binding()?;
            Ok(names)
        }
    }
    mod errno {
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
        fn pointer() -> *mut libc::c_int {
            unsafe { libc::__error() }
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        fn pointer() -> *mut libc::c_int {
            unsafe { libc::__errno_location() }
        }
        pub(super) fn clear() {
            unsafe {
                *pointer() = 0;
            }
        }
        pub(super) fn get() -> i32 {
            unsafe { *pointer() }
        }
    }
}

#[cfg(windows)]
mod filesystem {
    use super::*;
    use crate::vault::{
        acl_policy::Protection,
        windows_security::{self, CheckedFile, DirectoryGuard, Sharing},
    };
    use std::{
        fs,
        io::{Read, Write},
        sync::Arc,
    };
    #[derive(Clone)]
    pub(super) struct Directory {
        guard: Arc<DirectoryGuard>,
        path: PathBuf,
        parent: Option<Arc<Directory>>,
    }
    pub(super) struct FileGuard {
        checked: CheckedFile,
        path: PathBuf,
    }
    impl FileGuard {
        pub(super) fn verify_binding(&self) -> Result<()> {
            self.checked
                .verify_binding()
                .map_err(|_| changed(&self.path))
        }
    }
    fn protected(path: &Path, error: std::io::Error) -> WikiError {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            conflict("export object appeared during creation")
        } else if error.kind() == std::io::ErrorKind::Unsupported {
            WikiError::new(
                ErrorCode::CapabilityUnavailable,
                format!(
                    "protected skill export is unavailable at {}: {error}",
                    path.display()
                ),
            )
        } else {
            conflict(format!(
                "protected export path refused at {}: {error}",
                path.display()
            ))
        }
    }
    pub(super) fn acquire(output: &Path) -> Result<Location> {
        if output
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "skill export path must not contain ..",
            ));
        }
        // The cwd chain is pinned before deriving a relative destination.
        let cwd = std::env::current_dir().map_err(|e| io_error(output, e))?;
        let cwd_guard = if output.is_relative() {
            Some(
                windows_security::open_pinned_directory(&cwd, Protection::IntegrityProtected)
                    .map_err(|e| protected(&cwd, e))?,
            )
        } else {
            None
        };
        let absolute = if output.is_relative() {
            cwd.join(output)
        } else {
            output.to_path_buf()
        };
        let mut base = PathBuf::new();
        let mut names = Vec::new();
        let mut rooted = false;
        for c in absolute.components() {
            match c {
                Component::Prefix(p) if !rooted => base.push(p.as_os_str()),
                Component::RootDir if !rooted => {
                    base.push(c.as_os_str());
                    rooted = true;
                }
                Component::Normal(s) if rooted => {
                    let s = s
                        .to_str()
                        .ok_or_else(|| conflict("non-UTF8 export component"))?;
                    component(s)?;
                    names.push(s.to_owned());
                }
                Component::CurDir => {}
                _ => return Err(conflict("unsafe/unrooted export path")),
            }
        }
        if !rooted {
            return Err(conflict("export path lacks a supported root"));
        }
        let guard = windows_security::open_pinned_directory(&base, Protection::IntegrityProtected)
            .map_err(|e| protected(&base, e))?;
        let mut location = Location {
            held: Directory {
                guard: Arc::new(guard),
                path: base,
                parent: None,
            },
            missing: vec![],
        };
        for name in names {
            location = location.append(&name)?;
        }
        if let Some(cwd_guard) = cwd_guard {
            cwd_guard.verify_binding().map_err(|e| protected(&cwd, e))?;
        }
        location.held.verify_binding()?;
        Ok(location)
    }
    impl Directory {
        pub(super) fn path(&self) -> &Path {
            &self.path
        }
        pub(super) fn verify_binding(&self) -> Result<()> {
            let mut next = Some(self);
            while let Some(dir) = next {
                dir.guard.verify_binding().map_err(|_| changed(&dir.path))?;
                next = dir.parent.as_deref();
            }
            Ok(())
        }
        pub(super) fn child(&self, child: &str) -> Result<Option<Self>> {
            component(child)?;
            self.verify_binding()?;
            let path = self.path.join(child);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(protected(&path, e)),
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(conflict("symlink/special/file export directory component"));
            }
            let guard =
                windows_security::open_pinned_directory(&path, Protection::IntegrityProtected)
                    .map_err(|e| protected(&path, e))?;
            self.verify_binding()?;
            Ok(Some(Self {
                guard: Arc::new(guard),
                path,
                parent: Some(Arc::new(self.clone())),
            }))
        }
        pub(super) fn create_directory(
            &self,
            child: &str,
            created: &mut Vec<String>,
            events: &mut Events,
        ) -> Result<Self> {
            component(child)?;
            self.verify_binding()?;
            let path = self.path.join(child);
            events.fire(Event::DirectoryCreate, &path);
            // Held no-delete-sharing ancestor guards pin this full pathname chain.
            let guard = windows_security::create_private_directory(&path).map_err(|e| {
                if windows_security::created_before_failure(&e) {
                    created.push(path.display().to_string());
                }
                protected(&path, e)
            })?;
            created.push(path.display().to_string());
            self.verify_binding()?;
            Ok(Self {
                guard: Arc::new(guard),
                path,
                parent: Some(Arc::new(self.clone())),
            })
        }
        pub(super) fn write_new(
            &self,
            child: &str,
            bytes: &[u8],
            created: &mut Vec<String>,
            events: &mut Events,
        ) -> Result<()> {
            component(child)?;
            self.verify_binding()?;
            let path = self.path.join(child);
            events.fire(Event::FileCreate, &path);
            let mut file =
                windows_security::create_private_file(&path, Sharing::Stage).map_err(|e| {
                    if windows_security::created_before_failure(&e) {
                        created.push(path.display().to_string());
                    }
                    protected(&path, e)
                })?;
            created.push(path.display().to_string());
            self.verify_binding()?;
            file.verify_binding().map_err(|_| changed(&path))?;
            file.file_mut()
                .write_all(bytes)
                .map_err(|e| io_error(&path, e))?;
            file.file().sync_all().map_err(|e| io_error(&path, e))?;
            file.verify_binding().map_err(|_| changed(&path))?;
            self.verify_binding()
        }
        pub(super) fn read_file(&self, child: &str, size: usize) -> Result<(Vec<u8>, FileGuard)> {
            component(child)?;
            self.verify_binding()?;
            let path = self.path.join(child);
            let checked = windows_security::open_checked_file(
                &path,
                Protection::Private,
                Sharing::ReadOnly,
                false,
            )
            .map_err(|e| protected(&path, e))?;
            if checked
                .file()
                .metadata()
                .map_err(|e| io_error(&path, e))?
                .len()
                != size as u64
            {
                return Err(conflict("export resource size differs"));
            }
            let mut bytes = Vec::new();
            checked
                .file()
                .take(size as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| io_error(&path, e))?;
            if bytes.len() != size {
                return Err(conflict(
                    "export resource is not an exact bounded regular file",
                ));
            }
            let guard = FileGuard { checked, path };
            self.verify_binding()?;
            guard.verify_binding()?;
            Ok((bytes, guard))
        }
        pub(super) fn entries(&self, limit: usize) -> Result<Vec<String>> {
            self.verify_binding()?;
            let mut names = Vec::new();
            for entry in fs::read_dir(&self.path).map_err(|e| protected(&self.path, e))? {
                let name = entry
                    .map_err(|e| protected(&self.path, e))?
                    .file_name()
                    .into_string()
                    .map_err(|_| conflict("non-UTF8 export filename"))?;
                component(&name)?;
                if names.len() >= limit {
                    return Err(conflict("too many export directory entries"));
                }
                names.push(name);
            }
            self.verify_binding()?;
            Ok(names)
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod filesystem {
    use super::*;
    #[derive(Clone)]
    pub(super) struct Directory {
        path: PathBuf,
    }
    pub(super) struct FileGuard;
    impl FileGuard {
        pub(super) fn verify_binding(&self) -> Result<()> {
            Err(unavailable())
        }
    }
    fn unavailable() -> WikiError {
        WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "anchored skill export unavailable on this platform",
        )
    }
    pub(super) fn acquire(_: &Path) -> Result<Location> {
        Err(unavailable())
    }
    impl Directory {
        pub(super) fn path(&self) -> &Path {
            &self.path
        }
        pub(super) fn verify_binding(&self) -> Result<()> {
            Err(unavailable())
        }
        pub(super) fn child(&self, _: &str) -> Result<Option<Self>> {
            Err(unavailable())
        }
        pub(super) fn create_directory(
            &self,
            _: &str,
            _: &mut Vec<String>,
            _: &mut Events,
        ) -> Result<Self> {
            Err(unavailable())
        }
        pub(super) fn write_new(
            &self,
            _: &str,
            _: &[u8],
            _: &mut Vec<String>,
            _: &mut Events,
        ) -> Result<()> {
            Err(unavailable())
        }
        pub(super) fn read_file(&self, _: &str, _: usize) -> Result<(Vec<u8>, FileGuard)> {
            Err(unavailable())
        }
        pub(super) fn entries(&self, _: usize) -> Result<Vec<String>> {
            Err(unavailable())
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod anchored_tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        fs,
        os::unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, symlink},
        },
        rc::Rc,
        time::SystemTime,
    };
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let output = temp.path().join("output");
        let outside = temp.path().join("outside");
        fs::create_dir(&output).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"Unchanged outside destination").unwrap();
        (temp, output, outside)
    }
    fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
        fn walk(root: &Path, path: &Path, map: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
            let m = fs::symlink_metadata(path).unwrap();
            map.insert(
                path.strip_prefix(root).unwrap().into(),
                (
                    if m.is_file() {
                        fs::read(path).unwrap()
                    } else {
                        vec![]
                    },
                    m.modified().unwrap(),
                ),
            );
            if m.is_dir() {
                for e in fs::read_dir(path).unwrap() {
                    walk(root, &e.unwrap().path(), map);
                }
            }
        }
        let mut map = BTreeMap::new();
        walk(root, root, &mut map);
        map
    }
    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir(to).unwrap();
        for e in fs::read_dir(from).unwrap() {
            let p = e.unwrap().path();
            let target = to.join(p.file_name().unwrap());
            if p.is_dir() {
                copy_tree(&p, &target);
            } else {
                fs::copy(p, target).unwrap();
            }
        }
    }
    fn events(callback: impl FnMut(Event, &Path) + 'static) -> Events {
        Events {
            callback: Some(Box::new(callback)),
        }
    }
    fn assert_changed(error: &WikiError) {
        assert_eq!(error.code, ErrorCode::ContentConflict, "{error}");
        assert_eq!(error.details["binding_changed"], true);
    }
    #[test]
    fn anchored_file_creation_swap_never_writes_outside() {
        let (_temp, output, outside) = fixture();
        let before = tree(&outside);
        let fired = Rc::new(Cell::new(false));
        let seen = fired.clone();
        let destination = outside.clone();
        let mut hook = events(move |phase, path| {
            if matches!(phase, Event::FileCreate)
                && path.ends_with("references/commands.md")
                && !seen.get()
            {
                let parent = path.parent().unwrap();
                fs::rename(parent, parent.with_file_name("references-held")).unwrap();
                symlink(&destination, parent).unwrap();
                seen.set(true);
            }
        });
        let error = export_with_events("codex", &output, false, &mut hook).unwrap_err();
        assert_changed(&error);
        assert!(fired.get());
        assert_eq!(error.details["partial_export"], true);
        assert_eq!(tree(&outside), before);
        let retained = output.join(".agents/skills/llm-wiki/references-held/commands.md");
        assert_eq!(fs::metadata(retained).unwrap().len(), 0);
        assert!(
            !output
                .join(".agents/skills/llm-wiki/manifest.json")
                .exists()
        );
    }
    #[test]
    fn anchored_missing_directory_swap_never_creates_outside() {
        let (temp, output, outside) = fixture();
        let before = tree(&outside);
        let original = output.clone();
        let retained = temp.path().join("output-held");
        let held = retained.clone();
        let destination = outside.clone();
        let fired = Rc::new(Cell::new(false));
        let seen = fired.clone();
        let mut hook = events(move |phase, path| {
            if matches!(phase, Event::DirectoryCreate)
                && path == original.join(".agents")
                && !seen.get()
            {
                fs::rename(&original, &held).unwrap();
                symlink(&destination, &original).unwrap();
                seen.set(true);
            }
        });
        let error = export_with_events("codex", &output, false, &mut hook).unwrap_err();
        assert_changed(&error);
        assert!(fired.get());
        assert_eq!(error.details["partial_export"], true);
        assert!(retained.join(".agents").is_dir());
        assert_eq!(tree(&outside), before);
    }
    #[test]
    fn anchored_reuse_and_dry_run_refuse_swapped_matching_subtree() {
        for dry in [false, true] {
            let (_temp, output, outside) = fixture();
            export_skill("codex", &output, false).unwrap();
            let package = output.join(".agents/skills/llm-wiki");
            let original = package.join("references");
            let matching = outside.join("matching");
            copy_tree(&original, &matching);
            let before = tree(&outside);
            let original_bytes = tree(&original);
            let held = package.join("references-held");
            let retained = held.clone();
            let fired = Rc::new(Cell::new(false));
            let seen = fired.clone();
            let mut hook = events(move |phase, path| {
                if matches!(phase, Event::VerifyEnumeration) && path == original && !seen.get() {
                    fs::rename(&original, &held).unwrap();
                    symlink(&matching, &original).unwrap();
                    seen.set(true);
                }
            });
            let error = export_with_events("codex", &output, dry, &mut hook).unwrap_err();
            assert_changed(&error);
            assert!(fired.get());
            assert!(!error.details["partial_export"].as_bool().unwrap_or(false));
            assert_eq!(tree(&outside), before);
            assert_eq!(tree(&retained), original_bytes);
        }
    }
    #[test]
    fn anchored_final_binding_refuses_swapped_complete_package() {
        for dry in [false, true] {
            for suffix in ["", "references", "references/commands.md"] {
                let (_temp, output, outside) = fixture();
                export_skill("codex", &output, false).unwrap();
                let package = output.join(".agents/skills/llm-wiki");
                let original = if suffix.is_empty() {
                    package.clone()
                } else {
                    package.join(suffix)
                };
                let matching = outside.join("matching");
                if original.is_dir() {
                    copy_tree(&original, &matching);
                } else {
                    fs::copy(&original, &matching).unwrap();
                }
                let before = tree(&outside);
                let fired = Rc::new(Cell::new(false));
                let seen = fired.clone();
                let mut hook = events(move |phase, path| {
                    if matches!(phase, Event::Return) && path == package && !seen.get() {
                        fs::rename(&original, original.with_file_name("retained-original"))
                            .unwrap();
                        symlink(&matching, &original).unwrap();
                        seen.set(true);
                    }
                });
                let error = export_with_events("codex", &output, dry, &mut hook).unwrap_err();
                assert_changed(&error);
                assert!(fired.get());
                assert_eq!(tree(&outside), before);
            }
        }
    }
    #[test]
    fn anchored_leaf_symlink_fifo_and_exclusive_collision_refuse() {
        let (_temp, output, outside) = fixture();
        export_skill("codex", &output, false).unwrap();
        let leaf = output.join(".agents/skills/llm-wiki/references/commands.md");
        let outside_leaf = outside.join("commands.md");
        fs::copy(&leaf, &outside_leaf).unwrap();
        fs::remove_file(&leaf).unwrap();
        symlink(&outside_leaf, &leaf).unwrap();
        let before = tree(&outside);
        assert_eq!(
            export_skill("codex", &output, false).unwrap_err().code,
            ErrorCode::ContentConflict
        );
        assert_eq!(tree(&outside), before);
        fs::remove_file(&leaf).unwrap();
        let c = std::ffi::CString::new(leaf.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(
            export_skill("codex", &output, true).unwrap_err().code,
            ErrorCode::ContentConflict
        );
        assert!(fs::symlink_metadata(&leaf).unwrap().file_type().is_fifo());
        assert_eq!(tree(&outside), before);
        let (_temp, output, _outside) = fixture();
        let fired = Rc::new(Cell::new(false));
        let seen = fired.clone();
        let mut hook = events(move |phase, path| {
            if matches!(phase, Event::FileCreate)
                && path.ends_with("references/commands.md")
                && !seen.get()
            {
                fs::write(path, b"Concurrent author-owned bytes").unwrap();
                seen.set(true);
            }
        });
        let error = export_with_events("codex", &output, false, &mut hook).unwrap_err();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert_eq!(error.details["partial_export"], true);
        assert!(fired.get());
        assert_eq!(
            fs::read(output.join(".agents/skills/llm-wiki/references/commands.md")).unwrap(),
            b"Concurrent author-owned bytes"
        );
        assert!(!output.join(".agents/skills/llm-wiki/SKILL.md").exists());
    }
    #[test]
    fn anchored_dry_zero_writes_and_manifest_last() {
        let (temp, output, _outside) = fixture();
        let absent = output.join("planned");
        let before = tree(temp.path());
        let dry = export_skill("cursor", &absent, true).unwrap();
        assert_eq!(dry["complete"], false);
        assert_eq!(tree(temp.path()), before);
        let names = Rc::new(RefCell::new(Vec::new()));
        let seen = names.clone();
        let mut hook = events(move |phase, path| {
            if matches!(phase, Event::FileCreate) {
                seen.borrow_mut()
                    .push(path.file_name().unwrap().to_str().unwrap().to_owned());
            }
        });
        let result = export_with_events("codex", &output, false, &mut hook).unwrap();
        assert_eq!(result["complete"], true);
        let names = names.borrow();
        assert_eq!(&names[names.len() - 2..], &["SKILL.md", "manifest.json"]);
        drop(names);
        let before = tree(temp.path());
        assert_eq!(
            export_skill("codex", &output, false).unwrap()["reused"],
            true
        );
        assert_eq!(tree(temp.path()), before);
    }
}

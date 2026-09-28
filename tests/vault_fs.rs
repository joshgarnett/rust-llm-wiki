use lwiki::{
    domain::{Blake3Hash, ErrorCode, VaultRelativePath},
    vault::{
        BeforeImage, ExpectedState, VaultFs, VaultRoot, WriterPermit,
        fs::{DirectorySync, DurableIo, NativeIo},
    },
};
use std::{
    fs::{self, File},
    io,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

fn rel(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn fixture() -> (tempfile::TempDir, VaultRoot) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), "wiki").unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    (temp, root)
}

#[test]
fn missing_roots_return_vault_not_found() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing");
    for error in [
        VaultRoot::explicit(&missing).unwrap_err(),
        VaultRoot::discover(&missing).unwrap_err(),
    ] {
        assert_eq!(error.code, ErrorCode::VaultNotFound);
        assert_eq!(error.exit_code(), 3);
    }
}

#[test]
fn path_escape_symlink_reserved_and_case_collision() {
    let (_temp, root) = fixture();
    for invalid in [
        "",
        "../out",
        "/abs",
        "a/../../out",
        "a\\b",
        "a\0b",
        "CON.md",
        "com¹.txt",
        "a.",
        "a/",
        "a:N",
    ] {
        assert!(VaultRelativePath::new(invalid).is_err(), "{invalid:?}");
    }
    fs::write(root.path().join("Straße.md"), "old").unwrap();
    assert!(root.validate_portable_paths(&[rel("STRASSE.md")]).is_err());
    fs::write(root.path().join("ქართული.md"), "old").unwrap();
    assert!(root.validate_portable_paths(&[rel("ᲥᲐᲠᲗᲣᲚᲘ.md")]).is_err());
    assert!(
        root.validate_portable_paths(&[rel("new/Foo.md"), rel("new/foo.md")])
            .is_err()
    );
    assert!(
        root.validate_portable_paths(&[rel("Foo/a.md"), rel("foo/b.md")])
            .is_err()
    );
    assert!(
        root.validate_portable_paths(&[rel("new/a.md"), rel("new/b.md")])
            .is_ok()
    );
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        assert!(root.resolve(&rel("escape/out.md")).is_err());
        std::os::unix::fs::symlink(root.path(), outside.path().join("alias")).unwrap();
        assert!(VaultRoot::explicit(outside.path().join("alias")).is_err());
    }
}

#[test]
fn discovery_is_nearest_exact_case_and_explicit() {
    let (_temp, root) = fixture();
    let child = root.path().join("child/deep");
    fs::create_dir_all(&child).unwrap();
    fs::write(root.path().join("child/wiki.md"), "lowercase").unwrap();
    assert_eq!(VaultRoot::discover(&child).unwrap(), root);
    assert_eq!(
        VaultRoot::explicit(root.path().join("child"))
            .unwrap_err()
            .code,
        ErrorCode::VaultNotFound
    );
    // Remove lowercase before creating exact spelling on case-insensitive platforms.
    fs::remove_file(root.path().join("child/wiki.md")).unwrap();
    fs::write(root.path().join("child/WIKI.md"), "nested").unwrap();
    assert_eq!(
        VaultRoot::discover(&child).unwrap().path(),
        root.path().join("child")
    );
    assert_eq!(VaultRoot::explicit(root.path()).unwrap(), root);
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        VaultRoot::discover(empty.path()).unwrap_err().code,
        ErrorCode::VaultNotFound
    );
}

#[test]
fn scan_skips_control_payload_and_symlink_paths() {
    let (_temp, root) = fixture();
    for file in [
        "z.md",
        "A.md",
        "é.md",
        ".wiki/state/fake.md",
        ".git/fake.md",
        "changes/c/before/fake.md",
        "index.md",
        "pages/index.md",
        "sources/s/source.md",
        "sources/s/revisions/r/revision.md",
        "sources/s/revisions/r/content.md",
        "sources/s/revisions/r/original.md",
        "sources/s/revisions/r/original/frontmatter.md",
    ] {
        let path = root.path().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "---\nid: forged\n---").unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path().join("A.md"), root.path().join("alias.md")).unwrap();
    let names: Vec<_> = root
        .scan_markdown()
        .unwrap()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        names,
        [
            "A.md",
            "WIKI.md",
            "sources/s/revisions/r/revision.md",
            "sources/s/source.md",
            "z.md",
            "é.md"
        ]
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_are_explicit_errors() {
    use std::os::unix::ffi::OsStringExt;
    let (_temp, root) = fixture();
    let invalid = root
        .path()
        .join(std::ffi::OsString::from_vec(vec![0xff, b'.', b'm', b'd']));
    assert!(
        VaultRoot::explicit(&invalid)
            .unwrap_err()
            .message
            .contains("non-UTF-8")
    );
    assert!(
        VaultRoot::discover(&invalid)
            .unwrap_err()
            .message
            .contains("non-UTF-8")
    );
    // APFS refuses invalid-UTF-8 entries; entry scanning qualification runs on Unix
    // filesystems that can actually contain these filenames.
    #[cfg(not(target_os = "macos"))]
    {
        fs::write(invalid, "bad").unwrap();
        assert!(
            root.scan_markdown()
                .unwrap_err()
                .message
                .contains("non-UTF-8")
        );
        assert!(
            root.validate_portable_paths(&[rel("good.md")])
                .unwrap_err()
                .message
                .contains("non-UTF-8")
        );
    }
}

// Each failure wraps the very same production NativeIo call used without injection.
#[test]
fn nested_vaults_are_separate_scan_and_managed_path_scopes() {
    let (_temp, root) = fixture();
    for directory in ["nested", "lowercase", "marker-link"] {
        fs::create_dir(root.path().join(directory)).unwrap();
        fs::write(root.path().join(directory).join("page.md"), b"page").unwrap();
    }
    fs::write(root.path().join("nested/WIKI.md"), b"inner").unwrap();
    fs::write(root.path().join("lowercase/wiki.md"), b"lower").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        root.path().join("WIKI.md"),
        root.path().join("marker-link/WIKI.md"),
    )
    .unwrap();
    let names: Vec<_> = root
        .scan_markdown()
        .unwrap()
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        names,
        [
            "WIKI.md",
            "lowercase/page.md",
            "lowercase/wiki.md",
            "marker-link/page.md"
        ]
    );
    assert!(
        root.resolve(&rel("nested/page.md"))
            .unwrap_err()
            .message
            .contains("nested vault")
    );
    assert!(root.resolve(&rel("nested/new.md")).is_err());
    assert!(root.resolve(&rel("lowercase/page.md")).is_ok());
    assert!(root.resolve(&rel("marker-link/page.md")).is_ok());
    let child = VaultRoot::explicit(root.path().join("nested")).unwrap();
    assert_eq!(
        VaultRoot::discover(root.path().join("nested")).unwrap(),
        child
    );
    assert!(child.resolve(&rel("page.md")).is_ok());
    assert_eq!(
        child.scan_markdown().unwrap(),
        [rel("WIKI.md"), rel("page.md")]
    );
}

struct FaultIo {
    fail: &'static str,
    after: bool,
    events: Mutex<Vec<&'static str>>,
}
impl FaultIo {
    fn call<T>(
        &self,
        name: &'static str,
        operation: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        self.events.lock().unwrap().push(name);
        if self.fail == name && !self.after {
            return Err(io::Error::other("injected before I/O"));
        }
        let result = operation()?;
        if self.fail == name && self.after {
            return Err(io::Error::other("injected after I/O"));
        }
        Ok(result)
    }
}
impl DurableIo for FaultIo {
    fn create_stage(&self, p: &Path) -> io::Result<File> {
        self.call("create", || NativeIo.create_stage(p))
    }
    fn open_append(&self, p: &Path) -> io::Result<File> {
        self.call("append", || NativeIo.open_append(p))
    }
    fn truncate_file(&self, f: &File, n: u64) -> io::Result<()> {
        self.call("truncate", || NativeIo.truncate_file(f, n))
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> io::Result<()> {
        self.call("write", || NativeIo.write_stage(f, b))
    }
    fn sync_file(&self, f: &File) -> io::Result<()> {
        self.call("fsync", || NativeIo.sync_file(f))
    }
    fn replace(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.call("rename", || NativeIo.replace(a, b))
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        self.call("remove", || NativeIo.remove(p))
    }
    fn create_directory(&self, p: &Path) -> io::Result<()> {
        self.call("mkdir", || NativeIo.create_directory(p))
    }
    fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
        self.call("dirsync", || NativeIo.sync_directory(p))
    }
}

#[test]
fn interrupted_directory_creation_retry_resyncs_every_directory_and_parent() {
    for (fail, after) in [("mkdir", true), ("dirsync", false), ("dirsync", true)] {
        let (_temp, root) = fixture();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let interrupted = Arc::new(FaultIo {
            fail,
            after,
            events: Mutex::new(Vec::new()),
        });
        let vault = VaultFs::with_io(root.clone(), interrupted.clone());
        let directory = rel("interrupted/deep");
        assert!(vault.ensure_directory(&directory, &permit).is_err());
        assert!(root.path().join("interrupted").is_dir());
        let retry = Arc::new(FaultIo {
            fail: "",
            after: false,
            events: Mutex::new(Vec::new()),
        });
        let vault = VaultFs::with_io(root, retry.clone());
        let result = vault.ensure_directory(&directory, &permit).unwrap();
        #[cfg(unix)]
        assert_eq!(result, DirectorySync::Supported);
        #[cfg(not(unix))]
        assert_eq!(result, DirectorySync::Unsupported);
        // Both calls perform actual NativeIo syncs, including the existing ancestor.
        assert_eq!(
            retry
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| **event == "dirsync")
                .count(),
            4
        );
        retry.events.lock().unwrap().clear();
        assert_eq!(vault.ensure_directory(&directory, &permit).unwrap(), result);
        assert_eq!(
            retry
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| **event == "dirsync")
                .count(),
            4
        );
    }
}

#[cfg(unix)]
#[test]
fn staged_same_byte_symlink_cannot_be_published() {
    let (_temp, root) = fixture();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let vault = VaultFs::new(root.clone());
    let target = rel("page.md");
    let staged = vault.stage(&target, b"same bytes", &permit).unwrap();
    fs::write(root.path().join("payload.bin"), b"same bytes").unwrap();
    fs::remove_file(staged.path()).unwrap();
    std::os::unix::fs::symlink(root.path().join("payload.bin"), staged.path()).unwrap();
    assert_eq!(
        vault
            .replace(staged, &ExpectedState::Absent, &permit)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert!(!root.path().join("page.md").exists());
    assert_eq!(
        fs::read(root.path().join("payload.bin")).unwrap(),
        b"same bytes"
    );
}

#[test]
fn same_directory_replace_and_sync_failures() {
    for fail in ["create", "write", "fsync", "rename", "dirsync"] {
        for after in [false, true] {
            let (_temp, root) = fixture();
            let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
            let target = rel("page.md");
            fs::write(root.path().join(target.as_str()), "old").unwrap();
            let fault = Arc::new(FaultIo {
                fail,
                after,
                events: Mutex::new(Vec::new()),
            });
            let vault = VaultFs::with_io(root.clone(), fault.clone());
            let before = vault.read_before(&target).unwrap().unwrap();
            assert_eq!(
                before,
                BeforeImage {
                    bytes: b"old".to_vec(),
                    hash: Blake3Hash::digest(b"old")
                }
            );
            let staged = vault.stage(&target, b"new", &permit);
            if let Ok(staged) = &staged {
                assert_eq!(staged.path().parent().unwrap(), root.path());
            }
            let result = staged.and_then(|staged| {
                vault.replace(staged, &ExpectedState::Hash(before.hash), &permit)
            });
            assert!(result.is_err(), "{fail} {after}");
            let applied = fail == "dirsync" || (fail == "rename" && after);
            assert_eq!(
                fs::read(root.path().join("page.md")).unwrap(),
                if applied { b"new" } else { b"old" }
            );
            assert!(fault.events.lock().unwrap().contains(&fail));
            assert!(!fs::read_dir(root.path()).unwrap().any(|e| {
                e.unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .starts_with(".lwiki-stage-")
            }));
        }
    }
    for fail in ["remove", "dirsync"] {
        for after in [false, true] {
            let (_temp, root) = fixture();
            let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
            fs::write(root.path().join("delete.md"), b"old").unwrap();
            let fault = Arc::new(FaultIo {
                fail,
                after,
                events: Mutex::new(Vec::new()),
            });
            let vault = VaultFs::with_io(root.clone(), fault.clone());
            assert!(
                vault
                    .delete(
                        &rel("delete.md"),
                        &ExpectedState::Hash(Blake3Hash::digest(b"old")),
                        &permit
                    )
                    .is_err()
            );
            assert_eq!(
                root.path().join("delete.md").exists(),
                fail == "remove" && !after
            );
            assert!(fault.events.lock().unwrap().contains(&fail));
        }
    }
    let (_temp, root) = fixture();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let vault = VaultFs::new(root.clone());
    let target = rel("page.md");
    let stage = vault.stage(&target, b"new", &permit).unwrap();
    #[cfg(unix)]
    assert_eq!(
        vault
            .replace(stage, &ExpectedState::Absent, &permit)
            .unwrap(),
        DirectorySync::Supported
    );
    #[cfg(not(unix))]
    assert_eq!(
        vault
            .replace(stage, &ExpectedState::Absent, &permit)
            .unwrap(),
        DirectorySync::Unsupported
    );
    let stage = vault.stage(&target, b"replacement", &permit).unwrap();
    fs::write(root.path().join("page.md"), "external edit").unwrap();
    assert_eq!(
        vault
            .replace(
                stage,
                &ExpectedState::Hash(Blake3Hash::digest(b"new")),
                &permit
            )
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        vault
            .delete(
                &target,
                &ExpectedState::Hash(Blake3Hash::digest(b"new")),
                &permit
            )
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        fs::read(root.path().join("page.md")).unwrap(),
        b"external edit"
    );
    vault
        .delete(
            &target,
            &ExpectedState::Hash(Blake3Hash::digest(b"external edit")),
            &permit,
        )
        .unwrap();
    assert!(vault.read_before(&target).unwrap().is_none());
    let stage = vault.stage(&target, b"new", &permit).unwrap();
    fs::write(root.path().join("page.md"), "intruder").unwrap();
    assert_eq!(
        vault
            .replace(stage, &ExpectedState::Absent, &permit)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    let stage = vault.stage(&target, b"new", &permit).unwrap();
    fs::write(stage.path(), "tampered").unwrap();
    assert_eq!(
        vault
            .replace(
                stage,
                &ExpectedState::Hash(Blake3Hash::digest(b"intruder")),
                &permit
            )
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn operational_append_truncate_and_recovery_sync_are_real_boundaries() {
    let (_temp, root) = fixture();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let vault = VaultFs::new(root.clone());
    vault
        .ensure_directory(&rel(".wiki/state/changes"), &permit)
        .unwrap();
    let journal = rel(".wiki/state/changes/c.journal");
    vault
        .append_synced(&journal, b"full frame;partial", &permit)
        .unwrap();
    vault.truncate_synced(&journal, 11, &permit).unwrap();
    vault.append_synced(&journal, b"next", &permit).unwrap();
    assert_eq!(
        vault.read_before(&journal).unwrap().unwrap().bytes,
        b"full frame;next"
    );
    assert!(vault.truncate_synced(&journal, 1000, &permit).is_err());
    assert!(
        vault
            .append_synced(&rel("page.md"), b"bypass", &permit)
            .is_err()
    );
    for fail in ["append", "write", "fsync", "dirsync", "truncate", "mkdir"] {
        for after in [false, true] {
            let fault = Arc::new(FaultIo {
                fail,
                after,
                events: Mutex::new(Vec::new()),
            });
            let injected = VaultFs::with_io(root.clone(), fault.clone());
            let result = if fail == "truncate" {
                injected
                    .truncate_synced(&journal, 11, &permit)
                    .map(|()| DirectorySync::Supported)
            } else if fail == "mkdir" {
                injected.ensure_directory(&rel(&format!("folder-{after}")), &permit)
            } else {
                injected.append_synced(&journal, b"partial", &permit)
            };
            assert!(result.is_err(), "{fail} {after}");
            assert!(fault.events.lock().unwrap().contains(&fail));
        }
    }
    vault.sync_target(&journal, &permit).unwrap();
    vault.sync_target(&rel("deleted.md"), &permit).unwrap();
    let (_other, other_root) = fixture();
    let foreign = WriterPermit::acquire(&other_root, Duration::ZERO).unwrap();
    assert!(vault.stage(&rel("page.md"), b"wrong", &foreign).is_err());
}

#[test]
fn lock_subprocess_worker() {
    let Some(path) = std::env::var_os("LWIKI_P02_LOCK_CHILD") else {
        return;
    };
    let root = VaultRoot::explicit(path).unwrap();
    let _permit = WriterPermit::acquire(&root, Duration::from_secs(2)).unwrap();
    fs::write(root.path().join("child-held"), b"held").unwrap();
    thread::sleep(Duration::from_millis(400));
}

#[test]
fn cooperating_writers_serialize() {
    let (_temp, root) = fixture();
    // A stale/untrue PID has no locking authority.
    fs::create_dir_all(root.path().join(".wiki/state")).unwrap();
    fs::write(root.path().join(".wiki/state/writer.lock"), "pid=1").unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    assert!(WriterPermit::acquire(&root, Duration::ZERO).is_err());
    drop(permit);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_subprocess_worker", "--nocapture"])
        .env("LWIKI_P02_LOCK_CHILD", root.path())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !root.path().join("child-held").exists() {
        assert!(Instant::now() < deadline, "child failed to acquire lock");
        thread::sleep(Duration::from_millis(5));
    }
    let started = Instant::now();
    let error = WriterPermit::acquire(&root, Duration::from_millis(30)).unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(error.retryable);
    assert!(started.elapsed() >= Duration::from_millis(30));
    let permit = WriterPermit::acquire(&root, Duration::from_secs(2)).unwrap();
    assert!(child.wait().unwrap().success());
    drop(permit);
    fs::remove_file(root.path().join("child-held")).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_subprocess_worker", "--nocapture"])
        .env("LWIKI_P02_LOCK_CHILD", root.path())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !root.path().join("child-held").exists() {
        assert!(
            Instant::now() < deadline,
            "second child failed to acquire lock"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(WriterPermit::acquire(&root, Duration::ZERO).is_err());
    child.kill().unwrap();
    child.wait().unwrap();
    // OS authority is released on abrupt exit even though PID text remains.
    let _permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
}

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
struct FaultIo {
    native: NativeIo,
    rename: AtomicBool,
    file: AtomicBool,
    parent: std::sync::Mutex<Option<PathBuf>>,
    capability: bool,
    events: std::sync::Mutex<Vec<String>>,
    fail_file_at: std::sync::Mutex<Option<usize>>,
    edit_on_remove: std::sync::Mutex<Option<PathBuf>>,
    grow_on_sync: std::sync::Mutex<Option<PathBuf>>,
    swap: AtomicBool,
    stage_directory: AtomicBool,
    created: std::sync::Mutex<Option<PathBuf>>,
}
impl FaultIo {
    fn new() -> Self {
        Self {
            native: NativeIo,
            rename: AtomicBool::new(false),
            file: AtomicBool::new(false),
            parent: std::sync::Mutex::new(None),
            capability: true,
            events: std::sync::Mutex::new(Vec::new()),
            fail_file_at: std::sync::Mutex::new(None),
            edit_on_remove: std::sync::Mutex::new(None),
            grow_on_sync: std::sync::Mutex::new(None),
            swap: AtomicBool::new(false),
            stage_directory: AtomicBool::new(false),
            created: std::sync::Mutex::new(None),
        }
    }
}
impl DurableIo for FaultIo {
    fn create_stage(&self, p: &Path) -> std::io::Result<File> {
        self.native.create_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> std::io::Result<File> {
        let file = self.native.create_private_stage(p)?;
        *self.created.lock().unwrap() = Some(p.to_path_buf());
        Ok(file)
    }
    fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
        self.native.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> std::io::Result<File> {
        self.native.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> std::io::Result<()> {
        self.native.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> std::io::Result<()> {
        self.native.write_stage(f, b)?;
        if self.swap.swap(false, Ordering::SeqCst) {
            let path = self.created.lock().unwrap().clone().unwrap();
            fs::rename(&path, path.with_extension("owned-saved"))?;
            fs::write(&path, b)?;
        }
        Ok(())
    }
    fn sync_file(&self, f: &File) -> std::io::Result<()> {
        let count = {
            let mut events = self.events.lock().unwrap();
            events.push("file".to_owned());
            events.iter().filter(|e| e.as_str() == "file").count()
        };
        if *self.fail_file_at.lock().unwrap() == Some(count) {
            return Err(std::io::ErrorKind::Other.into());
        }
        if let Some(path) = self.grow_on_sync.lock().unwrap().take() {
            OpenOptions::new()
                .write(true)
                .open(path)?
                .set_len(MAX_BYTES as u64 + 1)?;
        }
        if self.file.load(Ordering::SeqCst) {
            Err(std::io::ErrorKind::Other.into())
        } else {
            self.native.sync_file(f)
        }
    }
    fn replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        self.native.replace(a, b)
    }
    fn supports_directory_install(&self) -> bool {
        self.capability && native_directory_install_supported()
    }
    fn install_directory_noreplace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        self.events.lock().unwrap().push("rename".to_owned());
        if self.rename.load(Ordering::SeqCst) {
            Err(std::io::ErrorKind::Other.into())
        } else {
            self.native.install_directory_noreplace(a, b)
        }
    }
    fn remove(&self, p: &Path) -> std::io::Result<()> {
        self.native.remove(p)?;
        if let Some(path) = self.edit_on_remove.lock().unwrap().take() {
            fs::write(path, b"external in-place bytes")?;
        }
        Ok(())
    }
    fn create_directory(&self, p: &Path) -> std::io::Result<()> {
        self.native.create_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> std::io::Result<DirectorySync> {
        self.events
            .lock()
            .unwrap()
            .push(format!("dir:{}", p.display()));
        if self.parent.lock().unwrap().as_deref() == Some(p)
            || (self.stage_directory.load(Ordering::SeqCst)
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("tree-")))
        {
            Err(std::io::ErrorKind::Other.into())
        } else {
            self.native.sync_directory(p)
        }
    }
}
fn fixture(
    io: Arc<dyn DurableIo>,
) -> (
    tempfile::TempDir,
    VaultFs,
    WriterPermit,
    VaultRelativePath,
    Vec<(VaultRelativePath, Vec<u8>)>,
) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
    let vault = VaultFs::with_io(root, io);
    let target = VaultRelativePath::new("sources/source1/revisions/revision1").unwrap();
    let members = [
        ("content.md", b"normalized".to_vec()),
        ("original.bin", b"original".to_vec()),
        ("revision.md", b"record".to_vec()),
    ]
    .into_iter()
    .map(|(n, b)| {
        (
            VaultRelativePath::new(format!("{}/{n}", target.as_str())).unwrap(),
            b,
        )
    })
    .collect();
    (temp, vault, permit, target, members)
}
#[test]
fn complete_install_and_recovery_preserve_bytes() {
    if !native_directory_install_supported() {
        return;
    }
    let (_temp, v, p, t, m) = fixture(Arc::new(NativeIo));
    let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
    let private = stage.path.clone();
    assert!(private.parent().unwrap().ends_with(".wiki"));
    assert!(!v.root.resolve(&t).unwrap().exists());
    v.install_revision_tree(stage, &p).unwrap();
    assert!(!private.exists());
    for (path, bytes) in &m {
        assert_eq!(fs::read(v.root.resolve(path).unwrap()).unwrap(), *bytes);
    }
    let expected = m
        .iter()
        .map(|(p, b)| (p.clone(), Blake3Hash::digest(b)))
        .collect::<Vec<_>>();
    v.sync_revision_tree(&t, &expected, &p).unwrap();
    fs::write(v.root.resolve(&m[0].0).unwrap(), b"changed").unwrap();
    assert!(v.sync_revision_tree(&t, &expected, &p).is_err());
}
#[test]
fn destination_empty_directory_is_never_replaced() {
    if !native_directory_install_supported() {
        return;
    }
    let (_temp, v, p, t, m) = fixture(Arc::new(NativeIo));
    let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
    fs::create_dir(v.root.resolve(&t).unwrap()).unwrap();
    assert!(v.install_revision_tree(stage, &p).is_err());
    assert_eq!(
        fs::read_dir(v.root.resolve(&t).unwrap()).unwrap().count(),
        0
    );
    assert!(v.stage_revision_tree(&t, &m, &p).is_err());
}
#[test]
fn tampered_stages_and_unfamiliar_entries_are_preserved() {
    if !native_directory_install_supported() {
        return;
    }
    for extra in [false, true] {
        let (_temp, v, p, t, m) = fixture(Arc::new(NativeIo));
        let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
        let path = stage.path.clone();
        let unfamiliar = path.join(if extra { "unfamiliar" } else { "content.md" });
        fs::write(&unfamiliar, b"external bytes").unwrap();
        assert!(v.install_revision_tree(stage, &p).is_err());
        assert_eq!(fs::read(unfamiliar).unwrap(), b"external bytes");
    }
}
#[cfg(unix)]
#[test]
fn symlinks_hardlinks_and_root_swaps_preserve_external_state() {
    if !native_directory_install_supported() {
        return;
    }
    for mode in 0..4 {
        let (_temp, v, p, t, m) = fixture(Arc::new(NativeIo));
        let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
        let path = stage.path.clone();
        match mode {
            0 => {
                fs::remove_file(path.join("content.md")).unwrap();
                std::os::unix::fs::symlink("original.bin", path.join("content.md")).unwrap();
            }
            1 => {
                fs::hard_link(
                    path.join("content.md"),
                    path.parent().unwrap().join("foreign-link"),
                )
                .unwrap();
            }
            2 => {
                fs::rename(&path, path.with_extension("saved")).unwrap();
                fs::create_dir(&path).unwrap();
                fs::write(path.join("external"), b"external").unwrap();
            }
            _ => {
                let parent = path.parent().unwrap();
                fs::rename(parent, parent.with_extension("saved")).unwrap();
                fs::create_dir(parent).unwrap();
                fs::create_dir(&path).unwrap();
                fs::write(path.join("external"), b"external").unwrap();
            }
        }
        assert!(v.install_revision_tree(stage, &p).is_err());
        assert!(path.exists());
    }
}
#[test]
fn injected_failures_keep_canonical_after_rename_and_allow_recovery() {
    if !native_directory_install_supported() {
        return;
    }
    for source_parent in [false, true] {
        let io = Arc::new(FaultIo::new());
        let (_temp, v, p, t, m) = fixture(io.clone());
        let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
        let parent = if source_parent {
            stage.path.parent().unwrap().to_path_buf()
        } else {
            v.root.resolve(&t).unwrap().parent().unwrap().to_path_buf()
        };
        *io.parent.lock().unwrap() = Some(parent);
        assert!(v.install_revision_tree(stage, &p).is_err());
        for (path, bytes) in &m {
            assert_eq!(fs::read(v.root.resolve(path).unwrap()).unwrap(), *bytes);
        }
        *io.parent.lock().unwrap() = None;
        v.sync_revision_tree(
            &t,
            &m.iter()
                .map(|(p, b)| (p.clone(), Blake3Hash::digest(b)))
                .collect::<Vec<_>>(),
            &p,
        )
        .unwrap();
    }
    let io = Arc::new(FaultIo::new());
    let (_temp, v, p, t, m) = fixture(io.clone());
    let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
    io.rename.store(true, Ordering::SeqCst);
    assert!(v.install_revision_tree(stage, &p).is_err());
    assert!(!v.root.resolve(&t).unwrap().exists());
    io.rename.store(false, Ordering::SeqCst);
    io.file.store(true, Ordering::SeqCst);
    assert!(v.stage_revision_tree(&t, &m, &p).is_err());
    assert!(!v.root.resolve(&t).unwrap().exists());
}
#[test]
fn unsupported_and_malformed_requests_allocate_no_stage() {
    let mut io = FaultIo::new();
    io.capability = false;
    let (_temp, v, p, t, m) = fixture(Arc::new(io));
    assert_eq!(
        v.stage_revision_tree(&t, &m, &p).err().unwrap().code,
        ErrorCode::CapabilityUnavailable
    );
    assert!(!v.root.path().join("sources").exists());
    if native_directory_install_supported() {
        let (_temp, v, p, t, m) = fixture(Arc::new(NativeIo));
        assert!(v.stage_revision_tree(&t, &m[..2], &p).is_err());
        assert!(!v.root.path().join("sources").exists());
    }
}
#[test]
fn stage_sync_failure_and_unfamiliar_leftovers_do_not_block_retry() {
    if !native_directory_install_supported() {
        return;
    }
    let io = Arc::new(FaultIo::new());
    let (_temp, v, p, t, m) = fixture(io.clone());
    let parent = VaultRelativePath::new("sources/source1/revisions").unwrap();
    v.ensure_directory(&parent, &p).unwrap();
    let container = v.root.resolve(&parent).unwrap().join(".wiki");
    fs::create_dir(&container).unwrap();
    fs::create_dir(container.join("old-partial")).unwrap();
    fs::write(container.join("old-partial/unfamiliar"), b"preserve").unwrap();
    *io.parent.lock().unwrap() = Some(container.clone());
    assert!(v.stage_revision_tree(&t, &m, &p).is_err());
    *io.parent.lock().unwrap() = None;
    let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
    let path = stage.path.clone();
    drop(stage);
    assert!(!path.exists());
    assert_eq!(
        fs::read(container.join("old-partial/unfamiliar")).unwrap(),
        b"preserve"
    );
    let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
    v.install_revision_tree(stage, &p).unwrap();
}
#[cfg(unix)]
#[test]
fn stage_container_symlink_type_and_case_alias_are_refused() {
    if !native_directory_install_supported() {
        return;
    }
    for mode in 0..3 {
        let (_temp, v, p, t, m) = fixture(Arc::new(NativeIo));
        let parent = VaultRelativePath::new("sources/source1/revisions").unwrap();
        v.ensure_directory(&parent, &p).unwrap();
        let parent = v.root.resolve(&parent).unwrap();
        match mode {
            0 => {
                std::os::unix::fs::symlink(v.root.path(), parent.join(".wiki")).unwrap();
            }
            1 => fs::write(parent.join(".wiki"), b"preserve").unwrap(),
            _ => fs::create_dir(parent.join(".WIKI")).unwrap(),
        }
        assert!(v.stage_revision_tree(&t, &m, &p).is_err());
        assert!(
            parent
                .join(if mode == 2 { ".WIKI" } else { ".wiki" })
                .exists()
        );
    }
}

#[test]
fn same_bytes_path_swap_during_write_is_not_adopted_or_deleted() {
    if !native_directory_install_supported() {
        return;
    }
    let io = Arc::new(FaultIo::new());
    let (_temp, v, p, t, m) = fixture(io.clone());
    io.swap.store(true, Ordering::SeqCst);
    assert!(v.stage_revision_tree(&t, &m, &p).is_err());
    let path = io.created.lock().unwrap().clone().unwrap();
    assert_eq!(fs::read(&path).unwrap(), m[0].1);
    assert!(path.with_extension("owned-saved").exists());
    assert!(!v.root.resolve(&t).unwrap().exists());
}

#[test]
fn staged_directory_sync_failure_never_publishes_partial_root() {
    if !native_directory_install_supported() {
        return;
    }
    let io = Arc::new(FaultIo::new());
    let (_temp, v, p, t, m) = fixture(io.clone());
    io.stage_directory.store(true, Ordering::SeqCst);
    assert!(v.stage_revision_tree(&t, &m, &p).is_err());
    assert!(!v.root.resolve(&t).unwrap().exists());
    io.stage_directory.store(false, Ordering::SeqCst);
    let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
    v.install_revision_tree(stage, &p).unwrap();
}

#[test]
fn restored_stage_requires_fresh_file_root_and_both_parent_syncs() {
    if !native_directory_install_supported() {
        return;
    }
    let io = Arc::new(FaultIo::new());
    let (_temp, v, p, t, m) = fixture(io.clone());
    let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
    let path = stage.path.clone();
    let member = path.join("content.md");
    let original_identity = identity(&member, false).unwrap();
    fs::write(&member, b"different").unwrap();
    File::open(&member).unwrap().sync_all().unwrap();
    fs::write(&member, &m[0].1).unwrap();
    let away = path.parent().unwrap().join("temporarily-away");
    fs::rename(&member, &away).unwrap();
    fs::rename(&away, &member).unwrap();
    assert!(identity(&member, false).unwrap() == original_identity);
    io.events.lock().unwrap().clear();
    let destination = v.root.resolve(&t).unwrap();
    v.install_revision_tree(stage, &p).unwrap();
    let events = io.events.lock().unwrap();
    assert_eq!(
        events.iter().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "file",
            "file",
            "file",
            &format!("dir:{}", path.display()),
            "rename",
            "file",
            "file",
            "file",
            &format!("dir:{}", destination.display()),
            &format!("dir:{}", path.parent().unwrap().display()),
            &format!("dir:{}", destination.parent().unwrap().display())
        ]
    );
}
#[test]
fn refreshed_install_sync_failures_do_not_report_success() {
    if !native_directory_install_supported() {
        return;
    }
    for directory in [false, true] {
        let io = Arc::new(FaultIo::new());
        let (_temp, v, p, t, m) = fixture(io.clone());
        let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
        if directory {
            *io.parent.lock().unwrap() = Some(stage.path.clone());
        } else {
            io.file.store(true, Ordering::SeqCst);
        }
        assert!(v.install_revision_tree(stage, &p).is_err());
        assert!(!v.root.resolve(&t).unwrap().exists());
        assert!(!io.events.lock().unwrap().iter().any(|e| e == "rename"));
    }
}
#[test]
fn drop_preserves_later_member_edited_by_first_unlink() {
    if !native_directory_install_supported() {
        return;
    }
    let io = Arc::new(FaultIo::new());
    let (_temp, v, p, t, m) = fixture(io.clone());
    let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
    let path = stage.path.clone();
    let later = path.join("original.bin");
    let before = identity(&later, false).unwrap();
    *io.edit_on_remove.lock().unwrap() = Some(later.clone());
    drop(stage);
    assert!(path.exists());
    assert!(!path.join("content.md").exists());
    assert_eq!(fs::read(&later).unwrap(), b"external in-place bytes");
    assert!(identity(&later, false).unwrap() == before);
    assert_eq!(fs::read(path.join("revision.md")).unwrap(), m[2].1);
}
#[test]
fn recovery_frozen_lengths_reject_growth_after_admission() {
    if !native_directory_install_supported() {
        return;
    }
    let io = Arc::new(FaultIo::new());
    let (_temp, v, p, t, m) = fixture(io.clone());
    let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
    v.install_revision_tree(stage, &p).unwrap();
    let expected = m
        .iter()
        .map(|(p, b)| (p.clone(), Blake3Hash::digest(b)))
        .collect::<Vec<_>>();
    // First member sync occurs after all metadata admission. Grow the next
    // member at that deterministic DurableIo boundary: its frozen read limit
    // must reject growth before that member can be synced or read at new size.
    *io.grow_on_sync.lock().unwrap() = Some(v.root.resolve(&m[1].0).unwrap());
    io.events.lock().unwrap().clear();
    assert_eq!(
        v.sync_revision_tree(&t, &expected, &p).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert_eq!(*io.events.lock().unwrap(), vec!["file".to_owned()]);
    assert_eq!(
        v.sync_revision_tree(&t, &expected, &p).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
}
#[test]
fn conservative_workspace_bound_checks_arithmetic_overflow() {
    assert!(workspace_bound(usize::MAX, 1).is_none());
    assert!(workspace_bound(0, usize::MAX / 128).is_none());
    assert!(workspace_bound(0, 0).unwrap() >= 88 * 1024);
}

#[test]
fn every_refreshed_file_sync_failure_preserves_postrename_tree_for_recovery() {
    if !native_directory_install_supported() {
        return;
    }
    for cut in 1..=7 {
        let io = Arc::new(FaultIo::new());
        let (_temp, v, p, t, m) = fixture(io.clone());
        let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
        io.events.lock().unwrap().clear();
        if cut <= 6 {
            *io.fail_file_at.lock().unwrap() = Some(cut);
        } else {
            *io.parent.lock().unwrap() = Some(v.root.resolve(&t).unwrap());
        }
        assert!(v.install_revision_tree(stage, &p).is_err());
        let canonical = v.root.resolve(&t).unwrap();
        assert_eq!(canonical.exists(), cut >= 4);
        if cut >= 4 {
            for (path, bytes) in &m {
                assert_eq!(fs::read(v.root.resolve(path).unwrap()).unwrap(), *bytes);
            }
            *io.fail_file_at.lock().unwrap() = None;
            *io.parent.lock().unwrap() = None;
            let expected = m
                .iter()
                .map(|(p, b)| (p.clone(), Blake3Hash::digest(b)))
                .collect::<Vec<_>>();
            v.sync_revision_tree(&t, &expected, &p).unwrap();
        }
    }
}
#[cfg(unix)]
#[test]
fn recovery_rejects_symlink_and_fifo_without_reading_them() {
    if !native_directory_install_supported() {
        return;
    }
    use std::os::unix::ffi::OsStrExt;
    for fifo in [false, true] {
        let io = Arc::new(FaultIo::new());
        let (_temp, v, p, t, m) = fixture(io.clone());
        let stage = v.stage_revision_tree(&t, &m, &p).unwrap();
        v.install_revision_tree(stage, &p).unwrap();
        let member = v.root.resolve(&m[1].0).unwrap();
        fs::remove_file(&member).unwrap();
        if fifo {
            let name = std::ffi::CString::new(member.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        } else {
            std::os::unix::fs::symlink("content.md", &member).unwrap();
        }
        io.events.lock().unwrap().clear();
        let expected = m
            .iter()
            .map(|(p, b)| (p.clone(), Blake3Hash::digest(b)))
            .collect::<Vec<_>>();
        assert!(v.sync_revision_tree(&t, &expected, &p).is_err());
        assert!(io.events.lock().unwrap().is_empty());
        assert!(fs::symlink_metadata(member).is_ok());
    }
}

#[test]
fn pure_input_admission_refuses_capacity_overflow_and_allocates_no_stage() {
    let (_temp, v, _p, t, m) = fixture(Arc::new(NativeIo));
    v.admit_revision_tree_input(
        &t,
        m.iter().map(|(p, b)| (p.owned_capacity(), b.capacity())),
    )
    .unwrap();
    for capacities in [
        vec![(0, 0); 2],
        vec![(0, 0); 4],
        vec![(0, MAX_BYTES); 3],
        vec![(usize::MAX, 1); 3],
    ] {
        assert_eq!(
            v.admit_revision_tree_input(&t, capacities.into_iter())
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
    }
    let workspace = workspace_bound(v.root.owned_capacity(), t.owned_capacity()).unwrap();
    v.admit_revision_tree_input(&t, [(0, MAX_BYTES - workspace), (0, 0), (0, 0)].into_iter())
        .unwrap();
    assert!(
        v.admit_revision_tree_input(
            &t,
            [(0, MAX_BYTES - workspace + 1), (0, 0), (0, 0)].into_iter()
        )
        .is_err()
    );
    assert!(!v.root.path().join("sources").exists());
}

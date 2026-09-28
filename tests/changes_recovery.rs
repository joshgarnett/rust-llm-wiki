use lwiki::{
    changes::*,
    domain::{Blake3Hash, ErrorCode, ReadSnapshot, Result, VaultRelativePath, WikiError},
    vault::{
        ExpectedState, VaultFs, VaultRoot, WriterPermit,
        fs::{DirectorySync, DurableIo, NativeIo},
    },
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn rel(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn fixture() -> (tempfile::TempDir, VaultRoot) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    fs::write(temp.path().join("alpha.md"), b"old alpha").unwrap();
    fs::write(temp.path().join("delete.md"), b"old delete").unwrap();
    fs::write(temp.path().join("unrelated.md"), b"context").unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    (temp, root)
}
fn draft() -> ChangeDraft {
    ChangeDraft {
        title: "fixture change".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        operations: vec![
            ExpectedWrite {
                target: rel("alpha.md"),
                expected: ExpectedState::Hash(Blake3Hash::digest(b"old alpha")),
                proposed: Some(b"new alpha".to_vec()),
                apply_after: vec![],
            },
            ExpectedWrite {
                target: rel("asset.bin"),
                expected: ExpectedState::Absent,
                proposed: Some(vec![0, 255, 1, 128]),
                apply_after: vec![],
            },
            ExpectedWrite {
                target: rel("delete.md"),
                expected: ExpectedState::Hash(Blake3Hash::digest(b"old delete")),
                proposed: None,
                apply_after: vec![rel("alpha.md")],
            },
            ExpectedWrite {
                target: rel("new.md"),
                expected: ExpectedState::Absent,
                proposed: Some(b"new note".to_vec()),
                apply_after: vec![],
            },
        ],
    }
}
struct Validator;
impl GraphValidator for Validator {
    fn validate(&self, _fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph> {
        assert!(
            input
                .documents
                .iter()
                .any(|d| d.path == rel("unrelated.md"))
        );
        let mut effective: BTreeMap<_, _> = input
            .documents
            .iter()
            .map(|d| (d.path.clone(), d.bytes.clone()))
            .collect();
        for target in &input.overlay {
            match &target.bytes {
                Some(bytes) => {
                    effective.insert(target.path.clone(), bytes.clone());
                }
                None => {
                    effective.remove(&target.path);
                }
            }
        }
        let mut hash = Vec::new();
        for (path, bytes) in effective {
            hash.extend(path.as_str().as_bytes());
            hash.extend(Blake3Hash::digest(&bytes).as_str().as_bytes());
        }
        Ok(ValidatedGraph {
            parser_fingerprint: Blake3Hash::digest(b"test-parser-v1"),
            control_manifest: Blake3Hash::digest(hash),
            dependencies: vec![],
        })
    }
}
#[derive(Default)]
struct Publisher {
    calls: AtomicUsize,
    unavailable: AtomicBool,
    mismatch: AtomicBool,
}
impl PublicationBackend for Publisher {
    fn check_available(&self) -> Result<()> {
        if self.unavailable.load(Ordering::SeqCst) {
            Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "mock catalog unavailable",
            ))
        } else {
            Ok(())
        }
    }
    fn publish(
        &self,
        fs: &VaultFs,
        permit: &PublicationPermit<'_>,
        _input: &ValidationInput,
    ) -> Result<ReadSnapshot> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let target = rel(".wiki/state/mock-catalog.json");
        let expected = fs
            .read_before(&target)?
            .map_or(ExpectedState::Absent, |b| ExpectedState::Hash(b.hash));
        let stage = fs.stage(&target, b"mock-generation", permit.writer())?;
        assert_eq!(
            fs.replace(stage, &expected, permit.writer())?,
            DirectorySync::Supported
        );
        Ok(ReadSnapshot {
            generation: self.calls.load(Ordering::SeqCst) as u64,
            parser_fingerprint: permit.graph().parser_fingerprint.clone(),
            control_manifest: if self.mismatch.load(Ordering::SeqCst) {
                Blake3Hash::digest(b"wrong")
            } else {
                permit.graph().control_manifest.clone()
            },
        })
    }
}
struct FaultIo {
    events: Mutex<Vec<String>>,
    boundary: Option<usize>,
    after: bool,
    fired: AtomicBool,
    pause: Option<PathBuf>,
}
impl FaultIo {
    fn new(boundary: Option<usize>, after: bool) -> Self {
        Self {
            events: Mutex::new(vec![]),
            boundary,
            after,
            fired: AtomicBool::new(false),
            pause: None,
        }
    }
    fn call<T>(&self, name: &str, op: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let index = {
            let mut events = self.events.lock().unwrap();
            let index = events.len();
            events.push(name.to_owned());
            index
        };
        let fail = self.boundary == Some(index) && !self.fired.swap(true, Ordering::SeqCst);
        if fail && !self.after {
            return self.interrupt();
        }
        let result = op();
        if fail && self.after {
            return self.interrupt();
        }
        result
    }
    fn interrupt<T>(&self) -> io::Result<T> {
        if let Some(marker) = &self.pause {
            fs::write(marker, b"boundary").unwrap();
            loop {
                thread::park_timeout(Duration::from_secs(1));
            }
        }
        Err(io::Error::other("injected production durability boundary"))
    }
}
impl DurableIo for FaultIo {
    fn create_stage(&self, p: &Path) -> io::Result<File> {
        self.call("create_stage", || NativeIo.create_stage(p))
    }
    fn open_append(&self, p: &Path) -> io::Result<File> {
        self.call("open_append", || NativeIo.open_append(p))
    }
    fn truncate_file(&self, f: &File, n: u64) -> io::Result<()> {
        self.call("truncate", || NativeIo.truncate_file(f, n))
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> io::Result<()> {
        self.call("write", || NativeIo.write_stage(f, b))
    }
    fn sync_file(&self, f: &File) -> io::Result<()> {
        self.call("sync_file", || NativeIo.sync_file(f))
    }
    fn replace(&self, s: &Path, t: &Path) -> io::Result<()> {
        self.call("replace", || NativeIo.replace(s, t))
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        self.call("remove", || NativeIo.remove(p))
    }
    fn create_directory(&self, p: &Path) -> io::Result<()> {
        self.call("mkdir", || NativeIo.create_directory(p))
    }
    fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
        self.call("sync_directory", || NativeIo.sync_directory(p))
    }
}
fn prepared(root: &VaultRoot) -> PreparedChange {
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(root, Duration::ZERO).unwrap();
    engine.prepare(&permit, draft()).unwrap().prepared
}
fn assert_new(root: &VaultRoot) {
    assert_eq!(
        fs::read(root.path().join("alpha.md")).unwrap(),
        b"new alpha"
    );
    assert_eq!(
        fs::read(root.path().join("asset.bin")).unwrap(),
        vec![0, 255, 1, 128]
    );
    assert!(!root.path().join("delete.md").exists());
    assert_eq!(fs::read(root.path().join("new.md")).unwrap(), b"new note");
}
fn resume(root: &VaultRoot, change: &PreparedChange) {
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(root, Duration::ZERO).unwrap();
    let publisher = Publisher::default();
    let result = engine.recover(&permit, &Validator, &publisher).unwrap();
    if result.staged.contains(change) {
        engine
            .apply(&permit, change, &Validator, &publisher)
            .unwrap();
    }
    assert_new(root);
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Committed
    );
}
#[test]
fn crash_matrix_journal_stage_replace_filesapplied_indexed_commit() {
    const IMMUTABLE: &str = "sources/source_matrix/revisions/revision_matrix/original.bin";
    const CAPTURED: &[u8] = &[0, 255, 128, 1, 0];
    fn matrix_prepared(root: &VaultRoot) -> PreparedChange {
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(root, Duration::ZERO).unwrap();
        let mut proposal = draft();
        proposal.operations.push(ExpectedWrite {
            target: rel(IMMUTABLE),
            expected: ExpectedState::Absent,
            proposed: Some(CAPTURED.to_vec()),
            apply_after: vec![],
        });
        engine.prepare(&permit, proposal).unwrap().prepared
    }
    let (_temp, root) = fixture();
    let change = matrix_prepared(&root);
    let io = Arc::new(FaultIo::new(None, false));
    let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), io.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    engine
        .apply(&permit, &change, &Validator, &Publisher::default())
        .unwrap();
    assert_eq!(fs::read(root.path().join(IMMUTABLE)).unwrap(), CAPTURED);
    let events = io.events.lock().unwrap().clone();
    drop(permit);
    drop(engine);
    eprintln!("fault matrix: {} boundaries x before/after", events.len());
    for after in [false, true] {
        for (boundary, name) in events.iter().enumerate() {
            let (_temp, root) = fixture();
            let change = matrix_prepared(&root);
            let io = Arc::new(FaultIo::new(Some(boundary), after));
            let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), io.clone())).unwrap();
            let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
            let _ = engine.apply(&permit, &change, &Validator, &Publisher::default());
            assert!(
                io.fired.load(Ordering::SeqCst),
                "boundary {boundary} {name}"
            );
            drop(permit);
            drop(engine);
            resume(&root, &change);
            assert_eq!(fs::read(root.path().join(IMMUTABLE)).unwrap(), CAPTURED);
        }
    }
    eprintln!("{} actual NativeIo boundaries x before/after", events.len());
}
#[test]
fn recovery_old_new_third_hash_and_absence() {
    for (target, third) in [
        ("alpha.md", Some(b"unfamiliar".as_slice())),
        ("alpha.md", None),
        ("asset.bin", Some(b"foreign binary".as_slice())),
    ] {
        let (_temp, root) = fixture();
        let change = prepared(&root);
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        if let Some(bytes) = third {
            fs::write(root.path().join(target), bytes).unwrap();
        } else {
            fs::remove_file(root.path().join(target)).unwrap();
        }
        let publisher = Publisher::default();
        assert_eq!(
            engine
                .apply(&permit, &change, &Validator, &publisher)
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            engine.inspect(&change.change_id).unwrap().status,
            ChangeStatus::Prepared
        );
        assert_eq!(
            engine
                .recover(&permit, &Validator, &publisher)
                .unwrap()
                .staged,
            vec![change]
        );
        assert_eq!(fs::read(root.path().join(target)).ok().as_deref(), third);
    }
}
#[test]
fn lost_state_reconstructs_from_payloads() {
    let (_temp, root) = fixture();
    let change = prepared(&root);
    fs::remove_file(
        root.path()
            .join(format!(".wiki/state/changes/{}.journal", change.change_id)),
    )
    .unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let publisher = Publisher::default();
    assert_eq!(
        engine
            .recover(&permit, &Validator, &publisher)
            .unwrap()
            .staged,
        vec![change.clone()]
    );
    fs::write(root.path().join("alpha.md"), b"new alpha").unwrap();
    assert_eq!(
        engine
            .recover(&permit, &Validator, &publisher)
            .unwrap_err()
            .code,
        ErrorCode::RecoveryRequired
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
    engine
        .apply(&permit, &change, &Validator, &publisher)
        .unwrap();
    assert_new(&root);
    fs::remove_file(
        root.path()
            .join(format!(".wiki/state/changes/{}.journal", change.change_id)),
    )
    .unwrap();
    fs::write(root.path().join("alpha.md"), b"later legitimate edit").unwrap();
    assert_eq!(
        engine
            .recover(&permit, &Validator, &publisher)
            .unwrap()
            .changes[0]
            .status,
        ChangeStatus::Committed
    );
    assert_eq!(
        engine
            .apply(&permit, &change, &Validator, &publisher)
            .unwrap()
            .status,
        ChangeStatus::Committed
    );
    assert_eq!(
        fs::read(root.path().join("alpha.md")).unwrap(),
        b"later legitimate edit"
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn rollback_preserves_unfamiliar_edits() {
    let (_temp, root) = fixture();
    let change = prepared(&root);
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let publisher = Publisher::default();
    engine
        .apply(&permit, &change, &Validator, &publisher)
        .unwrap();
    fs::write(root.path().join("alpha.md"), b"unfamiliar").unwrap();
    assert_eq!(
        engine.inverse_plan(&change).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        fs::read(root.path().join("alpha.md")).unwrap(),
        b"unfamiliar"
    );
    fs::write(root.path().join("alpha.md"), b"new alpha").unwrap();
    let inverse = engine
        .prepare_inverse(&permit, &change, &Validator)
        .unwrap();
    engine
        .apply(&permit, &inverse, &Validator, &publisher)
        .unwrap();
    assert_eq!(
        fs::read(root.path().join("alpha.md")).unwrap(),
        b"old alpha"
    );
    assert!(!root.path().join("asset.bin").exists());
}
#[test]
fn publication_refuses_unavailable_bad_payload_and_mismatched_snapshot() {
    for mode in [0, 1, 2] {
        let (_temp, root) = fixture();
        let change = prepared(&root);
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let publisher = Publisher::default();
        if mode == 0 {
            publisher.unavailable.store(true, Ordering::SeqCst);
        }
        if mode == 1 {
            let payload = engine
                .inspect(&change.change_id)
                .unwrap()
                .manifest
                .operations[0]
                .after_payload
                .clone()
                .unwrap();
            fs::write(root.path().join(payload.path.as_str()), b"tampered").unwrap();
        }
        if mode == 2 {
            publisher.mismatch.store(true, Ordering::SeqCst);
        }
        assert!(
            engine
                .apply(&permit, &change, &Validator, &publisher)
                .is_err()
        );
        assert_ne!(
            engine.inspect(&change.change_id).ok().map(|c| c.status),
            Some(ChangeStatus::Committed)
        );
        if mode < 2 {
            assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                fs::read(root.path().join("alpha.md")).unwrap(),
                b"old alpha"
            );
        }
    }
}
#[test]
fn overlapping_staged_changes_do_not_block_publication() {
    let (_temp, root) = fixture();
    let change = prepared(&root);
    let second = prepared(&root);
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let publisher = Publisher::default();
    engine
        .apply(&permit, &change, &Validator, &publisher)
        .unwrap();
    assert_eq!(
        engine
            .apply(&permit, &second, &Validator, &publisher)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert!(
        engine
            .recover(&permit, &Validator, &publisher)
            .unwrap()
            .staged
            .contains(&second)
    );
    let mut later = draft();
    later.operations = vec![ExpectedWrite {
        target: rel("later.md"),
        expected: ExpectedState::Absent,
        proposed: Some(b"later".to_vec()),
        apply_after: vec![],
    }];
    let next = engine.prepare(&permit, later).unwrap().prepared;
    engine
        .apply(&permit, &next, &Validator, &publisher)
        .unwrap();
    assert_new(&root);
}
#[test]
#[ignore]
fn crash_child() {
    let path = std::env::var("LWIKI_P03_CHILD").unwrap();
    let root = VaultRoot::explicit(&path).unwrap();
    let id = std::env::var("LWIKI_P03_CHANGE").unwrap();
    let native = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let change = native
        .inspect(&lwiki::domain::RecordId::new(id).unwrap())
        .unwrap()
        .prepared;
    let mut io = FaultIo::new(
        Some(
            std::env::var("LWIKI_P03_BOUNDARY")
                .unwrap()
                .parse()
                .unwrap(),
        ),
        true,
    );
    io.pause = Some(Path::new(&path).join("kill-ready"));
    let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), Arc::new(io))).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    engine
        .apply(&permit, &change, &Validator, &Publisher::default())
        .unwrap();
    panic!("kill boundary not reached");
}
#[test]
fn subprocess_abrupt_kill_restarts_at_real_replace_boundary() {
    // Determine the first canonical replace boundary from a complete real apply.
    let (_temp, root) = fixture();
    let change = prepared(&root);
    let io = Arc::new(FaultIo::new(None, false));
    let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), io.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    engine
        .apply(&permit, &change, &Validator, &Publisher::default())
        .unwrap();
    let events = io.events.lock().unwrap();
    let boundary = events
        .iter()
        .enumerate()
        .filter(|(_, n)| n.as_str() == "replace")
        .nth(1)
        .unwrap()
        .0;
    drop(events);
    drop(permit);
    let (_temp, root) = fixture();
    let change = prepared(&root);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--ignored", "--nocapture"])
        .env("LWIKI_P03_CHILD", root.path())
        .env("LWIKI_P03_CHANGE", change.change_id.as_str())
        .env("LWIKI_P03_BOUNDARY", boundary.to_string())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.path().join("kill-ready").exists() {
        assert!(Instant::now() < deadline, "child failed to reach replace");
        assert!(child.try_wait().unwrap().is_none(), "child exited early");
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    fs::remove_file(root.path().join("kill-ready")).unwrap();
    resume(&root, &change);
}

struct StopAfterEvent {
    event: &'static str,
    armed: AtomicBool,
    fired: AtomicBool,
}
impl StopAfterEvent {
    fn new(event: &'static str) -> Self {
        Self {
            event,
            armed: AtomicBool::new(false),
            fired: AtomicBool::new(false),
        }
    }
}
impl DurableIo for StopAfterEvent {
    fn create_stage(&self, p: &Path) -> io::Result<File> {
        NativeIo.create_stage(p)
    }
    fn open_append(&self, p: &Path) -> io::Result<File> {
        NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> io::Result<()> {
        NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> io::Result<()> {
        let result = NativeIo.write_stage(f, b);
        let needle = format!("\"event\":\"{}\"", self.event);
        if b.starts_with(b"LWJNL001") && b.windows(needle.len()).any(|w| w == needle.as_bytes()) {
            self.armed.store(true, Ordering::SeqCst);
        }
        result
    }
    fn sync_file(&self, f: &File) -> io::Result<()> {
        NativeIo.sync_file(f)?;
        if self.armed.swap(false, Ordering::SeqCst) && !self.fired.swap(true, Ordering::SeqCst) {
            return Err(io::Error::other("stop after synced journal event"));
        }
        Ok(())
    }
    fn replace(&self, s: &Path, t: &Path) -> io::Result<()> {
        NativeIo.replace(s, t)
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        NativeIo.remove(p)
    }
    fn create_directory(&self, p: &Path) -> io::Result<()> {
        NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
        NativeIo.sync_directory(p)
    }
}
fn stop_at(root: &VaultRoot, change: &PreparedChange, event: &'static str) {
    let io = Arc::new(StopAfterEvent::new(event));
    let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), io.clone())).unwrap();
    let permit = WriterPermit::acquire(root, Duration::ZERO).unwrap();
    assert!(
        engine
            .apply(&permit, change, &Validator, &Publisher::default())
            .is_err()
    );
    assert!(io.fired.load(Ordering::SeqCst));
}
#[test]
fn recovery_replays_old_despite_done_filesapplied_and_indexed_flags() {
    for phase in ["done", "files_applied", "indexed"] {
        let (_temp, root) = fixture();
        let change = prepared(&root);
        stop_at(&root, &change, phase);
        fs::write(root.path().join("alpha.md"), b"old alpha").unwrap();
        resume(&root, &change);
    }
}
#[test]
fn interrupted_apply_preserves_outside_scan_and_binary_read_dependencies() {
    let (_temp, root) = fixture();
    let change = prepared(&root);
    stop_at(&root, &change, "files_applied");
    fs::write(
        root.path().join("new-unrelated.md"),
        b"added after original validation",
    )
    .unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let publisher = Publisher::default();
    assert_eq!(
        engine
            .recover(&permit, &Validator, &publisher)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Conflict
    );
    assert_eq!(
        fs::read(root.path().join("new-unrelated.md")).unwrap(),
        b"added after original validation"
    );

    struct DependencyValidator;
    impl GraphValidator for DependencyValidator {
        fn validate(&self, fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph> {
            let mut graph = Validator.validate(fs, input)?;
            graph.dependencies.push(ReadDependency {
                path: rel("source.bin"),
                expected: ExpectedState::Hash(fs.read_before(&rel("source.bin"))?.unwrap().hash),
            });
            Ok(graph)
        }
    }
    let (_temp, root) = fixture();
    fs::write(root.path().join("source.bin"), [0, 255, 1]).unwrap();
    let change = prepared(&root);
    let io = Arc::new(StopAfterEvent::new("files_applied"));
    let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), io)).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    assert!(
        engine
            .apply(
                &permit,
                &change,
                &DependencyValidator,
                &Publisher::default()
            )
            .is_err()
    );
    drop(permit);
    drop(engine);
    fs::write(root.path().join("source.bin"), [0, 255, 2]).unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let publisher = Publisher::default();
    assert_eq!(
        engine
            .recover(&permit, &DependencyValidator, &publisher)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn applying_third_hash_and_absence_become_durable_conflicts() {
    for target in ["alpha.md", "asset.bin"] {
        let (_temp, root) = fixture();
        let change = prepared(&root);
        stop_at(&root, &change, "files_applied");
        fs::write(root.path().join(target), b"third party").unwrap();
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let publisher = Publisher::default();
        assert_eq!(
            engine
                .recover(&permit, &Validator, &publisher)
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        assert_eq!(
            engine.inspect(&change.change_id).unwrap().status,
            ChangeStatus::Conflict
        );
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
        assert_eq!(fs::read(root.path().join(target)).unwrap(), b"third party");
    }
    let (_temp, root) = fixture();
    let change = prepared(&root);
    stop_at(&root, &change, "files_applied");
    fs::remove_file(root.path().join("alpha.md")).unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let publisher = Publisher::default();
    assert!(engine.recover(&permit, &Validator, &publisher).is_err());
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Conflict
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn bookkeeping_edit_and_receipt_corruption_fail_closed() {
    let (_temp, root) = fixture();
    let change = prepared(&root);
    stop_at(&root, &change, "indexed");
    let note = root
        .path()
        .join(format!("changes/{}/change.md", change.change_id));
    let mut bytes = fs::read(&note).unwrap();
    bytes.extend(b"\nuser bookkeeping\n");
    fs::write(&note, &bytes).unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    assert_eq!(
        engine
            .recover(&permit, &Validator, &Publisher::default())
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(fs::read(&note).unwrap(), bytes);
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Conflict
    );
    drop(permit);
    let (_temp, root) = fixture();
    let change = prepared(&root);
    resume(&root, &change);
    fs::write(
        root.path()
            .join(format!("changes/{}/outcome.json", change.change_id)),
        b"{} ",
    )
    .unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let publisher = Publisher::default();
    assert!(engine.recover(&permit, &Validator, &publisher).is_err());
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn another_unresolved_apply_blocks_before_new_mutation() {
    let (_temp, root) = fixture();
    let change = prepared(&root);
    stop_at(&root, &change, "files_applied");
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let mut next = draft();
    next.operations = vec![ExpectedWrite {
        target: rel("later.md"),
        expected: ExpectedState::Absent,
        proposed: Some(b"later".to_vec()),
        apply_after: vec![],
    }];
    let pending = engine.prepare(&permit, next).unwrap().prepared;
    let publisher = Publisher::default();
    assert_eq!(
        engine
            .apply(&permit, &pending, &Validator, &publisher)
            .unwrap_err()
            .code,
        ErrorCode::RecoveryRequired
    );
    assert!(!root.path().join("later.md").exists());
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn inverse_keeps_immutable_captures_and_stale_prepared_abort_is_safe() {
    let (_temp, root) = fixture();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let mut original = draft();
    original.operations.push(ExpectedWrite {
        target: rel("sources/source_test/revisions/revision_new/original.bin"),
        expected: ExpectedState::Absent,
        proposed: Some(vec![0, 128, 255]),
        apply_after: vec![],
    });
    let change = engine.prepare(&permit, original).unwrap().prepared;
    let stale = engine.prepare(&permit, draft()).unwrap().prepared;
    engine
        .apply(&permit, &change, &Validator, &Publisher::default())
        .unwrap();
    assert_eq!(
        engine.abort(&permit, &stale).unwrap().status,
        ChangeStatus::Aborted
    );
    let inverse_plan = engine.inverse_plan(&change).unwrap();
    assert_eq!(
        inverse_plan.retained_paths,
        vec![rel(
            "sources/source_test/revisions/revision_new/original.bin"
        )]
    );
    let inverse = engine
        .prepare_inverse(&permit, &change, &Validator)
        .unwrap();
    engine
        .apply(&permit, &inverse, &Validator, &Publisher::default())
        .unwrap();
    assert_eq!(
        fs::read(
            root.path()
                .join("sources/source_test/revisions/revision_new/original.bin")
        )
        .unwrap(),
        vec![0, 128, 255]
    );
}

#[test]
fn missing_immutable_capture_after_filesapplied_is_restored_from_exact_payload() {
    let (_temp, root) = fixture();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let asset = "sources/source_test/revisions/revision_new/original.bin";
    let mut original = draft();
    original.operations.push(ExpectedWrite {
        target: rel(asset),
        expected: ExpectedState::Absent,
        proposed: Some(vec![0, 128, 255]),
        apply_after: vec![],
    });
    let change = engine.prepare(&permit, original).unwrap().prepared;
    drop(permit);
    drop(engine);
    stop_at(&root, &change, "files_applied");
    fs::remove_file(root.path().join(asset)).unwrap();
    resume(&root, &change);
    assert_eq!(
        fs::read(root.path().join(asset)).unwrap(),
        vec![0, 128, 255]
    );
}

struct StopValidationRename {
    armed: AtomicBool,
    fired: AtomicBool,
}
impl DurableIo for StopValidationRename {
    fn create_stage(&self, p: &Path) -> io::Result<File> {
        NativeIo.create_stage(p)
    }
    fn open_append(&self, p: &Path) -> io::Result<File> {
        NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> io::Result<()> {
        NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> io::Result<()> {
        NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &File) -> io::Result<()> {
        NativeIo.sync_file(f)
    }
    fn replace(&self, s: &Path, t: &Path) -> io::Result<()> {
        NativeIo.replace(s, t)?;
        if t.file_name().is_some_and(|n| n == "validation.json") {
            self.armed.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        NativeIo.remove(p)
    }
    fn create_directory(&self, p: &Path) -> io::Result<()> {
        NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
        if self.armed.swap(false, Ordering::SeqCst) && !self.fired.swap(true, Ordering::SeqCst) {
            return Err(io::Error::other("failed validation entry sync"));
        }
        NativeIo.sync_directory(p)
    }
}
#[test]
fn observed_validation_receipt_syncs_before_applying_on_retry() {
    let (_temp, root) = fixture();
    let change = prepared(&root);
    let io = Arc::new(StopValidationRename {
        armed: AtomicBool::new(false),
        fired: AtomicBool::new(false),
    });
    let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), io.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    assert!(
        engine
            .apply(&permit, &change, &Validator, &Publisher::default())
            .is_err()
    );
    assert!(io.fired.load(Ordering::SeqCst));
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Prepared
    );
    drop(permit);
    drop(engine);
    let io = Arc::new(FaultIo::new(None, false));
    let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), io.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    engine
        .apply(&permit, &change, &Validator, &Publisher::default())
        .unwrap();
    let events = io.events.lock().unwrap();
    assert_eq!(&events[..2], &["sync_file", "sync_directory"]);
    assert_new(&root);
}

fn revision_draft(tree: &str, members: &[(&str, &[u8])]) -> ChangeDraft {
    let mut result = draft();
    result.operations = members
        .iter()
        .map(|(member, bytes)| ExpectedWrite {
            target: rel(&format!("{tree}/{member}")),
            expected: ExpectedState::Absent,
            proposed: Some(bytes.to_vec()),
            apply_after: vec![],
        })
        .collect();
    result
}
#[test]
fn independently_prepared_revision_extension_refuses_even_permissive_graph() {
    for (first_namespace, second_namespace) in [
        ("sources", "sources"),
        ("Sources", "Sources"),
        ("ſources", "ſources"),
        ("sources", "Sources"),
    ] {
        let (_temp, root) = fixture();
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let first_tree = format!("{first_namespace}/source_s/Revisions/revision_r");
        let second_tree = format!("{second_namespace}/source_s/Revisions/revision_r");
        let first = engine
            .prepare(
                &permit,
                revision_draft(&first_tree, &[("original.bin", &[0, 255])]),
            )
            .unwrap()
            .prepared;
        let second = engine
            .prepare(
                &permit,
                revision_draft(&second_tree, &[("content.md", b"second payload")]),
            )
            .unwrap()
            .prepared;
        let publisher = Publisher::default();
        engine
            .apply(&permit, &first, &Validator, &publisher)
            .unwrap();
        assert_eq!(
            engine
                .apply(&permit, &second, &Validator, &publisher)
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        assert_eq!(
            fs::read(root.path().join(format!("{first_tree}/original.bin"))).unwrap(),
            vec![0, 255]
        );
        assert!(
            !root
                .path()
                .join(format!("{second_tree}/content.md"))
                .exists()
        );
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            engine.inspect(&second.change_id).unwrap().status,
            ChangeStatus::Prepared
        );
    }
}
#[test]
fn extra_unplanned_revision_member_before_apply_preserves_all_bytes() {
    let (_temp, root) = fixture();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let tree = "sources/source_s/revisions/revision_r";
    let change = engine
        .prepare(
            &permit,
            revision_draft(tree, &[("original.bin", b"planned")]),
        )
        .unwrap()
        .prepared;
    fs::create_dir_all(root.path().join(tree)).unwrap();
    fs::write(
        root.path().join(format!("{tree}/external.bin")),
        b"external",
    )
    .unwrap();
    let publisher = Publisher::default();
    assert_eq!(
        engine
            .apply(&permit, &change, &Validator, &publisher)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        fs::read(root.path().join(format!("{tree}/external.bin"))).unwrap(),
        b"external"
    );
    assert!(!root.path().join(format!("{tree}/original.bin")).exists());
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn own_interrupted_revision_members_resume_but_extras_become_conflict() {
    for extra in [false, true] {
        let (_temp, root) = fixture();
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let tree = "sources/source_s/revisions/revision_r";
        let change = engine
            .prepare(
                &permit,
                revision_draft(tree, &[("a.bin", b"a"), ("sub/b.bin", &[0, 255])]),
            )
            .unwrap()
            .prepared;
        drop(permit);
        drop(engine);
        stop_at(&root, &change, "done");
        if extra {
            fs::write(
                root.path().join(format!("{tree}/external.bin")),
                b"external",
            )
            .unwrap();
        }
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let publisher = Publisher::default();
        if extra {
            assert_eq!(
                engine
                    .recover(&permit, &Validator, &publisher)
                    .unwrap_err()
                    .code,
                ErrorCode::ContentConflict
            );
            assert_eq!(
                engine.inspect(&change.change_id).unwrap().status,
                ChangeStatus::Conflict
            );
            assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                fs::read(root.path().join(format!("{tree}/external.bin"))).unwrap(),
                b"external"
            );
        } else {
            engine.recover(&permit, &Validator, &publisher).unwrap();
            assert_eq!(
                fs::read(root.path().join(format!("{tree}/sub/b.bin"))).unwrap(),
                vec![0, 255]
            );
            assert_eq!(
                engine.inspect(&change.change_id).unwrap().status,
                ChangeStatus::Committed
            );
        }
        assert_eq!(
            fs::read(root.path().join(format!("{tree}/a.bin"))).unwrap(),
            b"a"
        );
    }
}
#[test]
fn missing_corrupt_or_misbound_revision_receipt_refuses_recovery() {
    let tree = "sources/source_s/revisions/revision_r";
    for damage in ["missing", "corrupt", "misbound"] {
        let (_temp, root) = fixture();
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let change = engine
            .prepare(
                &permit,
                revision_draft(tree, &[("a.bin", b"a"), ("b.bin", b"b")]),
            )
            .unwrap()
            .prepared;
        drop(permit);
        drop(engine);
        stop_at(&root, &change, "done");
        let receipt = root
            .path()
            .join(format!("changes/{}/revision-trees.json", change.change_id));
        match damage {
            "missing" => fs::remove_file(&receipt).unwrap(),
            "corrupt" => fs::write(&receipt, b"corrupt ownership receipt").unwrap(),
            "misbound" => {
                // A valid checksum from a different manifest must not authorize this one.
                let (_other_temp, other_root) = fixture();
                let other_engine = ChangeEngine::new(VaultFs::new(other_root.clone())).unwrap();
                let other_permit = WriterPermit::acquire(&other_root, Duration::ZERO).unwrap();
                let other_change = other_engine
                    .prepare(
                        &other_permit,
                        revision_draft(tree, &[("a.bin", b"a"), ("b.bin", b"b")]),
                    )
                    .unwrap()
                    .prepared;
                assert_ne!(change.change_id, other_change.change_id);
                drop(other_permit);
                drop(other_engine);
                stop_at(&other_root, &other_change, "done");
                let other_receipt = other_root.path().join(format!(
                    "changes/{}/revision-trees.json",
                    other_change.change_id
                ));
                fs::copy(other_receipt, &receipt).unwrap();
            }
            _ => unreachable!(),
        }
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let publisher = Publisher::default();
        assert!(engine.recover(&permit, &Validator, &publisher).is_err());
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(root.path().join(format!("{tree}/a.bin"))).unwrap(),
            b"a"
        );
        assert!(!root.path().join(format!("{tree}/b.bin")).exists());
    }
}
#[test]
fn lost_journal_revision_owner_is_bound_to_its_original_manifest_and_baseline() {
    let tree = "sources/source_s/revisions/revision_r";
    for lose_baseline in [false, true] {
        let (_temp, root) = fixture();
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let change = engine
            .prepare(
                &permit,
                revision_draft(tree, &[("a.bin", b"a"), ("b.bin", b"b")]),
            )
            .unwrap()
            .prepared;
        drop(permit);
        drop(engine);
        stop_at(&root, &change, "done");
        fs::remove_file(
            root.path()
                .join(format!(".wiki/state/changes/{}.journal", change.change_id)),
        )
        .unwrap();
        if lose_baseline {
            fs::remove_file(
                root.path()
                    .join(format!("changes/{}/validation.json", change.change_id)),
            )
            .unwrap();
        }
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let publisher = Publisher::default();
        assert!(engine.recover(&permit, &Validator, &publisher).is_err());
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
        if lose_baseline {
            assert_eq!(
                engine
                    .apply(&permit, &change, &Validator, &publisher)
                    .unwrap_err()
                    .code,
                ErrorCode::RecoveryRequired
            );
            assert!(!root.path().join(format!("{tree}/b.bin")).exists());
        } else {
            engine
                .apply(&permit, &change, &Validator, &publisher)
                .unwrap();
            assert_eq!(
                fs::read(root.path().join(format!("{tree}/b.bin"))).unwrap(),
                b"b"
            );
        }
    }
    let (_temp, root) = fixture();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let first = engine
        .prepare(
            &permit,
            revision_draft(tree, &[("original.bin", b"original")]),
        )
        .unwrap()
        .prepared;
    let second = engine
        .prepare(
            &permit,
            revision_draft(tree, &[("content.bin", b"content")]),
        )
        .unwrap()
        .prepared;
    engine
        .apply(&permit, &first, &Validator, &Publisher::default())
        .unwrap();
    fs::remove_file(
        root.path()
            .join(format!(".wiki/state/changes/{}.journal", first.change_id)),
    )
    .unwrap();
    fs::remove_file(
        root.path()
            .join(format!(".wiki/state/changes/{}.journal", second.change_id)),
    )
    .unwrap();
    let publisher = Publisher::default();
    assert_eq!(
        engine
            .apply(&permit, &second, &Validator, &publisher)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert!(!root.path().join(format!("{tree}/content.bin")).exists());
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn known_prepared_abort_ignores_obsolete_directory_nested_vault_and_symlink_targets() {
    for mode in [0, 1, 2] {
        let (_temp, root) = fixture();
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let mut proposal = draft();
        proposal.operations = vec![ExpectedWrite {
            target: rel("future/page.md"),
            expected: ExpectedState::Absent,
            proposed: Some(b"proposed".to_vec()),
            apply_after: vec![],
        }];
        let change = engine.prepare(&permit, proposal).unwrap().prepared;
        fs::create_dir(root.path().join("future")).unwrap();
        match mode {
            0 => fs::create_dir(root.path().join("future/page.md")).unwrap(),
            1 => fs::write(root.path().join("future/WIKI.md"), b"nested vault").unwrap(),
            2 => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(
                    root.path().join("unrelated.md"),
                    root.path().join("future/page.md"),
                )
                .unwrap();
                #[cfg(not(unix))]
                fs::create_dir(root.path().join("future/page.md")).unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            engine.abort(&permit, &change).unwrap().status,
            ChangeStatus::Aborted
        );
        assert_eq!(
            engine.inspect(&change.change_id).unwrap().status,
            ChangeStatus::Aborted
        );
        if mode == 0 {
            assert!(root.path().join("future/page.md").is_dir());
        }
        if mode == 1 {
            assert_eq!(
                fs::read(root.path().join("future/WIKI.md")).unwrap(),
                b"nested vault"
            );
        }
        if mode == 2 {
            assert_eq!(
                fs::read(root.path().join("unrelated.md")).unwrap(),
                b"context"
            );
        }
    }
}
#[test]
fn missing_journal_abort_still_refuses_obsolete_target_scope_and_types() {
    for mode in [0, 1, 2] {
        let (_temp, root) = fixture();
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        let mut proposal = draft();
        proposal.operations = vec![ExpectedWrite {
            target: rel("future/page.md"),
            expected: ExpectedState::Absent,
            proposed: Some(b"proposed".to_vec()),
            apply_after: vec![],
        }];
        let change = engine.prepare(&permit, proposal).unwrap().prepared;
        fs::remove_file(
            root.path()
                .join(format!(".wiki/state/changes/{}.journal", change.change_id)),
        )
        .unwrap();
        fs::create_dir(root.path().join("future")).unwrap();
        match mode {
            0 => fs::create_dir(root.path().join("future/page.md")).unwrap(),
            1 => fs::write(root.path().join("future/WIKI.md"), b"nested vault").unwrap(),
            2 => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(
                    root.path().join("unrelated.md"),
                    root.path().join("future/page.md"),
                )
                .unwrap();
                #[cfg(not(unix))]
                fs::create_dir(root.path().join("future/page.md")).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(engine.abort(&permit, &change).is_err());
        assert!(
            !root
                .path()
                .join(format!("changes/{}/outcome.json", change.change_id))
                .exists()
        );
    }
}

#[test]
fn unicode_namespace_captured_frontmatter_stays_out_of_canonical_scan() {
    let (_temp, root) = fixture();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let tree = "ſources/source_s/Revisions/revision_r";
    let fake=b"---\nwiki_schema: \"1\"\nwiki_id: page_capture\nwiki_kind: page\ntitle: Captured unadopted page\n---\n\nPayload\n";
    let change = engine
        .prepare(
            &permit,
            revision_draft(tree, &[("content.md", fake), ("original.bin", &[0, 255])]),
        )
        .unwrap()
        .prepared;
    engine
        .apply(&permit, &change, &Validator, &Publisher::default())
        .unwrap();
    assert_eq!(
        fs::read(root.path().join(format!("{tree}/content.md"))).unwrap(),
        fake
    );
    assert!(
        !root
            .scan_markdown()
            .unwrap()
            .iter()
            .any(|p| p.as_str().starts_with("ſources/"))
    );
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Committed
    );
}
#[test]
fn extra_revision_bytes_during_validation_block_publication_and_durably_conflict() {
    struct InjectExtra {
        calls: AtomicUsize,
        extra: PathBuf,
    }
    impl GraphValidator for InjectExtra {
        fn validate(&self, fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph> {
            let graph = Validator.validate(fs, input)?;
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                std::fs::write(&self.extra, b"unplanned external edit").unwrap();
            }
            Ok(graph)
        }
    }
    let (_temp, root) = fixture();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let tree = "sources/source_s/revisions/revision_r";
    let change = engine
        .prepare(
            &permit,
            revision_draft(tree, &[("original.bin", &[0, 255])]),
        )
        .unwrap()
        .prepared;
    let validator = InjectExtra {
        calls: AtomicUsize::new(0),
        extra: root.path().join(format!("{tree}/external.bin")),
    };
    let publisher = Publisher::default();
    assert_eq!(
        engine
            .apply(&permit, &change, &validator, &publisher)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Conflict
    );
    assert_eq!(
        fs::read(&validator.extra).unwrap(),
        b"unplanned external edit"
    );
    assert_eq!(
        fs::read(root.path().join(format!("{tree}/original.bin"))).unwrap(),
        vec![0, 255]
    );
}
#[test]
fn subprocess_killed_after_private_revision_stage_creation_recovers_exact_members() {
    let tree = "sources/source_s/revisions/revision_r";
    let (_temp, root) = fixture();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let change = engine
        .prepare(
            &permit,
            revision_draft(
                tree,
                &[("original.bin", &[0, 255]), ("sub/content.bin", b"content")],
            ),
        )
        .unwrap()
        .prepared;
    drop(permit);
    drop(engine);
    let io = Arc::new(FaultIo::new(None, false));
    let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), io.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    engine
        .apply(&permit, &change, &Validator, &Publisher::default())
        .unwrap();
    // Validation and revision claim stages precede the first canonical stage.
    let boundary = io
        .events
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, n)| n.as_str() == "create_stage")
        .nth(2)
        .unwrap()
        .0;
    drop(permit);
    drop(engine);
    let (_temp, root) = fixture();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let change = engine
        .prepare(
            &permit,
            revision_draft(
                tree,
                &[("original.bin", &[0, 255]), ("sub/content.bin", b"content")],
            ),
        )
        .unwrap()
        .prepared;
    drop(permit);
    drop(engine);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--ignored", "--nocapture"])
        .env("LWIKI_P03_CHILD", root.path())
        .env("LWIKI_P03_CHANGE", change.change_id.as_str())
        .env("LWIKI_P03_BOUNDARY", boundary.to_string())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.path().join("kill-ready").exists() {
        assert!(
            Instant::now() < deadline,
            "child failed to reach private stage"
        );
        assert!(child.try_wait().unwrap().is_none(), "child exited early");
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    fs::remove_file(root.path().join("kill-ready")).unwrap();
    assert!(fs::read_dir(root.path().join(tree)).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".lwiki-stage-")
    }));
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    engine
        .recover(&permit, &Validator, &Publisher::default())
        .unwrap();
    assert_eq!(
        fs::read(root.path().join(format!("{tree}/original.bin"))).unwrap(),
        vec![0, 255]
    );
    assert_eq!(
        fs::read(root.path().join(format!("{tree}/sub/content.bin"))).unwrap(),
        b"content"
    );
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Committed
    );
}

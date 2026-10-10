//! Reached schedules and exact applying-loop observations; disposable fixtures only.
use super::*;
use crate::{
    domain::ReadSnapshot,
    maintenance_parallel,
    vault::{VaultFs, VaultRoot},
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Copy)]
pub(crate) enum Event<'a> {
    LoopBegin,
    LoopEnd,
    PassBegin,
    PassEnd,
    BeforeRead(&'a VaultRelativePath),
    Read(&'a VaultRelativePath, Option<[u8; 32]>, usize),
    TargetRead(&'a VaultRelativePath, Option<[u8; 32]>, usize),
    AfterJoin,
    BeforeMutation(usize),
}
type Hook = Arc<dyn for<'a> Fn(Event<'a>) -> Result<()> + Send + Sync>;
static HOOKS: OnceLock<Mutex<BTreeMap<PathBuf, Hook>>> = OnceLock::new();
pub(crate) fn hook(root: &VaultRoot, event: Event<'_>) -> Result<()> {
    let callback = HOOKS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(root.path())
        .cloned();
    callback.map_or(Ok(()), |callback| callback(event))
}
struct HookGuard(PathBuf);
impl Drop for HookGuard {
    fn drop(&mut self) {
        HOOKS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(&self.0);
    }
}
fn install(
    root: &VaultRoot,
    callback: impl for<'a> Fn(Event<'a>) -> Result<()> + Send + Sync + 'static,
) -> HookGuard {
    assert!(
        HOOKS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(root.path().to_owned(), Arc::new(callback))
            .is_none()
    );
    HookGuard(root.path().to_owned())
}
#[derive(Default)]
struct PairGate(std::sync::Mutex<(u64, usize)>, std::sync::Condvar);
impl PairGate {
    fn wait(&self) -> Result<()> {
        let mut state = self.0.lock().unwrap();
        let generation = state.0;
        state.1 += 1;
        if state.1 == 2 {
            state.1 = 0;
            state.0 += 1;
            self.1.notify_all();
            return Ok(());
        }
        while state.0 == generation {
            let (next, timed) = self.1.wait_timeout(state, Duration::from_secs(5)).unwrap();
            state = next;
            if timed.timed_out() {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "second guard worker did not reach paired barrier",
                ));
            }
        }
        Ok(())
    }
}
fn path(value: impl Into<String>) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
struct Fixture {
    _temp: tempfile::TempDir,
    engine: ChangeEngine,
    writer: WriterPermit,
}
impl Fixture {
    fn new(retained: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
        )
        .unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        if retained {
            crate::storage::cleanup(&vault, &writer, &crate::storage::StorageOptions::default())
                .unwrap();
        }
        Self {
            _temp: temp,
            engine: ChangeEngine::new(vault).unwrap(),
            writer,
        }
    }
    fn root(&self) -> &VaultRoot {
        self.engine.fs().root()
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        fs::create_dir_all(self.root().path().join(name).parent().unwrap()).unwrap();
        fs::write(self.root().path().join(name), bytes).unwrap();
    }
    fn proposal(&self, writes: usize, guards: usize) -> ChangeDraft {
        let operations = (0..writes)
            .map(|i| {
                let name = format!("targets/{i:03}.bin");
                self.write(&name, b"old");
                ExpectedWrite {
                    target: path(name),
                    expected: ExpectedState::Hash(Blake3Hash::digest(b"old")),
                    proposed: Some(b"new".to_vec()),
                    apply_after: vec![],
                }
            })
            .collect();
        let read_preconditions = (0..guards)
            .map(|i| {
                let name = format!("guards/{i:03}.bin");
                self.write(&name, b"guard-before");
                ReadDependency {
                    path: path(name),
                    expected: ExpectedState::Hash(Blake3Hash::digest(b"guard-before")),
                }
            })
            .collect();
        ChangeDraft {
            title: "Exact guard boundary fixture".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            operations,
            read_preconditions,
        }
    }
}
struct Validator;
impl GraphValidator for Validator {
    fn validate(&self, _fs: &VaultFs, _input: &ValidationInput) -> Result<ValidatedGraph> {
        // Binary guard fixture isolates checkpoints; public graph oracles are separate.
        Ok(ValidatedGraph {
            parser_fingerprint: Blake3Hash::digest(b"guard-parser"),
            control_manifest: Blake3Hash::digest(b"guard-graph"),
            dependencies: vec![],
        })
    }
}
#[derive(Default)]
struct Publisher(AtomicUsize);
impl PublicationBackend for Publisher {
    fn check_available(&self) -> Result<()> {
        Ok(())
    }
    fn publish(
        &self,
        _fs: &VaultFs,
        permit: &PublicationPermit<'_>,
        _input: &ValidationInput,
    ) -> Result<ReadSnapshot> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ReadSnapshot::canonical(
            1,
            permit.graph().parser_fingerprint.clone(),
            permit.graph().control_manifest.clone(),
        ))
    }
}
#[derive(Default)]
struct Counts {
    in_loop: bool,
    passes: usize,
    current: BTreeMap<String, ([u8; 32], usize)>,
    totals: BTreeMap<String, usize>,
    bytes: usize,
    outside_reads: usize,
    mutations: usize,
    target_reads: BTreeMap<String, usize>,
    target_bytes: usize,
    outside_target_reads: usize,
}
#[test]
#[cfg(unix)]
fn exact_193_applying_passes_observe_all_119_paths_and_bytes_in_both_controls_and_layouts() {
    for retained in [false, true] {
        for sequential in [true, false] {
            let f = Fixture::new(retained);
            let proposal = f.proposal(64, 119);
            let prepared = f.engine.prepare(&f.writer, proposal).unwrap();
            assert_eq!(prepared.manifest.operations.len(), 64);
            assert_eq!(prepared.manifest.read_preconditions.len(), 119);
            let counts = Arc::new(Mutex::new(Counts::default()));
            let counted = counts.clone();
            let expected_paths: BTreeSet<_> = prepared
                .manifest
                .read_preconditions
                .iter()
                .map(|r| r.path.to_string())
                .collect();
            let expected = expected_paths.clone();
            let barrier = Arc::new(PairGate::default());
            let _hook = install(f.root(), move |event| {
                if retained && !sequential {
                    if let Event::BeforeRead(path) = &event {
                        if path.as_str() == "guards/000.bin" || path.as_str() == "guards/001.bin" {
                            barrier.wait()?;
                        }
                    }
                }
                let mut c = counted.lock().unwrap();
                match event {
                    Event::LoopBegin => {
                        assert!(!c.in_loop);
                        c.in_loop = true;
                    }
                    Event::LoopEnd => {
                        assert!(c.in_loop);
                        c.in_loop = false;
                    }
                    Event::PassBegin => c.current.clear(),
                    Event::Read(path, hash, bytes) => {
                        if c.in_loop {
                            assert_eq!(hash, Some(*blake3::hash(b"guard-before").as_bytes()));
                            assert_eq!(bytes, 12);
                            assert!(
                                c.current
                                    .insert(path.to_string(), (hash.unwrap(), bytes))
                                    .is_none()
                            );
                            *c.totals.entry(path.to_string()).or_default() += 1;
                            c.bytes += bytes;
                        } else {
                            c.outside_reads += 1;
                        }
                    }
                    Event::PassEnd => {
                        if c.in_loop {
                            assert_eq!(
                                c.current.keys().cloned().collect::<BTreeSet<_>>(),
                                expected
                            );
                            c.passes += 1;
                        }
                    }
                    Event::TargetRead(path, hash, bytes) => {
                        if c.in_loop {
                            assert!(
                                hash == Some(*blake3::hash(b"old").as_bytes())
                                    || hash == Some(*blake3::hash(b"new").as_bytes())
                            );
                            assert_eq!(bytes, 3);
                            *c.target_reads.entry(path.to_string()).or_default() += 1;
                            c.target_bytes += bytes;
                        } else {
                            c.outside_target_reads += 1;
                        }
                    }
                    Event::BeforeMutation(_) => c.mutations += 1,
                    _ => {}
                }
                Ok(())
            });
            let publisher = Publisher::default();
            let run = || {
                f.engine
                    .apply(&f.writer, &prepared.prepared, &Validator, &publisher)?;
                let m = maintenance_parallel::metrics();
                assert_eq!(m.failed, 0);
                if retained && !sequential {
                    assert!(m.max_active >= 2);
                } else {
                    assert_eq!(m.max_active, 1);
                }
                Ok(())
            };
            if sequential {
                maintenance_parallel::sequential_scope(run).unwrap();
            } else {
                maintenance_parallel::command_scope(run).unwrap();
            }
            let c = counts.lock().unwrap();
            assert!(!c.in_loop);
            assert_eq!(c.passes, 193);
            assert_eq!(
                c.totals.keys().cloned().collect::<BTreeSet<_>>(),
                expected_paths
            );
            assert!(c.totals.values().all(|n| *n == 193));
            assert_eq!(c.bytes, 22_967 * 12);
            assert_eq!(c.mutations, 64);
            assert_eq!(c.target_reads.len(), 64);
            assert!(c.target_reads.values().all(|n| *n == 2));
            assert_eq!(c.target_bytes, 128 * 3);
            assert!(c.outside_target_reads > 0);
            assert!(c.outside_reads > 0);
            assert_eq!(publisher.0.load(Ordering::SeqCst), 1);
            for i in 0..64 {
                assert_eq!(
                    fs::read(f.root().path().join(format!("targets/{i:03}.bin"))).unwrap(),
                    b"new"
                );
            }
            eprintln!(
                "layout={retained} sequential={sequential} applying_passes={} actual_guard_reads={} bytes={} lifecycle_guard_reads={} mutations={} operation_target_reads={} operation_target_bytes={} outside_target_reads={}",
                c.passes,
                c.totals.values().sum::<usize>(),
                c.bytes,
                c.outside_reads,
                c.mutations,
                c.target_reads.values().sum::<usize>(),
                c.target_bytes,
                c.outside_target_reads
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn reached_preobservation_and_after_join_edits_refuse_at_next_guard_without_publication() {
    for when in ["before", "held", "joined"] {
        for role in [0, 1, 2] {
            let f = Fixture::new(true);
            let proposal = f.proposal(2, 3);
            let prepared = f.engine.prepare(&f.writer, proposal).unwrap();
            let target = f.root().path().join(format!("guards/{role:03}.bin"));
            let state = Arc::new(Mutex::new((false, 0usize, 0usize, false)));
            let recorded = state.clone();
            let actual_reads = Arc::new(Mutex::new(BTreeMap::new()));
            let read_trace = actual_reads.clone();
            let gate = Arc::new((Mutex::new(0u8), std::sync::Condvar::new()));
            let held = gate.clone();
            let overlap = Arc::new(PairGate::default());
            let _hook = install(f.root(), move |event| {
                if when != "held" {
                    if let Event::BeforeRead(path) = event {
                        if path.as_str() == "guards/000.bin" || path.as_str() == "guards/001.bin" {
                            overlap.wait()?;
                        }
                    }
                }
                let mut s = recorded.lock().unwrap();
                match event {
                    Event::LoopBegin => s.0 = true,
                    Event::PassBegin if s.0 => s.1 += 1,
                    _ => {}
                }
                if s.0 {
                    if let Event::Read(path, hash, bytes) = event {
                        assert!(
                            read_trace
                                .lock()
                                .unwrap()
                                .insert((s.1, path.to_string()), (hash, bytes))
                                .is_none()
                        );
                    }
                }
                let injection_pass = if when == "joined" { 2 } else { 1 };
                if s.0 && s.1 == injection_pass && !s.3 {
                    if when == "before"
                        && matches!(&event,Event::BeforeRead(path) if path.as_str()==format!("guards/{role:03}.bin"))
                    {
                        fs::write(&target, b"guard-edited").unwrap();
                        s.3 = true;
                    }
                    if when == "held" {
                        // A completed sibling edits the target before releasing its held pre-read.
                        if matches!(&event,Event::Read(path,_,_) if path.as_str()==format!("guards/{:03}.bin",(role+1)%3))
                        {
                            s.3 = true;
                            // Wait outside the owner event lock so the target can
                            // announce its held pre-observation boundary.
                            drop(s);
                            let (lock, ready) = &*held;
                            let mut phase = lock.lock().unwrap();
                            while *phase == 0 {
                                let (next, timed) =
                                    ready.wait_timeout(phase, Duration::from_secs(5)).unwrap();
                                phase = next;
                                assert!(!timed.timed_out(), "target did not reach held guard");
                            }
                            fs::write(&target, b"guard-edited").unwrap();
                            *phase = 2;
                            ready.notify_all();
                            return Ok(());
                        }
                    }
                    if when == "joined" && matches!(event, Event::AfterJoin) {
                        fs::write(&target, b"guard-edited").unwrap();
                        s.3 = true;
                    }
                }
                if let Event::BeforeMutation(index) = event {
                    assert_eq!(index, s.2);
                    s.2 += 1;
                }
                let wait = when == "held"
                    && s.0
                    && s.1 == 1
                    && matches!(&event,Event::BeforeRead(path) if path.as_str()==format!("guards/{role:03}.bin"));
                drop(s);
                if wait {
                    let (lock, ready) = &*held;
                    let mut released = lock.lock().unwrap();
                    *released = 1;
                    ready.notify_all();
                    while *released != 2 {
                        let (next, timed) = ready
                            .wait_timeout(released, Duration::from_secs(5))
                            .unwrap();
                        released = next;
                        assert!(!timed.timed_out(), "sibling failed to release held guard");
                    }
                }
                Ok(())
            });
            let publisher = Publisher::default();
            maintenance_parallel::command_scope(|| {
                let error = f
                    .engine
                    .apply(&f.writer, &prepared.prepared, &Validator, &publisher)
                    .unwrap_err();
                assert_eq!(error.code, ErrorCode::ContentConflict);
                assert_eq!(
                    error.message,
                    format!(
                        "change conflict during read precondition: guards/{role:03}.bin; unfamiliar bytes preserved"
                    )
                );
                assert!(maintenance_parallel::metrics().max_active >= 2);
                Ok(())
            })
            .unwrap();
            let s = state.lock().unwrap();
            assert!(s.3);
            let reads = actual_reads.lock().unwrap();
            assert_eq!(reads.len(), if when == "joined" { 9 } else { 3 });
            assert_eq!(
                reads.values().map(|(_, bytes)| *bytes).sum::<usize>(),
                reads.len() * 12
            );
            for pass in 1..=if when == "joined" { 3 } else { 1 } {
                assert_eq!(reads.keys().filter(|(p, _)| *p == pass).count(), 3);
                for member in 0..3 {
                    let edited = member == role && (when != "joined" || pass == 3);
                    assert_eq!(
                        reads[&(pass, format!("guards/{member:03}.bin"))],
                        (
                            Some(
                                *blake3::hash(if edited {
                                    b"guard-edited"
                                } else {
                                    b"guard-before"
                                })
                                .as_bytes()
                            ),
                            12
                        ),
                        "exact observed bytes at {when}, pass {pass}, guard {member}"
                    );
                }
            }
            assert_eq!(
                fs::read(f.root().path().join(format!("guards/{role:03}.bin"))).unwrap(),
                b"guard-edited",
                "the conflicting external edit remains intact"
            );
            assert_eq!(s.2, if when == "joined" { 1 } else { 0 });
            assert_eq!(publisher.0.load(Ordering::SeqCst), 0);
            assert_eq!(
                fs::read(f.root().path().join("targets/000.bin")).unwrap(),
                if when == "joined" { b"new" } else { b"old" }
            );
            assert_eq!(
                fs::read(f.root().path().join("targets/001.bin")).unwrap(),
                b"old"
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn apply_guard_fifo_and_parent_symlink_races_refuse_and_preserve_targets() {
    use std::os::unix::ffi::OsStrExt;
    for fifo in [true, false] {
        let f = Fixture::new(true);
        let prepared = f.engine.prepare(&f.writer, f.proposal(2, 2)).unwrap();
        let target = f.root().path().join("guards/000.bin");
        let changed = target.clone();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("000.bin"), b"external").unwrap();
        let external = outside.path().to_owned();
        let _race = crate::vault::maintenance_read_tests::install_before_open(target, move || {
            if fifo {
                fs::remove_file(&changed).unwrap();
                let name = std::ffi::CString::new(changed.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            } else {
                let parent = changed.parent().unwrap();
                fs::rename(parent, parent.with_extension("old")).unwrap();
                std::os::unix::fs::symlink(&external, parent).unwrap();
            }
        });
        let publisher = Publisher::default();
        maintenance_parallel::command_scope(|| {
            assert_eq!(
                f.engine
                    .apply(&f.writer, &prepared.prepared, &Validator, &publisher)
                    .unwrap_err()
                    .code,
                ErrorCode::ContentConflict
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(publisher.0.load(Ordering::SeqCst), 0);
        for i in 0..2 {
            assert_eq!(
                fs::read(f.root().path().join(format!("targets/{i:03}.bin"))).unwrap(),
                b"old"
            );
        }
        assert_eq!(
            fs::read(outside.path().join("000.bin")).unwrap(),
            b"external"
        );
    }
}

use super::*;
use crate::vault::{DirectorySync, DurableIo, NativeIo};
use std::{fs::File, io, path::Path, sync::Arc, time::Duration};

fn fixture(
    retained: bool,
    adapter: Option<Arc<dyn DurableIo>>,
) -> (tempfile::TempDir, ChangeEngine, WriterPermit) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    let vault = VaultFs::new(root.clone());
    let writer = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    if retained {
        crate::storage::cleanup(&vault, &writer, &crate::storage::StorageOptions::default())
            .unwrap();
    }
    let vault = adapter.map_or(vault, |adapter| VaultFs::with_io(root, adapter));
    (temp, ChangeEngine::new(vault).unwrap(), writer)
}
fn draft(old: &[u8], after: [&[u8]; 2]) -> ChangeDraft {
    ChangeDraft {
        title: "Bounded independent retention".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: ["a.bin", "b.bin"]
            .into_iter()
            .zip(after)
            .map(|(path, bytes)| ExpectedWrite {
                target: VaultRelativePath::new(path).unwrap(),
                expected: ExpectedState::Hash(Blake3Hash::digest(old)),
                proposed: Some(bytes.to_vec()),
                apply_after: vec![],
            })
            .collect(),
    }
}
#[test]
fn preparation_retains_exact_bytes_in_both_layouts_and_deduplicates_objects() {
    for retained in [false, true] {
        let (temp, engine, writer) = fixture(retained, None);
        for path in ["a.bin", "b.bin"] {
            fs::write(temp.path().join(path), b"old").unwrap();
        }
        let inspection = crate::maintenance_parallel::command_scope(|| {
            let inspection = engine.prepare(&writer, draft(b"old", [b"new", b"new"]))?;
            assert_eq!(
                crate::maintenance_parallel::metrics().completed,
                if retained { 2 } else { 4 }
            );
            Ok(inspection)
        })
        .unwrap();
        assert_eq!(inspection.manifest.version, if retained { 2 } else { 1 });
        assert_eq!(inspection.journal.status, Some(ChangeStatus::Prepared));
        if retained {
            assert_eq!(
                inspection.manifest.operations[0].before_payload,
                inspection.manifest.operations[1].before_payload
            );
            assert_eq!(
                inspection.manifest.operations[0].after_payload,
                inspection.manifest.operations[1].after_payload
            );
        }
        for (index, op) in inspection.manifest.operations.iter().enumerate() {
            assert_eq!(
                fs::read(temp.path().join(op.target.as_str())).unwrap(),
                b"old"
            );
            assert_eq!(
                engine
                    .verify_payload(
                        &inspection.prepared.change_id,
                        index,
                        "before",
                        &op.target,
                        &op.before,
                        &op.before_payload
                    )
                    .unwrap()
                    .unwrap(),
                b"old"
            );
            assert_eq!(
                engine
                    .verify_payload(
                        &inspection.prepared.change_id,
                        index,
                        "proposed",
                        &op.target,
                        &op.after,
                        &op.after_payload
                    )
                    .unwrap()
                    .unwrap(),
                b"new"
            );
        }
    }
}

struct FailWrite;
impl DurableIo for FailWrite {
    fn create_stage(&self, path: &Path) -> io::Result<File> {
        NativeIo.create_stage(path)
    }
    fn create_private_stage(&self, path: &Path) -> io::Result<File> {
        NativeIo.create_private_stage(path)
    }
    fn create_private_directory(&self, path: &Path) -> io::Result<()> {
        NativeIo.create_private_directory(path)
    }
    fn open_append(&self, path: &Path) -> io::Result<File> {
        NativeIo.open_append(path)
    }
    fn truncate_file(&self, file: &File, length: u64) -> io::Result<()> {
        NativeIo.truncate_file(file, length)
    }
    fn write_stage(&self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        NativeIo.write_stage(file, bytes)?;
        if bytes == b"fail" {
            Err(io::Error::other("injected disk-full write failure"))
        } else {
            Ok(())
        }
    }
    fn sync_file(&self, file: &File) -> io::Result<()> {
        NativeIo.sync_file(file)
    }
    fn replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        NativeIo.replace(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        NativeIo.remove(path)
    }
    fn create_directory(&self, path: &Path) -> io::Result<()> {
        NativeIo.create_directory(path)
    }
    fn sync_directory(&self, path: &Path) -> io::Result<DirectorySync> {
        NativeIo.sync_directory(path)
    }
}
#[test]
fn staging_failure_drains_successes_without_prepared_authority_or_canonical_mutation() {
    for retained in [false, true] {
        let (temp, engine, writer) = fixture(retained, Some(Arc::new(FailWrite)));
        for path in ["a.bin", "b.bin"] {
            fs::write(temp.path().join(path), b"old").unwrap();
        }
        crate::maintenance_parallel::command_scope(|| {
            assert!(
                engine
                    .prepare(&writer, draft(b"old", [b"ok", b"fail"]))
                    .is_err()
            );
            let metrics = crate::maintenance_parallel::metrics();
            assert_eq!(metrics.completed, if retained { 3 } else { 4 });
            assert_eq!(metrics.failed, 1);
            Ok(())
        })
        .unwrap();
        for path in ["a.bin", "b.bin"] {
            assert_eq!(fs::read(temp.path().join(path)).unwrap(), b"old");
        }
        assert!(engine.change_ids().unwrap().is_empty());
        fn no_temps(path: &Path) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                assert!(
                    !entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".lwiki-stage-")
                );
                if entry.file_type().unwrap().is_dir() {
                    no_temps(&entry.path());
                }
            }
        }
        no_temps(temp.path());
    }
}

#[test]
fn stable_multi_guard_errors_use_manifest_order_after_all_reads_join() {
    let (temp, engine, writer) = fixture(false, None);
    for path in ["a.bin", "b.bin"] {
        fs::write(temp.path().join(path), b"old").unwrap();
    }
    let mut proposal = draft(b"old", [b"new", b"new"]);
    proposal.operations.clear();
    proposal.read_preconditions = ["b.bin", "a.bin"]
        .into_iter()
        .map(|path| ReadDependency {
            path: VaultRelativePath::new(path).unwrap(),
            expected: ExpectedState::Hash(Blake3Hash::digest(b"old")),
        })
        .collect();
    let inspection = engine.prepare(&writer, proposal).unwrap();
    for path in ["a.bin", "b.bin"] {
        fs::write(temp.path().join(path), b"edit").unwrap();
    }
    crate::maintenance_parallel::command_scope(|| {
        let error = engine
            .verify_read_preconditions(
                &writer,
                &inspection.manifest,
                &inspection.prepared.manifest_hash,
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert_eq!(error.message, "read precondition: a.bin");
        assert_eq!(crate::maintenance_parallel::metrics().completed, 2);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        journal::load_journal(
            engine.fs(),
            &inspection.manifest,
            &inspection.prepared.manifest_hash
        )
        .unwrap()
        .status,
        Some(ChangeStatus::Prepared)
    );
}

// Narrow NativeIo adapter following the existing preparation and owned-SIGKILL
// helpers. Every cut targets a real call; only retained storage uses worker overlap.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetentionCut {
    Write,
    Fsync,
    Directory,
    BeforeManifest,
    AfterManifest,
    BeforePrepared,
    AfterPrepared,
    StagingDeath,
}
#[cfg(unix)]
impl RetentionCut {
    fn name(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Fsync => "fsync",
            Self::Directory => "directory",
            Self::BeforeManifest => "before_manifest",
            Self::AfterManifest => "after_manifest",
            Self::BeforePrepared => "before_prepared",
            Self::AfterPrepared => "after_prepared",
            Self::StagingDeath => "staging_death",
        }
    }
    fn parse(name: &str) -> Self {
        [
            Self::Write,
            Self::Fsync,
            Self::Directory,
            Self::BeforeManifest,
            Self::AfterManifest,
            Self::BeforePrepared,
            Self::AfterPrepared,
            Self::StagingDeath,
        ]
        .into_iter()
        .find(|c| c.name() == name)
        .unwrap()
    }
}
#[cfg(unix)]
#[derive(Default)]
struct StageSeen {
    roles: std::collections::BTreeSet<u8>,
    threads: std::collections::BTreeSet<String>,
    worker_roles: std::collections::BTreeSet<u8>,
}
#[cfg(unix)]
struct RetentionCutIo {
    root: std::path::PathBuf,
    retained: bool,
    cut: RetentionCut,
    kill: bool,
    seen: (std::sync::Mutex<StageSeen>, std::sync::Condvar),
    designated: std::sync::Mutex<Option<(u64, u64)>>,
    journal: std::sync::Mutex<Option<(u64, u64)>>,
    installed: std::sync::atomic::AtomicBool,
    fired: std::sync::atomic::AtomicBool,
}
#[cfg(unix)]
impl RetentionCutIo {
    fn new(root: &Path, retained: bool, cut: RetentionCut, kill: bool) -> Self {
        Self {
            root: root.to_owned(),
            retained,
            cut,
            kill,
            seen: Default::default(),
            designated: Default::default(),
            journal: Default::default(),
            installed: false.into(),
            fired: false.into(),
        }
    }
    fn identity(file: &File) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata().unwrap();
        (m.dev(), m.ino())
    }
    fn reach(&self, cut: RetentionCut) -> io::Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.cut != cut || self.fired.swap(true, SeqCst) {
            return Ok(());
        }
        let seen = self.seen.0.lock().unwrap();
        assert_eq!(seen.roles.len(), 2);
        if self.retained {
            assert_eq!(seen.worker_roles.len(), 2);
            assert_eq!(seen.threads.len(), 2);
        } else {
            assert!(seen.worker_roles.is_empty());
            assert_eq!(seen.threads.len(), 1);
        }
        if self.kill {
            fs::write(self.root.join(".wiki/retention-cut-ready.json"),serde_json::to_vec(&serde_json::json!({"cut":cut.name(),"retained":self.retained,"reached":true,"roles":seen.roles,"worker_roles":seen.worker_roles,"threads":seen.threads})).unwrap()).unwrap();
            drop(seen);
            // Parent owns kill/reap; keep this actual reached call held.
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
        Err(io::Error::other(format!(
            "reached retention {} failure",
            cut.name()
        )))
    }
    fn overlap(&self, role: u8) -> io::Result<()> {
        let worker = crate::storage::maintenance_activation::worker_context_active();
        let (seen, ready) = (&self.seen.0, &self.seen.1);
        let mut state = seen.lock().unwrap();
        state.roles.insert(role);
        state
            .threads
            .insert(format!("{:?}", std::thread::current().id()));
        if worker {
            state.worker_roles.insert(role);
        }
        ready.notify_all();
        if self.retained {
            assert!(worker);
            while state.roles.len() != 2 {
                let (next, timed) = ready.wait_timeout(state, Duration::from_secs(5)).unwrap();
                state = next;
                if timed.timed_out() {
                    return Err(io::Error::other(
                        "second retained worker did not reach stage write",
                    ));
                }
            }
        }
        Ok(())
    }
}
#[cfg(unix)]
const RETAIN_A: &[u8] = b"A retention payload";
#[cfg(unix)]
const RETAIN_B: &[u8] = b"B retention payload";
#[cfg(unix)]
impl DurableIo for RetentionCutIo {
    fn create_stage(&self, p: &Path) -> io::Result<File> {
        NativeIo.create_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> io::Result<File> {
        NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> io::Result<()> {
        NativeIo.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> io::Result<File> {
        let f = NativeIo.open_append(p)?;
        if p.extension().is_some_and(|n| n == "journal") {
            *self.journal.lock().unwrap() = Some(Self::identity(&f));
        }
        Ok(f)
    }
    fn truncate_file(&self, f: &File, n: u64) -> io::Result<()> {
        NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, bytes: &[u8]) -> io::Result<()> {
        if bytes == RETAIN_A || bytes == RETAIN_B {
            let role = if bytes == RETAIN_A { 0 } else { 1 };
            self.overlap(role)?;
            if role == 1 {
                *self.designated.lock().unwrap() = Some(Self::identity(f));
            }
            NativeIo.write_stage(f, bytes)?;
            if role == 1 {
                self.reach(RetentionCut::Write)?;
                self.reach(RetentionCut::StagingDeath)?;
            }
            return Ok(());
        }
        if bytes.starts_with(b"LWJNL001") {
            self.reach(RetentionCut::BeforePrepared)?;
        }
        NativeIo.write_stage(f, bytes)
    }
    fn sync_file(&self, f: &File) -> io::Result<()> {
        NativeIo.sync_file(f)?;
        let id = Self::identity(f);
        if *self.designated.lock().unwrap() == Some(id) {
            self.reach(RetentionCut::Fsync)?;
        }
        if *self.journal.lock().unwrap() == Some(id) {
            self.reach(RetentionCut::AfterPrepared)?;
        }
        Ok(())
    }
    fn replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        let note = to.file_name().is_some_and(|n| n == "change.md");
        if note {
            self.reach(RetentionCut::BeforeManifest)?;
        }
        NativeIo.replace(from, to)?;
        if note {
            self.reach(RetentionCut::AfterManifest)?;
        } else {
            self.installed.store(true, SeqCst);
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
        let result = NativeIo.sync_directory(p)?;
        if self.installed.load(std::sync::atomic::Ordering::SeqCst) {
            self.reach(RetentionCut::Directory)?;
        }
        Ok(result)
    }
}
#[cfg(unix)]
fn absent_draft() -> ChangeDraft {
    ChangeDraft {
        title: "Two distinct retained temporary writes".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: [("a.bin", RETAIN_A), ("b.bin", RETAIN_B)]
            .into_iter()
            .map(|(name, bytes)| ExpectedWrite {
                target: VaultRelativePath::new(name).unwrap(),
                expected: ExpectedState::Absent,
                proposed: Some(bytes.to_vec()),
                apply_after: vec![],
            })
            .collect(),
    }
}
#[cfg(unix)]
fn retained_snapshot(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let e = entry.unwrap();
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                visit(root, &p, out);
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_owned(),
                    fs::read(p).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}
#[cfg(unix)]
struct NoActivation;
#[cfg(unix)]
impl GraphValidator for NoActivation {
    fn validate(&self, _: &VaultFs, _: &ValidationInput) -> Result<ValidatedGraph> {
        panic!("preparation recovery cannot validate/apply")
    }
}
#[cfg(unix)]
impl PublicationBackend for NoActivation {
    fn check_available(&self) -> Result<()> {
        panic!("preparation recovery cannot publish")
    }
    fn publish(
        &self,
        _: &VaultFs,
        _: &PublicationPermit<'_>,
        _: &ValidationInput,
    ) -> Result<crate::domain::ReadSnapshot> {
        panic!("preparation recovery cannot publish")
    }
}
#[cfg(unix)]
fn assert_retention_cut(root: &crate::vault::VaultRoot, cut: RetentionCut) {
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let writer = WriterPermit::acquire(root, Duration::ZERO).unwrap();
    assert!(!root.path().join("a.bin").exists());
    assert!(!root.path().join("b.bin").exists());
    assert_eq!(
        fs::read(root.path().join("unknown.bin")).unwrap(),
        b"unfamiliar retained fixture bytes"
    );
    let before = retained_snapshot(root.path());
    let recovery = engine
        .recover(&writer, &NoActivation, &NoActivation)
        .unwrap();
    assert!(recovery.changes.is_empty());
    for (path, bytes) in before {
        if path.to_string_lossy().ends_with("writer.lock") {
            continue;
        }
        assert_eq!(fs::read(root.path().join(path)).unwrap(), bytes);
    }
    let inventory =
        crate::storage::inventory(engine.fs(), &crate::storage::StorageOptions::default()).unwrap();
    assert!(inventory.complete);
    let temps: Vec<_> = inventory
        .files
        .iter()
        .filter(|f| f.path.as_str().contains(".lwiki-stage-"))
        .collect();
    assert!(temps.iter().all(|f| f.class == "recovery_accounting"));
    if cut == RetentionCut::StagingDeath {
        assert!(!temps.is_empty());
    }
    let ids = engine.change_ids().unwrap();
    if matches!(
        cut,
        RetentionCut::AfterManifest | RetentionCut::BeforePrepared | RetentionCut::AfterPrepared
    ) {
        assert_eq!(ids.len(), 1);
        assert_eq!(
            engine.inspect(&ids[0]).unwrap().status,
            ChangeStatus::Prepared
        );
        if cut == RetentionCut::AfterPrepared {
            let inspection = engine.inspect(&ids[0]).unwrap();
            assert_eq!(inspection.journal.frames.len(), 1);
            assert!(matches!(
                inspection.journal.frames[0].event,
                ChangeEvent::Prepared
            ));
        }
    } else {
        assert!(ids.is_empty());
        assert!(
            !engine.incomplete_preparations().unwrap().is_empty()
                || root.path().join(".wiki/retained/objects/blake3").exists()
        );
    }
}
#[test]
#[cfg(unix)]
fn reached_retention_write_fsync_directory_and_authority_errors_drain_both_layouts() {
    for retained in [false, true] {
        for cut in [
            RetentionCut::Write,
            RetentionCut::Fsync,
            RetentionCut::Directory,
            RetentionCut::BeforeManifest,
            RetentionCut::AfterManifest,
            RetentionCut::BeforePrepared,
            RetentionCut::AfterPrepared,
        ] {
            let (temp, plain, writer) = fixture(retained, None);
            fs::write(
                temp.path().join("unknown.bin"),
                b"unfamiliar retained fixture bytes",
            )
            .unwrap();
            let adapter = Arc::new(RetentionCutIo::new(temp.path(), retained, cut, false));
            let engine =
                ChangeEngine::new(VaultFs::with_io(plain.fs().root().clone(), adapter.clone()))
                    .unwrap();
            crate::maintenance_parallel::command_scope(|| {
                assert!(engine.prepare(&writer, absent_draft()).is_err());
                assert!(adapter.fired.load(std::sync::atomic::Ordering::SeqCst));
                let metrics = crate::maintenance_parallel::metrics();
                assert_eq!(metrics.completed, 2);
                assert_eq!(
                    metrics.failed,
                    u64::from(matches!(cut, RetentionCut::Write | RetentionCut::Fsync))
                );
                if retained {
                    assert!(metrics.max_active >= 2);
                } else {
                    assert_eq!(metrics.max_active, 1);
                }
                Ok(())
            })
            .unwrap();
            drop(writer);
            assert_retention_cut(engine.fs().root(), cut);
            eprintln!(
                "retention error cut={} retained={retained} reached=true",
                cut.name()
            );
        }
    }
}

#[test]
#[cfg(unix)]
#[ignore = "owned by enabled retention-specific SIGKILL wrapper"]
fn concurrent_retention_crash_child() {
    let root =
        crate::vault::VaultRoot::explicit(std::env::var_os("LWIKI_RETENTION_CHILD_ROOT").unwrap())
            .unwrap();
    let retained = std::env::var("LWIKI_RETENTION_CHILD_LAYOUT").unwrap() == "retained";
    let cut = RetentionCut::parse(&std::env::var("LWIKI_RETENTION_CHILD_CUT").unwrap());
    let adapter = Arc::new(RetentionCutIo::new(root.path(), retained, cut, true));
    let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), adapter)).unwrap();
    let writer = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
    crate::maintenance_parallel::command_scope(|| {
        engine.prepare(&writer, absent_draft())?;
        Ok(())
    })
    .unwrap();
    panic!("retention child did not reach expected cut");
}
#[test]
#[cfg(unix)]
fn owned_sigkill_during_retained_staging_and_manifest_prepared_cuts_both_layouts() {
    use std::os::unix::process::ExitStatusExt;
    use std::time::Instant;
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    for retained in [false, true] {
        for cut in [
            RetentionCut::StagingDeath,
            RetentionCut::BeforeManifest,
            RetentionCut::AfterManifest,
            RetentionCut::BeforePrepared,
            RetentionCut::AfterPrepared,
        ] {
            let (temp, engine, writer) = fixture(retained, None);
            fs::write(
                temp.path().join("unknown.bin"),
                b"unfamiliar retained fixture bytes",
            )
            .unwrap();
            let root = engine.fs().root().clone();
            drop(writer);
            drop(engine);
            let name = format!(
                "{}::concurrent_retention_crash_child",
                module_path!().split_once("::").unwrap().1
            );
            let mut child = Child(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &name, "--ignored", "--nocapture"])
                    .env("LWIKI_RETENTION_CHILD_ROOT", root.path())
                    .env(
                        "LWIKI_RETENTION_CHILD_LAYOUT",
                        if retained { "retained" } else { "original" },
                    )
                    .env("LWIKI_RETENTION_CHILD_CUT", cut.name())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let marker = root.path().join(".wiki/retention-cut-ready.json");
            let deadline = Instant::now() + Duration::from_secs(15);
            let reached = loop {
                if let Ok(raw) = fs::read(&marker) {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) {
                        if v["cut"] == cut.name() && v["reached"] == true {
                            break v;
                        }
                    }
                }
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "retention child exited before {cut:?}"
                );
                assert!(
                    Instant::now() < deadline,
                    "retention child failed to reach {cut:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(reached["roles"].as_array().unwrap().len(), 2);
            assert_eq!(
                reached["worker_roles"].as_array().unwrap().len(),
                if retained { 2 } else { 0 }
            );
            assert_eq!(
                reached["threads"].as_array().unwrap().len(),
                if retained { 2 } else { 1 }
            );
            child.0.kill().unwrap();
            assert_eq!(child.0.wait().unwrap().signal(), Some(9));
            assert_retention_cut(&root, cut);
            eprintln!(
                "retention SIGKILL cut={} retained={retained} reached=true reaped=true",
                cut.name()
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn conflicting_existing_equal_destination_refuses_without_overwriting_immutable_bytes() {
    let (temp, engine, writer) = fixture(true, None);
    let object = crate::storage::layout::object_path(&Blake3Hash::digest(RETAIN_A)).unwrap();
    let physical = temp.path().join(object.as_str());
    fs::create_dir_all(physical.parent().unwrap()).unwrap();
    let foreign = vec![b'X'; RETAIN_A.len()];
    fs::write(&physical, &foreign).unwrap();
    crate::maintenance_parallel::command_scope(|| {
        let error = engine.prepare(&writer, absent_draft()).unwrap_err();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert_eq!(error.message, "immutable storage bytes changed");
        assert_eq!(crate::maintenance_parallel::metrics().completed, 2);
        assert_eq!(crate::maintenance_parallel::metrics().failed, 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(fs::read(physical).unwrap(), foreign);
    assert!(!temp.path().join("a.bin").exists());
    assert!(!temp.path().join("b.bin").exists());
    assert!(engine.change_ids().unwrap().is_empty());
}

#[test]
#[cfg(unix)]
fn retention_authority_fifo_and_parent_symlink_races_refuse_without_installing_siblings() {
    use std::os::unix::ffi::OsStrExt;
    for fifo in [true, false] {
        let (temp, engine, writer) = fixture(true, None);
        let object = crate::storage::layout::object_path(&Blake3Hash::digest(RETAIN_A)).unwrap();
        let target = temp.path().join(object.as_str());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, RETAIN_A).unwrap();
        let changed = target.clone();
        let outside = tempfile::tempdir().unwrap();
        let external = outside.path().to_owned();
        fs::write(
            external.join(target.file_name().unwrap()),
            b"unrelated external bytes",
        )
        .unwrap();
        let _hook = crate::vault::maintenance_read_tests::install_before_open(target, move || {
            if fifo {
                fs::remove_file(&changed).unwrap();
                let name = std::ffi::CString::new(changed.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            } else {
                let parent = changed.parent().unwrap();
                fs::rename(parent, parent.with_extension("saved")).unwrap();
                std::os::unix::fs::symlink(&external, parent).unwrap();
            }
        });
        crate::maintenance_parallel::command_scope(|| {
            assert!(engine.prepare(&writer, absent_draft()).is_err());
            let metrics = crate::maintenance_parallel::metrics();
            assert_eq!(metrics.completed, 2);
            if fifo {
                assert_eq!(metrics.failed, 1);
            } else {
                assert!((1..=2).contains(&metrics.failed));
            }
            Ok(())
        })
        .unwrap();
        assert!(!temp.path().join("a.bin").exists());
        assert!(!temp.path().join("b.bin").exists());
        assert!(engine.change_ids().unwrap().is_empty());
        let sibling = crate::storage::layout::object_path(&Blake3Hash::digest(RETAIN_B)).unwrap();
        assert!(!temp.path().join(sibling.as_str()).exists());
        assert_eq!(
            fs::read(outside.path().join(changed_name(&object))).unwrap(),
            b"unrelated external bytes"
        );
    }
    fn changed_name(path: &VaultRelativePath) -> &str {
        path.as_str().rsplit('/').next().unwrap()
    }
}

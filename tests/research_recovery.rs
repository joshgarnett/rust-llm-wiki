use lwiki::{
    app::{OfflineApp, OperationOptions},
    catalog::{Catalog, CatalogGraphValidator},
    changes::ChangeEngine,
    domain::{CitationRef, ErrorCode, RecordId},
    research::{self, ResearchPacket, ResearchScope, ResearchStage},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore, SourceView},
    vault::{
        VaultFs, VaultRoot, WriterPermit,
        fs::{DirectorySync, DurableIo, NativeIo},
    },
};
use serde_json::json;
use std::{
    fs::{self, File},
    io,
    path::Path,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn scope(sources: Vec<RecordId>) -> ResearchScope {
    ResearchScope {
        question: "Where is Cedar's backup?".into(),
        urls: vec![],
        exclusions: vec![],
        source_ids: sources,
        source_ranges: vec![],
        offline: false,
        max_rounds: 3,
        max_sources: 2,
        max_source_bytes: 65536,
    }
}
fn collect(packet: &ResearchPacket, key: &str, content: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema": "lwiki.research-submission.v1",
        "run_id": packet.run_id,
        "packet_fingerprint": packet.packet_fingerprint,
        "response": {"stage":"collect_sources", "sources":[{
            "key":key, "title":"Cedar backup", "origin":"https://example.invalid/cedar",
            "content":content, "provenance":"Host supplied text"
        }], "gaps":[]}
    }))
    .unwrap()
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: VaultRoot,
    fs: VaultFs,
}
impl Fixture {
    fn new(io: Option<Arc<dyn DurableIo>>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: \"1\"\nwiki_id: vault_research_recovery\nwiki_kind: vault\ntitle: Recovery fixture\n---\n").unwrap();
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let fs = io.map_or_else(
            || VaultFs::new(root.clone()),
            |io| VaultFs::with_io(root.clone(), io),
        );
        Self {
            _temp: temp,
            root,
            fs,
        }
    }
    fn app(&self) -> OfflineApp {
        OfflineApp::new(
            self.fs.clone(),
            OperationOptions {
                lock_timeout_ms: 30_000,
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn count(&self, prefix: &str, suffix: &str) -> usize {
        self.root
            .scan_markdown()
            .unwrap()
            .into_iter()
            .filter(|path| path.as_str().starts_with(prefix) && path.as_str().ends_with(suffix))
            .count()
    }
    fn capture_many(&self, count: usize) -> Vec<RecordId> {
        self.capture_many_with(count, |n| format!("Cedar backup source number {n}."))
    }
    fn capture_many_with(&self, count: usize, content: impl Fn(usize) -> String) -> Vec<RecordId> {
        let store = SourceStore::new(self.fs.clone());
        let mut ids = vec![];
        let mut combined: Option<lwiki::changes::ChangeDraft> = None;
        for n in 0..count {
            let plan = store
                .plan_capture(CaptureRequest {
                    title: format!("Existing Cedar source {n}"),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: format!("fixture-{n}.txt"),
                    original: content(n).into_bytes(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: Some("text/plain".into()),
                })
                .unwrap();
            ids.push(plan.source_id);
            let draft = plan.draft.unwrap();
            if let Some(combined) = &mut combined {
                combined.operations.extend(draft.operations);
                combined.read_preconditions.extend(draft.read_preconditions);
            } else {
                combined = Some(draft);
            }
        }
        let engine = ChangeEngine::new(self.fs.clone()).unwrap();
        let writer = WriterPermit::acquire(&self.root, Duration::from_secs(2)).unwrap();
        let prepared = engine.prepare(&writer, combined.unwrap()).unwrap().prepared;
        engine
            .apply(
                &writer,
                &prepared,
                &CatalogGraphValidator,
                &Catalog::new(self.fs.clone(), id("vault_research_recovery")),
            )
            .unwrap();
        ids
    }
}

#[test]
fn exact_byte_limit_names_every_omitted_explicit_source_on_start_and_import() {
    let f = Fixture::new(None);
    let sources = f.capture_many_with(20, |n| {
        let prefix = format!("Cedar backup source number {n}. ");
        format!("{prefix}{}", "x".repeat(4096 - prefix.len()))
    });
    let app = f.app();
    let started = research::start(
        &app,
        scope(sources.clone()),
        Some(id("run_research_exact_byte_limit")),
        false,
    )
    .unwrap();
    let initial = started.packet.as_ref().unwrap();
    assert_eq!(initial.passages.len(), 16);
    assert_eq!(
        initial
            .passages
            .iter()
            .map(|p| p.quote.len())
            .sum::<usize>(),
        65536
    );
    assert_eq!(initial.warnings.len(), 1);
    assert!(initial.warnings[0].contains("4 candidate passages omitted"));
    for source in &sources[16..] {
        assert!(initial.warnings[0].contains(&source.to_string()));
    }

    let imported = research::import(
        &app,
        &serde_json::to_vec(&json!({
            "schema":"lwiki.research-submission.v1",
            "run_id":initial.run_id,
            "packet_fingerprint":initial.packet_fingerprint,
            "response":{"stage":"collect_sources","sources":[
                {"key":"new_1","title":"New one","origin":"local fixture","content":"New Cedar source one.","provenance":"fixture"},
                {"key":"new_2","title":"New two","origin":"local fixture","content":"New Cedar source two.","provenance":"fixture"}
            ],"gaps":[]}
        }))
        .unwrap(),
    )
    .unwrap();
    let next = imported.packet.as_ref().unwrap();
    assert_eq!(next.passages.len(), 17);
    assert_eq!(next.passages[0].quote, "New Cedar source one.");
    assert_eq!(next.passages[1].quote, "New Cedar source two.");
    assert!(next.warnings[0].contains("5 candidate passages omitted"));
    for source in &sources[15..] {
        assert!(next.warnings[0].contains(&source.to_string()));
    }
}

/// Fault the real durable adapter at the head CAS after child outputs and the
/// import receipt have been written. The adapter fails once, then permits replay.
struct FailHeadOnce {
    armed: AtomicBool,
    fired: AtomicUsize,
}
impl FailHeadOnce {
    fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
            fired: AtomicUsize::new(0),
        }
    }
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
}
impl DurableIo for FailHeadOnce {
    fn create_stage(&self, path: &Path) -> io::Result<File> {
        NativeIo.create_stage(path)
    }
    fn open_append(&self, path: &Path) -> io::Result<File> {
        NativeIo.open_append(path)
    }
    fn truncate_file(&self, file: &File, length: u64) -> io::Result<()> {
        NativeIo.truncate_file(file, length)
    }
    fn write_stage(&self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        NativeIo.write_stage(file, bytes)
    }
    fn sync_file(&self, file: &File) -> io::Result<()> {
        NativeIo.sync_file(file)
    }
    fn replace(&self, staged: &Path, target: &Path) -> io::Result<()> {
        if target.file_name().and_then(|name| name.to_str()) == Some("research.md")
            && self.armed.swap(false, Ordering::SeqCst)
        {
            self.fired.fetch_add(1, Ordering::SeqCst);
            return Err(io::Error::other(
                "injected research head replacement interruption",
            ));
        }
        NativeIo.replace(staged, target)
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
fn competing_submissions_commit_exactly_one_packet_winner() {
    let f = Fixture::new(None);
    let started = research::start(
        &f.app(),
        scope(vec![]),
        Some(id("run_research_race")),
        false,
    )
    .unwrap();
    let packet = started.packet.unwrap();
    let first = collect(&packet, "first", "Cedar backs up on Friday.");
    let second = collect(&packet, "second", "Cedar backs up on Sunday.");
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .map(|submission| {
            let fs = f.fs.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let app = OfflineApp::new(
                    fs,
                    OperationOptions {
                        lock_timeout_ms: 30_000,
                        ..Default::default()
                    },
                )
                .unwrap();
                barrier.wait();
                research::import(&app, &submission)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| result
                .as_ref()
                .is_err_and(|error| error.code == ErrorCode::ContentConflict))
            .count(),
        1
    );
    assert_eq!(f.count("sources/source_", "/source.md"), 1);
    assert_eq!(f.count("runs/run_research_race/outputs/receipt_", ".md"), 1);
    let status = research::status(&f.app(), &id("run_research_race")).unwrap();
    assert_eq!(status["imports"], 1);
    assert_eq!(status["captured_sources"], 1);
    assert_eq!(
        status["captured_bytes"],
        "Cedar backs up on Friday.".len() as u64
    );
}

#[test]
fn exact_retry_recovers_interrupted_apply_with_one_source_and_receipt() {
    let io = Arc::new(FailHeadOnce::new());
    let f = Fixture::new(Some(io.clone()));
    let app = f.app();
    let started =
        research::start(&app, scope(vec![]), Some(id("run_research_retry")), false).unwrap();
    let bytes = collect(
        started.packet.as_ref().unwrap(),
        "source_1",
        "Cedar backs up on Friday.",
    );
    io.arm();
    assert!(research::import(&app, &bytes).is_err());
    assert_eq!(io.fired.load(Ordering::SeqCst), 1);
    let recovered = research::import(&app, &bytes).unwrap();
    assert!(recovered.reused);
    assert_eq!(
        recovered.packet.as_ref().unwrap().stage,
        ResearchStage::Answer
    );
    assert_eq!(recovered.imported_sources.len(), 1);
    assert_eq!(f.count("sources/source_", "/source.md"), 1);
    assert_eq!(
        f.count("runs/run_research_retry/outputs/receipt_", ".md"),
        1
    );
    let status = research::status(&app, &started.run_id).unwrap();
    assert_eq!(status["imports"], 1);
    assert_eq!(status["captured_sources"], 1);
    assert_eq!(
        status["captured_bytes"],
        "Cedar backs up on Friday.".len() as u64
    );
}

#[test]
fn ordinary_resume_recovers_pending_import_before_returning_ready_packet() {
    let io = Arc::new(FailHeadOnce::new());
    let f = Fixture::new(Some(io.clone()));
    let app = f.app();
    let started =
        research::start(&app, scope(vec![]), Some(id("run_research_resume")), false).unwrap();
    let bytes = collect(
        started.packet.as_ref().unwrap(),
        "source_1",
        "Cedar backs up on Friday.",
    );
    io.arm();
    assert!(research::import(&app, &bytes).is_err());
    let reopened = f.app();
    let ready = research::resume(&reopened, &started.run_id, false).unwrap();
    assert!(ready.ready_to_import);
    assert_eq!(ready.packet.as_ref().unwrap().stage, ResearchStage::Answer);
    assert_eq!(ready.packet.as_ref().unwrap().passages.len(), 1);
    assert_eq!(f.count("sources/source_", "/source.md"), 1);
    assert_eq!(
        f.count("runs/run_research_resume/outputs/receipt_", ".md"),
        1
    );
    assert!(research::import(&reopened, &bytes).unwrap().reused);
}

#[test]
fn full_initial_packet_prioritizes_newly_captured_passage_and_warns() {
    let f = Fixture::new(None);
    let sources = f.capture_many(32);
    let app = f.app();
    let started =
        research::start(&app, scope(sources), Some(id("run_research_full")), false).unwrap();
    let initial = started.packet.as_ref().unwrap();
    assert_eq!(initial.passages.len(), 32);
    assert!(initial.warnings.is_empty());
    let imported = research::import(
        &app,
        &collect(initial, "fresh", "New Cedar backup evidence."),
    )
    .unwrap();
    let next = imported.packet.as_ref().unwrap();
    assert_eq!(next.stage, ResearchStage::Answer);
    assert_eq!(next.passages.len(), 32);
    assert!(
        next.warnings
            .iter()
            .any(|warning| warning.contains("1 candidate passages omitted"))
    );
    assert_eq!(next.passages[0].quote, "New Cedar backup evidence.");
    assert_eq!(next.passages[0].passage_id, "p1");
    let CitationRef::Source(reference) = &next.passages[0].citation else {
        panic!("source citation")
    };
    assert_eq!(reference.source_id, imported.imported_sources[0].source_id);
    let view = SourceView::from_fs_bounded(&f.fs, 64 * 1024 * 1024, 4096).unwrap();
    assert_eq!(
        view.verify(
            &next.passages[0].citation,
            lwiki::sources::CitationScope::Current
        )
        .unwrap()
        .quote,
        b"New Cedar backup evidence."
    );
    assert_eq!(next.remaining_sources, 1);
    assert_eq!(
        next.remaining_source_bytes,
        65536 - "New Cedar backup evidence.".len() as u64
    );
}

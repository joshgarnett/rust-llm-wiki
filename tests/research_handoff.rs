use lwiki::{
    app::{OfflineApp, OperationOptions},
    catalog::{Catalog, CatalogGraphValidator},
    changes::ChangeEngine,
    domain::{Blake3Hash, CitationRef, ErrorCode, RecordId},
    records::parse_note,
    research::{self, ResearchPacket, ResearchScope, ResearchStage},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

fn id(text: &str) -> RecordId {
    RecordId::new(text).unwrap()
}

struct Fixture {
    temp: tempfile::TempDir,
    fs: VaultFs,
}
impl Fixture {
    fn new() -> Self {
        Self::with_vault("vault_research_test")
    }
    fn with_vault(vault_id: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), format!("---\nwiki_schema: \"1\"\nwiki_id: {vault_id}\nwiki_kind: vault\ntitle: Research fixture\n---\n")).unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        Self { temp, fs }
    }
    fn app(&self, dry_run: bool, offline: bool) -> OfflineApp {
        OfflineApp::new(
            self.fs.clone(),
            OperationOptions {
                dry_run,
                offline,
                lock_timeout_ms: 5000,
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn apply(&self, draft: lwiki::changes::ChangeDraft) {
        let engine = ChangeEngine::new(self.fs.clone()).unwrap();
        let writer = WriterPermit::acquire(self.fs.root(), Duration::from_secs(2)).unwrap();
        let prepared = engine.prepare(&writer, draft).unwrap().prepared;
        engine
            .apply(
                &writer,
                &prepared,
                &CatalogGraphValidator,
                &Catalog::new(self.fs.clone(), id("vault_research_test")),
            )
            .unwrap();
    }
    fn capture(&self, content: &str) -> RecordId {
        let plan = SourceStore::new(self.fs.clone())
            .plan_capture(CaptureRequest {
                title: "Existing source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fixture.txt".into(),
                original: content.as_bytes().to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/plain".into()),
            })
            .unwrap();
        let source = plan.source_id;
        self.apply(plan.draft.unwrap());
        source
    }
    fn refresh(&self, source: &RecordId, content: &str) {
        let plan = SourceStore::new(self.fs.clone())
            .plan_refresh(
                source,
                CaptureRequest {
                    title: "Existing source".into(),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: "fixture.txt".into(),
                    original: content.as_bytes().to_vec(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: Some("text/plain".into()),
                },
            )
            .unwrap();
        self.apply(plan.draft.unwrap());
    }
    fn refresh_agent(&self, source: &RecordId, content: &str) {
        let plan = SourceStore::new(self.fs.clone())
            .plan_refresh(
                source,
                CaptureRequest {
                    title: "Cedar backup".into(),
                    origin_kind: SourceOrigin::AgentReport,
                    origin: "https://example.invalid/cedar".into(),
                    original: content.as_bytes().to_vec(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: Some("text/plain; charset=utf-8".into()),
                },
            )
            .unwrap();
        self.apply(plan.draft.unwrap());
    }
}
fn scope(offline: bool, sources: Vec<RecordId>) -> ResearchScope {
    ResearchScope {
        question: "Where is Cedar's backup?".into(),
        urls: vec![],
        exclusions: vec![],
        source_ids: sources,
        offline,
        max_rounds: 3,
        max_sources: 15,
        max_source_bytes: 524288,
    }
}
fn packet(value: &lwiki::research::ResearchOutcome) -> &ResearchPacket {
    let packet = value.packet.as_ref().unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/research-packet-v1.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&serde_json::to_value(packet).unwrap())
        .unwrap();
    packet
}
fn submission(packet: &ResearchPacket, response: Value) -> Vec<u8> {
    serde_json::to_vec(
        &json!({"schema":"lwiki.research-submission.v1","run_id":packet.run_id,
        "packet_fingerprint":packet.packet_fingerprint,"response":response}),
    )
    .unwrap()
}
fn collect(packet: &ResearchPacket, content: &str) -> Vec<u8> {
    submission(
        packet,
        json!({"stage":"collect_sources","sources":[{"key":"source_1","title":"Cedar backup",
        "origin":"https://example.invalid/cedar","content":content,"provenance":"Host supplied this text."}],"gaps":[]}),
    )
}
fn answer(packet: &ResearchPacket, passage_id: &str, follow_up: Option<&str>) -> Vec<u8> {
    submission(
        packet,
        json!({"stage":"answer","claims":[{"text":"Cedar backs up on Friday.","passage_ids":[passage_id]}],
        "gaps":[],"follow_up":follow_up}),
    )
}
fn tree(path: &Path) -> BTreeMap<PathBuf, (Blake3Hash, SystemTime)> {
    fn visit(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, (Blake3Hash, SystemTime)>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = entry.metadata().unwrap();
            out.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (
                    if meta.is_dir() {
                        Blake3Hash::digest([])
                    } else {
                        Blake3Hash::digest(fs::read(&path).unwrap())
                    },
                    meta.modified().unwrap(),
                ),
            );
            if meta.is_dir() {
                visit(root, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(path, path, &mut out);
    out
}
fn tree_except_writer_lock(path: &Path) -> BTreeMap<PathBuf, (Blake3Hash, SystemTime)> {
    let mut tree = tree(path);
    tree.remove(Path::new(".wiki/state/writer.lock"));
    tree
}

#[test]
fn collect_answer_cites_agent_report_and_retry_preserves_ids_and_counters() {
    let f = Fixture::new();
    let app = f.app(false, false);
    let started =
        research::start(&app, scope(false, vec![]), Some(id("run_cedar")), false).unwrap();
    assert!(started.persisted && started.ready_to_import && !started.network_used);
    assert_eq!(packet(&started).stage, ResearchStage::CollectSources);
    let first = collect(packet(&started), "Project Cedar backups run every Friday.");
    let imported = research::import(&app, &first).unwrap();
    assert_eq!(packet(&imported).stage, ResearchStage::Answer);
    assert_eq!(packet(&imported).passages.len(), 1);
    let passage = &packet(&imported).passages[0];
    assert_eq!(passage.passage_id, "p1");
    let CitationRef::Source(source_citation) = &passage.citation else {
        panic!("captured source citation required")
    };
    let note = parse_note(
        &fs::read(
            f.temp
                .path()
                .join(format!("sources/{}/source.md", source_citation.source_id)),
        )
        .unwrap(),
    );
    let source = note.canonical.as_ref().unwrap();
    assert_eq!(source.string("wiki_origin_kind"), Some("agent-report"));
    assert_eq!(
        source.string("wiki_origin"),
        Some("https://example.invalid/cedar")
    );
    let before_retry = research::status(&app, &started.run_id).unwrap();
    let repeated = research::import(&app, &first).unwrap();
    assert!(repeated.reused);
    assert_eq!(
        research::status(&app, &started.run_id).unwrap(),
        before_retry
    );
    assert_eq!(
        packet(&repeated).packet_fingerprint,
        packet(&imported).packet_fingerprint
    );
    assert_eq!(packet(&repeated).passages[0].citation, passage.citation);
    let different = collect(packet(&started), "Different Cedar source bytes.");
    assert_eq!(
        research::import(&app, &different).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    f.refresh_agent(
        &source_citation.source_id,
        "Project Cedar backups moved to Saturday.",
    );
    let historical_retry = research::import(&app, &first).unwrap();
    assert!(historical_retry.reused);
    assert_eq!(historical_retry.status, "import_already_committed");
    assert_eq!(historical_retry.freshness, "stale");
    assert!(!historical_retry.ready_to_import);
    assert!(historical_retry.packet.is_none());
    assert!(
        historical_retry
            .next_command
            .as_deref()
            .unwrap()
            .contains("--refresh")
    );
    let refreshed = research::resume(&app, &started.run_id, true).unwrap();
    assert_eq!(packet(&refreshed).stage, ResearchStage::Answer);
    let fresh_passage = &packet(&refreshed).passages[0];
    assert!(fresh_passage.quote.contains("Saturday"));
    let final_result = research::import(
        &app,
        &answer(packet(&refreshed), &fresh_passage.passage_id, None),
    )
    .unwrap();
    assert!(final_result.packet.is_none());
    let report = research::report(&app, &started.run_id).unwrap();
    assert_eq!(report.claims.len(), 1);
    assert_eq!(
        report.claims[0].citations,
        vec![fresh_passage.citation.clone()]
    );
    assert_eq!(report.claims[0].assessment, "unassessed");
    assert_eq!(report.external_tool_usage, "unobserved");
    assert!(!report.partial);
    assert_eq!(
        research::status(&app, &started.run_id).unwrap()["imports"],
        2
    );
    f.refresh_agent(&source_citation.source_id, "Cedar now uses offsite backup.");
    let completed_retry = research::import(&app, &first).unwrap();
    assert!(completed_retry.reused);
    assert_eq!(completed_retry.status, "completed");
    assert_eq!(completed_retry.freshness, "retained");
    assert!(completed_retry.packet.is_none());
}

#[test]
fn stale_source_requires_refresh_and_withdrawn_source_loses_current_passage() {
    let f = Fixture::new();
    let source = f.capture("Project Cedar backs up on Friday.");
    let app = f.app(false, false);
    let started = research::start(
        &app,
        scope(false, vec![source.clone()]),
        Some(id("run_stale")),
        false,
    )
    .unwrap();
    assert!(!packet(&started).passages.is_empty());
    f.refresh(&source, "Project Cedar backs up on Saturday.");
    assert_eq!(
        research::resume(&app, &started.run_id, false)
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
    assert_eq!(
        research::import(
            &app,
            &submission(
                packet(&started),
                json!({"stage":"collect_sources","sources":[],"gaps":[]})
            )
        )
        .unwrap_err()
        .code,
        ErrorCode::FreshnessConflict
    );
    let refreshed = research::resume(&app, &started.run_id, true).unwrap();
    assert_ne!(
        packet(&refreshed).packet_fingerprint,
        packet(&started).packet_fingerprint
    );
    assert!(
        packet(&refreshed)
            .passages
            .iter()
            .any(|p| p.quote.contains("Saturday"))
    );
    assert_eq!(
        research::import(
            &app,
            &submission(
                packet(&started),
                json!({"stage":"collect_sources","sources":[],"gaps":[]})
            )
        )
        .unwrap_err()
        .code,
        ErrorCode::ContentConflict
    );
    let withdraw = SourceStore::new(f.fs.clone())
        .plan_withdraw(&source, "Fixture withdrawal")
        .unwrap();
    f.apply(withdraw.draft.unwrap());
    assert_eq!(
        research::resume(&app, &started.run_id, false)
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
    let after_withdraw = research::resume(&app, &started.run_id, true).unwrap();
    assert!(
        packet(&after_withdraw)
            .passages
            .iter()
            .all(|p| match &p.citation {
                CitationRef::Source(s) => s.source_id != source,
                CitationRef::Assertion(a) => a.source_id != source,
            })
    );
}

#[test]
fn malformed_and_foreign_submissions_fail_without_writes() {
    let f = Fixture::new();
    let app = f.app(false, false);
    let started =
        research::start(&app, scope(false, vec![]), Some(id("run_invalid")), false).unwrap();
    let current = submission(
        packet(&started),
        json!({"stage":"collect_sources","sources":[],"gaps":[]}),
    );
    let mut wrong_packet: Value = serde_json::from_slice(&current).unwrap();
    wrong_packet["packet_fingerprint"] = json!(Blake3Hash::digest(b"wrong"));
    let mut unknown_field: Value = serde_json::from_slice(&current).unwrap();
    unknown_field["unexpected"] = json!(true);
    let mut foreign: Value = serde_json::from_slice(&current).unwrap();
    foreign["run_id"] = json!("run_foreign");
    let duplicate = format!(
        "{{\"schema\":\"lwiki.research-submission.v1\",\"schema\":\"lwiki.research-submission.v1\",\"run_id\":\"{}\",\"packet_fingerprint\":\"{}\",\"response\":{{\"stage\":\"collect_sources\",\"sources\":[],\"gaps\":[]}}}}",
        started.run_id,
        packet(&started).packet_fingerprint
    );
    let oversized = vec![b' '; 262145];
    let oversized_source = collect(packet(&started), &"x".repeat(65537));
    let other = Fixture::with_vault("vault_research_other");
    let other_app = other.app(false, false);
    let other_started = research::start(
        &other_app,
        scope(false, vec![]),
        Some(started.run_id.clone()),
        false,
    )
    .unwrap();
    assert_ne!(
        packet(&other_started).packet_fingerprint,
        packet(&started).packet_fingerprint
    );
    assert_eq!(
        research::import(&other_app, &current).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    let before = tree_except_writer_lock(f.temp.path());
    for bytes in [
        serde_json::to_vec(&wrong_packet).unwrap(),
        serde_json::to_vec(&unknown_field).unwrap(),
        serde_json::to_vec(&foreign).unwrap(),
        duplicate.into_bytes(),
        oversized,
        oversized_source,
    ] {
        assert!(research::import(&app, &bytes).is_err());
        assert_eq!(tree_except_writer_lock(f.temp.path()), before);
    }
    let dry_before = tree(f.temp.path());
    let dry = research::import(&f.app(true, false), &current).unwrap();
    assert!(!dry.persisted && !dry.ready_to_import);
    assert_eq!(tree(f.temp.path()), dry_before);
    let imported = research::import(&app, &current).unwrap();
    let answer_packet = packet(&imported);
    let quote_hash = submission(
        answer_packet,
        json!({"stage":"answer","claims":[{"text":"Unsupported","passage_ids":[Blake3Hash::digest(b"quote").to_string()]}],"gaps":[]}),
    );
    let unknown_id = submission(
        answer_packet,
        json!({"stage":"answer","claims":[{"text":"Unsupported","passage_ids":["p999"]}],"gaps":[]}),
    );
    // An empty collection has no passage IDs. Hash-shaped and invented selectors are refused.
    let before = tree_except_writer_lock(f.temp.path());
    for bytes in [quote_hash, unknown_id] {
        assert!(research::import(&app, &bytes).is_err());
        assert_eq!(tree_except_writer_lock(f.temp.path()), before);
    }
}

#[test]
fn answered_collection_gap_is_resolved_in_final_report() {
    let f = Fixture::new();
    let app = f.app(false, false);
    let started = research::start(&app, scope(false, vec![]), Some(id("run_gap")), false).unwrap();
    let collected = research::import(
        &app,
        &submission(
            packet(&started),
            json!({"stage":"collect_sources","sources":[{
            "key":"source_gap","title":"Cedar schedule","origin":"host-notes",
            "content":"Cedar backups run on Friday."}],"gaps":["Schedule unknown"]}),
        ),
    )
    .unwrap();
    let passage_id = packet(&collected).passages[0].passage_id.clone();
    research::import(&app, &answer(packet(&collected), &passage_id, None)).unwrap();
    let report = research::report(&app, &started.run_id).unwrap();
    assert!(report.gaps.is_empty());
    assert!(!report.partial);
}

#[test]
fn preview_dry_run_offline_and_follow_up_survive_reopen() {
    let f = Fixture::new();
    let run_id = id("run_preview");
    let before = tree(f.temp.path());
    let incompatible = research::start(
        &f.app(false, true),
        scope(false, vec![]),
        Some(id("run_offline_mismatch")),
        false,
    )
    .unwrap_err();
    assert_eq!(incompatible.code, ErrorCode::OfflineUnavailable);
    assert_eq!(tree(f.temp.path()), before);
    let plan = research::start(
        &f.app(false, true),
        scope(true, vec![]),
        Some(run_id.clone()),
        true,
    )
    .unwrap();
    assert!(!plan.persisted && !plan.ready_to_import && !plan.network_used);
    assert_eq!(tree(f.temp.path()), before);
    let dry = research::start(
        &f.app(true, true),
        scope(true, vec![]),
        Some(run_id.clone()),
        false,
    )
    .unwrap();
    assert!(!dry.persisted && !dry.ready_to_import);
    assert_eq!(tree(f.temp.path()), before);
    let app = f.app(false, true);
    let started = research::start(&app, scope(true, vec![]), Some(run_id.clone()), false).unwrap();
    assert!(
        packet(&started)
            .tasks
            .iter()
            .all(|task| !task.contains("authorized_host_tools"))
    );
    let next = research::import(&app, &collect(packet(&started), "Cedar Friday backup.")).unwrap();
    let cited = packet(&next).passages[0].passage_id.clone();
    let follow_up = research::import(
        &app,
        &answer(packet(&next), &cited, Some("Check destination.")),
    )
    .unwrap();
    let first_report = research::report(&app, &run_id).unwrap();
    assert_eq!(packet(&follow_up).stage, ResearchStage::CollectSources);
    assert_eq!(packet(&follow_up).round, 2);
    drop(app);
    let reopened = f.app(false, true);
    let resumed = research::resume(&reopened, &run_id, false).unwrap();
    assert_eq!(
        packet(&resumed).packet_fingerprint,
        packet(&follow_up).packet_fingerprint
    );
    assert_eq!(research::status(&reopened, &run_id).unwrap()["round"], 2);
    let second_answer = research::import(
        &reopened,
        &submission(
            packet(&resumed),
            json!({"stage":"collect_sources","sources":[],"gaps":[]}),
        ),
    )
    .unwrap();
    assert_eq!(packet(&second_answer).stage, ResearchStage::Answer);
    let second_follow_up = research::import(
        &reopened,
        &answer(packet(&second_answer), &cited, Some("Check destination.")),
    )
    .unwrap();
    let second_report = research::report(&reopened, &run_id).unwrap();
    assert_eq!(
        serde_json::to_value(first_report).unwrap(),
        serde_json::to_value(second_report).unwrap()
    );
    assert_eq!(packet(&second_follow_up).round, 3);
    let final_packet = research::import(
        &reopened,
        &submission(
            packet(&second_follow_up),
            json!({"stage":"collect_sources","sources":[],"gaps":["Destination unavailable."]}),
        ),
    )
    .unwrap();
    assert_eq!(packet(&final_packet).stage, ResearchStage::Answer);
    let completed = research::import(
        &reopened,
        &submission(
            packet(&final_packet),
            json!({"stage":"answer","claims":[],"gaps":["Destination remains unknown."]}),
        ),
    )
    .unwrap();
    assert!(completed.packet.is_none());
    let report = research::report(&reopened, &run_id).unwrap();
    assert!(report.partial);
    assert!(report.gaps.iter().any(|gap| gap.contains("Destination")));
}

#[test]
fn offline_answer_cannot_publish_an_online_follow_up() {
    let f = Fixture::new();
    let app = f.app(false, false);
    let started = research::start(
        &app,
        scope(false, vec![]),
        Some(id("run_offline_followup")),
        false,
    )
    .unwrap();
    let collected = research::import(
        &app,
        &submission(
            packet(&started),
            json!({"stage":"collect_sources","sources":[],"gaps":[]}),
        ),
    )
    .unwrap();
    let pending = packet(&collected);
    let answer = submission(
        pending,
        json!({"stage":"answer","claims":[],"gaps":["More sources needed"],"follow_up":"Collect more sources"}),
    );
    let before = research::status(&app, &started.run_id).unwrap();
    let error = research::import(&f.app(false, true), &answer).unwrap_err();
    assert_eq!(error.code, ErrorCode::OfflineUnavailable);
    assert_eq!(research::status(&app, &started.run_id).unwrap(), before);
    assert_eq!(
        research::report(&app, &started.run_id).unwrap_err().code,
        ErrorCode::RecordNotFound
    );
    assert_eq!(
        packet(&research::resume(&app, &started.run_id, false).unwrap()).packet_fingerprint,
        pending.packet_fingerprint
    );
}

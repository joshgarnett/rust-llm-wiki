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

#[test]
fn explicit_sources_cover_lexical_subsets_on_start_and_refresh() {
    let f = Fixture::new();
    let first = f.capture("Cedar backup is stored in the north vault every Friday. Extra context for the first source.");
    let second = f.capture("Cedar backup is copied to the south vault every Saturday. Extra context for the second source.");
    let app = f.app(false, false);
    let started = research::start(
        &app,
        scope(false, vec![first.clone(), second.clone()]),
        Some(id("run_research_subset_refresh")),
        false,
    )
    .unwrap();
    let initial = packet(&started);
    assert_eq!(initial.passages.len(), 2);
    assert!(initial.warnings.is_empty());
    assert!(
        matches!(&initial.passages[0].citation, CitationRef::Source(r) if r.source_id == first)
    );
    assert!(
        matches!(&initial.passages[1].citation, CitationRef::Source(r) if r.source_id == second)
    );

    f.refresh(
        &first,
        "Cedar backup now stays in the east vault every Sunday. Updated first source context.",
    );
    let refreshed = research::resume(&app, &started.run_id, true).unwrap();
    let fresh = packet(&refreshed);
    assert_eq!(fresh.passages.len(), 2);
    assert!(fresh.warnings.is_empty());
    assert!(fresh.passages[0].quote.contains("east vault"));
    assert!(matches!(&fresh.passages[0].citation, CitationRef::Source(r) if r.source_id == first));
    assert!(matches!(&fresh.passages[1].citation, CitationRef::Source(r) if r.source_id == second));
}

#[test]
fn refresh_recovers_captured_source_evicted_by_packet_limit() {
    let f = Fixture::new();
    let app = f.app(false, false);
    let mut research_scope = scope(false, vec![]);
    research_scope.max_sources = 20;
    let started =
        research::start(&app, research_scope, Some(id("run_evicted_capture")), false).unwrap();
    let initial_sources: Vec<_> = (0..16)
        .map(|n| {
            let prefix = format!("Cedar backup source {n}. ");
            json!({"key":format!("old_{n}"),"title":format!("Old {n}"),"origin":"local fixture",
            "content":format!("{prefix}{}", "x".repeat(4096-prefix.len()))})
        })
        .collect();
    let first = research::import(
        &app,
        &submission(
            packet(&started),
            json!({
                "stage":"collect_sources","sources":initial_sources,"gaps":[]
            }),
        ),
    )
    .unwrap();
    assert_eq!(packet(&first).passages.len(), 16);
    let evicted = first.imported_sources[15].source_id.clone();
    let continuation = research::import(
        &app,
        &submission(
            packet(&first),
            json!({
                "stage":"answer","claims":[],"gaps":[],"follow_up":"Check one more local source."
            }),
        ),
    )
    .unwrap();
    let prefix = "Newest Cedar backup source. ";
    let newest = research::import(&app, &submission(packet(&continuation), json!({
        "stage":"collect_sources","sources":[{"key":"newest","title":"Newest","origin":"local fixture",
            "content":format!("{prefix}{}", "y".repeat(4096-prefix.len()))}],"gaps":[]
    }))).unwrap();
    let bounded = packet(&newest);
    assert_eq!(bounded.passages.len(), 16);
    assert!(bounded.warnings[0].contains(&evicted.to_string()));
    assert!(
        bounded
            .passages
            .iter()
            .all(|passage| !matches!(&passage.citation,
        CitationRef::Source(source) if source.source_id == evicted))
    );
    let newest_id = newest.imported_sources[0].source_id.clone();
    let withdraw = SourceStore::new(f.fs.clone())
        .plan_withdraw(&newest_id, "Fixture withdrawal")
        .unwrap();
    f.apply(withdraw.draft.unwrap());
    let refreshed = research::resume(&app, &started.run_id, true).unwrap();
    let fresh = packet(&refreshed);
    assert_eq!(fresh.passages.len(), 16);
    assert!(
        fresh
            .passages
            .iter()
            .any(|passage| matches!(&passage.citation,
        CitationRef::Source(source) if source.source_id == evicted))
    );
    assert!(
        fresh
            .passages
            .iter()
            .all(|passage| !matches!(&passage.citation,
        CitationRef::Source(source) if source.source_id == newest_id))
    );
}

#[test]
fn withdrawn_explicit_source_stays_out_of_refreshed_import() {
    let f = Fixture::new();
    let source = f.capture("Cedar backup source to withdraw.");
    let app = f.app(false, false);
    let started = research::start(
        &app,
        scope(false, vec![source.clone()]),
        Some(id("run_withdraw_explicit")),
        false,
    )
    .unwrap();
    let withdraw = SourceStore::new(f.fs.clone())
        .plan_withdraw(&source, "Fixture withdrawal")
        .unwrap();
    f.apply(withdraw.draft.unwrap());
    let refreshed = research::resume(&app, &started.run_id, true).unwrap();
    assert!(packet(&refreshed).passages.is_empty());
    let imported = research::import(
        &app,
        &submission(
            packet(&refreshed),
            json!({
                "stage":"collect_sources","sources":[],"gaps":[]
            }),
        ),
    )
    .unwrap();
    assert!(packet(&imported).passages.is_empty());
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
fn host_claimed_retrieval_time_and_exact_bytes_survive_capture_and_replay() {
    let f = Fixture::new();
    let app = f.app(false, false);
    let started = research::start(
        &app,
        scope(false, vec![]),
        Some(id("run_retrieved_at")),
        false,
    )
    .unwrap();
    let content = "Cedar café backup.\nExact UTF-8 bytes remain.\n";
    let bytes = submission(
        packet(&started),
        json!({
            "stage":"collect_sources","sources":[
                {"key":"dated","title":"Dated","origin":"https://example.invalid/dated","content":content,
                 "provenance":"Host supplied text","retrieved_at":"2026-09-29T10:11:12-04:00"},
                {"key":"undated","title":"Undated","origin":"local notes","content":"Undated Cedar note.",
                 "provenance":"Host supplied text"}
            ],"gaps":[]
        }),
    );
    let imported = research::import(&app, &bytes).unwrap();
    assert_eq!(imported.imported_sources.len(), 2);
    for (index, captured) in imported.imported_sources.iter().enumerate() {
        let revision = parse_note(
            &fs::read(f.temp.path().join(format!(
                "sources/{}/revisions/{}/revision.md",
                captured.source_id, captured.source_revision
            )))
            .unwrap(),
        );
        let fields = revision.canonical.as_ref().unwrap();
        if index == 0 {
            assert_eq!(
                fields.string("origin_retrieved_at"),
                Some("2026-09-29T10:11:12-04:00")
            );
            assert_eq!(
                fields.string("origin_retrieved_at_kind"),
                Some("agent-claimed")
            );
            assert_eq!(
                fs::read(f.temp.path().join(format!(
                    "sources/{}/revisions/{}/original.bin",
                    captured.source_id, captured.source_revision
                )))
                .unwrap(),
                content.as_bytes()
            );
        } else {
            assert_eq!(fields.string("origin_retrieved_at"), None);
            assert_eq!(fields.string("origin_retrieved_at_kind"), None);
        }
    }
    let status = research::status(&app, &started.run_id).unwrap();
    let replay = research::import(&app, &bytes).unwrap();
    assert!(replay.reused);
    assert_eq!(replay.imported_sources, imported.imported_sources);
    assert_eq!(research::status(&app, &started.run_id).unwrap(), status);
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
fn handoff_validation_names_invalid_field_or_selector_before_writes() {
    let f = Fixture::new();
    let app = f.app(false, false);
    let started = research::start(
        &app,
        scope(false, vec![]),
        Some(id("run_diagnostics")),
        false,
    )
    .unwrap();
    let invalid_collection = [
        (
            json!({"stage":"collect_sources","sources":[{"key":"dated","title":"Dated","origin":"host notes","content":"Cedar backup.","retrieved_at":"2026-09-29"}],"gaps":[]}),
            "source retrieved_at must be an RFC3339 timestamp",
        ),
        (
            json!({"stage":"collect_sources","sources":[{"key":"oversized","title":"Oversized","origin":"host notes","content":"x".repeat(65537)}],"gaps":[]}),
            "source content exceeds 65536 UTF-8 bytes",
        ),
        (
            json!({"stage":"collect_sources","sources":[{"key":"nul","title":"NUL","origin":"host notes","content":"Cedar\u{0}backup"}],"gaps":[]}),
            "source content contains a NUL character",
        ),
    ];
    let before = tree_except_writer_lock(f.temp.path());
    for (response, diagnostic) in invalid_collection {
        let error = research::import(&app, &submission(packet(&started), response)).unwrap_err();
        assert!(error.message.contains(diagnostic), "{}", error.message);
        assert_eq!(tree_except_writer_lock(f.temp.path()), before);
    }

    let imported = research::import(
        &app,
        &collect(packet(&started), "Cedar backup is on Friday."),
    )
    .unwrap();
    let answer_packet = packet(&imported);
    let CitationRef::Source(source) = &answer_packet.passages[0].citation else {
        panic!("source passage")
    };
    let invalid_answers = [
        (
            json!({"stage":"answer","claims":[{"text":" ","passage_ids":["p1"]}],"gaps":[]}),
            "claim text is empty",
        ),
        (
            json!({"stage":"answer","claims":[{"text":"Cedar claim","passage_ids":[source.quote_hash.to_string()]}],"gaps":[]}),
            "source IDs and quote hashes are not passage IDs",
        ),
        (
            json!({"stage":"answer","claims":[{"text":"Cedar claim","passage_ids":[source.source_id.to_string()]}],"gaps":[]}),
            "source IDs and quote hashes are not passage IDs",
        ),
    ];
    let before = tree_except_writer_lock(f.temp.path());
    for (response, diagnostic) in invalid_answers {
        let error = research::import(&app, &submission(answer_packet, response)).unwrap_err();
        assert!(error.message.contains(diagnostic), "{}", error.message);
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
    assert_eq!(report.completion_reason.as_deref(), Some("agent_finished"));
}

#[test]
fn finished_answer_with_honest_gaps_is_not_marked_interrupted() {
    let f = Fixture::new();
    let app = f.app(false, false);
    let started = research::start(
        &app,
        scope(false, vec![]),
        Some(id("run_honest_gaps")),
        false,
    )
    .unwrap();
    let collected = research::import(
        &app,
        &collect(packet(&started), "Cedar backup is on Friday."),
    )
    .unwrap();
    let cited = packet(&collected).passages[0].passage_id.clone();
    let completed = research::import(
        &app,
        &submission(
            packet(&collected),
            json!({
                "stage":"answer",
                "claims":[{"text":"Cedar backup is on Friday.","passage_ids":[cited]}],
                "gaps":["The destination remains unknown."]
            }),
        ),
    )
    .unwrap();
    assert!(completed.packet.is_none());
    let report = research::report(&app, &started.run_id).unwrap();
    assert!(!report.partial);
    assert_eq!(report.completion_reason.as_deref(), Some("agent_finished"));
    assert_eq!(
        report.gaps,
        vec!["The destination remains unknown.".to_string()]
    );
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
    assert!(first_report.partial);
    assert_eq!(
        first_report.completion_reason.as_deref(),
        Some("follow_up_requested")
    );
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
    assert!(!report.partial);
    assert_eq!(report.completion_reason.as_deref(), Some("agent_finished"));
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

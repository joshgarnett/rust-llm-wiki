//! Integrated public extraction/lifecycle workflow plus graph selected admission.
use crate::{
    app::{OfflineApp, OperationOptions},
    catalog::Catalog,
    changes::PreparedChange,
    domain::*,
    graph::{self, ExportRequest, ExtractionLimits, GraphPlan},
    retrieval::VerificationBudget,
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceView},
    vault::{VaultFs, VaultRoot},
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

struct PublicFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    app: OfflineApp,
    catalog: Catalog,
    source: RecordId,
    root_entity: RecordId,
    assertion: RecordId,
}
impl PublicFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("public normalized graph vault");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("WIKI.md"),"---\nwiki_schema: '1'\nwiki_id: vault_public_neighbors\nwiki_kind: vault\ntitle: Public neighbors\n---\n").unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        let app = OfflineApp::new(
            handle.clone(),
            OperationOptions {
                offline: true,
                ..Default::default()
            },
        )
        .unwrap();
        let captured = app.source_add(capture("Relay uses Cedar.\n")).unwrap();
        let source = captured.allocated_ids["source"].clone();
        let packet = app
            .graph_extract_agent(&ExportRequest {
                source_id: source.clone(),
                revision_id: None,
                windows: vec![],
                limits: ExtractionLimits::default(),
                candidate_context: vec![],
            })
            .unwrap()
            .packet;
        let response = json!({"schema":graph::EXTRACTION_SCHEMA,"packet_id":packet.packet_id,"packet_fingerprint":packet.packet_fingerprint,
            "mentions":[{"id":"m1","window_id":"w1","label":"Relay","type":"component","quote":"Relay","span":{"start":0,"end":5}},
                {"id":"m2","window_id":"w1","label":"Cedar","type":"component","quote":"Cedar","span":{"start":11,"end":16}}],
            "assertions":[{"id":"a1","subject":"m1","predicate":"uses","object":{"kind":"mention","mention_id":"m2"},"negated":false,"modality":"asserted",
                "evidence":[{"window_id":"w1","stance":"supports","quote":"Relay uses Cedar."}]}],"unresolved":[]});
        let imported = app
            .graph_import(&serde_json::to_vec(&response).unwrap(), false)
            .unwrap();
        apply_staged(&app, &imported);
        let extraction: RecordId =
            serde_json::from_value(imported["extraction"]["record"]["record_id"].clone()).unwrap();
        let assertion: RecordId =
            serde_json::from_value(imported["allocations"]["assertions"]["a1"].clone()).unwrap();
        let evidence: RecordId =
            serde_json::from_value(imported["allocations"]["evidence"]["a1"][0].clone()).unwrap();
        let view = SourceView::from_fs_bounded(&handle, 64 * 1024 * 1024, 4096).unwrap();
        let (_, note) = view
            .resolve(&extraction, RecordKind::Extraction, None)
            .unwrap();
        let resolution = json!({"schema":graph::RESOLUTION_SCHEMA,"extraction_id":extraction,"expected_hash":note.source_hash,
            "mappings":[{"mention_id":"m1","operation":"CreateEntity","title":"Relay","entity_type":"component","reason":"Public synthetic identity"},
                {"mention_id":"m2","operation":"CreateEntity","title":"Cedar","entity_type":"component","reason":"Public synthetic identity"}]});
        let resolved = app
            .graph_resolve(&serde_json::to_vec(&resolution).unwrap())
            .unwrap();
        apply_staged(&app, &resolved);
        let view = SourceView::from_fs_bounded(&handle, 64 * 1024 * 1024, 4096).unwrap();
        let (_, assertion_note) = view
            .resolve(&assertion, RecordKind::Assertion, None)
            .unwrap();
        let root_entity = RecordId::new(
            assertion_note
                .canonical
                .as_ref()
                .unwrap()
                .string("wiki_subject_id")
                .unwrap(),
        )
        .unwrap();
        let (_, evidence_note) = view.resolve(&evidence, RecordKind::Evidence, None).unwrap();
        let review = json!({"schema":graph::GRAPH_REVIEW_SCHEMA,"decisions":[{"assertion_id":assertion,"expected_hash":assertion_note.source_hash,"decision":"accept","reason":"Exact synthetic supports claim",
            "evidence_checks":[{"evidence_id":evidence,"expected_hash":evidence_note.source_hash,"assessment":"supports"}]}]});
        let reviewed = app
            .graph_review(&serde_json::to_vec(&review).unwrap())
            .unwrap();
        apply_staged(&app, &reviewed);
        app.index_rebuild_normalized().unwrap();
        let catalog = Catalog::new(handle, RecordId::new("vault_public_neighbors").unwrap());
        Self {
            _temp: temp,
            root,
            app,
            catalog,
            source,
            root_entity,
            assertion,
        }
    }
    fn graph(&self) -> graph::GraphResult {
        graph::indexed_neighbors::neighbors(
            &self.catalog,
            &self.root_entity,
            &GraphPlan::default(),
            &VerificationBudget::default(),
            true,
        )
        .unwrap()
    }
}
fn capture(text: &str) -> CaptureRequest {
    CaptureRequest {
        title: "Synthetic relay facts".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "fixture.txt".into(),
        original: text.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: None,
    }
}
fn apply_staged(app: &OfflineApp, staged: &Value) {
    let prepared: PreparedChange = serde_json::from_value(staged["prepared"].clone()).unwrap();
    app.changes_apply(prepared.change_id).unwrap();
}
#[test]
fn public_packet_import_resolve_review_neighbors_and_exact_evidence_read() {
    let f = PublicFixture::new();
    let out = f.graph();
    assert_eq!(out.assertions.len(), 1);
    assert_eq!(out.assertions[0].record_ref.record_id, f.assertion);
    assert_eq!(out.assertions[0].predicate, "uses");
    let evidence = &out.assertions[0].support[0];
    let view = SourceView::from_fs_bounded(f.app.fs(), 64 * 1024 * 1024, 4096).unwrap();
    let verified = view
        .verify(
            evidence.citation.as_ref().unwrap(),
            crate::sources::CitationScope::Current,
        )
        .unwrap();
    assert_eq!(verified.quote, b"Relay uses Cedar.");
    let (revision_path, revision_note) = view
        .resolve(&evidence.source.source_revision, RecordKind::Revision, None)
        .unwrap();
    let parent = revision_path.as_str().rsplit_once('/').unwrap().0;
    let content = revision_note
        .canonical
        .as_ref()
        .unwrap()
        .string("wiki_content_path")
        .unwrap();
    let path = format!("{parent}/{content}");
    let read = cli(
        &f,
        &[
            "read",
            "--path",
            &path,
            "--start",
            &evidence.source.span.start().to_string(),
            "--end",
            &evidence.source.span.end().to_string(),
        ],
    );
    assert_eq!(read["data"]["body"], "Relay uses Cedar.");
    assert_eq!(read["data"]["source_citation"]["eligibility"], "current");
    let discovered = cli(&f, &["graph", "neighbors", f.root_entity.as_str()]);
    assert_eq!(discovered["meta"]["freshness"], "indexed_evidence");
    assert_eq!(discovered["data"]["assertions"][0]["predicate"], "uses");
    let cached = cli(
        &f,
        &["graph", "neighbors", f.root_entity.as_str(), "--no-sync"],
    );
    assert_eq!(cached["meta"]["freshness"], "index_snapshot");
    assert!(cached["data"]["assertions"][0]["support"][0]["citation"].is_null());
    let explicit = cli(
        &f,
        &[
            "graph",
            "neighbors",
            f.root_entity.as_str(),
            "--no-sync",
            "--verify-selected",
        ],
    );
    assert_eq!(explicit["meta"]["freshness"], "indexed_evidence");
}
#[test]
fn public_refresh_and_withdraw_remove_current_assertions_preserving_historical_quote() {
    let f = PublicFixture::new();
    let prior = f.graph();
    let citation = prior.assertions[0].support[0].citation.clone().unwrap();
    f.app
        .source_refresh(f.source.clone(), capture("Relay now uses Birch.\n"))
        .unwrap();
    assert!(f.graph().assertions.is_empty());
    let view = SourceView::from_fs_bounded(f.app.fs(), 64 * 1024 * 1024, 4096).unwrap();
    assert_eq!(
        view.verify(&citation, crate::sources::CitationScope::Historical)
            .unwrap()
            .quote,
        b"Relay uses Cedar."
    );
    f.app
        .source_withdraw(f.source.clone(), "Public lifecycle test")
        .unwrap();
    assert!(f.graph().assertions.is_empty());
    let view = SourceView::from_fs_bounded(f.app.fs(), 64 * 1024 * 1024, 4096).unwrap();
    assert_eq!(
        view.verify(&citation, crate::sources::CitationScope::Historical)
            .unwrap()
            .state,
        crate::sources::CitationState::Withdrawn
    );
}
#[test]
fn public_rebuild_reconstructs_same_substantive_graph() {
    let f = PublicFixture::new();
    let before = f.graph();
    // Cache loss keeps canonical Markdown, retained changes and outside authority.
    let saved = f.root.parent().unwrap().join("preserved predecessor cache");
    fs::rename(f.root.join(".wiki/cache"), &saved).unwrap();
    cli(&f, &["index", "rebuild", "--normalized"]);
    let after = f.graph();
    assert_eq!(before.seeds, after.seeds);
    assert_eq!(before.entities, after.entities);
    assert_eq!(before.assertions, after.assertions);
    assert_eq!(before.navigation, after.navigation);
    assert_eq!(before.coverage, after.coverage);
    assert!(f.root.join("WIKI.md").exists());
}

fn cli(f: &PublicFixture, tail: &[&str]) -> Value {
    use clap::Parser;
    let mut args = vec![
        "lwiki".to_owned(),
        "--wiki".into(),
        f.root.to_str().unwrap().into(),
        "--json".into(),
        "--offline".into(),
    ];
    args.extend(tail.iter().map(|s| (*s).into()));
    let parsed = crate::cli::Arguments::try_parse_from(args).unwrap();
    let (envelope, status) = crate::cli::execute(&parsed);
    let value = serde_json::to_value(envelope).unwrap();
    assert_eq!(status, 0, "{value}");
    value
}

#[test]
fn public_coordinator_rechecks_at_reached_selected_mutation_cut() {
    use clap::Parser;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let f = PublicFixture::new();
    let selected = f
        .root
        .join("knowledge/entities")
        .join(format!("{}.md", f.root_entity));
    let original = fs::read(&selected).unwrap();
    let mut changed = original.clone();
    let position = original
        .windows(5)
        .position(|window| window == b"Relay")
        .unwrap();
    changed[position] = b'D';
    let reached = Arc::new(AtomicBool::new(false));
    let signal = reached.clone();
    let target = selected.clone();
    crate::catalog::query_diagnostics::on_before_final(move || {
        fs::write(target, changed).unwrap();
        signal.store(true, Ordering::SeqCst);
    });
    let request = crate::cli::Arguments::try_parse_from([
        "lwiki",
        "--wiki",
        f.root.to_str().unwrap(),
        "--offline",
        "--json",
        "graph",
        "neighbors",
        f.root_entity.as_str(),
    ])
    .unwrap();
    let (envelope, status) = crate::cli::execute(&request);
    crate::catalog::query_diagnostics::clear_before_final();
    fs::write(selected, original).unwrap();
    assert!(
        reached.load(Ordering::SeqCst),
        "actual public graph coordinator must reach the final cut"
    );
    assert_ne!(status, 0);
    assert_eq!(envelope.error.unwrap().code, "FRESHNESS_CONFLICT");
    assert_eq!(envelope.data, Value::Null);
}

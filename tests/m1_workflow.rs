use lwiki::{
    app::{offline::init, *},
    catalog::*,
    changes::*,
    domain::*,
    graph::*,
    retrieval::{verification::context, *},
    sources::*,
    vault::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, sync::Arc, time::Duration};
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn rel(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn note(kind: &str, name: &str, extra: Value, body: &str) -> Vec<u8> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".to_owned(), json!("1")),
        ("wiki_id".into(), json!(name)),
        ("wiki_kind".into(), json!(kind)),
        ("title".into(), json!(name)),
    ]);
    fields.extend(extra.as_object().unwrap().clone());
    CanonicalRecord::new(fields.clone()).unwrap();
    let mut bytes = b"---\n".to_vec();
    for (k, v) in fields {
        bytes.extend_from_slice(format!("{k}: {v}\n").as_bytes())
    }
    bytes.extend_from_slice(b"---\n");
    bytes.extend_from_slice(body.as_bytes());
    bytes
}
fn capture(content: &[u8], name: &str) -> CaptureRequest {
    CaptureRequest {
        title: name.into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: format!("{name}.txt"),
        original: content.to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/plain".into()),
    }
}
fn apply(root: &VaultRoot, catalog: &Catalog, draft: ChangeDraft) -> PreparedChange {
    let w = WriterPermit::acquire(root, Duration::from_secs(1)).unwrap();
    let engine = ChangeEngine::new(catalog.fs().clone()).unwrap();
    let p = engine.prepare(&w, draft).unwrap().prepared;
    engine
        .apply(&w, &p, &CatalogGraphValidator, catalog)
        .unwrap();
    p
}
fn request() -> ContextRequest {
    let mut r = ContextRequest {
        target: ContextTarget::Graph,
        graph: Some(GraphPlan {
            strategy: GraphStrategy::Relationship,
            ..Default::default()
        }),
        ..Default::default()
    };
    r.verification_budget.max_elapsed_ms = 30000;
    r
}
fn ctx(root: &VaultRoot, catalog: &Catalog) -> ContextResult {
    let w = WriterPermit::acquire(root, Duration::from_secs(1)).unwrap();
    context(catalog, Some(&w), "claim", &request()).unwrap()
}
struct AfterCommit;
impl PublicationFault for AfterCommit {
    fn check(&self, point: PublicationCheckpoint) -> Result<()> {
        if point == PublicationCheckpoint::AfterCommit {
            Err(WikiError::new(
                ErrorCode::Internal,
                "fixture interrupted after SQL commit",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn m1_vertical_rename_refresh_revalidate_recover_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("wiki");
    init(
        &path,
        "M1 fixture",
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    let root = VaultRoot::explicit(&path).unwrap();
    let app = OfflineApp::new(
        VaultFs::new(root.clone()),
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    let catalog = Catalog::new(VaultFs::new(root.clone()), app.vault_id().clone());
    let original = b"A uses B.\r\nUnicode cafe\xc3\xa9 support.\r\n";
    let s1 = app.source_add(capture(original, "First")).unwrap();
    let s2 = app
        .source_add(capture(b"Independent A uses B.\n", "Second"))
        .unwrap();
    let source1 = s1.allocated_ids["source"].clone();
    let source2 = s2.allocated_ids["source"].clone();
    let revision1 = s1.allocated_ids["revision"].clone();
    let revision2 = s2.allocated_ids["revision"].clone();
    for name in ["a", "b"] {
        fs::write(path.join(format!("knowledge/entities/{name}.md")),note("entity",name,json!({"wiki_status":"active","wiki_entity_type":"component","wiki_depends_on_ids":["claim"]}),"Derived description A uses B")).unwrap();
    }
    fs::write(path.join("knowledge/assertions/claim.md"),note("assertion","claim",json!({"wiki_status":"accepted","wiki_subject_id":"a","wiki_subject":"[[knowledge/entities/a.md]]","wiki_predicate":"uses","wiki_object_id":"b"}),"Directed fixture")).unwrap();
    let store = SourceStore::new(catalog.fs().clone());
    let e1 = store
        .plan_evidence(EvidenceRequest {
            assertion_id: id("claim"),
            source_id: source1.clone(),
            revision_id: revision1.clone(),
            quote: b"A uses B.".to_vec(),
            window: ByteSpan::new(0, original.len() as u64).unwrap(),
            stance: EvidenceStance::Supports,
            explanation: "Authored fixture".into(),
            title: "First support".into(),
        })
        .unwrap();
    let evidence1 = e1.evidence_id.clone();
    apply(&root, &catalog, e1.draft);
    let e2 = store
        .plan_evidence(EvidenceRequest {
            assertion_id: id("claim"),
            source_id: source2.clone(),
            revision_id: revision2,
            quote: b"A uses B.".to_vec(),
            window: ByteSpan::new(0, 22).unwrap(),
            stance: EvidenceStance::Supports,
            explanation: "Independent fixture".into(),
            title: "Second support".into(),
        })
        .unwrap();
    apply(&root, &catalog, e2.draft);
    let page = note(
        "page",
        "summary",
        json!({"wiki_status":"reviewed","wiki_depends_on_ids":["claim"]}),
        "# Summary\nA uses B. [[knowledge/entities/a.md]]\n",
    );
    app.page_put(rel("knowledge/pages/summary.md"), page, None)
        .unwrap();
    let first = ctx(&root, &catalog);
    assert_eq!(first.bundles().len(), 1);
    assert_eq!(first.bundles()[0].subject.record_id, id("a"));
    let projection = lwiki::catalog::scan::scan(catalog.fs(), catalog.vault_id()).unwrap();
    let entity_hash = projection.records[&id("a")].hash.clone();
    assert_eq!(
        app.page_rename(
            id("a"),
            rel("knowledge/entities/renamed_a.md"),
            Blake3Hash::digest("wrong")
        )
        .unwrap_err()
        .code,
        ErrorCode::ContentConflict
    );
    app.page_rename(id("a"), rel("knowledge/entities/renamed_a.md"), entity_hash)
        .unwrap();
    assert!(
        fs::read_to_string(path.join("knowledge/assertions/claim.md"))
            .unwrap()
            .contains("[[knowledge/entities/renamed_a.md]]")
    );
    assert!(
        fs::read_to_string(path.join("knowledge/pages/summary.md"))
            .unwrap()
            .contains("[[knowledge/entities/renamed_a.md]]")
    );
    assert_eq!(ctx(&root, &catalog).bundles()[0].subject.record_id, id("a"));
    let refreshed = app
        .source_refresh(
            source1.clone(),
            capture(
                b"Prefix A uses B.\r\nUnicode cafe\xc3\xa9 support.\r\n",
                "First",
            ),
        )
        .unwrap();
    let revision_new = refreshed.allocated_ids["revision"].clone();
    assert_ne!(revision_new, revision1);
    let after_refresh = ctx(&root, &catalog);
    assert!(
        after_refresh
            .passages()
            .iter()
            .flat_map(|p| &p.citations)
            .all(|c| !matches!(c,CitationRef::Assertion(r)if r.evidence_id==evidence1))
    );
    let p = lwiki::catalog::scan::scan(catalog.fs(), catalog.vault_id()).unwrap();
    let staged = app
        .evidence_revalidate(
            evidence1.clone(),
            revision_new.clone(),
            p.records[&evidence1].hash.clone(),
        )
        .unwrap();
    assert_eq!(staged.status, Some(ChangeStatus::Prepared));
    app.changes_apply(staged.change.unwrap().change_id).unwrap();
    let revalidated = ctx(&root, &catalog);
    assert!(
        revalidated
            .passages()
            .iter()
            .flat_map(|p| &p.citations)
            .any(|c| matches!(c,CitationRef::Assertion(r)if r.source_revision==revision_new))
    );
    app.source_withdraw(source2, "one support withdrawn")
        .unwrap();
    assert!(!ctx(&root, &catalog).bundles().is_empty());
    app.source_withdraw(source1, "all support withdrawn")
        .unwrap();
    assert!(ctx(&root, &catalog).bundles().is_empty());
    let p = lwiki::catalog::scan::scan(catalog.fs(), catalog.vault_id()).unwrap();
    assert_eq!(
        p.records[&id("claim")].eligibility,
        Eligibility::Unsupported
    );
    assert_eq!(
        p.records[&id("a")].identity_eligibility,
        Some(Eligibility::Current)
    );
    assert_eq!(
        p.records[&id("a")].description_eligibility,
        Some(Eligibility::Stale)
    );
    assert_eq!(p.records[&id("summary")].eligibility, Eligibility::Stale);
    // Real production changes/publisher interruption retains FilesApplied and is
    // recovered by context's operational coordinator before its bounded proof.
    let draft = ChangeDraft {
        title: "Recovery page".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![ExpectedWrite {
            target: rel("knowledge/pages/recovered.md"),
            expected: ExpectedState::Absent,
            proposed: Some(note(
                "page",
                "recovered",
                json!({"wiki_status":"reviewed"}),
                "Recovered after SQL commit",
            )),
            apply_after: vec![],
        }],
    };
    let failing = Catalog::with_options(
        catalog.fs().clone(),
        catalog.vault_id().clone(),
        CatalogOptions {
            fault: Some(Arc::new(AfterCommit)),
            ..Default::default()
        },
    );
    let writer = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
    let engine = ChangeEngine::new(catalog.fs().clone()).unwrap();
    let change = engine.prepare(&writer, draft).unwrap().prepared;
    assert!(
        engine
            .apply(&writer, &change, &CatalogGraphValidator, &failing)
            .is_err()
    );
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::FilesApplied
    );
    let mut doc_request = ContextRequest::default();
    doc_request.verification_budget.max_elapsed_ms = 30000;
    let recovered = context(&catalog, Some(&writer), "recovered", &doc_request).unwrap();
    assert!(recovered.text().contains("Recovered after SQL commit"));
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Committed
    );
    drop(writer);
    let before = lwiki::catalog::scan::scan(catalog.fs(), catalog.vault_id()).unwrap();
    fs::remove_file(path.join(".wiki/cache/index.sqlite")).unwrap();
    let writer = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
    catalog.rebuild(&writer).unwrap();
    drop(writer);
    let after = lwiki::catalog::scan::scan(catalog.fs(), catalog.vault_id()).unwrap();
    assert_eq!(before, after);
    let mut history = request();
    history.scope = ContextScope::Historical;
    let writer = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
    let historical = context(&catalog, Some(&writer), "claim", &history).unwrap();
    assert!(!historical.bundles().is_empty());
    let view = SourceView::from_fs(catalog.fs()).unwrap();
    for p in historical.passages() {
        for c in &p.citations {
            view.verify(c, CitationScope::Historical).unwrap();
        }
    }
    drop(writer);
    let doctor = app.doctor().unwrap();
    assert!(!doctor.history_check_performed);
    assert!(doctor.check.is_none());
    assert!(doctor.unresolved_changes.is_empty());
    assert!(!doctor.provider_probe_performed);
}

#[path = "../test_support/paths.rs"]
mod test_paths;
use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    graph::{query as graph_query, *},
    retrieval::{context as assembly, verification::*, *},
    sources::*,
    vault::*,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
const VAULT: &str = "vault_00000000-0000-7000-8000-00000000001b";
const CLAIM: &str = "assertion_00000000-0000-7000-8000-00000000000a";
const PAGE: &str = "page_00000000-0000-7000-8000-000000000011";
const NORTH: &str = "entity_00000000-0000-7000-8000-000000000003";
const SOURCE1: &str = "source_00000000-0000-7000-8000-000000000005";
const SOURCE2: &str = "source_00000000-0000-7000-8000-000000000006";
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let t = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy(&p, &t)
        } else {
            fs::copy(p, t).unwrap();
        }
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    root: VaultRoot,
    catalog: Catalog,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        copy(
            &test_paths::fixture(env!("CARGO_MANIFEST_DIR"), "tests/fixtures/bootstrap/vault"),
            temp.path(),
        );
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let catalog = Catalog::new(VaultFs::new(root.clone()), id(VAULT));
        Self {
            temp,
            root,
            catalog,
        }
    }
    fn writer(&self) -> WriterPermit {
        WriterPermit::acquire(&self.root, Duration::from_millis(1000)).unwrap()
    }
    fn run(&self, query: &str, request: &ContextRequest) -> ContextResult {
        let writer = self.writer();
        context(&self.catalog, Some(&writer), query, request).unwrap()
    }
    fn withdraw(&self, source: &str) {
        let writer = self.writer();
        let draft = SourceStore::new(self.catalog.fs().clone())
            .plan_withdraw(&id(source), "fixture")
            .unwrap()
            .draft
            .unwrap();
        let engine = ChangeEngine::new(self.catalog.fs().clone()).unwrap();
        let p = engine.prepare(&writer, draft).unwrap().prepared;
        engine
            .apply(&writer, &p, &CatalogGraphValidator, &self.catalog)
            .unwrap();
    }
}
fn graph_request() -> ContextRequest {
    ContextRequest {
        target: ContextTarget::Graph,
        graph: Some(GraphPlan {
            strategy: GraphStrategy::Relationship,
            ..Default::default()
        }),
        ..Default::default()
    }
}
enum Mutation {
    Copy,
    Decision,
    AlwaysEdit,
    Sleep,
}
struct Hook {
    root: PathBuf,
    kind: Mutation,
    calls: AtomicUsize,
}
impl ContextFault for Hook {
    fn check(&self, _: ContextCheckpoint) -> Result<()> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match self.kind {
            Mutation::Copy if call == 0 => {
                fs::copy(
                    self.root.join("knowledge/entities/north_lab.md"),
                    self.root.join("copied.md"),
                )
                .unwrap();
            }
            Mutation::Decision if call == 0 => {
                fs::write(self.root.join("decision.md"),format!("---\nwiki_schema: \"1\"\nwiki_id: decision_probe\nwiki_kind: decision\ntitle: Decision\nwiki_status: active\nwiki_action: accept\nwiki_input_ids: [\"{CLAIM}\"]\nwiki_output_ids: [\"{CLAIM}\"]\nwiki_created_at: \"2026-09-28T00:00:00Z\"\n---\nExact accepted outcome.\n")).unwrap();
            }
            Mutation::AlwaysEdit => {
                let p = self.root.join("knowledge/pages/architecture.md");
                let mut bytes = fs::read(&p).unwrap();
                bytes.extend_from_slice(b"changed\n");
                fs::write(p, bytes).unwrap();
            }
            Mutation::Sleep => std::thread::sleep(Duration::from_millis(10)),
            _ => {}
        }
        Ok(())
    }
}
#[test]
fn new_decision_or_copied_id_before_emit_is_detected() {
    for kind in [Mutation::Decision, Mutation::Copy] {
        let f = Fixture::new();
        let writer = f.writer();
        f.catalog.sync(&writer).unwrap();
        let before = f.catalog.index_snapshot().unwrap().snapshot().generation;
        let copied = matches!(kind, Mutation::Copy);
        let hook = Arc::new(Hook {
            root: f.temp.path().into(),
            kind,
            calls: AtomicUsize::new(0),
        });
        let result = context_with_options(
            &f.catalog,
            Some(&writer),
            CLAIM,
            &graph_request(),
            &ContextOptions {
                fault: Some(hook.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hook.calls.load(Ordering::SeqCst), 2);
        assert!(result.snapshot().generation > before);
        assert!(matches!(
            result.verification(),
            SnapshotVerification::VerifiedSnapshot { .. }
        ));
        if copied {
            assert!(result.bundles().is_empty())
        } else {
            assert!(
                result
                    .bundles()
                    .iter()
                    .any(|b| b.assertion.record_id == id(CLAIM))
            )
        }
    }
}
#[test]
fn second_change_fails_without_a_third_attempt() {
    let f = Fixture::new();
    let writer = f.writer();
    f.catalog.sync(&writer).unwrap();
    let hook = Arc::new(Hook {
        root: f.temp.path().into(),
        kind: Mutation::AlwaysEdit,
        calls: AtomicUsize::new(0),
    });
    assert_eq!(
        context_with_options(
            &f.catalog,
            Some(&writer),
            PAGE,
            &ContextRequest::default(),
            &ContextOptions {
                fault: Some(hook.clone()),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::FreshnessConflict
    );
    assert_eq!(hook.calls.load(Ordering::SeqCst), 2);
}
#[test]
fn freshness_budget_cannot_claim_verified() {
    fn check_error(error: WikiError, flag: &str) {
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert!(!error.retryable);
        let hint = error.hint.as_deref().unwrap();
        assert!(hint.contains(flag));
        assert!(hint.contains("only if below its ceiling"));
    }
    let f = Fixture::new();
    let writer = f.writer();
    let mut r = ContextRequest::default();
    r.verification_budget.max_files = 1;
    check_error(
        context(&f.catalog, Some(&writer), "uses", &r).unwrap_err(),
        "--verification-max-files",
    );
    assert!(!f.temp.path().join(".wiki/cache/index.sqlite").exists());
    r.verification_budget = VerificationBudget::default();
    r.verification_budget.max_bytes = 1;
    check_error(
        context(&f.catalog, Some(&writer), "uses", &r).unwrap_err(),
        "--verification-max-bytes",
    );
    assert!(!f.temp.path().join(".wiki/cache/index.sqlite").exists());
    r.verification_budget = VerificationBudget::default();
    r.verification_budget.max_entries = 1;
    check_error(
        context(&f.catalog, Some(&writer), "uses", &r).unwrap_err(),
        "--verification-max-entries",
    );
    r.verification_budget = VerificationBudget::default();
    r.verification_budget.max_elapsed_ms = 5;
    let hook = Arc::new(Hook {
        root: f.temp.path().into(),
        kind: Mutation::Sleep,
        calls: AtomicUsize::new(0),
    });
    check_error(
        context_with_options(
            &f.catalog,
            Some(&writer),
            "uses",
            &r,
            &ContextOptions {
                fault: Some(hook),
                ..Default::default()
            },
        )
        .unwrap_err(),
        "--verification-max-elapsed-ms",
    );
    // Explicitly restoring adequate bounds can produce verified context again.
    r.verification_budget = VerificationBudget::default();
    let result = context(&f.catalog, Some(&writer), "uses", &r).unwrap();
    assert!(matches!(
        result.verification(),
        SnapshotVerification::VerifiedSnapshot { .. }
    ));
}
#[test]
fn both_attempts_share_the_file_budget() {
    let f = Fixture::new();
    let base = f.run(CLAIM, &graph_request());
    let writer = f.writer();
    let mut request = graph_request();
    request.verification_budget.max_files = base.usage().verification_files + 10;
    let hook = Arc::new(Hook {
        root: f.temp.path().into(),
        kind: Mutation::Decision,
        calls: AtomicUsize::new(0),
    });
    assert_eq!(
        context_with_options(
            &f.catalog,
            Some(&writer),
            CLAIM,
            &request,
            &ContextOptions {
                fault: Some(hook),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
}
#[test]
fn bundle_support_and_contradiction_fit_or_omit() {
    let f = Fixture::new();
    let complete = f.run(CLAIM, &graph_request());
    assert_eq!(complete.bundles().len(), 1);
    assert!(
        complete
            .passages()
            .iter()
            .flat_map(|p| &p.contributors)
            .any(|c| c.stance == EvidenceStance::Supports)
    );
    assert!(
        complete
            .passages()
            .iter()
            .flat_map(|p| &p.contributors)
            .any(|c| c.stance == EvidenceStance::Contradicts)
    );
    let mut tight = graph_request();
    tight.budget.max_bytes = complete.text().len() - 1;
    let omitted = f.run(CLAIM, &tight);
    assert!(omitted.bundles().is_empty());
    assert!(omitted.passages().is_empty());
    assert!(
        omitted
            .omissions()
            .iter()
            .any(|o| o.reason == "required_bundle_or_passage_does_not_fit")
    );
    let mut reserved = graph_request();
    reserved.budget.instruction_bytes = 12000;
    reserved.budget.instruction_tokens = 3000;
    let omitted = f.run(CLAIM, &reserved);
    assert!(omitted.text().is_empty());
    assert_eq!(omitted.usage().reserved_bytes, 12000);
}
#[test]
fn direct_source_vs_note_vs_assertion_citations() {
    let f = Fixture::new();
    let source = f.run("North Lab", &ContextRequest::default());
    assert!(source.passages().iter().any(|p| {
        p.citations
            .iter()
            .any(|c| matches!(c, CitationRef::Source(_)))
    }));
    let note = f.run(PAGE, &ContextRequest::default());
    assert!(!note.passages().is_empty());
    assert!(
        note.passages()
            .iter()
            .all(|p| p.label == ExcerptLabel::NoteText && p.citations.is_empty())
    );
    let assertion = f.run(CLAIM, &graph_request());
    assert!(assertion.passages().iter().any(|p| {
        p.citations
            .iter()
            .any(|c| matches!(c, CitationRef::Assertion(_)))
    }));
    let view = SourceView::from_fs(f.catalog.fs()).unwrap();
    for p in source.passages().iter().chain(assertion.passages()) {
        for c in &p.citations {
            let verified = view.verify(c, CitationScope::Current).unwrap();
            assert!(!verified.quote.is_empty())
        }
    }
}
#[test]
fn historical_citation_lifecycle_matches_verified_rejected_assertion() {
    let f = Fixture::new();
    let path = f.temp.path().join("knowledge/assertions/forward.md");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("wiki_status: \"accepted\"", "wiki_status: \"rejected\"");
    fs::write(path, text).unwrap();
    let mut request = graph_request();
    request.scope = ContextScope::Historical;
    let result = f.run(CLAIM, &request);
    assert_eq!(result.bundles().len(), 1);
    assert_eq!(
        result.bundles()[0].authored_status.as_deref(),
        Some("rejected")
    );
    assert_eq!(result.bundles()[0].eligibility, Eligibility::Historical);
    let view = SourceView::from_fs(f.catalog.fs()).unwrap();
    let mut count = 0;
    for passage in result.passages() {
        for citation in &passage.citations {
            assert_eq!(
                view.verify(citation, CitationScope::Historical)
                    .unwrap()
                    .state,
                CitationState::Historical
            );
            assert!(matches!(citation, CitationRef::Assertion(_)));
            count += 1;
        }
        // Active evidence against current source bytes retains its own state;
        // the rejected assertion changes the citation association's lifecycle.
        assert!(
            passage
                .contributors
                .iter()
                .all(|c| c.eligibility == Eligibility::Current
                    && c.authored_status.as_deref() == Some("active"))
        );
    }
    assert!(count > 0);
    assert!(result.text().contains("Citation (Historical):"));
    assert!(!result.text().contains("Citation (Current):"));
}
#[test]
fn one_support_then_all_support_withdrawal_closure() {
    let f = Fixture::new();
    assert!(!f.run(CLAIM, &graph_request()).bundles().is_empty());
    f.withdraw(SOURCE1);
    assert!(!f.run(CLAIM, &graph_request()).bundles().is_empty());
    assert!(
        !f.run(PAGE, &ContextRequest::default())
            .passages()
            .is_empty()
    );
    f.withdraw(SOURCE2);
    assert!(f.run(CLAIM, &graph_request()).bundles().is_empty());
    assert!(
        f.run(PAGE, &ContextRequest::default())
            .passages()
            .is_empty()
    );
    let projection = lwiki::catalog::scan::scan(f.catalog.fs(), &id(VAULT)).unwrap();
    assert_eq!(
        projection.records[&id(NORTH)].identity_eligibility,
        Some(Eligibility::Current)
    );
    let mut historical = graph_request();
    historical.scope = ContextScope::Historical;
    let result = f.run(CLAIM, &historical);
    assert!(!result.bundles().is_empty());
    assert!(result.text().contains("Withdrawn"));
    assert!(
        result
            .passages()
            .iter()
            .flat_map(|p| &p.contributors)
            .all(|c| c.eligibility == Eligibility::Withdrawn)
    );
    let view = SourceView::from_fs(f.catalog.fs()).unwrap();
    for p in result.passages() {
        for c in &p.citations {
            assert_eq!(
                view.verify(c, CitationScope::Historical).unwrap().state,
                CitationState::Withdrawn
            )
        }
    }
}
#[test]
fn unverified_snapshot_labels_text_and_never_issues_citations() {
    let f = Fixture::new();
    let old = f.run(CLAIM, &graph_request());
    f.withdraw(SOURCE1);
    let mut request = graph_request();
    request.scope = ContextScope::Snapshot;
    let result = context(&f.catalog, None, CLAIM, &request).unwrap();
    assert!(matches!(
        result.verification(),
        SnapshotVerification::IndexSnapshot
    ));
    assert!(result.text().contains("index_snapshot; unverified"));
    assert!(result.passages().iter().all(|p| p.citations.is_empty()));
    assert_eq!(result.usage().verification_bytes, 0);
    assert_ne!(old.snapshot(), result.snapshot());
}
#[test]
fn pure_assembly_rebinds_public_candidates_and_refuses_bad_paths_or_budgets() {
    let f = Fixture::new();
    let writer = f.writer();
    f.catalog.sync(&writer).unwrap();
    let reader = f.catalog.index_snapshot().unwrap();
    let hits = lwiki::retrieval::lexical::search(&reader, PAGE, &QueryPlan::default()).unwrap();
    let mut forged = hits.clone();
    for h in &mut forged.hits {
        h.excerpt.text = "Fabricated content".into();
        h.eligibility = Eligibility::Invalid;
        h.excerpt.citation = Some(CitationRef::Source(SourceSpanRef {
            source_id: id(SOURCE1),
            source_revision: id("not_revision"),
            span: ByteSpan::new(0, 1).unwrap(),
            quote_hash: Blake3Hash::digest("x"),
        }));
    }
    let draft = assembly::assemble(&reader, &ContextRequest::default(), &forged, None).unwrap();
    assert!(!draft.text().contains("Fabricated"));
    assert_eq!(draft.usage().rendered_bytes, draft.text().len());
    let mut outside = ContextRequest::default();
    outside.documents.filters.tags = vec!["absent_tag".into()];
    assert!(
        assembly::assemble(&reader, &outside, &forged, None)
            .unwrap()
            .passages()
            .is_empty()
    );
    let mut bad = ContextRequest::default();
    bad.budget.instruction_bytes = usize::MAX;
    bad.budget.output_bytes = usize::MAX;
    assert_eq!(
        assembly::assemble(&reader, &bad, &hits, None)
            .err()
            .unwrap()
            .code,
        ErrorCode::Usage
    );
    let graph = graph_query::query(&reader, CLAIM, &GraphPlan::default()).unwrap();
    let mut forged = graph.clone();
    for a in &mut forged.assertions {
        a.predicate = "invented".into();
        a.qualifiers.negated = true;
    }
    let draft = assembly::assemble(&reader, &graph_request(), &hits, Some(&forged)).unwrap();
    assert_eq!(draft.bundles()[0].predicate, "uses");
    assert!(!draft.bundles()[0].qualifiers.negated);
    let mut outside = graph_request();
    outside.graph.as_mut().unwrap().filters.path_prefix = Some("outside/".into());
    assert!(
        assembly::assemble(&reader, &outside, &hits, Some(&forged))
            .unwrap()
            .bundles()
            .is_empty()
    );
    let mut forged = graph.clone();
    for a in &mut forged.assertions {
        a.path.clear()
    }
    assert!(assembly::assemble(&reader, &graph_request(), &hits, Some(&forged)).is_err());
}

#[test]
fn public_navigation_candidates_cannot_invent_links_or_provenance() {
    let fixture = Fixture::new();
    for name in ["unlinked_a", "unlinked_b"] {
        fs::write(fixture.temp.path().join(format!("{name}.md")), format!("---\nwiki_schema: \"1\"\nwiki_id: {name}\nwiki_kind: page\ntitle: {name}\nwiki_status: reviewed\n---\nReadable unrelated page\n")).unwrap();
    }
    let writer = fixture.writer();
    fixture.catalog.sync(&writer).unwrap();
    let reader = fixture.catalog.index_snapshot().unwrap();
    let hits =
        lwiki::retrieval::lexical::search(&reader, "unmatchedword", &QueryPlan::default()).unwrap();
    let mut request = graph_request();
    request.graph.as_mut().unwrap().include_navigation = true;
    let graph = graph_query::query(&reader, CLAIM, request.graph.as_ref().unwrap()).unwrap();
    let locator = |name: &str| {
        let row = &reader.projection().records[&id(name)];
        DocumentLocator {
            record: Some(RecordRef {
                vault_id: id(VAULT),
                record_id: row.record.id().clone(),
                expected_kind: RecordKind::Page,
            }),
            path: row.path.clone(),
            observed_hash: row.hash.clone(),
        }
    };
    for reason in [NavigationReason::PageLink, NavigationReason::Provenance] {
        let mut forged = graph.clone();
        forged.navigation = vec![NavigationEdge {
            from: locator("unlinked_a"),
            to: locator("unlinked_b"),
            reason,
        }];
        let error = assembly::assemble(&reader, &request, &hits, Some(&forged))
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::RecordInvalid);
        assert!(error.message.contains("canonical links or provenance"));
    }
}
#[test]
fn draft_and_unsupported_descriptions_remain_discovery_only() {
    let f = Fixture::new();
    fs::write(f.temp.path().join("draft.md"),b"---\nwiki_schema: \"1\"\nwiki_id: draft_page\nwiki_kind: page\ntitle: SecretDraft\nwiki_status: draft\n---\nSecretDraft body\n").unwrap();
    assert!(
        f.run("SecretDraft", &ContextRequest::default())
            .passages()
            .is_empty()
    );
    let entity = f.run(NORTH, &ContextRequest::default());
    assert!(entity.passages().is_empty());
    let reader = f.catalog.index_snapshot().unwrap();
    let discovery =
        lwiki::retrieval::lexical::search(&reader, NORTH, &QueryPlan::default()).unwrap();
    assert!(!discovery.hits.is_empty());
    assert_eq!(
        discovery.hits[0].identity_eligibility,
        Some(Eligibility::Current)
    );
}
#[test]
fn closed_source_input_never_falls_back_and_enumeration_can_stop() {
    let f = Fixture::new();
    let mut calls = 0;
    let error = f
        .root
        .scan_markdown_budgeted(&mut || {
            calls += 1;
            if calls == 2 {
                Err(WikiError::new(ErrorCode::BudgetExceeded, "stop"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(calls, 2);
    let mut documents = Vec::new();
    for path in f.root.scan_markdown().unwrap() {
        let bytes = fs::read(f.root.resolve(&path).unwrap()).unwrap();
        documents.push(ScanDocument {
            path,
            hash: Blake3Hash::digest(&bytes),
            bytes,
        });
    }
    let input = ValidationInput {
        vault_id: id(VAULT),
        documents,
        overlay: vec![],
    };
    let view = SourceView::from_closed_input(f.catalog.fs(), &input).unwrap();
    let source = id(SOURCE1);
    let revision = id("revision_00000000-0000-7000-8000-000000000008");
    let path = f
        .temp
        .path()
        .join(format!("sources/{source}/revisions/{revision}/content.md"));
    let bytes = fs::read(path).unwrap();
    let citation = CitationRef::Source(SourceSpanRef {
        source_id: source,
        source_revision: revision,
        span: ByteSpan::new(0, 6).unwrap(),
        quote_hash: Blake3Hash::digest(&bytes[..6]),
    });
    assert_eq!(
        view.verify(&citation, CitationScope::Current)
            .unwrap_err()
            .code,
        ErrorCode::SourceIntegrity
    );
}
#[test]
fn combined_graph_share_and_full_render_accounting_are_bounded() {
    let f = Fixture::new();
    let mut request = graph_request();
    request.target = ContextTarget::Combined;
    request.budget.instruction_bytes = 400;
    request.budget.instruction_tokens = 100;
    let result = f.run("uses", &request);
    let usage = result.usage();
    assert_eq!(usage.rendered_bytes, result.text().len());
    assert_eq!(usage.estimated_tokens, result.text().len().div_ceil(4));
    assert!(usage.rendered_bytes + usage.reserved_bytes <= 12000);
    assert!(usage.estimated_tokens + usage.reserved_tokens <= 3000);
    assert!(usage.graph_bytes <= (12000 - usage.reserved_bytes) / 2);
    assert!(usage.graph_estimated_tokens <= (3000 - usage.reserved_tokens) / 2);
    let mut per_owner = BTreeMap::new();
    for p in result.passages() {
        *per_owner.entry(&p.locator.path).or_insert(0) += 1;
    }
    assert!(per_owner.values().all(|n| *n <= 2));
}
#[test]
fn consistently_forged_cache_text_cannot_be_verified() {
    let f = Fixture::new();
    f.run(PAGE, &ContextRequest::default());
    let db = rusqlite::Connection::open(f.temp.path().join(".wiki/cache/index.sqlite")).unwrap();
    let (generation,serialized):(i64,String)=db.query_row("SELECT published_gen,projection_json FROM index_meta JOIN generations ON gen=published_gen",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let mut projection: CatalogProjection = serde_json::from_str(&serialized).unwrap();
    let row = projection
        .documents
        .iter_mut()
        .find(|d| d.record_id.as_ref() == Some(&id(PAGE)))
        .unwrap();
    row.raw_text = row
        .raw_text
        .replace("North Lab uses South Lab", "FAKE CLAIM fabricated facts");
    let serialized_row = serde_json::to_string(row).unwrap();
    db.execute(
        "UPDATE documents SET raw_text=?1,row_json=?2 WHERE gen=?3 AND path=?4",
        rusqlite::params![row.raw_text, serialized_row, generation, row.path.as_str()],
    )
    .unwrap();
    db.execute(
        "UPDATE generations SET projection_json=?1 WHERE gen=?2",
        rusqlite::params![serde_json::to_string(&projection).unwrap(), generation],
    )
    .unwrap();
    drop(db);
    assert!(
        f.catalog.index_snapshot().is_ok(),
        "the forgery consistently binds every cached row"
    );
    assert_eq!(
        context(&f.catalog, None, PAGE, &ContextRequest::default())
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
    let repaired = f.run(PAGE, &ContextRequest::default());
    assert!(!repaired.text().contains("FAKE CLAIM"));
    assert!(repaired.text().contains("North Lab uses South Lab"));
    assert!(repaired.snapshot().generation > generation as u64);
}
#[test]
fn closed_projection_matches_existing_invalid_and_missing_assets() {
    let f = Fixture::new();
    fs::write(f.temp.path().join("broken.md"),b"---\nwiki_schema: \"1\"\nwiki_id: broken\nwiki_kind: entity\ntitle: Bad\nwiki_status: active\nwiki_entity_type: concept\nwiki_subject_id: missing\n---\nReadable\n").unwrap();
    let missing = f.temp.path().join(format!(
        "sources/{SOURCE1}/revisions/revision_00000000-0000-7000-8000-000000000008/original.md"
    ));
    fs::remove_file(missing).unwrap();
    let writer = f.writer();
    f.catalog.sync(&writer).unwrap();
    let result = context(&f.catalog, None, PAGE, &ContextRequest::default()).unwrap();
    assert!(matches!(
        result.verification(),
        SnapshotVerification::VerifiedSnapshot { .. }
    ));
    assert!(result.passages().iter().all(|p| p.citations.is_empty()));
}

#[test]
fn complete_registry_mirrors_overlap_and_capped_discovery_preserve_contributors() {
    let f = Fixture::new();
    let writer = f.writer();
    let projection = lwiki::catalog::scan::scan(f.catalog.fs(), &id(VAULT)).unwrap();
    let original = projection
        .documents
        .iter()
        .find(|d| {
            d.source_id.as_ref() == Some(&id(SOURCE1))
                && d.eligibility == Eligibility::Current
                && d.owner_revision.is_some()
        })
        .unwrap()
        .raw_text
        .clone();
    let store = SourceStore::new(f.catalog.fs().clone());
    let mirror = store
        .plan_capture(CaptureRequest {
            title: "Mirror".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "mirror.txt".into(),
            original: original.into_bytes(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let engine = ChangeEngine::new(f.catalog.fs().clone()).unwrap();
    let p = engine
        .prepare(&writer, mirror.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &p, &CatalogGraphValidator, &f.catalog)
        .unwrap();
    let source =
        fs::read_to_string(f.temp.path().join("knowledge/evidence/forward_short.md")).unwrap();
    let mirror_note = source
        .replace(
            "evidence_00000000-0000-7000-8000-000000000012",
            "aaa_mirror",
        )
        .replace(SOURCE1, mirror.source_id.as_str())
        .replace(
            "revision_00000000-0000-7000-8000-000000000008",
            mirror.revision_id.as_str(),
        );
    fs::write(f.temp.path().join("aaa_mirror.md"), mirror_note).unwrap();
    let inner = source
        .replace(
            "evidence_00000000-0000-7000-8000-000000000012",
            "aaa_overlap",
        )
        .replace("wiki_span_end: 54", "wiki_span_end: 37")
        .replace(
            "blake3:b6013766798c2bea006c6e653212b9690d276d1df6d3125517937803e265ea49",
            Blake3Hash::digest("North Lab").as_str(),
        );
    let body =
        lwiki::sources::evidence::exact_quote_body(b"North Lab", "\n", "Overlap fixture").unwrap();
    let mut inner = inner
        .split("---\n")
        .take(2)
        .collect::<Vec<_>>()
        .join("---\n")
        .into_bytes();
    inner.extend_from_slice(b"---\n");
    inner.extend(body);
    fs::write(f.temp.path().join("aaa_overlap.md"), inner).unwrap();
    let mut request = graph_request();
    request.graph.as_mut().unwrap().limits.support_per_assertion = 0;
    request
        .graph
        .as_mut()
        .unwrap()
        .limits
        .contradictions_per_assertion = 0;
    let result = context(&f.catalog, Some(&writer), CLAIM, &request).unwrap();
    assert_eq!(result.bundles().len(), 1);
    let refs = result
        .passages()
        .iter()
        .flat_map(|p| &p.contributors)
        .collect::<Vec<_>>();
    assert!(
        refs.iter()
            .any(|c| c.reference.evidence_id == id("aaa_mirror"))
    );
    assert!(
        refs.iter()
            .any(|c| c.reference.evidence_id == id("aaa_overlap"))
    );
    assert!(
        refs.iter()
            .any(|c| c.reference.source_id == id(SOURCE2) && c.stance == EvidenceStance::Supports)
    );
    assert!(refs.iter().any(|c| c.stance == EvidenceStance::Contradicts));
    let groups = result
        .passages()
        .iter()
        .filter(|p| {
            p.contributors
                .iter()
                .any(|c| c.stance == EvidenceStance::Supports)
        })
        .map(|p| p.support_group.clone().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(groups.len(), 2);
    assert!(
        result
            .passages()
            .iter()
            .filter(|p| p
                .contributors
                .iter()
                .any(|c| c.stance == EvidenceStance::Supports))
            .count()
            <= 2
    );
    assert!(
        result
            .passages()
            .iter()
            .filter(|p| p
                .contributors
                .iter()
                .any(|c| c.stance == EvidenceStance::Contradicts))
            .count()
            <= 1
    );
    let view = SourceView::from_fs(f.catalog.fs()).unwrap();
    for p in result.passages() {
        for c in &p.citations {
            view.verify(c, CitationScope::Current).unwrap();
        }
    }
    drop(writer);
    f.withdraw(mirror.source_id.as_str());
    let mut history = graph_request();
    history.scope = ContextScope::Historical;
    let historical = f.run(CLAIM, &history);
    assert!(
        historical
            .passages()
            .iter()
            .flat_map(|p| &p.contributors)
            .any(|c| c.reference.evidence_id == id("aaa_mirror")
                && c.eligibility == Eligibility::Withdrawn)
    );
    assert!(
        historical
            .passages()
            .iter()
            .flat_map(|p| &p.contributors)
            .any(|c| c.eligibility == Eligibility::Current)
    );
    assert!(historical.text().contains("Withdrawn"));
}

#[test]
fn bridge_overlap_closure_preserves_all_stances_and_remaps_prior_bundles() {
    let f = Fixture::new();
    let writer = f.writer();
    let store = SourceStore::new(f.catalog.fs().clone());
    let source = store
        .plan_capture(CaptureRequest {
            title: "Bridge".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "bridge.txt".into(),
            original: b"abcdefghijklmnop".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let engine = ChangeEngine::new(f.catalog.fs().clone()).unwrap();
    let p = engine
        .prepare(&writer, source.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &p, &CatalogGraphValidator, &f.catalog)
        .unwrap();
    let claim = fs::read_to_string(f.temp.path().join("knowledge/assertions/forward.md")).unwrap();
    for (name, start, end) in [
        ("a_left", 0usize, 5usize),
        ("b_right", 10, 15),
        ("c_bridge", 4, 11),
    ] {
        fs::write(
            f.temp.path().join(format!("{name}.md")),
            claim.replace(CLAIM, &format!("{name}_claim")),
        )
        .unwrap();
        let quote = &b"abcdefghijklmnop"[start..end];
        let mut note=format!("---\nwiki_schema: \"1\"\nwiki_id: {name}_evidence\nwiki_kind: evidence\ntitle: {name}\nwiki_status: active\nwiki_assertion_id: {name}_claim\nwiki_source_id: {}\nwiki_source_revision: {}\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: {start}\nwiki_span_end: {end}\nwiki_quote_hash: \"{}\"\n---\n",source.source_id,source.revision_id,Blake3Hash::digest(quote)).into_bytes();
        note.extend(
            lwiki::sources::evidence::exact_quote_body(quote, "\n", "Bridge fixture").unwrap(),
        );
        fs::write(f.temp.path().join(format!("{name}_evidence.md")), note).unwrap();
    }
    f.catalog.sync(&writer).unwrap();
    let reader = f.catalog.index_snapshot().unwrap();
    let hits = lwiki::retrieval::lexical::search(&reader, "not_in_fixture", &QueryPlan::default())
        .unwrap();
    let mut graph =
        graph_query::neighbors(&reader, &id("a_left_claim"), &GraphPlan::default()).unwrap();
    graph.assertions.clear();
    for name in ["a_left", "b_right", "c_bridge"] {
        let row = graph_query::neighbors(
            &reader,
            &id(&format!("{name}_claim")),
            &GraphPlan::default(),
        )
        .unwrap();
        graph.assertions.push(
            row.assertions
                .into_iter()
                .find(|a| a.record_ref.record_id == id(&format!("{name}_claim")))
                .unwrap(),
        );
    }
    let draft = assembly::assemble(&reader, &graph_request(), &hits, Some(&graph)).unwrap();
    assert_eq!(draft.passages().len(), 1);
    assert_eq!(draft.passages()[0].span, ByteSpan::new(0, 15).unwrap());
    assert_eq!(draft.passages()[0].text, "abcdefghijklmno");
    assert_eq!(draft.passages()[0].contributors.len(), 3);
    assert_eq!(draft.bundles().len(), 3);
    assert!(draft.bundles().iter().all(|b| b.passage_indices == [0]));
    // The same bridge inside one assertion must close before independent-source
    // caps, retaining both older spans and a contradictory bridge contributor.
    for name in ["b_right", "c_bridge"] {
        let path = f.temp.path().join(format!("{name}_evidence.md"));
        let text = fs::read_to_string(&path).unwrap().replace(
            &format!("wiki_assertion_id: {name}_claim"),
            "wiki_assertion_id: a_left_claim",
        );
        let text = if name == "c_bridge" {
            text.replace("wiki_stance: supports", "wiki_stance: contradicts")
        } else {
            text
        };
        fs::write(path, text).unwrap();
    }
    f.catalog.sync(&writer).unwrap();
    drop(reader);
    let result = context(&f.catalog, Some(&writer), "a_left_claim", &graph_request()).unwrap();
    assert_eq!(result.passages().len(), 1);
    assert_eq!(result.passages()[0].contributors.len(), 3);
    assert!(
        result.passages()[0]
            .contributors
            .iter()
            .any(|c| c.stance == EvidenceStance::Contradicts)
    );
    assert_eq!(result.bundles()[0].omitted_support, 0);
    assert_eq!(result.bundles()[0].omitted_contradictions, 0);
}

#[test]
fn context_eligibility_filters_precede_literal_and_lexical_candidate_caps() {
    let f = Fixture::new();
    for n in 0..12 {
        for (kind, name, extra) in [
            ("page", format!("aaa_draft{n:02}"), "wiki_status: draft\n"),
            (
                "entity",
                format!("bbb_unsupported{n:02}"),
                "wiki_status: active\nwiki_entity_type: concept\n",
            ),
        ] {
            fs::write(f.temp.path().join(format!("{name}.md")),format!("---\nwiki_schema: \"1\"\nwiki_id: {name}\nwiki_kind: {kind}\ntitle: NeedleEligible\n{extra}---\nNeedleEligible excluded body\n")).unwrap();
        }
    }
    fs::write(f.temp.path().join("zzz_reviewed.md"),b"---\nwiki_schema: \"1\"\nwiki_id: zzz_reviewed\nwiki_kind: page\ntitle: NeedleEligible\nwiki_status: reviewed\n---\nNeedleEligible reviewed body\n").unwrap();
    for mode in [SearchMode::Literal, SearchMode::Lexical] {
        let mut request = ContextRequest::default();
        request.documents.mode = mode;
        request.documents.limits.candidates = 1;
        request.documents.limits.hits = 1;
        request.verification_budget.max_elapsed_ms = 30000;
        let result = f.run("NeedleEligible", &request);
        assert_eq!(result.passages().len(), 1);
        assert_eq!(
            result.passages()[0]
                .locator
                .record
                .as_ref()
                .unwrap()
                .record_id,
            id("zzz_reviewed")
        );
        assert!(result.text().contains("reviewed body"));
        assert!(!result.text().contains("excluded body"));
    }
}

fn capture_context_fixture(f: &Fixture, body: &str) -> RecordId {
    let writer = f.writer();
    let plan = SourceStore::new(f.catalog.fs().clone())
        .plan_capture(CaptureRequest {
            title: "Operations reference".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "operations.md".into(),
            original: body.as_bytes().to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: Some("text/markdown".into()),
        })
        .unwrap();
    let engine = ChangeEngine::new(f.catalog.fs().clone()).unwrap();
    let prepared = engine
        .prepare(&writer, plan.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &prepared, &CatalogGraphValidator, &f.catalog)
        .unwrap();
    plan.source_id
}

#[test]
fn context_expands_beyond_discovery_anchor_with_exact_complete_commands() {
    let f = Fixture::new();
    let body = format!(
        "# Operations\n\nanchoronly introduction.\n\n{}\n## Audit archived records\n\nAudit archived records before restoring them:\n\n```sh\nvaultctl audit --archived\n```\n\n{}\n## Restore records\n\nRestore archived records while preserving UTF-8 café names:\n\n```sh\nvaultctl restore --preserve-names\n```\n",
        "Unrelated setup instructions.\n\n".repeat(40),
        "Unrelated deployment details.\n\n".repeat(40),
    );
    let source = capture_context_fixture(&f, &body);
    let writer = f.writer();
    let mut request = ContextRequest::default();
    request.documents.limits.excerpt_bytes = 512;
    request.documents.filters.source_ids = vec![source];
    request.budget.max_bytes = 6000;
    request.budget.max_tokens = 1500;
    let result = context_with_retrieval(
        &f.catalog,
        Some(&writer),
        "audit archived records and restore preserving names",
        &request,
        &ContextOptions::default(),
        |reader, normalized| {
            let mut plan = normalized.documents.clone();
            plan.mode = SearchMode::Literal;
            plan.limits.excerpt_bytes = 80;
            let hits = search(reader, "anchoronly", &plan)?;
            assert_eq!(hits.hits.len(), 1);
            assert!(!hits.hits[0].excerpt.text.contains("vaultctl"));
            Ok((hits, None))
        },
    )
    .unwrap();
    assert!(
        result.text().contains("vaultctl audit --archived"),
        "{}",
        result.text()
    );
    assert!(
        result.text().contains("vaultctl restore --preserve-names"),
        "{}",
        result.text()
    );
    assert!(result.text().len() <= 6000);
    assert!(result.passages().len() <= 4);
    for passage in result.passages() {
        assert!(passage.text.len() <= 512);
        assert_eq!(passage.span.slice(&body).unwrap(), passage.text);
        assert_eq!(passage.text.matches("```").count() % 2, 0);
        assert_eq!(passage.citations.len(), 1);
        for citation in &passage.citations {
            let CitationRef::Source(reference) = citation else {
                panic!("expected direct source citation")
            };
            assert_eq!(reference.span, passage.span);
            assert_eq!(
                reference.quote_hash,
                Blake3Hash::digest(reference.span.slice(&body).unwrap().as_bytes())
            );
        }
    }
}

#[test]
fn source_aware_mirrored_content_retains_each_revision_once() {
    let f = Fixture::new();
    let body = "# Mirror instructions\n\nAudit archived records before restoration.\n\nRestore archived records with vaultctl restore --preserve-names.\n";
    let first = capture_context_fixture(&f, body);
    let second = capture_context_fixture(&f, body);
    assert_ne!(first, second);
    let mut request = ContextRequest::default();
    request.documents.filters.source_ids = vec![first.clone(), second.clone()];
    request.budget.max_bytes = 6000;
    request.budget.max_tokens = 1500;
    let result = f.run("audit archived records restore preserve names", &request);
    assert!(!result.passages().is_empty());
    let mut cited = std::collections::BTreeSet::new();
    for passage in result.passages() {
        let mut revisions = std::collections::BTreeSet::new();
        assert!(passage.contributors.is_empty());
        assert_eq!(passage.span.slice(body).unwrap(), passage.text);
        for citation in &passage.citations {
            let CitationRef::Source(reference) = citation else {
                panic!("expected direct source citation")
            };
            assert!(revisions.insert((
                reference.source_id.clone(),
                reference.source_revision.clone()
            )));
            cited.insert(reference.source_id.clone());
            assert_eq!(reference.span, passage.span);
            assert_eq!(
                reference.quote_hash,
                Blake3Hash::digest(passage.text.as_bytes())
            );
        }
    }
    assert_eq!(cited, [first, second].into_iter().collect());
}

#[test]
fn source_aware_context_respects_small_bounds_reservations_and_snapshot_scope() {
    let f = Fixture::new();
    let body = format!(
        "# Bounded reference\n\n{}",
        "restore café records safely with backups. ".repeat(80)
    );
    let source = capture_context_fixture(&f, &body);
    let mut request = ContextRequest::default();
    request.documents.filters.source_ids = vec![source];
    request.documents.limits.excerpt_bytes = 64;
    request.budget.max_bytes = 1800;
    request.budget.max_tokens = 450;
    request.budget.instruction_bytes = 200;
    request.budget.instruction_tokens = 50;
    let result = f.run("restore café", &request);
    assert!(!result.passages().is_empty());
    assert!(result.text().len() + 200 <= 1800);
    assert!(result.usage().estimated_tokens + 50 <= 450);
    for p in result.passages() {
        assert!(p.text.len() <= 64);
        assert_eq!(p.span.slice(&body).unwrap(), p.text);
    }
    request.scope = ContextScope::Snapshot;
    let snapshot = f.run("restore café", &request);
    assert!(matches!(
        snapshot.verification(),
        SnapshotVerification::IndexSnapshot
    ));
    assert!(snapshot.passages().iter().all(|p| p.citations.is_empty()));
    assert!(
        snapshot
            .text()
            .starts_with("[context index_snapshot; unverified]")
    );
}

#[test]
fn host_selection_replays_exact_current_cards_and_rejects_changed_bindings() {
    use context_selection_packet::{SelectionAction, SelectionReply};
    let f = Fixture::new();
    let request = ContextRequest::default();
    let prepared = {
        let writer = f.writer();
        context_with_options(
            &f.catalog,
            Some(&writer),
            "uses",
            &request,
            &ContextOptions {
                selection: SelectionAction::Prepare,
                ..Default::default()
            },
        )
        .unwrap()
    };
    assert!(prepared.passages().is_empty());
    assert!(matches!(
        prepared.verification(),
        SnapshotVerification::VerifiedSnapshot { .. }
    ));
    let packet = prepared.selection_packet().unwrap();
    assert!(packet.input_bytes <= 131072 && packet.estimated_tokens <= 32768);
    let card = packet
        .cards
        .iter()
        .find(|card| {
            card.passage
                .citations
                .iter()
                .any(|c| matches!(c, CitationRef::Source(_)))
        })
        .unwrap();
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: vec![card.id.clone()],
    };
    let options = ContextOptions {
        selection: SelectionAction::Apply(reply.clone()),
        ..Default::default()
    };
    let selected = context_with_options(&f.catalog, None, "uses", &request, &options).unwrap();
    assert!(selected.selection_packet().is_none());
    assert_eq!(selected.passages().len(), 1);
    assert_eq!(selected.passages()[0].text, card.passage.text);
    assert_eq!(selected.passages()[0].span, card.passage.span);
    assert!(selected.text().len() <= request.budget.max_bytes);
    assert!(selected.usage().estimated_tokens <= request.budget.max_tokens);
    assert!(!selected.network_used);
    assert!(context_with_options(&f.catalog, None, "uses North", &request, &options).is_err());
    let mut smaller = request.clone();
    smaller.budget.max_bytes -= 1;
    assert!(context_with_options(&f.catalog, None, "uses", &smaller, &options).is_err());
    let empty = ContextOptions {
        selection: SelectionAction::Apply(SelectionReply {
            packet_fingerprint: packet.fingerprint.clone(),
            ordered_ids: vec![],
        }),
        ..Default::default()
    };
    let result = context_with_options(&f.catalog, None, "uses", &request, &empty).unwrap();
    assert!(result.passages().is_empty() && result.text().is_empty());
    let source = card
        .passage
        .citations
        .iter()
        .find_map(|citation| match citation {
            CitationRef::Source(reference) => Some(reference.source_id.clone()),
            _ => None,
        })
        .unwrap();
    f.withdraw(source.as_str());
    let writer = f.writer();
    assert!(context_with_options(&f.catalog, Some(&writer), "uses", &request, &options).is_err());
}

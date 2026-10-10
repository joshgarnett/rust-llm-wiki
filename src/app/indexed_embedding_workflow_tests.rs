//! Integrated normalized workflows over disposable sources and accounted mock responses.
//! Corpus vectors come only from embeddings_sync; query vectors are explicit cache fixtures.
#[path = "../../tests/fixtures/p17/common.rs"]
mod common;

use crate::{
    app::{OfflineApp, OperationOptions, embeddings::EmbeddingReport},
    catalog::{Catalog, SnapshotVerification},
    domain::*,
    jobs::*,
    providers::types::{EmbeddingInput, RemoteInput, RemoteOperation},
    retrieval::{spaces::EmbeddingSettings, types::*, vectors::VectorStore},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
};
use common::{Fixture, Responses, dispatcher, options, runtime};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const TITLE: &str = "Immutable capture title";
const FIRST: &str = "Signalneedle stores 17 amber tokens. Unicode café 東京 🦀.\n";
const SECOND: &str = "Signalneedle stores 29 violet tokens. Unicode café 東京 🦀.\n";

fn normalized() -> Fixture {
    let fixture = Fixture::new();
    fixture.app.index_rebuild_normalized().unwrap();
    assert!(
        Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone())
            .operation_state()
            .unwrap()
            .is_some()
    );
    fixture
}

// Bulk fixture creation uses the existing capture planner and its exact file
// layout, avoiding quadratic rebuilds while preparing disposable public inputs.
fn captured_scope_fixture(count: usize) -> Fixture {
    let fixture = Fixture::new();
    for index in 0..count {
        let plan = crate::sources::SourceStore::new(fixture.fs.clone())
            .plan_capture(capture(TITLE, &format!("scope-{index}.txt"), FIRST))
            .unwrap();
        for operation in plan.draft.unwrap().operations {
            let target = fixture.fs.root().path().join(operation.target.as_str());
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, operation.proposed.unwrap()).unwrap();
        }
    }
    fixture.app.index_rebuild_normalized().unwrap();
    fixture
}

#[test]
fn normalized_captured_scope_and_scheduling_boundaries_complete_without_duplicate_requests() {
    for count in [15, 16, 17, 127, 128, 129] {
        let fixture = captured_scope_fixture(count);
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let runtime = runtime(&fixture.service, &dispatch);
        let settings = EmbeddingSettings::default();
        let synced = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
        complete(&synced, count);
        assert_eq!(
            synced.generated_inputs, 1,
            "equal rendered captures across scopes/pages"
        );
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        let inspection = ledger(&fixture, &synced).inspect().unwrap();
        assert_eq!(paid_inputs(&fixture, &inspection).len(), 1);
        let cached = fixture.offline().embeddings_sync_cached(&settings).unwrap();
        complete(&cached, count);
        assert_eq!(cached.generated_inputs, 0);
        assert_eq!(
            cached.reused_inputs, 0,
            "unchanged acknowledged owners are excluded"
        );
        let checked = fixture
            .offline()
            .embeddings_check(&settings, None, false)
            .unwrap();
        assert_eq!(checked.coverage.eligible_units, count);
        assert_eq!(checked.coverage.available_units, count);
        assert_eq!(checked.coverage.missing_units, 0);
        assert_eq!(checked.coverage.pending_units, 0);
        assert!(!checked.network_used);
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn normalized_interrupted_inner_prefix_is_atomic_and_resumes_exact_suffix() {
    use crate::{
        catalog::query_types::{QueryCatalog, QueryReadLimits},
        retrieval::{context_types::VerificationBudget, unit_inventory_types::*},
    };
    let fixture = captured_scope_fixture(17);
    let settings = EmbeddingSettings::default();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let writer = fixture.writer();
    let inventory = catalog
        .prepare_unit_inventory_page(&writer, &settings, 128)
        .unwrap();
    assert!(inventory.complete);
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let bindings = reader
        .unit_owner_bindings_page(
            &inventory.policy,
            None,
            0,
            reader.snapshot().generation,
            128,
        )
        .unwrap();
    assert_eq!(bindings.len(), 17);
    let owners: Vec<_> = bindings
        .iter()
        .map(|binding| binding.owner.clone())
        .collect();
    let mut inputs = super::indexed_embedding_inputs::materialize(
        &catalog,
        &settings,
        Some(&owners),
        &VerificationBudget::default(),
    )
    .unwrap();
    let spec = fixture.spec();
    let mut store = VectorStore::open(&fixture.fs, Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    let input = &inputs.units[0];
    assert!(
        inputs
            .units
            .iter()
            .all(|unit| unit.input_hash == input.input_hash)
    );
    store
        .put_batch(
            &space,
            std::slice::from_ref(&input.input_hash),
            &[vec![1.0, 0.0]],
            true,
            &input.dependency_fingerprint,
        )
        .unwrap();
    let original = PreparationCursor {
        version: INVENTORY_VERSION,
        incarnation: catalog_incarnation(reader.vault_id(), reader.snapshot()).unwrap(),
        policy: inventory.policy.clone(),
        since_seq: 0,
        through_seq: reader.snapshot().generation,
        after: None,
        complete: false,
    };
    let prefix = PreparationCursor {
        after: Some(UnitOwnerCursor {
            modified_seq: bindings[15].modified_seq,
            owner: bindings[15].owner.clone(),
        }),
        ..original.clone()
    };
    let first: Vec<_> = bindings[..16]
        .iter()
        .map(|binding| {
            (
                binding.clone(),
                reader
                    .unit_descriptors_for_owner(&inventory.policy, &binding.owner, 4096)
                    .unwrap(),
            )
        })
        .collect();
    store
        .acknowledge_unit_owners_checked(&space, &first, Some(&prefix), false, &spec, || {
            inputs.recheck(&catalog)
        })
        .unwrap();
    let suffix = reader
        .unit_owner_bindings_page(
            &inventory.policy,
            prefix.after.as_ref(),
            prefix.since_seq,
            prefix.through_seq,
            128,
        )
        .unwrap();
    assert_eq!(suffix, bindings[16..]);
    let final_cursor = PreparationCursor {
        after: Some(UnitOwnerCursor {
            modified_seq: suffix[0].modified_seq,
            owner: suffix[0].owner.clone(),
        }),
        complete: true,
        ..prefix.clone()
    };
    let final_owners = vec![(
        suffix[0].clone(),
        reader
            .unit_descriptors_for_owner(&inventory.policy, &suffix[0].owner, 4096)
            .unwrap(),
    )];
    let failed = store.acknowledge_unit_owners_checked(
        &space,
        &final_owners,
        Some(&final_cursor),
        true,
        &spec,
        || {
            Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "injected final guard failure",
            ))
        },
    );
    assert_eq!(failed.unwrap_err().code, ErrorCode::FreshnessConflict);
    assert_eq!(
        store
            .preparation_cursor(&space, &inventory.policy, &original.incarnation)
            .unwrap(),
        Some(prefix)
    );
    assert!(!store.owner_binding_ready(&space, &suffix[0]).unwrap());
    assert!(store.active().unwrap().is_none());
    drop(store);
    drop(inputs);
    drop(reader);
    drop(writer);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let resumed = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&resumed, 17);
    assert_eq!(resumed.generated_inputs, 0);
    assert_eq!(
        resumed.reused_inputs, 1,
        "only the remaining owner input is selected"
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
    let checked = fixture
        .offline()
        .embeddings_check(&settings, None, false)
        .unwrap();
    assert_eq!(checked.coverage.available_units, 17);
    assert_eq!(checked.coverage.pending_units, 0);
}

#[test]
fn normalized_evidence_set_final_proof_rejects_changed_selected_source() {
    use crate::retrieval::{
        ContextCheckpoint, ContextFault, ContextOptions, ContextRequest, ContextScope,
        context_evidence, indexed_semantic,
    };
    struct ChangeBeforeEmission {
        path: std::path::PathBuf,
        reached: AtomicBool,
    }
    impl ContextFault for ChangeBeforeEmission {
        fn check(&self, checkpoint: ContextCheckpoint) -> Result<()> {
            assert_eq!(
                checkpoint,
                ContextCheckpoint::BeforeFinalVerification { attempt: 0 }
            );
            assert!(!self.reached.swap(true, Ordering::SeqCst));
            std::fs::write(&self.path, SECOND).unwrap();
            Ok(())
        }
    }
    // The private coordinator is the same route used by ordinary app dispatch.
    // Each arm gets an independent disposable vault; no paid/live evidence is
    // changed and the fault runs only after allocation, before final proof.
    for arm in ["L", "F0", "F1", "hybrid-lexical"] {
        let fixture = normalized();
        let (source, revision) = add(&fixture, TITLE, "allocation-proof.txt", FIRST);
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let runtime = runtime(&fixture.service, &dispatch);
        let synced = fixture
            .app
            .embeddings_sync(&EmbeddingSettings::default(), &runtime)
            .unwrap();
        complete(&synced, 1);
        let store = VectorStore::open(&fixture.fs, None).unwrap();
        let state = store.active().unwrap().unwrap();
        let fault = Arc::new(ChangeBeforeEmission {
            path: fixture
                .fs
                .root()
                .path()
                .join(content_path(&source, &revision).as_str()),
            reached: AtomicBool::new(false),
        });
        let options = ContextOptions {
            fault: Some(fault.clone()),
            experimental_hybrid_lexical_evidence: arm == "hybrid-lexical",
            ..Default::default()
        };
        let request = ContextRequest {
            scope: ContextScope::IndexedDocuments,
            documents: plan(
                if arm == "hybrid-lexical" {
                    SearchMode::Hybrid
                } else {
                    SearchMode::Semantic
                },
                vec![source],
                8,
            ),
            ..Default::default()
        };
        let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
        let result = context_evidence::with_policy_for_test(
            if arm == "hybrid-lexical" { "B" } else { arm },
            || {
                indexed_semantic::context(
                    &catalog,
                    "Signalneedle",
                    &request,
                    &options,
                    &state,
                    &[1.0, 0.0],
                )
            },
        );
        assert_eq!(
            result.unwrap_err().code,
            ErrorCode::FreshnessConflict,
            "{arm}"
        );
        assert!(fault.reached.load(Ordering::SeqCst), "{arm}");
        assert_eq!(
            responses.calls.load(Ordering::SeqCst),
            1,
            "only fixture vector acquisition"
        );
    }
}

fn capture(title: &str, origin: &str, body: &str) -> CaptureRequest {
    CaptureRequest {
        title: title.into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: origin.into(),
        original: body.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: None,
    }
}

fn add(fixture: &Fixture, title: &str, origin: &str, body: &str) -> (RecordId, RecordId) {
    let added = fixture
        .app
        .source_add(capture(title, origin, body))
        .unwrap();
    (
        added.allocated_ids["source"].clone(),
        added.allocated_ids["revision"].clone(),
    )
}

fn content_path(source: &RecordId, revision: &RecordId) -> VaultRelativePath {
    VaultRelativePath::new(format!("sources/{source}/revisions/{revision}/content.md")).unwrap()
}

fn ledger(fixture: &Fixture, report: &EmbeddingReport) -> JobLedger {
    JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        report
            .run_id
            .clone()
            .expect("paid sync must identify its accounting run"),
        options(),
    )
    .unwrap()
}

// Inspect immutable inputs actually submitted by the accounted public workflow.
// Do not seed a corpus through the legacy reader or call the new renderer directly.
fn paid_inputs(fixture: &Fixture, inspection: &LedgerInspection) -> Vec<EmbeddingInput> {
    let mut inputs = Vec::new();
    for task in inspection.tasks.values() {
        let reference = &task.spec.input;
        assert!(reference.byte_len <= 256 * 1024);
        let bytes = std::fs::read(fixture.fs.root().path().join(reference.path.as_str())).unwrap();
        assert_eq!(bytes.len() as u64, reference.byte_len);
        assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
        let remote: RemoteInput = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            crate::graph::packet::canonical_json(&remote).unwrap(),
            bytes
        );
        let RemoteOperation::Embed { inputs: batch, .. } = remote.operation else {
            panic!("sync retained a non-embedding task");
        };
        for input in batch {
            assert_eq!(input.input_hash, Blake3Hash::digest(input.utf8.as_bytes()));
            inputs.push(input);
        }
    }
    inputs
}

fn complete(report: &EmbeddingReport, count: usize) {
    assert!(report.published);
    assert_eq!(report.coverage.eligible_units, count);
    assert_eq!(report.coverage.available_units, count);
    assert_eq!(report.coverage.missing_units, 0);
    assert_eq!(report.coverage.corrupt_units, 0);
    assert_eq!(report.coverage.pending_units, 0);
}

fn plan(mode: SearchMode, sources: Vec<RecordId>, candidates: usize) -> QueryPlan {
    QueryPlan {
        mode,
        filters: SearchFilters {
            source_ids: sources,
            ..Default::default()
        },
        limits: SearchLimits {
            hits: candidates,
            candidates,
            excerpt_bytes: 256,
        },
        cursor: None,
    }
}

fn exact_source_hit(hit: &SearchHit, source: &RecordId, revision: &RecordId, body: &str) {
    assert_eq!(hit.source_id.as_ref(), Some(source));
    assert_eq!(hit.owner_revision.as_ref(), Some(revision));
    assert_eq!(hit.locator.path, content_path(source, revision));
    for excerpt in std::iter::once(&hit.excerpt).chain(&hit.secondary_excerpts) {
        assert_eq!(excerpt.label, ExcerptLabel::CapturedSource);
        assert!(!excerpt.text.is_empty());
        let slice = body
            .get(excerpt.span.start() as usize..excerpt.span.end() as usize)
            .expect("selected Source span must be inside exact captured UTF-8 boundaries");
        assert_eq!(slice, excerpt.text);
        let Some(CitationRef::Source(reference)) = &excerpt.citation else {
            panic!("selected captured evidence must carry a SourceRef");
        };
        assert_eq!(&reference.source_id, source);
        assert_eq!(&reference.source_revision, revision);
        assert_eq!(reference.span, excerpt.span);
        assert_eq!(reference.quote_hash, Blake3Hash::digest(slice.as_bytes()));
    }
}

fn selected_verification(hits: &HitSet) {
    assert!(!hits.network_used);
    assert!(matches!(
        &hits.verification,
        SnapshotVerification::IndexedEvidence {
            global_membership_verified: false,
            ..
        }
    ));
}

#[test]
fn normalized_accounted_sync_reuses_exact_capture_after_own_jobs_and_source_title_change() {
    let fixture = normalized();
    let (source, revision) = add(&fixture, TITLE, "amber.txt", FIRST);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let settings = EmbeddingSettings::default();

    let first = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&first, 1);
    assert_eq!(first.generated_inputs, 1);
    assert!(first.network_used);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    let job = ledger(&fixture, &first);
    let inspection = job.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Completed);
    assert_eq!(inspection.budget.dispatched_requests, 1);
    assert_eq!(inspection.attempts.len(), 1);
    assert!(
        inspection
            .tasks
            .values()
            .all(|task| task.state == TaskState::Completed)
    );
    let attempt = &inspection.attempts[0];
    assert_eq!(attempt.phase, AttemptPhase::Settled);
    assert!(attempt.outputs.is_empty());
    assert_eq!(attempt.cache_outputs.len(), 1);
    let receipt = crate::jobs::checkpoint::receipt(
        &fixture.fs,
        attempt.receipt.as_ref().expect("settled attempt receipt"),
    )
    .unwrap();
    assert_eq!(receipt.attempt, attempt.attempt);
    assert_eq!(receipt.output_disposition, OutputDisposition::Validated);
    assert_eq!(receipt.cache_outputs, attempt.cache_outputs);
    assert_eq!(receipt.billing, attempt.billing);
    let inputs = paid_inputs(&fixture, &inspection);
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].utf8.contains(TITLE));
    assert!(inputs[0].utf8.contains(FIRST.trim_end()));
    assert_eq!(inputs[0].input_hash, attempt.cache_outputs[0].input_hash);
    assert!(
        VectorStore::open(&fixture.fs, None)
            .unwrap()
            .verify_ref(&attempt.cache_outputs[0])
            .unwrap()
    );

    // Run/RunEvent/checkpoint publication must neither become eligible text nor
    // leave the normalized cache different from a canonical maintenance check.
    let check = fixture.app.check().unwrap();
    assert!(check.complete);
    assert_eq!(check.error_count, 0, "{:?}", check.diagnostics);
    assert_eq!(check.cache_matches_canonical, Some(true));
    let local = fixture
        .app
        .embeddings_check(&settings, None, false)
        .unwrap();
    assert_eq!(local.coverage.eligible_units, 1);
    assert_eq!(local.coverage.available_units, 1);
    assert!(!local.network_used);
    let repeat = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&repeat, 1);
    assert_eq!(repeat.generated_inputs, 0);
    assert_eq!(repeat.reused_inputs, 0);
    assert!(!repeat.network_used);

    let renamed = fixture
        .app
        .source_refresh_with_title(
            source.clone(),
            capture(TITLE, "amber.txt", FIRST),
            Some("Mutable Source title changed"),
        )
        .unwrap();
    assert!(
        renamed.change.is_some(),
        "fixture must exercise a real Source title write"
    );
    assert_eq!(renamed.allocated_ids["revision"], revision);
    let titled = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&titled, 1);
    assert_eq!(titled.generated_inputs, 0);
    assert_eq!(titled.reused_inputs, 1);
    assert!(!titled.network_used);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    assert!(paid_inputs(&fixture, &job.inspect().unwrap()) == inputs);
    let store = VectorStore::open(&fixture.fs, None).unwrap();
    assert!(store.verify_ref(&attempt.cache_outputs[0]).unwrap());
    drop(store);
    let checked = fixture.app.check().unwrap();
    assert!(checked.complete);
    assert_eq!(checked.error_count, 0, "{:?}", checked.diagnostics);
    assert_eq!(checked.cache_matches_canonical, Some(true));
}

#[test]
fn normalized_cached_semantic_and_hybrid_filter_before_cap_emit_exact_utf8_sources() {
    let fixture = normalized();
    let (outside, _) = add(
        &fixture,
        "Higher scoring outside source",
        "outside.txt",
        FIRST,
    );
    let responses = Arc::new(Responses {
        calls: Default::default(),
        hook: None,
        vectors: vec![vec![1.0, 0.0], vec![0.6, 0.8]],
    });
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let settings = EmbeddingSettings::default();
    complete(
        &fixture.app.embeddings_sync(&settings, &runtime).unwrap(),
        1,
    );
    let (wanted, revision) = add(&fixture, "Wanted captured source", "wanted.txt", SECOND);
    let synced = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&synced, 2);
    assert_eq!(synced.generated_inputs, 1);
    assert_eq!(synced.reused_inputs, 0);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1.0, 0.0]);
    let offline = fixture.offline();
    let unfiltered = offline
        .semantic_search(
            "Signalneedle",
            &plan(SearchMode::Semantic, vec![], 1),
            None,
            true,
            false,
            None,
        )
        .unwrap();
    assert_eq!(unfiltered.hits.len(), 1);
    assert_eq!(unfiltered.hits[0].source_id.as_ref(), Some(&outside));

    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let request = plan(mode, vec![wanted.clone()], 1);
        let cached = offline
            .semantic_search("Signalneedle", &request, None, true, false, None)
            .unwrap();
        assert!(!cached.network_used);
        assert_eq!(cached.hits.len(), 1, "{mode:?}");
        assert_eq!(cached.hits[0].source_id.as_ref(), Some(&wanted));
        assert_eq!(cached.hits[0].owner_revision.as_ref(), Some(&revision));
        let selected = offline
            .semantic_search_selected("Signalneedle", &request, None, false)
            .unwrap();
        selected_verification(&selected);
        assert_eq!(selected.hits.len(), 1, "{mode:?}");
        exact_source_hit(&selected.hits[0], &wanted, &revision, SECOND);
        assert!(selected.hits[0].excerpt.text.contains("café 東京 🦀"));
        assert_eq!(selected.candidate_count, 1);
        let context = offline
            .semantic_context(
                "Signalneedle",
                &crate::retrieval::ContextRequest {
                    scope: crate::retrieval::ContextScope::IndexedDocuments,
                    documents: request.clone(),
                    ..Default::default()
                },
                None,
                true,
                false,
            )
            .unwrap();
        assert!(!context.network_used);
        assert!(!context.passages().is_empty());
        assert!(context.text().contains("29 violet tokens"));
        for passage in context.passages() {
            assert_eq!(passage.locator.path, content_path(&wanted, &revision));
            assert_eq!(
                SECOND.get(passage.span.start() as usize..passage.span.end() as usize),
                Some(passage.text.as_str())
            );
            assert!(!passage.citations.is_empty());
            for citation in &passage.citations {
                let CitationRef::Source(reference) = citation else {
                    panic!("captured context needs SourceRef");
                };
                assert_eq!(reference.source_id, wanted);
                assert_eq!(reference.source_revision, revision);
                assert_eq!(
                    reference.quote_hash,
                    Blake3Hash::digest(passage.text.as_bytes())
                );
            }
        }
    }
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn normalized_cache_only_sync_needs_no_runtime_and_missing_query_or_cache_is_explicit() {
    let fixture = normalized();
    add(&fixture, TITLE, "cached.txt", FIRST);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let settings = EmbeddingSettings::default();
    let offline = fixture.offline();
    assert_eq!(
        offline.embeddings_sync_cached(&settings).unwrap_err().code,
        ErrorCode::OfflineUnavailable
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
    complete(
        &fixture.app.embeddings_sync(&settings, &runtime).unwrap(),
        1,
    );
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1.0, 0.0]);
    // No provider configuration remains available to the cache-only operations.
    std::fs::remove_file(&fixture.config).unwrap();
    let cached = offline.embeddings_sync_cached(&settings).unwrap();
    complete(&cached, 1);
    assert_eq!(cached.generated_inputs, 0);
    assert!(cached.run_id.is_none());
    assert!(!cached.network_used);
    let local = offline.embeddings_check(&settings, None, false).unwrap();
    assert_eq!(local.coverage.available_units, 1);
    assert!(!local.network_used);
    let request = plan(SearchMode::Semantic, vec![], 1);
    let hits = offline
        .semantic_search_selected("Signalneedle", &request, None, false)
        .unwrap();
    selected_verification(&hits);
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(
        offline
            .semantic_search_selected("Never cached query", &request, None, false)
            .unwrap_err()
            .code,
        ErrorCode::OfflineUnavailable
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);

    for name in [
        "embeddings.sqlite3",
        "embeddings.sqlite3-wal",
        "embeddings.sqlite3-shm",
    ] {
        let path = fixture.fs.root().path().join(".wiki/cache").join(name);
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    assert_eq!(
        offline.embeddings_sync_cached(&settings).unwrap_err().code,
        ErrorCode::OfflineUnavailable
    );
    assert_eq!(
        offline
            .semantic_search_selected("Signalneedle", &request, None, false)
            .unwrap_err()
            .code,
        ErrorCode::OfflineUnavailable
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn normalized_refresh_new_owner_and_withdrawal_use_only_current_vector_membership() {
    let fixture = normalized();
    let (changing, old_revision) = add(&fixture, TITLE, "changing.txt", FIRST);
    let stable_body = "Signalneedle stable owner retains 41 green tokens.\n";
    let (stable, _) = add(&fixture, "Stable title", "stable.txt", stable_body);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let settings = EmbeddingSettings::default();
    let first = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&first, 2);
    assert_eq!(first.generated_inputs, 2);
    let old_inputs = paid_inputs(&fixture, &ledger(&fixture, &first).inspect().unwrap());
    let old_input = old_inputs
        .iter()
        .find(|input| input.utf8.contains("17 amber tokens"))
        .unwrap();
    let space = first.space.clone().unwrap();
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1.0, 0.0]);

    let refreshed = fixture
        .app
        .source_refresh(changing.clone(), capture(TITLE, "changing.txt", SECOND))
        .unwrap();
    let new_revision = refreshed.allocated_ids["revision"].clone();
    assert_ne!(new_revision, old_revision);
    let missing = fixture
        .app
        .embeddings_check(&settings, None, false)
        .unwrap();
    assert_eq!(missing.coverage.eligible_units, 2);
    assert_eq!(missing.coverage.available_units, 1);
    assert_eq!(missing.coverage.missing_units, 1);
    let request = plan(SearchMode::Semantic, vec![changing.clone()], 8);
    let before = fixture
        .offline()
        .semantic_search_selected("Signalneedle", &request, None, false)
        .unwrap();
    selected_verification(&before);
    assert!(
        before.hits.is_empty(),
        "old owner's vector cannot stand in for changed input"
    );

    let new_body = "Signalneedle new owner stores 53 silver tokens. Unicode café 東京 🦀.\n";
    let (new_owner, new_owner_revision) = add(&fixture, "New owner title", "new.txt", new_body);
    let missing = fixture
        .app
        .embeddings_check(&settings, None, false)
        .unwrap();
    assert_eq!(missing.coverage.eligible_units, 3);
    assert_eq!(missing.coverage.available_units, 1);
    assert_eq!(missing.coverage.missing_units, 2);
    let replacement = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&replacement, 3);
    assert_eq!(replacement.generated_inputs, 2);
    assert_eq!(replacement.reused_inputs, 0);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 4);
    let current = fixture
        .offline()
        .semantic_search_selected("Signalneedle", &request, None, false)
        .unwrap();
    selected_verification(&current);
    assert_eq!(current.hits.len(), 1);
    exact_source_hit(&current.hits[0], &changing, &new_revision, SECOND);
    let added = fixture
        .offline()
        .semantic_search_selected(
            "Signalneedle",
            &plan(SearchMode::Semantic, vec![new_owner.clone()], 8),
            None,
            false,
        )
        .unwrap();
    assert_eq!(added.hits.len(), 1);
    exact_source_hit(&added.hits[0], &new_owner, &new_owner_revision, new_body);

    fixture
        .app
        .source_withdraw(
            stable.clone(),
            "Withdraw captured source from current evidence",
        )
        .unwrap();
    let cached = fixture.offline().embeddings_sync_cached(&settings).unwrap();
    complete(&cached, 2);
    assert_eq!(cached.generated_inputs, 0);
    assert!(!cached.network_used);
    let withdrawn = fixture
        .offline()
        .semantic_search_selected(
            "Signalneedle",
            &plan(SearchMode::Semantic, vec![stable.clone()], 8),
            None,
            false,
        )
        .unwrap();
    selected_verification(&withdrawn);
    assert!(withdrawn.hits.is_empty());
    let all = fixture
        .offline()
        .semantic_search_selected(
            "Signalneedle",
            &plan(SearchMode::Semantic, vec![], 8),
            None,
            false,
        )
        .unwrap();
    selected_verification(&all);
    assert_eq!(all.hits.len(), 2);
    assert!(
        all.hits
            .iter()
            .all(|hit| hit.source_id.as_ref() != Some(&stable)
                && hit.owner_revision.as_ref() != Some(&old_revision))
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 4);
    // Immutable vectors/revisions can remain physically retained while being
    // excluded from the current corpus; deletion is not the membership proof.
    assert!(
        VectorStore::open(&fixture.fs, None)
            .unwrap()
            .vector(&space, &old_input.input_hash)
            .unwrap()
            .is_some()
    );
    let old_bytes = std::fs::read(
        fixture
            .fs
            .root()
            .path()
            .join(content_path(&changing, &old_revision).as_str()),
    )
    .unwrap();
    assert_eq!(old_bytes, FIRST.as_bytes());
}

struct ReceivedOnce {
    armed: AtomicBool,
}
impl LedgerFault for ReceivedOnce {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterReceived && self.armed.swap(false, Ordering::SeqCst) {
            Err(WikiError::new(
                ErrorCode::Internal,
                "stop at retained Received boundary",
            ))
        } else {
            Ok(())
        }
    }
}

#[test]
fn normalized_received_source_change_rejects_paid_output_without_resend_or_hold_release() {
    let fixture = normalized();
    let (source, old_revision) = add(&fixture, TITLE, "received.txt", FIRST);
    let callback_app = OfflineApp::new(fixture.fs.clone(), OperationOptions::default()).unwrap();
    let callback_source = source.clone();
    let callback_reached = Arc::new(AtomicBool::new(false));
    let reached = callback_reached.clone();
    let responses = Arc::new(Responses {
        calls: Default::default(),
        vectors: vec![],
        hook: Some(Box::new(move |index| {
            if index == 0 {
                // The real public mutation inside the existing mock transport
                // hook also requires dispatch to release the vault writer.
                callback_app
                    .source_refresh(
                        callback_source.clone(),
                        capture(TITLE, "received.txt", SECOND),
                    )
                    .unwrap();
                reached.store(true, Ordering::SeqCst);
            }
        })),
    });
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let mut interrupted = runtime(&fixture.service, &dispatch);
    let fault = Arc::new(ReceivedOnce {
        armed: AtomicBool::new(true),
    });
    interrupted.job_options.fault = Some(fault.clone());
    let settings = EmbeddingSettings::default();
    let stopped = fixture
        .app
        .embeddings_sync(&settings, &interrupted)
        .unwrap_err();
    assert!(
        callback_reached.load(Ordering::SeqCst),
        "source-change transport callback must have completed"
    );
    assert!(
        !fault.armed.load(Ordering::SeqCst),
        "AfterReceived fault must have been reached"
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    let run: RecordId = serde_json::from_value(stopped.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run,
        options(),
    )
    .unwrap();
    let before = job.inspect().unwrap();
    assert_eq!(before.attempts.len(), 1);
    assert_eq!(before.attempts[0].phase, AttemptPhase::Received);
    assert!(before.attempts[0].spool.is_some());
    assert!(before.attempts[0].receipt.is_none());
    assert_eq!(before.budget.dispatched_requests, 1);
    assert_eq!(before.budget.outstanding.requests, 1);
    let changed = fixture
        .app
        .source_refresh(
            source.clone(),
            capture(
                TITLE,
                "received.txt",
                "Signalneedle current owner after Received stores 61 blue tokens.\n",
            ),
        )
        .unwrap();
    assert_ne!(changed.allocated_ids["revision"], old_revision);

    let resumed = runtime(&fixture.service, &dispatch);
    let replacement = fixture.app.embeddings_sync(&settings, &resumed).unwrap();
    complete(&replacement, 1);
    assert_eq!(replacement.generated_inputs, 1);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    assert_ne!(replacement.run_id.as_ref(), Some(&before.spec.run_id));
    assert!(
        replacement
            .warnings
            .iter()
            .any(|warning| warning.contains("rejected embedding response"))
    );
    let after = job.inspect().unwrap();
    assert_eq!(after.state, RunState::Failed);
    assert_eq!(
        after.tasks[&before.attempts[0].attempt.task_key].state,
        TaskState::Failed
    );
    assert_eq!(after.attempts.len(), 1);
    let attempt = &after.attempts[0];
    assert_eq!(attempt.attempt, before.attempts[0].attempt);
    assert_eq!(attempt.phase, AttemptPhase::Settled);
    assert_eq!(attempt.spool, before.attempts[0].spool);
    assert!(attempt.outputs.is_empty());
    assert!(attempt.cache_outputs.is_empty());
    let receipt =
        crate::jobs::checkpoint::receipt(&fixture.fs, attempt.receipt.as_ref().unwrap()).unwrap();
    assert_eq!(receipt.output_disposition, OutputDisposition::Rejected);
    assert!(receipt.outputs.is_empty());
    assert!(receipt.cache_outputs.is_empty());
    assert_eq!(receipt.attempt, before.attempts[0].attempt);
    // This mock profile supplies no rate card. Known usage cannot erase its
    // unknown cost reservation merely because stale output was rejected.
    assert_eq!(attempt.billing, BillingDisposition::UnknownReserved);
    assert_eq!(
        after.budget.unknown_attempts,
        vec![attempt.attempt.attempt_id.clone()]
    );
    assert_eq!(after.budget.outstanding, before.budget.outstanding);
    assert_eq!(
        after.budget.dispatched_requests,
        before.budget.dispatched_requests
    );
    let active = VectorStore::open(&fixture.fs, None)
        .unwrap()
        .active()
        .unwrap()
        .unwrap();
    assert_eq!(Some(&active.id), replacement.active_space.as_ref());
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1.0, 0.0]);
    let current = fixture
        .offline()
        .semantic_search_selected(
            "Signalneedle",
            &plan(SearchMode::Semantic, vec![source.clone()], 8),
            None,
            false,
        )
        .unwrap();
    selected_verification(&current);
    assert_eq!(current.hits.len(), 1);
    assert_eq!(
        current.hits[0].owner_revision.as_ref(),
        Some(&changed.allocated_ids["revision"])
    );
    assert_eq!(
        std::fs::read(
            fixture
                .fs
                .root()
                .path()
                .join(content_path(&source, &old_revision).as_str())
        )
        .unwrap(),
        FIRST.as_bytes()
    );

    let repeated = fixture.app.embeddings_sync(&settings, &resumed).unwrap();
    complete(&repeated, 1);
    assert_eq!(repeated.generated_inputs, 0);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    let retained = job.inspect().unwrap();
    assert_eq!(retained.attempts, after.attempts);
    assert_eq!(
        retained.budget.unknown_attempts,
        after.budget.unknown_attempts
    );
    assert_eq!(retained.budget.outstanding, after.budget.outstanding);
}

#[test]
fn normalized_empty_preparation_and_withdrawal_preserve_the_active_space() {
    let fixture = normalized();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let settings = EmbeddingSettings::default();
    let empty = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    assert_eq!(empty.coverage.eligible_units, 0);
    assert!(!empty.published);
    assert!(empty.active_space.is_none());
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
    let (source, _) = add(&fixture, TITLE, "empty-then-captured.txt", FIRST);
    let prepared = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&prepared, 1);
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1.0, 0.0]);
    fixture
        .app
        .source_withdraw(source, "All captured support withdrawn")
        .unwrap();
    let replacement_settings = EmbeddingSettings {
        document_prefix: "replacement:".into(),
        ..settings.clone()
    };
    let replacement = fixture
        .app
        .embeddings_sync(&replacement_settings, &runtime)
        .unwrap();
    assert_eq!(replacement.coverage.eligible_units, 0);
    assert!(!replacement.published);
    assert_eq!(replacement.active_space, prepared.active_space);
    assert_ne!(replacement.space, prepared.active_space);
    let offline = fixture.offline();
    let cached = offline.embeddings_sync_cached(&settings).unwrap();
    assert_eq!(cached.coverage.eligible_units, 0);
    assert!(!cached.published);
    assert_eq!(cached.active_space, prepared.active_space);
    let hits = offline
        .semantic_search(
            "Signalneedle",
            &plan(SearchMode::Semantic, vec![], 1),
            None,
            true,
            false,
            None,
        )
        .unwrap();
    assert!(hits.hits.is_empty());
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn normalized_membership_final_source_recheck_rolls_back_active_publication() {
    let fixture = normalized();
    let (source, revision) = add(&fixture, TITLE, "final-membership.txt", FIRST);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let prepared = fixture
        .app
        .embeddings_sync(
            &EmbeddingSettings::default(),
            &runtime(&fixture.service, &dispatch),
        )
        .unwrap();
    complete(&prepared, 1);
    let settings = EmbeddingSettings {
        document_prefix: "candidate:".into(),
        ..Default::default()
    };
    let spec =
        crate::retrieval::spaces::SpaceSpec::from_service(&fixture.service, settings.clone())
            .unwrap();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let mut inputs = super::indexed_embedding_inputs::materialize(
        &catalog,
        &settings,
        None,
        &crate::retrieval::VerificationBudget::default(),
    )
    .unwrap();
    let units = std::mem::take(&mut inputs.units);
    let snapshot = inputs.snapshot.clone();
    let writer = fixture.writer();
    let mut store = VectorStore::open(&fixture.fs, Some(&writer)).unwrap();
    let candidate = store.prepare_space(&spec).unwrap();
    // Direct cache fixture for transaction/freshness mechanics only; this does
    // not stand in for accounted corpus acquisition or quality observations.
    store
        .put_batch(
            &candidate,
            &[units[0].input_hash.clone()],
            &[vec![1.0, 0.0]],
            true,
            &units[0].dependency_fingerprint,
        )
        .unwrap();
    let changed = FIRST.replace("17", "99");
    assert_eq!(changed.len(), FIRST.len());
    let raw_path = fixture
        .fs
        .root()
        .path()
        .join(content_path(&source, &revision).as_str());
    let mut reached = false;
    let result = store.memberships_with_spec_checked(
        &candidate,
        &snapshot,
        &units,
        true,
        Some(&spec),
        || {
            reached = true;
            std::fs::write(&raw_path, &changed).unwrap();
            inputs.recheck(&catalog)
        },
    );
    assert!(reached);
    assert_eq!(result.unwrap_err().code, ErrorCode::FreshnessConflict);
    assert_eq!(
        store.active().unwrap().map(|state| state.id),
        prepared.active_space
    );
    assert!(!store.space(&candidate).unwrap().unwrap().active);
    assert_eq!(std::fs::read(&raw_path).unwrap(), changed.as_bytes());
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn normalized_vector_read_reservation_stops_wide_valid_cache_reads() {
    let fixture = normalized();
    let spec = fixture.spec();
    let input = Blake3Hash::digest(b"wide cached vector mechanics fixture");
    let writer = fixture.writer();
    let mut store = VectorStore::open(&fixture.fs, Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    store
        .put_batch(
            &space,
            &[input.clone()],
            &[vec![1.0; 65_536]],
            true,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    drop(store);
    drop(writer);
    let budget = crate::retrieval::vectors::VectorReadBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
    .unwrap();
    let store = VectorStore::open_bounded_snapshot(&fixture.fs, &budget).unwrap();
    let mut stopped = false;
    for _ in 0..512 {
        match store.vector(&space, &input) {
            Ok(Some(vector)) => assert_eq!(vector.len(), 65_536),
            Err(error) => {
                assert_eq!(error.code, ErrorCode::BudgetExceeded);
                stopped = true;
                break;
            }
            other => panic!("valid wide cache vector became unavailable: {other:?}"),
        }
    }
    assert!(
        stopped,
        "actual repeated cache reads must stop within the shared reservation"
    );
    let usage = budget.usage();
    assert!(usage.vector_reads > 0);
    assert!(usage.vector_bytes_scanned > 0 && usage.vector_bytes_scanned <= 64 * 1024 * 1024);
    assert!(usage.sql_vm_steps <= 10_000_000);
}

#[test]
fn normalized_semantic_cursor_tracks_relevant_cache_contents() {
    let fixture = normalized();
    add(&fixture, "Cursor first", "cursor-a.txt", FIRST);
    let responses = Arc::new(Responses {
        calls: Default::default(),
        hook: None,
        vectors: vec![vec![1.0, 0.0], vec![0.6, 0.8]],
    });
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    fixture
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap();
    add(&fixture, "Cursor second", "cursor-b.txt", SECOND);
    fixture
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap();
    let spec = fixture.spec();
    fixture.query_seed(&spec, "Signalneedle", vec![1.0, 0.0]);
    let offline = fixture.offline();
    let request = QueryPlan {
        limits: SearchLimits {
            hits: 1,
            candidates: 2,
            excerpt_bytes: 256,
        },
        ..plan(SearchMode::Semantic, vec![], 2)
    };
    let first = offline
        .semantic_search("Signalneedle", &request, None, true, false, None)
        .unwrap();
    let mut page = request.clone();
    page.cursor = Some(first.next_cursor.expect("two owners need next page"));
    fixture.query_seed(&spec, "Signalneedle", vec![0.0, 1.0]);
    assert!(
        offline
            .semantic_search("Signalneedle", &page, None, true, false, None)
            .is_err(),
        "changed query-vector ranking must invalidate cursor"
    );
    fixture.query_seed(&spec, "Signalneedle", vec![1.0, 0.0]);
    let writer = fixture.writer();
    let mut store = VectorStore::open(&fixture.fs, Some(&writer)).unwrap();
    store
        .put_batch(
            &spec.id().unwrap(),
            &[Blake3Hash::digest(b"unindexed irrelevant input")],
            &[vec![0.0, 1.0]],
            false,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    drop(store);
    drop(writer);
    let second = offline
        .semantic_search("Signalneedle", &page, None, true, false, None)
        .unwrap();
    assert_eq!(second.hits.len(), 1);
    assert_ne!(second.hits[0].locator.path, first.hits[0].locator.path);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
}

// Explicit, supervisor-owned cost control. This never opens retained live fixtures.
mod scale_attribution {
    use super::*;
    use crate::{
        catalog::query_types::{QueryCatalog, QueryReadLimits},
        config::providers::ProviderConfig,
        providers::{credentials::*, dispatcher::Dispatcher, types::*},
        retrieval::{
            indexed_units::{CachedDocumentUnits, UnitBudget, UnitLimits},
            render::TargetKind,
            spaces::SpaceSpec,
            vectors::{DenseScan, VectorReadBudget},
        },
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use serde::Deserialize;
    use serde_json::{Value, json};
    use std::{
        collections::BTreeSet,
        fs::{self, OpenOptions},
        io::Write,
        path::{Path, PathBuf},
        sync::atomic::AtomicUsize,
        time::{Duration, Instant},
    };

    const DIMENSIONS: usize = 1536;
    const QUERY: &str = "refreshprobe000000";
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Task {
        version: u32,
        cell: String,
        mode: String,
        owners: usize,
        bytes_per_source: usize,
        vault: PathBuf,
        config: PathBuf,
        report: PathBuf,
        max_input_bytes: usize,
        quality_target_bytes: Option<usize>,
    }
    fn plain(path: &Path) -> Result<()> {
        if !path.is_absolute() {
            return Err(WikiError::invalid("scale path must be absolute"));
        }
        for ancestor in path.ancestors() {
            if fs::symlink_metadata(ancestor).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(WikiError::invalid("scale path has a symlink component"));
            }
        }
        Ok(())
    }
    fn vector(target: bool) -> Vec<f32> {
        let mut value = vec![0.0; DIMENSIONS];
        value[usize::from(!target)] = 1.0;
        value
    }
    struct NoJitter;
    impl JitterSource for NoJitter {
        fn sample_inclusive(&self, _: u64) -> Result<u64> {
            Ok(0)
        }
    }
    struct NoExternalSecrets;
    impl SecretInputs for NoExternalSecrets {
        fn environment(&self, _: &str, _: usize) -> Result<Option<SecretBytes>> {
            Err(WikiError::invalid(
                "scale mock forbids environment credentials",
            ))
        }
        fn file(&self, _: &Path, _: usize) -> Result<SecretBytes> {
            Err(WikiError::invalid("scale mock forbids credential files"))
        }
    }
    struct NoHelpers;
    impl HelperRunner for NoHelpers {
        fn run(
            &self,
            _: &HelperInvocation,
            _: &HelperLimits,
            _: &dyn JobClock,
            _: &CancellationToken,
        ) -> Result<HelperOutput> {
            Err(WikiError::invalid("scale mock forbids credential helpers"))
        }
    }
    struct Mock {
        fs: VaultFs,
        vault: RecordId,
        calls: AtomicUsize,
        items: AtomicUsize,
        task_read_bytes: AtomicUsize,
        max_calls: usize,
    }
    impl Transport for Mock {
        fn execute<'a>(
            &'a self,
            request: AuthenticatedRequest<'a>,
            _: TransportContext,
        ) -> TransportFuture<'a> {
            // Sealed request bytes stay private. Authenticate its retained task.
            let summary = request.summary();
            assert!(self.calls.fetch_add(1, Ordering::SeqCst) < self.max_calls);
            let ledger = JobLedger::new(
                self.fs.clone(),
                self.vault.clone(),
                summary.attempt.run_id.clone(),
                options(),
            )
            .unwrap();
            let inspection = ledger.inspect().unwrap();
            let reference = &inspection.tasks[&summary.attempt.task_key].spec.input;
            assert!(reference.byte_len <= 512 * 1024);
            let path = self.fs.root().path().join(reference.path.as_str());
            plain(&path).unwrap();
            assert!(fs::symlink_metadata(&path).unwrap().is_file());
            let bytes = fs::read(path).unwrap();
            assert_eq!(bytes.len() as u64, reference.byte_len);
            assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
            self.task_read_bytes
                .fetch_add(bytes.len(), Ordering::SeqCst);
            let input: RemoteInput = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(crate::graph::packet::canonical_json(&input).unwrap(), bytes);
            let RemoteOperation::Embed {
                inputs,
                expected_dimensions,
                ..
            } = input.operation
            else {
                panic!("scale mock received a non-embedding task");
            };
            assert!(!inputs.is_empty() && inputs.len() <= 16);
            assert_eq!(expected_dimensions, Some(DIMENSIONS as u32));
            self.items.fetch_add(inputs.len(), Ordering::SeqCst);
            let data: Vec<_> = inputs
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    assert_eq!(Blake3Hash::digest(input.utf8.as_bytes()), input.input_hash);
                    json!({"index":index,"embedding":vector(input.utf8.contains(QUERY))})
                })
                .collect();
            // Omitted usage remains unknown in the actual ledger; no fake cost.
            let reply = TransportReply::new(
                200,
                vec![],
                serde_json::to_vec(&json!({
                    "model":"scale-attribution-mock", "data":data,
                }))
                .unwrap(),
            )
            .unwrap();
            Box::pin(async move { Ok(reply) })
        }
    }
    fn scan_value(scan: &DenseScan) -> Value {
        json!({"hits":scan.hits,"coverage":scan.coverage,
            "available_by_target":scan.available_by_target,
            "owner_cap_reached_by_target":scan.owner_cap_reached_by_target})
    }
    fn run(task: &Task) -> Result<Value> {
        if task.version != 1
            || task.max_input_bytes != 12000
            || task.quality_target_bytes.is_some()
            || !matches!(
                (task.cell.as_str(), task.owners, task.bytes_per_source),
                ("A", 64, 16384) | ("B", 256, 16384) | ("C", 64, 1024) | ("D", 64, 102400)
            )
            || !matches!(task.mode.as_str(), "initial" | "changed" | "exact")
        {
            return Err(WikiError::invalid("unfrozen scale attribution task"));
        }
        for path in [&task.vault, &task.config, &task.report] {
            plain(path)?;
        }
        if task.report.exists()
            || !task.vault.is_dir()
            || !task.config.is_file()
            || !task.config.starts_with(task.vault.parent().unwrap())
        {
            return Err(WikiError::invalid("scale attribution ownership differs"));
        }
        let fs = VaultFs::new(VaultRoot::explicit(&task.vault)?);
        let app = OfflineApp::new(fs.clone(), OperationOptions::default())?;
        let catalog = Catalog::new(fs.clone(), app.vault_id().clone());
        let settings = EmbeddingSettings::default();
        let service = ProviderConfig::load(&task.config)?.authorize(
            &fs,
            app.vault_id(),
            "primary",
            Capability::Embed,
        )?;
        let spec = SpaceSpec::from_service(&service, settings.clone())?;
        if spec.model != "scale-attribution-mock"
            || spec.dimensions != Some(DIMENSIONS as u32)
            || service.service().max_batch_items != Some(16)
            || service.service().max_batch_bytes != Some(262144)
        {
            return Err(WikiError::invalid("scale mock profile differs"));
        }
        let deadline = Instant::now() + Duration::from_millis(2000);
        let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
        let units_budget = UnitBudget::with_deadline(UnitLimits::default(), deadline)?;
        let corpus = CachedDocumentUnits::new(&reader, &settings)?;
        let construction = Instant::now();
        let mut compact = Vec::new();
        let mut owners = BTreeSet::new();
        let mut unique = BTreeSet::new();
        let mut compact_bytes = 0usize;
        let mut identity_hash = blake3::Hasher::new();
        for unit in corpus.replay(&units_budget)? {
            let mut unit = unit?;
            owners.insert(unit.owner.clone());
            unique.insert(unit.input_hash.clone());
            unit.utf8.clear();
            unit.utf8.shrink_to_fit();
            let bytes =
                serde_json::to_vec(&unit).map_err(|_| WikiError::invalid("compact unit"))?;
            compact_bytes += bytes.len();
            if compact.len() >= 4096 || compact_bytes > 4 * 1024 * 1024 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "compact attribution ceiling",
                ));
            }
            identity_hash.update(&(bytes.len() as u64).to_le_bytes());
            identity_hash.update(&bytes);
            compact.push(unit);
        }
        let construction_ns = construction.elapsed().as_nanos();
        let (initial_ceiling, initial_requests) = match task.cell.as_str() {
            "A" => (256, 16),
            "B" => (800, 50),
            "C" => (128, 8),
            "D" => (704, 44),
            _ => (0, 0),
        };
        if owners.len() != task.owners || unique.len() > initial_ceiling {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "scale owner/input admission",
            ));
        }
        let construction_work = units_budget.usage();
        let construction_cache = reader.usage();
        let mut result = json!({"settings":settings,"dimensions":DIMENSIONS,
            "construction_ns":construction_ns,"owners":owners.len(),"units":compact.len(),
            "unique_inputs":unique.len(),"compact_bytes":compact_bytes,
            "identity_hash":format!("blake3:{}",identity_hash.finalize()),
            "construction_work":{"rendered_units":construction_work.units,
                "rendered_bytes":construction_work.render_bytes,"cache_rows":construction_cache.rows,
                "cache_bytes":construction_cache.bytes},
            "construction_scope":"separate cached rendering for admission; no canonical authority",
            "mock_requests":0,"mock_items":0,"immutable_mock_task_read_bytes":0,
            "application_invocations":0,"explicit_query_cache_writes":0,
            "logical_unmeasured_stage_reservation_bytes":512_u64*1024*1024});
        if task.mode == "exact" {
            let phase = VectorReadBudget::new(deadline)?;
            let store = VectorStore::open_bounded_snapshot(&fs, &phase)?;
            let active = store.active()?.ok_or_else(|| {
                WikiError::new(ErrorCode::OfflineUnavailable, "scale active space missing")
            })?;
            if active.spec != spec {
                return Err(WikiError::invalid("scale space changed"));
            }
            let start = Instant::now();
            let cached = store.exact_stream(
                &active.id,
                &vector(true),
                || corpus.replay(&units_budget),
                &[TargetKind::Document],
                80,
                |_| Ok(true),
            )?;
            result["cached_scan_ns"] = json!(start.elapsed().as_nanos());
            result["cached_scan_vector_work"] = serde_json::to_value(phase.usage()).unwrap();
            let before = phase.usage();
            let start = Instant::now();
            let compact_scan = store.exact_stream(
                &active.id,
                &vector(true),
                || Ok(compact.iter().cloned().map(Ok)),
                &[TargetKind::Document],
                80,
                |_| Ok(true),
            )?;
            result["compact_scan_ns"] = json!(start.elapsed().as_nanos());
            let after = phase.usage();
            result["compact_scan_vector_work"] = json!({
                "vector_reads":after.vector_reads-before.vector_reads,
                "vector_bytes_scanned":after.vector_bytes_scanned-before.vector_bytes_scanned,
                "metadata_bytes":after.metadata_bytes_decoded-before.metadata_bytes_decoded,
                "sql_vm_steps":after.sql_vm_steps-before.sql_vm_steps});
            if scan_value(&cached) != scan_value(&compact_scan) {
                return Err(WikiError::invalid("exact compact/cached scan mismatch"));
            }
            if store
                .active()?
                .is_none_or(|now| now.id != active.id || now.spec != active.spec)
            {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "scale active space changed",
                ));
            }
            result["exact_scan_equal"] = json!(true);
            result["exact_scan_result_hash"] = json!(Blake3Hash::digest(
                crate::graph::packet::canonical_json(&scan_value(&cached))?
            ));
            let final_work = units_budget.usage();
            result["cached_render_work"] = json!({"units":final_work.units-construction_work.units,
                "bytes":final_work.render_bytes-construction_work.render_bytes});
            drop(store);
            drop(reader);
            let start = Instant::now();
            let (outcome, observation) =
                super::super::indexed_embedding_inputs::attribution::with_observation(|| {
                    app.embeddings_sync_cached(&settings)
                });
            result["diagnostic_cached_sync_ns"] = json!(start.elapsed().as_nanos());
            result["diagnostic_cached_sync_observation"] =
                serde_json::to_value(observation).unwrap();
            result["diagnostic_cached_sync"] = serde_json::to_value(outcome?).unwrap();
            result["application_invocations"] = json!(1);
            return Ok(result);
        }
        let expected_reused = if task.mode == "initial" {
            0
        } else {
            let policy = crate::retrieval::unit_inventory_types::RenderPolicyId::for_settings(
                &reader.snapshot().parser_fingerprint,
                &settings,
            )?;
            let store = VectorStore::open(&fs, None)?;
            let space = spec.id()?;
            let mut pending_owners = BTreeSet::new();
            for owner in &owners {
                if match reader.unit_owner_binding(&policy, owner)? {
                    Some(binding) => !store.owner_binding_ready(&space, &binding)?,
                    None => true,
                } {
                    pending_owners.insert(owner.clone());
                }
            }
            let pending_hashes = compact
                .iter()
                .filter(|unit| pending_owners.contains(&unit.owner))
                .map(|unit| unit.input_hash.clone())
                .collect::<BTreeSet<_>>();
            pending_hashes.iter().try_fold(0usize, |count, hash| {
                Ok::<_, WikiError>(count + usize::from(store.vector(&space, hash)?.is_some()))
            })?
        };
        result["expected_selected_reused_inputs"] = json!(expected_reused);
        drop(reader);
        let missing = if task.mode == "initial" {
            unique.len()
        } else {
            let store = VectorStore::open(&fs, None)?;
            unique.iter().try_fold(0usize, |count, hash| {
                Ok::<_, WikiError>(count + usize::from(store.vector(&spec.id()?, hash)?.is_none()))
            })?
        };
        let request_ceiling = if task.mode == "changed" {
            1
        } else {
            initial_requests
        };
        if missing == 0
            || (task.mode == "changed" && missing > 16)
            || missing.div_ceil(16) > request_ceiling
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "scale mock request/input admission",
            ));
        }
        result["missing_inputs_before_sync"] = json!(missing);
        let transport = Arc::new(Mock {
            fs: fs.clone(),
            vault: app.vault_id().clone(),
            calls: AtomicUsize::new(0),
            items: AtomicUsize::new(0),
            task_read_bytes: AtomicUsize::new(0),
            max_calls: request_ceiling,
        });
        let dispatch = Dispatcher::new(
            fs.clone(),
            DispatchOptions {
                broker: Arc::new(CredentialBroker::new(CredentialOptions {
                    clock: Arc::new(common::TestClock),
                    inputs: Arc::new(NoExternalSecrets),
                    runner: Arc::new(NoHelpers),
                })),
                transport: transport.clone(),
                jitter: Arc::new(NoJitter),
            },
        );
        let mut runtime = runtime(&service, &dispatch);
        runtime.limits.requests = request_ceiling as u64;
        runtime.limits.request_bytes = Some(16 * 1024 * 1024);
        runtime.limits.response_bytes = Some(request_ceiling as u64 * 8 * 1024 * 1024);
        let start = Instant::now();
        let (outcome, observation) =
            super::super::indexed_embedding_inputs::attribution::with_observation(|| {
                app.embeddings_sync(&settings, &runtime)
            });
        result["sync_elapsed_ns"] = json!(start.elapsed().as_nanos());
        result["sync_observation"] = serde_json::to_value(observation).unwrap();
        result["application_invocations"] = json!(1);
        result["mock_requests"] = json!(transport.calls.load(Ordering::SeqCst));
        result["mock_items"] = json!(transport.items.load(Ordering::SeqCst));
        result["immutable_mock_task_read_bytes"] =
            json!(transport.task_read_bytes.load(Ordering::SeqCst));
        match outcome {
            Err(error) => {
                result["sync_error"] = serde_json::to_value(&error).unwrap();
                return Ok(result);
            }
            Ok(report) => {
                if !report.published
                    || report.generated_inputs != missing
                    || report.reused_inputs != expected_reused
                    || report.coverage.available_units != compact.len()
                {
                    return Err(WikiError::invalid("scale sync coverage/count mismatch"));
                }
                result["sync_report"] = serde_json::to_value(report).unwrap();
            }
        }
        let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1))?;
        let mut store = VectorStore::open(&fs, Some(&writer))?;
        let query = spec.query(QUERY)?;
        store.put_batch(
            &spec.id()?,
            &[query.input_hash],
            &[vector(true)],
            false,
            &Blake3Hash::digest([]),
        )?;
        result["explicit_query_cache_writes"] = json!(1);
        Ok(result)
    }

    #[test]
    #[ignore = "explicit four-cell supervisor-owned mock/attribution control only"]
    fn fixed_scale_attribution() {
        let path = PathBuf::from(
            std::env::var_os("LWIKI_SCALE_ATTRIBUTION_TASK").expect("explicit scale task"),
        );
        plain(&path).unwrap();
        let info = fs::symlink_metadata(&path).unwrap();
        assert!(info.is_file() && info.len() <= 16384);
        let task: Task = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let outcome = run(&task);
        let envelope = match outcome {
            Ok(data) => json!({"ok":data.get("sync_error").is_none(),"meta":{"network_used":false,
                "transport":"in-process synthetic only"},"data":data}),
            Err(error) => json!({"ok":false,"meta":{"network_used":false},"error":error,
                "logical_unmeasured_stage_reservation_bytes":512_u64*1024*1024,
                "partial_counters":"unavailable; retain whole-stage reservation"}),
        };
        let bytes = serde_json::to_vec_pretty(&envelope).unwrap();
        assert!(bytes.len() <= 65536);
        plain(&task.report).unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&task.report)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
    }

    // Separate supervisor-owned lifecycle entry point. The four-cell control above
    // deliberately retains its original fixture rules and diagnostic oracle.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LifecycleLimits {
        requests: u64,
        items: usize,
        request_bytes: u64,
        response_bytes: u64,
        deadline_ms: u64,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LifecycleTask {
        version: u32,
        experiment_id: String,
        invocation_id: String,
        mode: String,
        vault: PathBuf,
        vault_id: RecordId,
        config: PathBuf,
        config_hash: Blake3Hash,
        report: PathBuf,
        queries: Option<PathBuf>,
        query_hash: Option<Blake3Hash>,
        limits: LifecycleLimits,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LifecycleQuery {
        task_id: String,
        text: String,
    }
    fn lifecycle_label(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    }
    fn lifecycle_read(path: &Path, cap: u64) -> Result<Vec<u8>> {
        plain(path)?;
        let info = fs::symlink_metadata(path)
            .map_err(|_| WikiError::invalid("lifecycle input metadata unavailable"))?;
        if !info.is_file() || info.len() > cap {
            return Err(WikiError::invalid("lifecycle regular input ceiling"));
        }
        let mut bytes = Vec::new();
        use std::io::Read;
        fs::File::open(path)
            .map_err(|_| WikiError::invalid("lifecycle input unavailable"))?
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| WikiError::invalid("lifecycle input read failed"))?;
        if bytes.len() as u64 != info.len() || bytes.len() as u64 > cap {
            return Err(WikiError::invalid(
                "lifecycle input changed or exceeded ceiling",
            ));
        }
        Ok(bytes)
    }
    fn lifecycle_vector(input: &[u8]) -> Vec<f32> {
        let mut xof = blake3::Hasher::new();
        xof.update(input);
        let mut bytes = [0_u8; DIMENSIONS * 4];
        xof.finalize_xof().fill(&mut bytes);
        let magnitude = 1.0 / (DIMENSIONS as f32).sqrt();
        bytes
            .chunks_exact(4)
            .map(|word| {
                if word[0] & 1 == 0 {
                    magnitude
                } else {
                    -magnitude
                }
            })
            .collect()
    }
    #[derive(Default)]
    struct LifecycleCounters {
        requests: u64,
        items: usize,
        wire_bytes: u64,
        immutable_task_bytes: u64,
        response_bytes: u64,
        response_reservations: u64,
        ledger_inspections: usize,
        observed_journal_bytes: u64,
        requests_observed: Vec<Value>,
        transport_errors: Vec<Value>,
        runs: BTreeSet<RecordId>,
    }
    struct LifecycleMock {
        fs: VaultFs,
        vault: RecordId,
        options: JobOptions,
        limits: LifecycleLimits,
        deadline: Instant,
        counters: std::sync::Mutex<LifecycleCounters>,
    }
    impl LifecycleMock {
        fn reply(&self, request: AuthenticatedRequest<'_>) -> Result<TransportReply> {
            let summary = request.summary();
            {
                let mut counts = self.counters.lock().unwrap();
                counts.requests += 1;
                counts.wire_bytes = counts
                    .wire_bytes
                    .checked_add(summary.request_bytes)
                    .ok_or_else(|| WikiError::invalid("lifecycle wire count overflow"))?;
                counts.response_reservations += 8 * 1024 * 1024;
                counts.runs.insert(summary.attempt.run_id.clone());
                counts
                    .requests_observed
                    .push(json!({"summary":summary,"authenticated":false}));
                if Instant::now() >= self.deadline
                    || counts.requests > self.limits.requests
                    || counts.wire_bytes > self.limits.request_bytes
                    || counts.response_reservations > self.limits.response_bytes
                    || summary.request_bytes > 262144
                {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "lifecycle mock safety ceiling",
                    ));
                }
            }
            let ledger = JobLedger::new(
                self.fs.clone(),
                self.vault.clone(),
                summary.attempt.run_id.clone(),
                self.options.clone(),
            )?;
            let inspection = ledger.inspect()?;
            {
                let mut counts = self.counters.lock().unwrap();
                counts.ledger_inspections += 1;
                counts.observed_journal_bytes += inspection.budget.journal_bytes_used;
            }
            let task = inspection
                .tasks
                .get(&summary.attempt.task_key)
                .ok_or_else(|| WikiError::invalid("lifecycle retained task absent"))?;
            let reference = &task.spec.input;
            let bytes = lifecycle_read(
                &self.fs.root().path().join(reference.path.as_str()),
                512 * 1024,
            )?;
            if bytes.len() as u64 != reference.byte_len
                || Blake3Hash::digest(&bytes) != reference.hash
            {
                return Err(WikiError::invalid("lifecycle immutable task differs"));
            }
            let input: RemoteInput = serde_json::from_slice(&bytes)
                .map_err(|_| WikiError::invalid("lifecycle immutable task malformed"))?;
            if crate::graph::packet::canonical_json(&input)? != bytes {
                return Err(WikiError::invalid("lifecycle task encoding differs"));
            }
            let RemoteOperation::Embed {
                inputs,
                expected_dimensions,
                ..
            } = input.operation
            else {
                return Err(WikiError::invalid(
                    "lifecycle mock accepts only embedding tasks",
                ));
            };
            if inputs.is_empty()
                || inputs.len() > 16
                || expected_dimensions != Some(DIMENSIONS as u32)
                || inputs
                    .iter()
                    .any(|input| Blake3Hash::digest(input.utf8.as_bytes()) != input.input_hash)
            {
                return Err(WikiError::invalid("lifecycle embedding input differs"));
            }
            {
                let mut counts = self.counters.lock().unwrap();
                counts.items = counts
                    .items
                    .checked_add(inputs.len())
                    .ok_or_else(|| WikiError::invalid("lifecycle item count overflow"))?;
                counts.immutable_task_bytes += bytes.len() as u64;
                if counts.items > self.limits.items {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "lifecycle mock item ceiling",
                    ));
                }
            }
            let data: Vec<_> = inputs.iter().enumerate().map(|(index,input)|
                json!({"index":index,"embedding":lifecycle_vector(input.utf8.as_bytes())})).collect();
            let body =
                serde_json::to_vec(&json!({"model":"lifecycle-attribution-mock-v1","data":data}))
                    .map_err(|_| WikiError::invalid("lifecycle response encoding failed"))?;
            {
                let mut counts = self.counters.lock().unwrap();
                counts.response_bytes += body.len() as u64;
                let entry = counts.requests_observed.last_mut().unwrap();
                entry["authenticated"] = json!(true);
                entry["immutable_task_bytes"] = json!(bytes.len());
                entry["inputs"] = json!(
                    inputs
                        .iter()
                        .map(|i| json!({"hash":i.input_hash,"bytes":i.utf8.len()}))
                        .collect::<Vec<_>>()
                );
                entry["response_bytes"] = json!(body.len());
                entry["response_hash"] = json!(Blake3Hash::digest(&body));
            }
            // Deliberately no usage: retained production accounting remains unknown.
            TransportReply::new(200, vec![], body)
        }
    }
    impl Transport for LifecycleMock {
        fn execute<'a>(
            &'a self,
            request: AuthenticatedRequest<'a>,
            _: TransportContext,
        ) -> TransportFuture<'a> {
            let result = self.reply(request).map_err(|error| {
                self.counters
                    .lock()
                    .unwrap()
                    .transport_errors
                    .push(json!(error));
                TransportFailure::new(TransportFailureCode::InvalidResponse)
            });
            Box::pin(async move { result })
        }
    }
    fn lifecycle_inventory(
        catalog: &Catalog,
        settings: &EmbeddingSettings,
        deadline: Instant,
    ) -> Value {
        use crate::retrieval::{
            indexed_units::InventoryDocumentUnits, unit_inventory_types::RenderPolicyId,
        };
        let start = Instant::now();
        let outcome = (|| -> Result<Value> {
            let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
            let policy =
                RenderPolicyId::for_settings(&reader.snapshot().parser_fingerprint, settings)?;
            let state = reader.unit_inventory_state(&policy)?;
            let mut value = json!({"state":state.as_ref().map(|s| json!({"complete":s.complete,
                "after_owner":s.after_owner,"through_seq":s.through_seq,"unit_count":s.unit_count})),
                "scope":"cached compact observation; no canonical authority"});
            let budget = UnitBudget::with_deadline(
                UnitLimits::default(),
                deadline.min(Instant::now() + Duration::from_secs(30)),
            )?;
            let observation = (|| -> Result<Value> {
                let inventory = InventoryDocumentUnits::new(&reader, settings, &budget)?;
                let mut owners = BTreeSet::new();
                let mut hashes = BTreeSet::new();
                let mut digest = blake3::Hasher::new();
                let mut count = 0usize;
                for descriptor in inventory.replay(&budget)? {
                    let descriptor = descriptor?;
                    owners.insert(descriptor.owner.clone());
                    hashes.insert(descriptor.input_hash.clone());
                    let bytes = crate::graph::packet::canonical_json(&descriptor)?;
                    digest.update(&(bytes.len() as u64).to_le_bytes());
                    digest.update(&bytes);
                    count += 1;
                }
                Ok(
                    json!({"owners":owners.len(),"units":count,"unique_inputs":hashes.len(),
                    "descriptor_digest":format!("blake3:{}",digest.finalize())}),
                )
            })();
            value["observation"] = match observation {
                Ok(v) => json!({"ok":true,"data":v}),
                Err(e) => json!({"ok":false,"error":e}),
            };
            let usage = budget.usage();
            let reads = reader.usage();
            value["work"] = json!({"descriptors":usage.descriptors,"descriptor_bytes":usage.descriptor_bytes,
                "rendered_units":usage.units,"rendered_bytes":usage.render_bytes,
                "cache_rows":reads.rows,"cache_bytes":reads.bytes});
            Ok(value)
        })();
        json!({"elapsed_ns":start.elapsed().as_nanos(),"result":match outcome {Ok(v)=>json!({"ok":true,"data":v}),Err(e)=>json!({"ok":false,"error":e})}})
    }
    fn lifecycle_run(task: &LifecycleTask) -> Result<Value> {
        let started = Instant::now();
        let lim = &task.limits;
        let acquisition_limits_valid = if task.mode == "inventory" {
            lim.requests == 0 && lim.items == 0 && lim.request_bytes == 0 && lim.response_bytes == 0
        } else {
            lim.requests > 0
                && lim.requests <= 128
                && lim.items > 0
                && lim.items <= 2048
                && lim.request_bytes > 0
                && lim.request_bytes <= 32 * 1024 * 1024
                && lim.response_bytes > 0
                && lim.response_bytes <= 1024 * 1024 * 1024
                && lim.response_bytes >= lim.requests * 8 * 1024 * 1024
        };
        if task.version != 1
            || !lifecycle_label(&task.experiment_id)
            || !lifecycle_label(&task.invocation_id)
            || !matches!(task.mode.as_str(), "sync" | "queries" | "inventory")
            || !acquisition_limits_valid
            || lim.deadline_ms == 0
            || lim.deadline_ms > 120000
            || (task.mode == "queries") != (task.queries.is_some() && task.query_hash.is_some())
            || (task.mode != "queries" && (task.queries.is_some() || task.query_hash.is_some()))
        {
            return Err(WikiError::invalid("unfrozen lifecycle task"));
        }
        plain(&task.vault)?;
        plain(&task.report)?;
        if fs::symlink_metadata(&task.report).is_ok() || !task.vault.is_dir() {
            return Err(WikiError::invalid("lifecycle vault/report differs"));
        }
        let config_bytes = lifecycle_read(&task.config, 65536)?;
        if Blake3Hash::digest(config_bytes) != task.config_hash {
            return Err(WikiError::invalid("lifecycle config hash differs"));
        }
        let fs = VaultFs::new(VaultRoot::explicit(&task.vault)?);
        let app = OfflineApp::new(fs.clone(), OperationOptions::default())?;
        if app.vault_id() != &task.vault_id {
            return Err(WikiError::invalid("lifecycle vault identity differs"));
        }
        let catalog = Catalog::new(fs.clone(), app.vault_id().clone());
        if catalog.operation_state()?.is_none() {
            return Err(WikiError::invalid("lifecycle requires normalized vault"));
        }
        let settings = EmbeddingSettings::default();
        let service = ProviderConfig::load(&task.config)?.authorize(
            &fs,
            app.vault_id(),
            "primary",
            Capability::Embed,
        )?;
        let spec = SpaceSpec::from_service(&service, settings.clone())?;
        use crate::config::providers::{AuthConfig, StaticSource};
        if spec.model != "lifecycle-attribution-mock-v1"
            || spec.dimensions != Some(DIMENSIONS as u32)
            || service.service().max_batch_items != Some(16)
            || service.service().max_batch_bytes != Some(262144)
            || !service.service().url.starts_with("http://127.0.0.1:")
            || !service.service().secret_headers.is_empty()
            || !matches!(&service.service().auth,AuthConfig::Static {source:StaticSource::Literal(v),..} if v==b"SYNTHETIC")
        {
            return Err(WikiError::invalid("lifecycle synthetic profile differs"));
        }
        let space_id = spec.id()?;
        let clock: Arc<dyn JobClock> = Arc::new(NativeCredentialClock::default());
        let options = JobOptions {
            clock: clock.clone(),
            fault: None,
            cancel: CancellationToken::default(),
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 1000,
        };
        let now = clock.read()?;
        let deadline = started + Duration::from_millis(lim.deadline_ms);
        let transport = Arc::new(LifecycleMock {
            fs: fs.clone(),
            vault: app.vault_id().clone(),
            options: options.clone(),
            limits: LifecycleLimits {
                requests: lim.requests,
                items: lim.items,
                request_bytes: lim.request_bytes,
                response_bytes: lim.response_bytes,
                deadline_ms: lim.deadline_ms,
            },
            deadline,
            counters: Default::default(),
        });
        let dispatch = Dispatcher::new(
            fs.clone(),
            DispatchOptions {
                broker: Arc::new(CredentialBroker::new(CredentialOptions {
                    clock: clock.clone(),
                    inputs: Arc::new(NoExternalSecrets),
                    runner: Arc::new(NoHelpers),
                })),
                transport: transport.clone(),
                jitter: Arc::new(NoJitter),
            },
        );
        let limits = LifetimeLimits {
            requests: lim.requests,
            attempts_per_task: 1,
            concurrency: 1,
            request_bytes: Some(lim.request_bytes),
            response_bytes: Some(lim.response_bytes),
            ..Default::default()
        };
        let requested = super::super::remote::RequestedJobLimits {
            limits: limits.clone(),
            specified: BTreeSet::from([
                "requests",
                "attempts_per_task",
                "concurrency",
                "request_bytes",
                "response_bytes",
            ]),
            deadline_ms: None,
        };
        let runtime = crate::app::embeddings::EmbeddingRuntime::new(
            &service,
            &dispatch,
            options.clone(),
            limits,
            now.utc_ms,
            now.utc_ms + lim.deadline_ms as i64,
            Some(requested),
        );
        let mut result = json!({"experiment_id":task.experiment_id,"invocation_id":task.invocation_id,
            "mode":task.mode,"settings":settings,"space":spec,"dimensions":DIMENSIONS,
            "clock":"shared NativeCredentialClock for runtime/ledger/credentials; owning Instant intervals",
            "mock_rule":"input-only BLAKE3 XOF 1536 low-bit signs / sqrt(1536); mechanics only",
            "inventory_before":lifecycle_inventory(&catalog,&settings,deadline)});
        let mut extra_runs = BTreeSet::new();
        if task.mode == "sync" {
            let phase = Instant::now();
            let (outcome, observation) =
                super::super::indexed_embedding_inputs::attribution::with_observation(|| {
                    app.embeddings_sync(&settings, &runtime)
                });
            result["sync_elapsed_ns"] = json!(phase.elapsed().as_nanos());
            result["sync_observation"] = serde_json::to_value(observation).unwrap();
            result["sync"] = match outcome {
                Ok(report) => {
                    if let Some(id) = &report.run_id {
                        extra_runs.insert(id.clone());
                    }
                    json!({"ok":true,"report":report})
                }
                Err(error) => {
                    if let Some(id) = error
                        .details
                        .get("run_id")
                        .and_then(Value::as_str)
                        .and_then(|v| RecordId::new(v).ok())
                    {
                        extra_runs.insert(id);
                    }
                    json!({"ok":false,"error":error})
                }
            };
        } else if task.mode == "queries" {
            let bytes = lifecycle_read(task.queries.as_ref().unwrap(), 16384)?;
            if Some(Blake3Hash::digest(&bytes)) != task.query_hash {
                return Err(WikiError::invalid("lifecycle query hash differs"));
            }
            let queries: Vec<LifecycleQuery> = serde_json::from_slice(&bytes)
                .map_err(|_| WikiError::invalid("lifecycle queries malformed"))?;
            let mut ids = BTreeSet::new();
            let mut texts = BTreeSet::new();
            if queries.len() != 12
                || queries.iter().any(|q| {
                    !lifecycle_label(&q.task_id)
                        || q.text.is_empty()
                        || q.text.len() > 12000
                        || !ids.insert(q.task_id.clone())
                        || !texts.insert(q.text.clone())
                })
            {
                return Err(WikiError::invalid(
                    "lifecycle requires twelve unique external queries",
                ));
            }
            let mut rows = Vec::new();
            let mut stopped = false;
            for query in queries {
                if stopped || Instant::now() >= deadline {
                    rows.push(json!({"task_id":query.task_id,"status":"UNRUN"}));
                    stopped = true;
                    continue;
                }
                let start = Instant::now();
                let mut query_plan = plan(SearchMode::Semantic, vec![], 5);
                query_plan.limits.candidates = 80;
                query_plan.filters.kinds = vec![RecordKind::Source];
                let outcome =
                    app.semantic_search_selected(&query.text, &query_plan, Some(&runtime), false);
                let input = spec.query(&query.text)?;
                let cached = (|| -> Result<bool> {
                    let store = VectorStore::open(&fs, None)?;
                    if store.active()?.is_none_or(|s| s.id != space_id) {
                        return Ok(false);
                    }
                    Ok(store.vector(&space_id, &input.input_hash)?.is_some())
                })();
                let acquired = matches!(&cached, Ok(true));
                stopped = !acquired;
                rows.push(json!({"task_id":query.task_id,"elapsed_ns":start.elapsed().as_nanos(),
                    "query_input_hash":input.input_hash,"query_cached":match cached {Ok(v)=>json!({"ok":true,"cached":v}),Err(e)=>json!({"ok":false,"error":e})},
                    "search":match outcome {Ok(v)=>json!({"ok":true,"report":v}),Err(e)=>json!({"ok":false,"error":e})}}));
            }
            result["queries"] = json!(rows);
        }
        result["inventory_after"] = lifecycle_inventory(&catalog, &settings, deadline);
        let counts = transport.counters.lock().unwrap();
        extra_runs.extend(counts.runs.iter().cloned());
        result["mock"] = json!({"requests":counts.requests,"items":counts.items,"wire_bytes":counts.wire_bytes,
            "immutable_task_read_bytes":counts.immutable_task_bytes,"observed_response_bytes":counts.response_bytes,
            "response_reservations":counts.response_reservations,"mock_ledger_inspections":counts.ledger_inspections,
            "sum_observed_journal_bytes_at_mock_inspection":counts.observed_journal_bytes,
            "journal_observation_scope":"recorded prefix sizes; not measured physical IO or all inspect reads",
            "requests_observed":counts.requests_observed,"usage":"UNKNOWN; preserve actual ledger holds"});
        result["mock"]["transport_errors"] = json!(counts.transport_errors);
        drop(counts);
        let mut retained = Vec::new();
        for run in extra_runs {
            let inspection = JobLedger::new(
                fs.clone(),
                task.vault_id.clone(),
                run.clone(),
                options.clone(),
            )
            .and_then(|l| l.inspect());
            retained.push(match inspection {Ok(i)=>json!({"run_id":run,"state":i.state,"effective_limits":i.effective_limits,
                "effective_deadline_utc_ms":i.effective_deadline_utc_ms,"budget":i.budget,"attempts":i.attempts}),
                Err(e)=>json!({"run_id":run,"error":e})});
        }
        result["retained_runs"] = json!(retained);
        result["elapsed_ns"] = json!(started.elapsed().as_nanos());
        Ok(result)
    }
    #[test]
    #[ignore = "explicit manifest-bound supervisor-owned lifecycle attribution only"]
    fn manifest_lifecycle_attribution() {
        let task_path = PathBuf::from(
            std::env::var_os("LWIKI_LIFECYCLE_ATTRIBUTION_TASK").expect("explicit lifecycle task"),
        );
        let task: LifecycleTask =
            serde_json::from_slice(&lifecycle_read(&task_path, 16384).unwrap()).unwrap();
        let started = Instant::now();
        let outcome = lifecycle_run(&task);
        let envelope = match outcome {
            Ok(data) => {
                json!({"ok":true,"meta":{"network_used":false,"transport":"in-process synthetic only"},"data":data})
            }
            Err(error) => {
                json!({"ok":false,"error":error,"elapsed_ns":started.elapsed().as_nanos(),
                "partial_counters":"unavailable; supervisor retains entire prospective invocation debit"})
            }
        };
        let bytes = serde_json::to_vec_pretty(&envelope).unwrap();
        assert!(
            bytes.len() <= 2 * 1024 * 1024,
            "lifecycle report ceiling; retain full prospective debit"
        );
        plain(&task.report).unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&task.report)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
    }
}

#[path = "indexed_embedding_inventory_tests.rs"]
mod inventory;

#[test]
fn experimental_hybrid_lexical_evidence_keeps_exact_discovery_and_semantic_work() {
    use crate::retrieval::{
        ContextOptions, ContextRequest, ContextScope, context, indexed_semantic,
    };
    let fixture = normalized();
    add(&fixture, TITLE, "experimental-first.txt", FIRST);
    add(&fixture, TITLE, "experimental-second.txt", SECOND);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let synced = fixture
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap();
    complete(&synced, 2);
    let store = VectorStore::open(&fixture.fs, None).unwrap();
    let state = store.active().unwrap().unwrap();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let mut request = ContextRequest {
        scope: ContextScope::IndexedDocuments,
        documents: plan(SearchMode::Hybrid, vec![], 80),
        ..Default::default()
    };
    request.documents.limits.hits = 10;
    request.documents.limits.excerpt_bytes = 1024;
    let run = |enabled| {
        context::with_candidate_ordering_trace(|| {
            indexed_semantic::context(
                &catalog,
                "Signalneedle",
                &request,
                &ContextOptions {
                    experimental_hybrid_lexical_evidence: enabled,
                    ..Default::default()
                },
                &state,
                &[1.0, 0.0],
            )
            .unwrap()
        })
    };
    let (control, control_trace) = run(false);
    let (candidate, candidate_trace) = run(true);
    let rows = |trace: &[serde_json::Value], stage: &str| {
        trace.iter().find(|event| event["stage"] == stage).unwrap()["rows"].clone()
    };
    let discovered = rows(&control_trace, "evidence_input_owners");
    assert_eq!(discovered.as_array().unwrap().len(), 2);
    assert_eq!(
        discovered,
        rows(&candidate_trace, "evidence_input_owners"),
        "same full hits, order and anchors before evidence assembly"
    );
    assert_eq!(
        rows(&control_trace, "scored_units"),
        rows(&candidate_trace, "scored_units")
    );
    assert_eq!(
        rows(&control_trace, "normalized_read_work"),
        rows(&candidate_trace, "normalized_read_work")
    );
    let control_pool = rows(&control_trace, "candidate_pool");
    let candidate_pool = rows(&candidate_trace, "candidate_pool");
    assert!(!control_pool["proposals"].as_array().unwrap().is_empty());
    assert!(!candidate_pool["proposals"].as_array().unwrap().is_empty());
    assert!(
        candidate_pool["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .all(|packet| {
                packet["unit_score"].is_null()
                    && packet["lexical_candidate"]["semantic_affinity"].is_null()
            })
    );
    assert!(
        control_pool["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .all(|packet| packet["key"].as_str().unwrap().starts_with("unit:"))
    );
    assert!(
        candidate_pool["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .all(|packet| packet["key"].as_str().unwrap().starts_with("document:"))
    );
    assert!(
        !control
            .warnings()
            .iter()
            .any(|warning| warning.contains("experimental evidence strategy"))
    );
    assert!(candidate.warnings().iter().any(|warning| {
        warning.contains("experimental evidence strategy hybrid_owner_lexical_evidence")
    }));
    assert!(!control.network_used && !candidate.network_used);
    assert_eq!(control.snapshot(), candidate.snapshot());
    assert_eq!(
        control.dependency_fingerprint(),
        candidate.dependency_fingerprint()
    );
    assert!(!candidate.passages().is_empty());
    for passage in candidate.passages() {
        assert!(passage.text.len() <= 1024);
        for citation in &passage.citations {
            let CitationRef::Source(reference) = citation else {
                panic!("source citation required")
            };
            assert_eq!(reference.span, passage.span);
            assert_eq!(
                reference.quote_hash,
                Blake3Hash::digest(passage.text.as_bytes())
            );
        }
    }
    assert!(candidate.usage().rendered_bytes <= 12000);
    assert!(candidate.usage().estimated_tokens <= 3000);
}

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
    assert_eq!(repeat.reused_inputs, 1);
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
    assert_eq!(synced.reused_inputs, 1);
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
    assert_eq!(replacement.reused_inputs, 1);
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
    let conflict = fixture
        .app
        .embeddings_sync(&settings, &resumed)
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::FreshnessConflict, "{conflict:?}");
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    let after = job.inspect().unwrap();
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
    assert!(
        VectorStore::open(&fixture.fs, None)
            .unwrap()
            .active()
            .unwrap()
            .is_none()
    );

    let replacement = fixture.app.embeddings_sync(&settings, &resumed).unwrap();
    complete(&replacement, 1);
    assert_eq!(replacement.generated_inputs, 1);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    assert_ne!(replacement.run_id.as_ref(), Some(&before.spec.run_id));
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

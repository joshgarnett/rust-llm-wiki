//! Disposable policy and mock transport tests; no live provider or helper dispatch.
use super::*;
#[allow(
    clippy::duplicate_mod,
    reason = "Independent disposable fixture instance"
)]
#[path = "../../tests/fixtures/p17/common.rs"]
mod common;
use common::{Fixture, Responses, dispatcher, runtime};
use std::sync::{Arc, atomic::Ordering};

#[test]
fn lookup_reserves_one_exact_query_and_shares_the_invocation() {
    let fixture = Fixture::new();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let raw = runtime(&fixture.service, &dispatch);
    let settings = EmbeddingSettings::default();
    let scoped = raw
        .for_automatic_operation(
            PreparationTrigger::Lookup {
                query: "quoted \"question\" 東京".into(),
            },
            &settings,
        )
        .unwrap();
    let nested = scoped.scoped_for_operation().unwrap();
    assert!(Arc::ptr_eq(
        scoped.invocation.as_ref().unwrap(),
        nested.invocation.as_ref().unwrap()
    ));
    assert!(Rc::ptr_eq(
        scoped.automatic.as_ref().unwrap(),
        nested.automatic.as_ref().unwrap()
    ));
    assert!(Rc::ptr_eq(
        &scoped.automatic.as_ref().unwrap().vectors,
        &nested.automatic.as_ref().unwrap().vectors,
    ));
    let spec = fixture.spec();
    let first = spec.query("first corpus input").unwrap();
    scoped.automatic_admit(&spec, &[first], true).unwrap();
    let second = spec.query("second corpus input").unwrap();
    assert_eq!(
        nested
            .automatic_admit(&spec, &[second], true)
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    let query = spec.query("quoted \"question\" 東京").unwrap();
    nested.automatic_admit(&spec, &[query], false).unwrap();
    assert_eq!(nested.automatic_observations().admitted_inputs, 2);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn lower_limits_are_binding_and_uncertain_retry_is_never_inherited() {
    let fixture = Fixture::new();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses);
    let mut raw = runtime(&fixture.service, &dispatch);
    raw.limits.requests = 1;
    let scoped = raw
        .for_automatic_operation(
            PreparationTrigger::Lookup { query: "q".into() },
            &EmbeddingSettings::default(),
        )
        .unwrap();
    let spec = fixture.spec();
    assert_eq!(
        scoped
            .automatic_admit(&spec, &[spec.query("corpus").unwrap()], true)
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    raw.job_options.policy.retry_uncertain = true;
    assert!(matches!(
        raw.for_automatic_operation(PreparationTrigger::Setup, &EmbeddingSettings::default()),
        Err(WikiError {
            code: ErrorCode::RecoveryRequired,
            ..
        })
    ));
}

#[test]
fn exact_encoder_preview_counts_escaping_prefixes_and_dimensions() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let input = spec.query("é \"quoted\" \\ newline\n").unwrap();
    let bound = crate::providers::preview_embedding_request(
        &fixture.service,
        std::slice::from_ref(&input),
        spec.dimensions,
        &spec.id().unwrap(),
    )
    .unwrap();
    let expected = serde_json::to_vec(&serde_json::json!({
        "model":"test-model", "input":[input.utf8], "encoding_format":"float"
    }))
    .unwrap();
    assert_eq!(bound.request_bytes, expected.len() as u64);
    assert_eq!(bound.response_bytes, OTHER_SPOOL_MAX_BYTES);
    assert!(bound.request_bytes > input.utf8.len() as u64);
}

#[test]
fn selected_legacy_renderer_emits_only_requested_owners() {
    let fixture = Fixture::new();
    fixture.page("alpha", "alpha evidence");
    fixture.page("beta", "beta evidence");
    let reader = fixture.reader();
    let settings = EmbeddingSettings::default();
    let paths = vec![VaultRelativePath::new("knowledge/pages/alpha.md").unwrap()];
    let units = render::corpus_selected_iter(&reader, &settings, Some(&paths))
        .unwrap()
        .collect::<Result<Vec<_>>>()
        .unwrap();
    assert!(!units.is_empty());
    assert!(units.iter().all(|unit| paths.contains(&unit.owner)));
    assert!(
        units
            .iter()
            .all(|unit| !unit.utf8.contains("beta evidence"))
    );
}

#[test]
fn failure_outcome_preserves_partial_progress_and_run_ids() {
    let fixture = Fixture::new();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses);
    let scoped = runtime(&fixture.service, &dispatch)
        .for_automatic_operation(PreparationTrigger::Setup, &EmbeddingSettings::default())
        .unwrap();
    let run = RecordId::new("run-retained").unwrap();
    scoped.automatic_touch(&run);
    scoped.automatic_generated(3);
    let outcome = AutomaticPreparationOutcome::from_result(Err(exhausted("limit")), &scoped);
    assert_eq!(outcome.state, AutomaticPreparationState::Partial);
    assert_eq!(outcome.observations.generated_inputs, 3);
    assert!(outcome.observations.touched_run_ids.contains(&run));
    assert!(!outcome.observations.counts_complete);
    assert!(outcome.report.is_none());
}

#[test]
fn advancing_clock_automatic_query_reaches_transport_and_restores_frozen_codec() {
    use std::sync::atomic::AtomicI64;
    struct Advancing(AtomicI64);
    impl JobClock for Advancing {
        fn read(&self) -> Result<ClockReading> {
            let elapsed = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ClockReading {
                utc_ms: 1_800_000_000_000 + elapsed,
                monotonic_ms: 1000 + elapsed as u64,
            })
        }
    }
    for trigger in [
        PreparationTrigger::Lookup {
            query: "where is amber".into(),
        },
        PreparationTrigger::AfterWrite { owners: Vec::new() },
    ] {
        let fixture = Fixture::new();
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let mut raw = runtime(&fixture.service, &dispatch);
        raw.job_options.clock = Arc::new(Advancing(AtomicI64::new(0)));
        let scoped = raw
            .for_automatic_operation(trigger, &EmbeddingSettings::default())
            .unwrap();
        let spec = fixture.spec();
        let query = spec.query("where is amber").unwrap();
        let (ledger, tasks) = fixture
            .app
            .embedding_job(
                &spec,
                &[vec![query]],
                &[],
                &scoped,
                "embeddings_query",
                false,
            )
            .unwrap();
        let original = tasks[0].clone();
        let outcome = dispatch
            .execute_with_invocation(
                &ledger,
                &fixture.service,
                &original.key,
                DispatchPurpose::Task,
                scoped.invocation.as_ref().unwrap(),
            )
            .unwrap_or_else(|error| panic!("automatic dispatch failed: {}", error.error.message));
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        assert!(dispatch.network_used());
        let inspection = ledger.inspect().unwrap();
        assert_eq!(inspection.tasks[&original.key].spec, original);
        let bound = &inspection.attempts[0].bound;
        assert!(bound.timeout_ms > 0 && bound.timeout_ms <= scoped.deadline_ms / 2);
        assert!(bound.codec.is_some());
        let restored = dispatch
            .decode_retained_classified(
                &ledger,
                &fixture.service,
                &original.key,
                DispatchPurpose::Task,
                &outcome.attempt,
            )
            .unwrap_or_else(|failure| {
                panic!("historical decode failed: {}", failure.error().message)
            });
        assert!(matches!(restored, ValidatedOutput::Embeddings { .. }));
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        let next = runtime(&fixture.service, &dispatch)
            .for_automatic_operation(PreparationTrigger::Setup, &EmbeddingSettings::default())
            .unwrap();
        assert!(
            !next.automatic_observations().network_used,
            "prior transport on the same dispatcher is not activity in a new invocation"
        );
    }
}

#[test]
fn automatic_timeout_requires_headroom_and_explicit_timeout_is_unchanged() {
    use crate::providers::invocation_budget::InvocationBudget;
    let explicit = InvocationBudget::new(LifetimeLimits::default(), 1000, 6000).unwrap();
    assert_eq!(
        explicit.automatic_timeout(60_000, 1000, 6000).unwrap(),
        None
    );
    let auto = InvocationBudget::new_automatic(
        LifetimeLimits::default(),
        1000,
        6000,
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    let timeout = auto.automatic_timeout(60_000, 2000, 6000).unwrap().unwrap();
    assert!(timeout <= 2000 && timeout > 0);
    assert!(auto.automatic_timeout(60_000, 6000, 6000).is_err());
}

#[test]
fn missing_retained_marker_blocks_automatic_replacement_without_transport() {
    let fixture = Fixture::new();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let raw = runtime(&fixture.service, &dispatch)
        .scoped_for_operation()
        .unwrap();
    let spec = fixture.spec();
    let input = spec.query("retained input").unwrap();
    let (ledger, _) = fixture
        .app
        .embedding_job(&spec, &[vec![input]], &[], &raw, "embeddings_query", false)
        .unwrap();
    let run = ledger.inspect().unwrap().spec.run_id;
    let markers = fixture.fs.root().path().join(".wiki/state/embedding-jobs");
    for entry in std::fs::read_dir(markers).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let auto = runtime(&fixture.service, &dispatch)
        .for_automatic_operation(PreparationTrigger::Setup, &EmbeddingSettings::default())
        .unwrap();
    let outcome = fixture
        .app
        .prepare_automatic_embeddings(&EmbeddingSettings::default(), &auto);
    assert_eq!(outcome.state, AutomaticPreparationState::Blocked);
    assert!(outcome.observations.blocked_run_ids.contains(&run));
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn pure_batch_prefix_planning_does_not_consume_the_real_invocation() {
    let fixture = Fixture::new();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses);
    let scoped = runtime(&fixture.service, &dispatch)
        .for_automatic_operation(
            PreparationTrigger::Lookup { query: "q".into() },
            &EmbeddingSettings::default(),
        )
        .unwrap();
    let spec = fixture.spec();
    let batches = vec![
        vec![spec.query("first").unwrap()],
        vec![spec.query("second").unwrap()],
    ];
    let planned = scoped.automatic_limit_batches(&spec, batches).unwrap();
    assert_eq!(planned.len(), 1);
    assert_eq!(scoped.automatic_observations().admitted_inputs, 0);
    scoped.automatic_admit(&spec, &planned[0], true).unwrap();
    assert_eq!(scoped.automatic_observations().admitted_inputs, 1);
}

#[test]
fn setup_sixty_second_scope_has_one_shared_vector_allowance() {
    let fixture = Fixture::new();
    let dispatch = dispatcher(&fixture.fs, Arc::new(Responses::new()));
    let scoped = runtime(&fixture.service, &dispatch)
        .for_automatic_operation(PreparationTrigger::Setup, &EmbeddingSettings::default())
        .unwrap();
    let first = OfflineApp::embedding_phase_for(Some(&scoped)).unwrap();
    let second = OfflineApp::embedding_phase_for(Some(&scoped)).unwrap();
    assert!(Rc::ptr_eq(&first, &second));
    assert!(first.remaining_ms().unwrap() <= scoped.deadline_ms);
}

#[test]
fn temporary_vector_lock_is_pending_but_corruption_stays_failed() {
    let fixture = Fixture::new();
    let dispatch = dispatcher(&fixture.fs, Arc::new(Responses::new()));
    let runtime = runtime(&fixture.service, &dispatch)
        .for_automatic_operation(PreparationTrigger::Setup, &EmbeddingSettings::default())
        .unwrap();
    let mut busy = WikiError::new(ErrorCode::IndexCorrupt, "embedding cache is locked");
    busy.details = serde_json::json!({"sqlite_failure":"temporary_contention"});
    let outcome = AutomaticPreparationOutcome::from_result(Err(busy), &runtime);
    assert_eq!(outcome.state, AutomaticPreparationState::Pending);
    assert_eq!(
        outcome.error.unwrap().details["reason"],
        "vector_cache_busy"
    );
    let corrupt = WikiError::new(ErrorCode::IndexCorrupt, "embedding cache is corrupt");
    assert_eq!(
        AutomaticPreparationOutcome::from_result(Err(corrupt), &runtime).state,
        AutomaticPreparationState::Failed
    );
}

#[test]
fn normalized_inventory_caps_setup_deadline_and_rejects_expired_admission() {
    let fixture = Fixture::new();
    fixture.page("amber", "Amber tokens are stored in the north archive.");
    fixture.app.index_rebuild_normalized().unwrap();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let writer = fixture.writer();
    let settings = EmbeddingSettings::default();
    let inventory = catalog
        .prepare_unit_inventory_page_with_deadline(
            &writer,
            &settings,
            128,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    assert!(inventory.complete);
    assert!(inventory.unit_count > 0);
    assert_eq!(
        catalog
            .prepare_unit_inventory_page_with_deadline(&writer, &settings, 128, Instant::now())
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
}

#[test]
fn automatic_setup_query_and_repeat_reuse_accounted_vectors_in_both_catalogs() {
    for normalized in [false, true] {
        let fixture = Fixture::new();
        fixture.page("amber", "Amber tokens are stored in the north archive.");
        if normalized {
            fixture.app.index_rebuild_normalized().unwrap();
        }
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let settings = EmbeddingSettings::default();
        let scoped = runtime(&fixture.service, &dispatch)
            .for_automatic_operation(PreparationTrigger::Setup, &settings)
            .unwrap();
        let prepared = fixture.app.prepare_automatic_embeddings(&settings, &scoped);
        assert_eq!(
            prepared.state,
            AutomaticPreparationState::Ready,
            "{prepared:?}"
        );
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        assert!(prepared.observations.network_used);
        assert_eq!(prepared.observations.generated_inputs, 1);
        assert!(prepared.observations.canonical_transactions >= 2);
        let run = prepared.report.as_ref().unwrap().run_id.clone().unwrap();
        let ledger = fixture.app.embedding_ledger_for(&scoped, run).unwrap();
        let paid = ledger.inspect_accounting_verified().unwrap();
        assert!(
            paid.attempts
                .iter()
                .all(|attempt| attempt.phase == AttemptPhase::Settled)
        );
        assert!(
            paid.tasks
                .values()
                .all(|task| task.state == TaskState::Completed)
        );
        let lookup = runtime(&fixture.service, &dispatch)
            .for_automatic_operation(
                PreparationTrigger::Lookup {
                    query: "where are amber tokens".into(),
                },
                &settings,
            )
            .unwrap();
        let repeated = fixture.app.prepare_automatic_embeddings(&settings, &lookup);
        assert_eq!(
            repeated.state,
            AutomaticPreparationState::Ready,
            "{repeated:?}"
        );
        assert!(!repeated.observations.network_used);
        let plan = QueryPlan {
            mode: SearchMode::Hybrid,
            ..QueryPlan::default()
        };
        let hits = fixture
            .app
            .semantic_search(
                "where are amber tokens",
                &plan,
                Some(&lookup),
                false,
                false,
                None,
            )
            .unwrap();
        assert!(
            hits.hits
                .iter()
                .any(|hit| hit.locator.path.as_str() == "knowledge/pages/amber.md")
        );
        let hit = hits
            .hits
            .iter()
            .find(|hit| hit.locator.path.as_str() == "knowledge/pages/amber.md")
            .unwrap();
        let canonical =
            std::fs::read(fixture.fs.root().resolve(&hit.locator.path).unwrap()).unwrap();
        assert_eq!(hit.locator.observed_hash, Blake3Hash::digest(&canonical));
        assert_eq!(
            &canonical[hit.excerpt.span.start() as usize..hit.excerpt.span.end() as usize],
            hit.excerpt.text.as_bytes()
        );
        assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
        let final_observed = fixture.app.refresh_automatic_accounting(&lookup);
        assert!(final_observed.network_used);
        assert_eq!(final_observed.generated_inputs, 1);
        let cached = runtime(&fixture.service, &dispatch)
            .for_automatic_operation(
                PreparationTrigger::Lookup {
                    query: "where are amber tokens".into(),
                },
                &settings,
            )
            .unwrap();
        fixture
            .app
            .semantic_search(
                "where are amber tokens",
                &plan,
                Some(&cached),
                false,
                false,
                None,
            )
            .unwrap();
        assert!(!cached.automatic_observations().network_used);
        assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn automatic_after_write_uploads_only_changed_owner() {
    let fixture = Fixture::new();
    fixture.page("amber", "Amber tokens are stored in the north archive.");
    fixture.page("violet", "Violet tokens are stored in the south archive.");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let settings = EmbeddingSettings::default();
    let setup = runtime(&fixture.service, &dispatch)
        .for_automatic_operation(PreparationTrigger::Setup, &settings)
        .unwrap();
    let initial = fixture.app.prepare_automatic_embeddings(&settings, &setup);
    assert_eq!(
        initial.state,
        AutomaticPreparationState::Ready,
        "{initial:?}"
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    fixture.page("amber", "Amber tokens moved to the west archive.");
    let changed = runtime(&fixture.service, &dispatch)
        .for_automatic_operation(
            PreparationTrigger::AfterWrite {
                owners: vec![VaultRelativePath::new("knowledge/pages/amber.md").unwrap()],
            },
            &settings,
        )
        .unwrap();
    let updated = fixture
        .app
        .prepare_automatic_embeddings(&settings, &changed);
    assert_eq!(
        updated.state,
        AutomaticPreparationState::Ready,
        "{updated:?}"
    );
    assert_eq!(updated.observations.generated_inputs, 1);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
}

struct ControlledClock(std::sync::atomic::AtomicI64);
impl ControlledClock {
    fn new() -> Self {
        Self(std::sync::atomic::AtomicI64::new(1_800_000_000_000))
    }
    fn expire(&self) {
        self.0.store(1_800_000_061_000, Ordering::SeqCst);
    }
}
impl JobClock for ControlledClock {
    fn read(&self) -> Result<ClockReading> {
        let utc_ms = self.0.load(Ordering::SeqCst);
        Ok(ClockReading {
            utc_ms,
            monotonic_ms: (utc_ms - 1_800_000_000_000) as u64 + 1000,
        })
    }
}

fn assert_current_amber_readable(fixture: &Fixture, normalized: bool, expected: &str) {
    use crate::catalog::{
        query_types::{QueryCatalog, QueryReadLimits},
        transient_read::CurrentQueryAttempt,
    };
    let path = VaultRelativePath::new("knowledge/pages/amber.md").unwrap();
    let document = if normalized {
        let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
        let CurrentQueryAttempt::Ready(reader) = catalog
            .current_query_snapshot(QueryReadLimits::default())
            .unwrap()
        else {
            panic!("tiny normalized fixture must support ordinary current observation");
        };
        let document = reader.document(&path).unwrap().unwrap();
        reader.final_recheck(&catalog).unwrap();
        document
    } else {
        fixture.reader().document(&path).unwrap().unwrap()
    };
    assert!(document.body.contains(expected), "{}", document.body);
    let bytes = std::fs::read(fixture.fs.root().resolve(&path).unwrap()).unwrap();
    assert_eq!(document.hash, Blake3Hash::digest(bytes));
}

#[test]
fn response_after_admission_expiry_stays_received_and_recovers_without_http() {
    for normalized in [false, true] {
        let fixture = Fixture::new();
        fixture.page(
            "amber",
            "Amber evidence persists through response deferral.",
        );
        if normalized {
            fixture.app.index_rebuild_normalized().unwrap();
        }
        let clock = Arc::new(ControlledClock::new());
        let expire = clock.clone();
        let responses = Arc::new(Responses {
            hook: Some(Box::new(move |_| expire.expire())),
            ..Responses::new()
        });
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let mut raw = runtime(&fixture.service, &dispatch);
        raw.job_options.clock = clock.clone();
        let settings = EmbeddingSettings::default();
        let scoped = raw
            .for_automatic_operation(PreparationTrigger::Setup, &settings)
            .unwrap();
        let deferred = fixture.app.prepare_automatic_embeddings(&settings, &scoped);
        assert_eq!(
            deferred.state,
            AutomaticPreparationState::Pending,
            "{deferred:?}"
        );
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        assert!(deferred.observations.network_used);
        let run = deferred
            .observations
            .touched_run_ids
            .iter()
            .next()
            .unwrap()
            .clone();
        let ledger = fixture.app.embedding_ledger_for(&scoped, run).unwrap();
        let retained = ledger.inspect_accounting_verified().unwrap();
        assert_eq!(retained.attempts[0].phase, AttemptPhase::Received);
        assert!(retained.attempts[0].spool.is_some());
        assert!(retained.attempts[0].receipt.is_none());
        let next = raw
            .for_automatic_operation(PreparationTrigger::Setup, &settings)
            .unwrap();
        let recovered = fixture.app.prepare_automatic_embeddings(&settings, &next);
        assert_eq!(
            recovered.state,
            AutomaticPreparationState::Ready,
            "{recovered:?}"
        );
        assert!(!recovered.observations.network_used);
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        let after = ledger.inspect_accounting_verified().unwrap();
        assert_eq!(after.attempts[0].phase, AttemptPhase::Settled);
        assert!(
            after
                .tasks
                .values()
                .all(|task| task.state == TaskState::Completed)
        );
    }
}

#[test]
fn expiry_before_bootstrap_creates_no_run_or_remote_attempt() {
    let fixture = Fixture::new();
    fixture.page("amber", "Evidence remains locally searchable.");
    let clock = Arc::new(ControlledClock::new());
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let mut raw = runtime(&fixture.service, &dispatch);
    raw.job_options.clock = clock.clone();
    let settings = EmbeddingSettings::default();
    let scoped = raw
        .for_automatic_operation(PreparationTrigger::Setup, &settings)
        .unwrap();
    clock.expire();
    let outcome = fixture.app.prepare_automatic_embeddings(&settings, &scoped);
    assert_eq!(
        outcome.state,
        AutomaticPreparationState::Pending,
        "{outcome:?}"
    );
    assert_eq!(outcome.observations.canonical_transactions, 0);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
    assert!(outcome.observations.touched_run_ids.is_empty());
}

struct ExpireOnReceiptIo {
    clock: Arc<ControlledClock>,
    armed: Arc<std::sync::atomic::AtomicBool>,
    fired: Arc<std::sync::atomic::AtomicBool>,
}
impl crate::vault::DurableIo for ExpireOnReceiptIo {
    fn create_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        crate::vault::NativeIo.create_stage(p)
    }
    fn create_private_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        crate::vault::NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::NativeIo.create_private_directory(p)
    }
    fn open_append(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        crate::vault::NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &std::fs::File, n: u64) -> std::io::Result<()> {
        crate::vault::NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut std::fs::File, b: &[u8]) -> std::io::Result<()> {
        crate::vault::NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &std::fs::File) -> std::io::Result<()> {
        crate::vault::NativeIo.sync_file(f)
    }
    fn replace(&self, staged: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
        crate::vault::NativeIo.replace(staged, target)?;
        if self.armed.load(Ordering::SeqCst)
            && target.components().any(|part| part.as_os_str() == "events")
            && target
                .extension()
                .is_some_and(|extension| extension == "md")
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            self.clock.expire();
        }
        Ok(())
    }
    fn remove(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::NativeIo.remove(p)
    }
    fn remove_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::NativeIo.remove_directory(p)
    }
    fn create_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &std::path::Path) -> std::io::Result<crate::vault::DirectorySync> {
        crate::vault::NativeIo.sync_directory(p)
    }
}

#[test]
fn expiry_during_receipt_apply_finishes_exact_settlement_and_confirmation() {
    for normalized in [false, true] {
        let mut fixture = Fixture::new();
        fixture.page(
            "amber",
            "Amber evidence survives receipt completion after admission stops.",
        );
        if normalized {
            fixture.app.index_rebuild_normalized().unwrap();
        }
        let clock = Arc::new(ControlledClock::new());
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        fixture.fs = crate::vault::VaultFs::with_io(
            fixture.fs.root().clone(),
            Arc::new(ExpireOnReceiptIo {
                clock: clock.clone(),
                armed: armed.clone(),
                fired: fired.clone(),
            }),
        );
        fixture.app =
            OfflineApp::new(fixture.fs.clone(), crate::app::OperationOptions::default()).unwrap();
        let responses = Arc::new(Responses {
            hook: Some(Box::new(move |_| armed.store(true, Ordering::SeqCst))),
            ..Responses::new()
        });
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let mut raw = runtime(&fixture.service, &dispatch);
        raw.job_options.clock = clock;
        let settings = EmbeddingSettings::default();
        let scoped = raw
            .for_automatic_operation(PreparationTrigger::Setup, &settings)
            .unwrap();
        let outcome = fixture.app.prepare_automatic_embeddings(&settings, &scoped);
        assert!(
            fired.load(Ordering::SeqCst),
            "fault did not observe canonical receipt replacement"
        );
        assert_eq!(
            outcome.state,
            AutomaticPreparationState::Partial,
            "{outcome:?}"
        );
        assert_eq!(outcome.observations.generated_inputs, 1);
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        let run = outcome
            .observations
            .touched_run_ids
            .iter()
            .next()
            .unwrap()
            .clone();
        let ledger = fixture.app.embedding_ledger_for(&scoped, run).unwrap();
        let inspected = ledger.inspect_accounting_verified().unwrap();
        assert_eq!(inspected.attempts[0].phase, AttemptPhase::Settled);
        assert!(
            inspected
                .tasks
                .values()
                .all(|task| task.state == TaskState::Completed)
        );
        let store = VectorStore::open(&fixture.fs, None).unwrap();
        assert!(
            inspected.attempts[0]
                .cache_outputs
                .iter()
                .all(|reference| store.verify_ref(reference).unwrap())
        );
        drop(store);
        // Existing canonical recovery classification remains clean after the entered transaction.
        assert_current_amber_readable(
            &fixture,
            normalized,
            "Amber evidence survives receipt completion after admission stops.",
        );
    }
}

#[test]
fn expiry_during_bootstrap_preserves_marker_and_never_starts_transport() {
    struct ExpireBootstrap(Arc<ControlledClock>);
    impl LedgerFault for ExpireBootstrap {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            if point == LedgerCheckpoint::AfterJournalSync {
                self.0.expire();
            }
            Ok(())
        }
    }
    for normalized in [false, true] {
        let fixture = Fixture::new();
        fixture.page("amber", "Local evidence survives a bootstrap deadline.");
        if normalized {
            fixture.app.index_rebuild_normalized().unwrap();
        }
        let clock = Arc::new(ControlledClock::new());
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let mut raw = runtime(&fixture.service, &dispatch);
        raw.job_options.clock = clock.clone();
        raw.job_options.fault = Some(Arc::new(ExpireBootstrap(clock)));
        let scoped = raw
            .for_automatic_operation(PreparationTrigger::Setup, &EmbeddingSettings::default())
            .unwrap();
        let outcome = fixture
            .app
            .prepare_automatic_embeddings(&EmbeddingSettings::default(), &scoped);
        assert_eq!(
            outcome.state,
            AutomaticPreparationState::Pending,
            "{outcome:?}"
        );
        assert_eq!(outcome.observations.canonical_transactions, 1);
        assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
        let markers = fixture.fs.root().path().join(".wiki/state/embedding-jobs");
        assert_eq!(std::fs::read_dir(markers).unwrap().count(), 1);
        let run = outcome
            .observations
            .touched_run_ids
            .iter()
            .next()
            .unwrap()
            .clone();
        let ledger = fixture.app.embedding_ledger_for(&scoped, run).unwrap();
        assert!(
            ledger
                .inspect_accounting_verified()
                .unwrap()
                .attempts
                .is_empty()
        );
        assert_current_amber_readable(
            &fixture,
            normalized,
            "Local evidence survives a bootstrap deadline.",
        );
        let next = runtime(&fixture.service, &dispatch)
            .for_automatic_operation(PreparationTrigger::Setup, &EmbeddingSettings::default())
            .unwrap();
        let recovered = fixture
            .app
            .prepare_automatic_embeddings(&EmbeddingSettings::default(), &next);
        assert_eq!(
            recovered.state,
            AutomaticPreparationState::Ready,
            "{recovered:?}"
        );
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        let closed = ledger.inspect_accounting_verified().unwrap();
        assert_eq!(closed.state, RunState::Failed);
        assert!(closed.attempts.is_empty());
    }
}

#[test]
fn late_retained_hold_blocks_new_corpus_and_query_admission() {
    let fixture = Fixture::new();
    fixture.page("amber", "Amber evidence is available locally.");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let spec = fixture.spec();
    let raw = runtime(&fixture.service, &dispatch)
        .scoped_for_operation()
        .unwrap();
    let mut held = None;
    for index in 0..3 {
        let input = spec.query(&format!("retained question {index}")).unwrap();
        let (ledger, tasks) = fixture
            .app
            .embedding_job(
                &spec,
                &[vec![input.clone()]],
                &[],
                &raw,
                "embeddings_query",
                false,
            )
            .unwrap();
        if index == 2 {
            let bound = crate::providers::preview_embedding_request(
                &fixture.service,
                &[input],
                spec.dimensions,
                &spec.id().unwrap(),
            )
            .unwrap();
            let reservation = ledger.reserve(&tasks[0].key, bound).unwrap();
            drop(ledger.dispatch_intent(reservation).unwrap());
            held = Some(ledger);
        }
    }
    let held = held.unwrap();
    let before = held.inspect_accounting_verified().unwrap();
    let auto = runtime(&fixture.service, &dispatch)
        .for_automatic_operation(
            PreparationTrigger::Lookup {
                query: "where are amber tokens".into(),
            },
            &EmbeddingSettings::default(),
        )
        .unwrap();
    let outcome = fixture
        .app
        .prepare_automatic_embeddings(&EmbeddingSettings::default(), &auto);
    assert_eq!(
        outcome.state,
        AutomaticPreparationState::Blocked,
        "{outcome:?}"
    );
    assert!(
        outcome
            .observations
            .blocked_run_ids
            .contains(&before.spec.run_id)
    );
    assert!(
        auto.automatic_admit(
            &spec,
            &[spec.query("where are amber tokens").unwrap()],
            false
        )
        .is_err()
    );
    let after = held.inspect_accounting_verified().unwrap();
    assert_eq!(before.budget, after.budget);
    assert_eq!(before.attempts, after.attempts);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn automatic_unsent_scope_closes_and_replans_with_current_limits_on_both_layouts() {
    for normalized in [false, true] {
        for released in [
            None,
            Some(NotSentReason::TransportNotEntered),
            Some(NotSentReason::CredentialsUnavailable),
        ] {
            let mut fixture = Fixture::new();
            // A query-only scope requires dimensions established by its model
            // configuration or earlier corpus work. This fixture has no corpus
            // dispatch, so declare the mock's fixed two-dimensional output.
            let provider_text = std::fs::read_to_string(&fixture.config)
                .unwrap()
                .replace("max_batch_items=1", "dimensions=2\nmax_batch_items=1");
            common::private_write(&fixture.config, provider_text);
            fixture.service = crate::config::providers::ProviderConfig::load(&fixture.config)
                .unwrap()
                .authorize(
                    &fixture.fs,
                    fixture.app.vault_id(),
                    "primary",
                    Capability::Embed,
                )
                .unwrap();
            fixture.page("amber", "Amber recovery remains useful.");
            if normalized {
                fixture.app.index_rebuild_normalized().unwrap();
            }
            let responses = Arc::new(Responses::new());
            let dispatch = dispatcher(&fixture.fs, responses.clone());
            let settings = EmbeddingSettings::default();
            let spec = fixture.spec();
            let input = spec.query("where is amber").unwrap();
            let batches = vec![vec![input.clone()]];
            let first = runtime(&fixture.service, &dispatch)
                .for_automatic_operation(PreparationTrigger::Setup, &settings)
                .unwrap();
            let (old, tasks) = fixture
                .app
                .embedding_job(&spec, &batches, &[], &first, "embeddings_query", false)
                .unwrap();
            if let Some(reason) = released {
                let mut bound = crate::providers::preview_embedding_request(
                    &fixture.service,
                    &[input],
                    spec.dimensions,
                    &spec.id().unwrap(),
                )
                .unwrap();
                bound.timeout_ms = 1000;
                bound.bounds_fingerprint = jobs::budgets::bound_fingerprint(&bound).unwrap();
                let reservation = old.reserve(&tasks[0].key, bound).unwrap();
                let attempt = old.inspect().unwrap().attempts[0].attempt.clone();
                drop(old.dispatch_intent(reservation).unwrap());
                old.pause(StopReason::ReconciliationRequired).unwrap();
                old.release_not_sent(&attempt, jobs::NotSentObservation { reason })
                    .unwrap();
            }
            let before = old.inspect_accounting_verified().unwrap();
            assert!(old.can_close_unstarted_embedding(&before.spec).unwrap());
            assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
            let mut raw = runtime(&fixture.service, &dispatch);
            raw.limits.requests = 1;
            let next = raw
                .for_automatic_operation(PreparationTrigger::Setup, &settings)
                .unwrap();
            let (successor, next_tasks) = fixture
                .app
                .embedding_job(&spec, &batches, &[], &next, "embeddings_query", false)
                .unwrap();
            let closed = old.inspect_accounting_verified().unwrap();
            assert_eq!(closed.state, RunState::Failed);
            assert_eq!(closed.spec, before.spec);
            assert_eq!(closed.effective_limits, before.effective_limits);
            assert_eq!(
                closed.effective_deadline_utc_ms,
                before.effective_deadline_utc_ms
            );
            assert_eq!(closed.attempts, before.attempts);
            assert_eq!(closed.budget.outstanding, before.budget.outstanding);
            let replacement = successor.inspect_accounting_verified().unwrap();
            assert_ne!(replacement.spec.run_id, before.spec.run_id);
            assert_eq!(replacement.spec.tasks, before.spec.tasks);
            assert_eq!(replacement.spec.limits.requests, 1);
            assert!(replacement.spec.title.contains(before.spec.run_id.as_str()));
            let generated = fixture
                .app
                .dispatch_embedding_task(
                    &successor,
                    &next_tasks[0],
                    &spec,
                    &[],
                    &next,
                    TaskMaterialization {
                        probe: false,
                        corpus: false,
                    },
                )
                .unwrap();
            assert_eq!(generated, 1);
            assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                successor.inspect().unwrap().attempts[0].phase,
                AttemptPhase::Settled
            );
        }
    }
}

#[test]
fn automatic_unsent_closure_preserves_explicit_intent_and_unresolved_attempts() {
    for case in [
        "user_pause",
        "stop",
        "reserved",
        "dispatch_intent",
        "send_authorized",
    ] {
        let fixture = Fixture::new();
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let settings = EmbeddingSettings::default();
        let spec = fixture.spec();
        let input = spec.query("preserve deliberate controls").unwrap();
        let scoped = runtime(&fixture.service, &dispatch)
            .for_automatic_operation(PreparationTrigger::Setup, &settings)
            .unwrap();
        let (ledger, tasks) = fixture
            .app
            .embedding_job(
                &spec,
                &[vec![input.clone()]],
                &[],
                &scoped,
                "embeddings_query",
                false,
            )
            .unwrap();
        match case {
            "user_pause" => {
                ledger.pause(StopReason::Budget).unwrap();
                // Deliberate intent must remain durable even while already paused.
                ledger
                    .pause(StopReason::User("keep this paused".into()))
                    .unwrap();
            }
            "stop" => {
                ledger.stop().unwrap();
            }
            _ => {
                let mut bound = crate::providers::preview_embedding_request(
                    &fixture.service,
                    &[input],
                    spec.dimensions,
                    &spec.id().unwrap(),
                )
                .unwrap();
                bound.timeout_ms = 1000;
                bound.bounds_fingerprint = jobs::budgets::bound_fingerprint(&bound).unwrap();
                let reservation = ledger.reserve(&tasks[0].key, bound).unwrap();
                if case != "reserved" {
                    let intent = ledger.dispatch_intent(reservation).unwrap();
                    if case == "send_authorized" {
                        drop(ledger.begin_send(intent).unwrap());
                        let attempt = ledger.inspect().unwrap().attempts[0].attempt.clone();
                        ledger
                            .release_not_sent(
                                &attempt,
                                jobs::NotSentObservation {
                                    reason: NotSentReason::TransportNotEntered,
                                },
                            )
                            .unwrap();
                    }
                }
            }
        }
        let before = ledger.inspect_accounting_verified().unwrap();
        // Reopen using independent options to prove pause/release state is durable.
        let reopened = fixture
            .app
            .embedding_ledger_for(&scoped, before.spec.run_id.clone())
            .unwrap();
        assert!(
            !reopened
                .can_close_unstarted_embedding(&before.spec)
                .unwrap(),
            "{case}"
        );
        assert_eq!(
            reopened
                .close_unstarted_embedding(&before.spec)
                .unwrap_err()
                .code,
            ErrorCode::RecoveryRequired
        );
        let after = reopened.inspect_accounting_verified().unwrap();
        assert_eq!(before.state, after.state, "{case}");
        assert_eq!(before.attempts, after.attempts, "{case}");
        assert_eq!(before.budget, after.budget, "{case}");
        assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn expiry_immediately_after_receipt_commit_finishes_settlement_on_both_layouts() {
    struct ExpireBeforeAcknowledgment {
        clock: Arc<ControlledClock>,
        fired: Arc<std::sync::atomic::AtomicBool>,
    }
    impl LedgerFault for ExpireBeforeAcknowledgment {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            // This hook is inside verified canonical receipt acknowledgment,
            // after the canonical transaction has committed, before its event.
            if point == LedgerCheckpoint::BeforeOutputsCommitted {
                self.fired.store(true, Ordering::SeqCst);
                self.clock.expire();
            }
            Ok(())
        }
    }
    for normalized in [false, true] {
        let fixture = Fixture::new();
        fixture.page("amber", "Amber remains readable after the receipt commit.");
        if normalized {
            fixture.app.index_rebuild_normalized().unwrap();
        }
        let clock = Arc::new(ControlledClock::new());
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let mut raw = runtime(&fixture.service, &dispatch);
        raw.job_options.clock = clock.clone();
        raw.job_options.fault = Some(Arc::new(ExpireBeforeAcknowledgment {
            clock,
            fired: fired.clone(),
        }));
        let settings = EmbeddingSettings::default();
        let scoped = raw
            .for_automatic_operation(PreparationTrigger::Setup, &settings)
            .unwrap();
        let outcome = fixture.app.prepare_automatic_embeddings(&settings, &scoped);
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(
            outcome.state,
            AutomaticPreparationState::Partial,
            "{outcome:?}"
        );
        assert_eq!(outcome.observations.generated_inputs, 1);
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        let run = outcome
            .observations
            .touched_run_ids
            .iter()
            .next()
            .unwrap()
            .clone();
        let inspected = fixture
            .app
            .embedding_ledger_for(&scoped, run)
            .unwrap()
            .inspect_accounting_verified()
            .unwrap();
        assert_eq!(inspected.attempts[0].phase, AttemptPhase::Settled);
        assert!(inspected.attempts[0].receipt.is_some());
        assert!(
            inspected
                .tasks
                .values()
                .all(|task| task.state == TaskState::Completed)
        );
        let store = VectorStore::open(&fixture.fs, None).unwrap();
        assert!(
            inspected.attempts[0]
                .cache_outputs
                .iter()
                .all(|reference| store.verify_ref(reference).unwrap())
        );
        drop(store);
        assert_current_amber_readable(
            &fixture,
            normalized,
            "Amber remains readable after the receipt commit.",
        );
    }
}

#[test]
fn automatic_replanning_does_not_cancel_an_identical_explicit_run() {
    let fixture = Fixture::new();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let settings = EmbeddingSettings::default();
    let spec = fixture.spec();
    let batches = vec![vec![spec.query("same exact question").unwrap()]];
    let explicit = runtime(&fixture.service, &dispatch)
        .scoped_for_operation()
        .unwrap();
    let (old, _) = fixture
        .app
        .embedding_job(&spec, &batches, &[], &explicit, "embeddings_query", false)
        .unwrap();
    let before = old.inspect_accounting_verified().unwrap();
    let auto = runtime(&fixture.service, &dispatch)
        .for_automatic_operation(PreparationTrigger::Setup, &settings)
        .unwrap();
    assert!(
        !automatic_unstarted_scope(&auto, &before, "embeddings_query", &spec.id().unwrap(), &[])
            .unwrap()
    );
    let error = fixture
        .app
        .embedding_job(&spec, &batches, &[], &auto, "embeddings_query", false)
        .err()
        .expect("unmarked pending work requires explicit continuation");
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    let after = old.inspect_accounting_verified().unwrap();
    assert_eq!(before, after);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn credential_and_pre_send_cancellation_stays_blocked_with_a_fresh_token() {
    use crate::providers::credentials::{
        CredentialBroker, CredentialOptions, HelperInvocation, HelperLimits, HelperOutput,
        HelperRunner, NativeSecretInputs,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    struct NoJitter;
    impl crate::providers::types::JitterSource for NoJitter {
        fn sample_inclusive(&self, _: u64) -> Result<u64> {
            Ok(0)
        }
    }

    struct CancelHelper {
        during_helper: bool,
        returned: Arc<AtomicBool>,
        calls: AtomicUsize,
    }
    impl HelperRunner for CancelHelper {
        fn run(
            &self,
            _: &HelperInvocation,
            _: &HelperLimits,
            _: &dyn JobClock,
            cancel: &CancellationToken,
        ) -> Result<HelperOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.returned.store(true, Ordering::SeqCst);
            if self.during_helper {
                cancel.cancel();
            }
            HelperOutput::new(b"fixture-key".to_vec())
        }
    }
    struct CancelOnFinalCredentialClock {
        returned: Arc<AtomicBool>,
        after_helper: AtomicUsize,
        cancel: CancellationToken,
    }
    impl JobClock for CancelOnFinalCredentialClock {
        fn read(&self) -> Result<ClockReading> {
            // The broker's completed/final readings follow the helper. Cancel
            // during the final reading, after its policy check: the valid lease
            // returns and dispatcher policy must catch cancellation before send.
            if self.returned.load(Ordering::SeqCst)
                && self.after_helper.fetch_add(1, Ordering::SeqCst) == 1
            {
                self.cancel.cancel();
            }
            common::TestClock.read()
        }
    }
    struct FailCancellationStop {
        returned: Arc<AtomicBool>,
        appends: AtomicUsize,
    }
    impl LedgerFault for FailCancellationStop {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            // Persist the cancelled release, then fail the separate Run stop.
            if point == LedgerCheckpoint::BeforeAppend
                && self.returned.load(Ordering::SeqCst)
                && self.appends.fetch_add(1, Ordering::SeqCst) == 1
            {
                return Err(WikiError::new(
                    ErrorCode::Internal,
                    "fixture cancellation stop failed",
                ));
            }
            Ok(())
        }
    }
    for normalized in [false, true] {
        for mode in ["helper", "pre_send", "stop_failure"] {
            let mut fixture = Fixture::new();
            fixture.page(
                "amber",
                "Amber cancellation leaves local evidence available.",
            );
            let text = std::fs::read_to_string(&fixture.config).unwrap().replace(
                "kind='static'\nkey='mock-secret'",
                "kind='command'\ncommand=['/fixture/helper']\noutput='text'\nttl_seconds=300\nrefresh_skew_seconds=60",
            );
            common::private_write(&fixture.config, text);
            fixture.service = crate::config::providers::ProviderConfig::load(&fixture.config)
                .unwrap()
                .authorize(
                    &fixture.fs,
                    fixture.app.vault_id(),
                    "primary",
                    Capability::Embed,
                )
                .unwrap();
            if normalized {
                fixture.app.index_rebuild_normalized().unwrap();
            }
            let returned = Arc::new(AtomicBool::new(false));
            let cancel = CancellationToken::default();
            let helper = Arc::new(CancelHelper {
                during_helper: mode != "pre_send",
                returned: returned.clone(),
                calls: AtomicUsize::new(0),
            });
            let broker_clock = Arc::new(CancelOnFinalCredentialClock {
                returned: returned.clone(),
                after_helper: AtomicUsize::new(0),
                cancel: cancel.clone(),
            });
            let responses = Arc::new(Responses::new());
            let dispatch = Dispatcher::new(
                fixture.fs.clone(),
                DispatchOptions {
                    broker: Arc::new(CredentialBroker::new(CredentialOptions {
                        clock: broker_clock.clone(),
                        inputs: Arc::new(NativeSecretInputs),
                        runner: helper.clone(),
                    })),
                    transport: responses.clone(),
                    jitter: Arc::new(NoJitter),
                },
            );
            let mut raw = runtime(&fixture.service, &dispatch);
            raw.job_options.cancel = cancel;
            if mode == "stop_failure" {
                raw.job_options.fault = Some(Arc::new(FailCancellationStop {
                    returned,
                    appends: AtomicUsize::new(0),
                }));
            }
            let settings = EmbeddingSettings::default();
            let scoped = raw
                .for_automatic_operation(PreparationTrigger::Setup, &settings)
                .unwrap();
            let failed = fixture.app.prepare_automatic_embeddings(&settings, &scoped);
            let error = failed.error.as_ref().unwrap();
            if mode == "stop_failure" {
                assert_eq!(error.code, ErrorCode::Internal, "{failed:?}");
                assert!(
                    error
                        .details
                        .to_string()
                        .contains("cancellation_recording_failed"),
                    "{failed:?}"
                );
            } else {
                assert_eq!(error.code, ErrorCode::Cancelled, "{failed:?}");
            }
            assert_eq!(helper.calls.load(Ordering::SeqCst), 1);
            if mode == "pre_send" {
                assert_eq!(broker_clock.after_helper.load(Ordering::SeqCst), 2);
            }
            assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
            let run = failed
                .observations
                .touched_run_ids
                .iter()
                .next()
                .unwrap()
                .clone();
            let fresh = runtime(&fixture.service, &dispatch)
                .for_automatic_operation(PreparationTrigger::Setup, &settings)
                .unwrap();
            let ledger = fixture
                .app
                .embedding_ledger_for(&fresh, run.clone())
                .unwrap();
            let before = ledger.inspect_accounting_verified().unwrap();
            assert_eq!(
                before.state,
                if mode == "stop_failure" {
                    RunState::Running
                } else {
                    RunState::Stopped
                }
            );
            assert_eq!(before.attempts.len(), 1);
            assert_eq!(
                before.attempts[0].billing,
                BillingDisposition::ReleasedNotSent
            );
            assert!(before.attempts[0].spool.is_none());
            assert!(!ledger.can_close_unstarted_embedding(&before.spec).unwrap());
            let blocked = fixture.app.prepare_automatic_embeddings(&settings, &fresh);
            assert_eq!(
                blocked.state,
                AutomaticPreparationState::Blocked,
                "{blocked:?}"
            );
            assert!(blocked.observations.blocked_run_ids.contains(&run));
            assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
            assert_eq!(helper.calls.load(Ordering::SeqCst), 1);
            assert_eq!(ledger.inspect_accounting_verified().unwrap(), before);
            assert_eq!(
                std::fs::read_dir(fixture.fs.root().path().join(".wiki/state/embedding-jobs"))
                    .unwrap()
                    .count(),
                1
            );
            assert_current_amber_readable(
                &fixture,
                normalized,
                "Amber cancellation leaves local evidence available.",
            );
        }
    }
}

#[test]
fn cancellation_before_reservation_stops_running_and_machine_paused_automatic_runs() {
    for paused in [false, true] {
        let fixture = Fixture::new();
        fixture.page("amber", "Amber remains available after cancellation.");
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let settings = EmbeddingSettings::default();
        let spec = fixture.spec();
        let raw = runtime(&fixture.service, &dispatch);
        let cancel = raw.job_options.cancel.clone();
        let scoped = raw
            .for_automatic_operation(PreparationTrigger::Setup, &settings)
            .unwrap();
        let (ledger, tasks) = fixture
            .app
            .embedding_job(
                &spec,
                &[vec![spec.query("amber").unwrap()]],
                &[],
                &scoped,
                "embeddings_query",
                false,
            )
            .unwrap();
        if paused {
            ledger.pause(StopReason::Budget).unwrap();
        }
        let before = ledger.inspect_accounting_verified().unwrap();
        cancel.cancel();
        let failure = dispatch
            .execute_with_invocation(
                &ledger,
                &fixture.service,
                &tasks[0].key,
                DispatchPurpose::Task,
                scoped.invocation.as_ref().unwrap(),
            )
            .err()
            .expect("cancelled dispatch must stop before reservation");
        assert_eq!(failure.error.code, ErrorCode::Cancelled);
        assert!(failure.attempt.is_none());
        let fresh = runtime(&fixture.service, &dispatch)
            .for_automatic_operation(PreparationTrigger::Setup, &settings)
            .unwrap();
        let reopened = fixture
            .app
            .embedding_ledger_for(&fresh, before.spec.run_id.clone())
            .unwrap();
        let after = reopened.inspect_accounting_verified().unwrap();
        assert_eq!(after.state, RunState::Stopped);
        assert_eq!(after.spec, before.spec);
        assert_eq!(after.attempts, before.attempts);
        assert!(after.attempts.is_empty());
        assert_eq!(after.budget.outstanding, before.budget.outstanding);
        let blocked = fixture.app.prepare_automatic_embeddings(&settings, &fresh);
        assert_eq!(
            blocked.state,
            AutomaticPreparationState::Blocked,
            "{blocked:?}"
        );
        assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read_dir(fixture.fs.root().path().join(".wiki/state/embedding-jobs"))
                .unwrap()
                .count(),
            1
        );
    }
}

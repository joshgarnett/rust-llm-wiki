//! Public-dispatch sealed Received fixtures; no live provider or fabricated codec.
use super::*;
use crate as library;
#[allow(dead_code)]
#[allow(
    clippy::duplicate_mod,
    reason = "Instantiate shared fixture code in independent test namespaces; sharing module state would change the test contract."
)]
#[path = "../../tests/fixtures/p16c/common.rs"]
mod provider_fixture;
use crate::{
    app::{
        source_relation_inputs::{InputLimits, tests::Fixture},
        source_relation_planning::plan_captured,
    },
    config::providers::{ProviderConfig, TrustedService},
    jobs::{JobOptions, checkpoint, test_support},
    providers::{
        credentials::{CredentialBroker, CredentialOptions},
        dispatcher::Dispatcher,
        types::*,
    },
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const OPERATION_PERMUTATIONS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];
fn permutation(draft: &ChangeDraft, order: [usize; 3]) -> ChangeDraft {
    assert_eq!(draft.operations.len(), 3);
    let mut result = draft.clone();
    result.operations = order
        .into_iter()
        .map(|index| draft.operations[index].clone())
        .collect();
    result
}

fn fixture_deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

struct MockResponse {
    calls: AtomicUsize,
    text: String,
}
impl Transport for MockResponse {
    fn execute<'a>(
        &'a self,
        request: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.summary().role, ServiceRole::Generate);
        let body = serde_json::to_vec(&json!({"id":"response_fictional","object":"response","status":"completed","model":"fictional-model","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":self.text}]}],"usage":{"input_tokens":10,"output_tokens":10,"total_tokens":20}})).unwrap();
        Box::pin(async move { Ok(TransportReply::new(200, vec![], body).unwrap()) })
    }
}
struct ReceivedFixture {
    source: Fixture,
    _config: tempfile::TempDir,
    job: JobLedger,
    task: TaskSpec,
    attempt: AttemptRef,
    mock: Arc<MockResponse>,
}
fn raw_response(scope: &str) -> String {
    serde_json::to_string(&json!({"schema":"lwiki.source-relation-response.v1","nodes":[{"id":"rule_notice","kind":"rule","scope":scope,"span_id":"span_000000"}],"edges":[]})).unwrap()
}
fn service(source: &Fixture) -> (tempfile::TempDir, TrustedService) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("providers.toml");
    let root = serde_json::to_string(source.fs.root().path().to_str().unwrap()).unwrap();
    provider_fixture::private(
        &path,
        format!(
            "version=1\n[profiles.primary]\ngeneration=\"generate\"\n[services.generate]\nadapter=\"responses-v1\"\nurl=\"https://fictional.example/v1/responses\"\nmodel=\"fictional-model\"\nrevision=\"r1\"\nmax_output_tokens=4096\n[services.generate.auth]\nkind=\"static\"\nkey_env=\"FICTIONAL_MOCK_ONLY\"\n[vault_bindings.main]\nroot={root}\nwiki_id=\"fictional_vault\"\nallowed_profiles=[\"primary\"]\n"
        ),
    );
    let service = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&source.fs, &source.vault, "primary", Capability::Generate)
        .unwrap();
    (temp, service)
}
fn activate(source: &Fixture, writer: &WriterPermit) {
    use crate::catalog::{
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        scan, selector,
    };
    let identity = BuildIdentity {
        selection: CatalogSelection::new(source.vault.clone(), 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(&source.fs, writer, &identity.selection).unwrap();
    let mut builder =
        NormalizedBuilder::begin(&source.fs, writer, identity, BuildLimits::default()).unwrap();
    let input = scan::scan_input(&source.fs, &source.vault).unwrap();
    let projection =
        scan::project_normalized_with_sink(&source.fs, &input, false, &mut builder).unwrap();
    let done = builder.finish_normalized(&projection).unwrap();
    selector::publish(&source.fs, writer, &done.identity.selection, Duration::ZERO).unwrap();
}
fn received(normalized: bool) -> ReceivedFixture {
    let source = Fixture::new(false);
    for (path, bytes) in &source.captured {
        let path = source.fs.root().resolve(path).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let binding = source.authenticate().unwrap().selected_binding;
    let (_config, service) = service(&source);
    let planned = plan_captured(
        &source.fs,
        &binding,
        &source.captured,
        &service,
        1024,
        InputLimits {
            max_captured_bytes: 1024 * 1024,
            max_hash_bytes: 14 * 1024 * 1024,
            deadline: fixture_deadline(),
        },
    )
    .unwrap();
    let task = planned.generation.task.clone();
    let path = source.fs.root().resolve(&task.input.path).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, planned.generation.descriptor).unwrap();
    let clock = Arc::new(provider_fixture::Clock::new());
    let now = clock.read().unwrap().utc_ms;
    let mut spec = test_support::spec(&source.fs, "run_relations", 0);
    spec.vault_id = source.vault.clone();
    spec.tasks = vec![task.clone()];
    spec.created_at_utc_ms = now;
    spec.deadline_utc_ms = now + 900_000;
    spec.scope.operation = "prepare-source-relations".into();
    spec.scope.profile_fingerprints = BTreeMap::from([(
        "primary".into(),
        service.summary().profile_fingerprint.clone(),
    )]);
    spec.config_fingerprint = service.summary().config_fingerprint;
    spec.input_fingerprint = crate::jobs::tasks::input_fingerprint(&spec).unwrap();
    let job = JobLedger::new(
        source.fs.clone(),
        source.vault.clone(),
        spec.run_id.clone(),
        JobOptions {
            clock: clock.clone(),
            fault: None,
            cancel: CancellationToken::default(),
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        },
    )
    .unwrap();
    let writer = WriterPermit::acquire(source.fs.root(), Duration::from_secs(1)).unwrap();
    if normalized {
        activate(&source, &writer);
    }
    job.create(&writer, spec).unwrap();
    drop(writer);
    job.start().unwrap();
    let broker = Arc::new(CredentialBroker::new(CredentialOptions {
        clock,
        inputs: Arc::new(provider_fixture::Inputs(AtomicUsize::new(0))),
        runner: Arc::new(provider_fixture::Runner {
            calls: AtomicUsize::new(0),
            hook: None,
        }),
    }));
    let mock = Arc::new(MockResponse {
        calls: AtomicUsize::new(0),
        text: raw_response("fictional notice rule"),
    });
    let dispatcher = Dispatcher::new(
        source.fs.clone(),
        DispatchOptions {
            broker,
            transport: mock.clone(),
            jitter: Arc::new(provider_fixture::ZeroJitter),
        },
    );
    let out = dispatcher
        .execute(&job, &service, &task.key, DispatchPurpose::Task)
        .unwrap_or_else(|f| panic!("sealed mock dispatch failed: {}", f.error.code));
    let inspected = job.inspect().unwrap();
    assert_eq!(inspected.attempts[0].phase, AttemptPhase::Received);
    assert!(
        inspected.attempts[0].bound.codec.is_some(),
        "must use genuine generation codec"
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    ReceivedFixture {
        source,
        _config,
        job,
        task,
        attempt: out.attempt,
        mock,
    }
}
fn fresh(f: &ReceivedFixture) -> MaterializationPlan {
    match received_plan(&f.job, &f.attempt, fixture_deadline()).unwrap() {
        SourceRelationPreparation::Fresh(plan) => plan,
        _ => panic!("fresh Received required"),
    }
}
#[test]
fn initial_received_publishes_raw_annotation_receipt_in_both_catalog_states() {
    for normalized in [false, true] {
        let f = received(normalized);
        let plan = fresh(&f);
        assert_eq!(plan.draft.operations.len(), 3);
        assert_eq!(plan.receipt.outputs.len(), 2);
        let raw: store::RawGeneration = decode_fence(
            plan.draft.operations[0].proposed.as_deref().unwrap(),
            store::RAW_FENCE,
        )
        .unwrap();
        assert_eq!(raw.response, f.mock.text);
        assert_eq!(raw.attempt, f.attempt);
        assert!(plan.receipt.cache_outputs.is_empty());
        let mut records: Vec<_> = plan
            .receipt
            .outputs
            .iter()
            .map(|output| IndexedJobTarget {
                path: output.path.clone(),
                id: output.record.record_id.clone(),
                kind: output.record.expected_kind,
            })
            .collect();
        records.push(IndexedJobTarget {
            path: event_path(&f.attempt.run_id, &plan.receipt.receipt_id).unwrap(),
            id: plan.receipt.receipt_id.clone(),
            kind: RecordKind::RunEvent,
        });
        records.sort_by(|a, b| a.path.cmp(&b.path));
        for record in &records {
            assert_eq!(
                record.path.as_str(),
                format!("runs/{}/events/{}.md", f.attempt.run_id, record.id)
            );
        }
        IndexedWriteOperation::JobBatch {
            run_id: f.attempt.run_id.clone(),
            records,
            checkpoint: None,
        }
        .validate()
        .unwrap();
        let writer = WriterPermit::acquire(f.source.fs.root(), Duration::from_secs(1)).unwrap();
        let change = publish_job_draft(&f.source.fs, &writer, &f.job, plan.draft.clone()).unwrap();
        drop(writer);
        assert_eq!(
            f.job.inspect().unwrap().attempts[0].phase,
            AttemptPhase::Received
        );
        for output in &plan.receipt.outputs {
            assert!(checkpoint::output(&f.source.fs, output).is_ok());
        }
        match received_plan(&f.job, &f.attempt, fixture_deadline()).unwrap() {
            SourceRelationPreparation::Committed {
                change: actual,
                usage,
                ..
            } => {
                assert_eq!(actual, change);
                assert_eq!(usage, plan.receipt);
            }
            _ => panic!("durable publication must be discovered before ack"),
        }
        assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn self_consistent_alternate_raw_pair_is_refused_before_writes_in_both_catalog_states() {
    for normalized in [false, true] {
        let f = received(normalized);
        let descriptor =
            std::fs::read(f.source.fs.root().resolve(&f.task.input.path).unwrap()).unwrap();
        let forged = f.job.with(false,|_,loaded| {
            let received = loaded.frames.iter().find(|frame|matches!(&frame.event.payload,EventPayload::Received {spool} if spool.attempt==f.attempt)).unwrap();
            let pair = materialization::materialize(&f.source.fs,&f.source.vault,&f.task,&f.source.captured,&descriptor,raw_response("forged but schema-valid scope").as_bytes(),&f.attempt,&timestamp(received.event.occurred_at_utc_ms)?,MaterializationLimits {max_captured_bytes:1024*1024,max_hash_bytes:HASH_BYTES,deadline:fixture_deadline()})?;
            receipt_plan_locked(&f.job,loaded,&f.attempt,OutputDisposition::Validated,vec![pair.raw.reference,pair.annotation.reference],vec![],vec![pair.raw.write,pair.annotation.write])
        }).unwrap();
        let before = f.job.inspect().unwrap();
        let engine = ChangeEngine::new(f.source.fs.clone()).unwrap();
        let changes = engine.change_ids().unwrap();
        let writer = WriterPermit::acquire(f.source.fs.root(), Duration::from_secs(1)).unwrap();
        for order in OPERATION_PERMUTATIONS {
            assert!(
                publish_job_draft(
                    &f.source.fs,
                    &writer,
                    &f.job,
                    permutation(&forged.draft, order),
                )
                .is_err()
            );
        }
        drop(writer);
        assert_eq!(engine.change_ids().unwrap(), changes);
        for op in &forged.draft.operations {
            assert!(!f.source.fs.root().resolve(&op.target).unwrap().exists());
        }
        let after = f.job.inspect().unwrap();
        assert_eq!(after.attempts, before.attempts);
        assert_eq!(after.budget, before.budget);
        assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    }
}
fn application(f: &ReceivedFixture) -> crate::app::OfflineApp {
    crate::app::OfflineApp::new(f.source.fs.clone(), crate::app::OperationOptions::default())
        .unwrap()
}
fn reopen(f: &mut ReceivedFixture) {
    let (fs, vault, run, options) = f.job.dispatcher_bindings();
    f.job = JobLedger::new(fs, vault, run, options).unwrap();
}
fn withdraw_source(f: &mut ReceivedFixture) {
    f.source.edit(true, "wiki_status", json!("withdrawn"));
    assert!(f.source.authenticate().is_err());
    let path = f
        .task
        .source_bindings
        .iter()
        .find(|d| d.path.as_str().ends_with("/source.md"))
        .unwrap()
        .path
        .clone();
    std::fs::write(
        f.source.fs.root().resolve(&path).unwrap(),
        &f.source.captured[&path],
    )
    .unwrap();
}
#[test]
fn committed_before_ack_keeps_metadata_then_acknowledged_cleanup_replays_without_send() {
    for normalized in [false, true] {
        let mut f = received(normalized);
        let app = application(&f);
        let plan = fresh(&f);
        // This unpriced mock omits cached/reasoning usage details. Durable
        // completion must preserve unknown billing, never invent known charges.
        assert_eq!(plan.receipt.billing, BillingDisposition::UnknownReserved);
        assert_eq!(plan.receipt.computed_cost, KnownOrUnknown::Unknown);
        let KnownOrUnknown::Known(usage) = &plan.receipt.usage else {
            panic!("observed transport bytes must remain available")
        };
        assert!(usage.request_bytes > 0 && usage.response_bytes > 0);
        assert!(
            usage
                .billable_units
                .values()
                .all(|units| *units == KnownOrUnknown::Unknown)
        );
        let writer = WriterPermit::acquire(f.source.fs.root(), Duration::from_secs(1)).unwrap();
        let change = publish_job_draft(&f.source.fs, &writer, &f.job, plan.draft.clone()).unwrap();
        drop(writer);
        let spool = f.job.inspect().unwrap().attempts[0].spool.clone().unwrap();
        assert_eq!(
            f.job.inspect().unwrap().attempts[0].phase,
            AttemptPhase::Received
        );
        // A lost body/descriptor must not invalidate already committed output;
        // metadata remains the protected unacknowledged accounting authority.
        std::fs::remove_file(f.source.fs.root().resolve(&f.task.input.path).unwrap()).unwrap();
        std::fs::remove_file(f.source.fs.root().resolve(&spool.response.path).unwrap()).unwrap();
        assert!(
            f.source
                .fs
                .root()
                .resolve(&spool.metadata.path)
                .unwrap()
                .exists()
        );
        reopen(&mut f);
        match received_plan(&f.job, &f.attempt, fixture_deadline()).unwrap() {
            SourceRelationPreparation::Committed {
                change: actual,
                receipt,
                usage,
            } => {
                assert_eq!(actual, change);
                assert_eq!(usage, plan.receipt);
                f.job
                    .outputs_committed(
                        &f.attempt,
                        &actual,
                        receipt,
                        usage.outputs,
                        usage.cache_outputs,
                    )
                    .unwrap();
                let settlement = f.job.settle(&f.attempt).unwrap();
                assert_eq!(f.job.settle(&f.attempt).unwrap(), settlement);
            }
            _ => panic!("durable completion must precede transient body and descriptor reads"),
        }
        // Cleanup is legitimate only after verified acknowledgement. Both spool
        // files may now disappear. Acknowledgement already completed the task;
        // replay publishes its missing checkpoint without changing charge authority.
        f.job
            .remove_spool_after_verified_commit(&f.attempt)
            .unwrap();
        assert!(
            !f.source
                .fs
                .root()
                .resolve(&spool.metadata.path)
                .unwrap()
                .exists()
        );
        let protected_budget = f.job.inspect().unwrap().budget;
        let protected_frames = f
            .job
            .with(false, |_, loaded| Ok(loaded.frames.clone()))
            .unwrap();
        assert_eq!(protected_frames.iter().filter(|frame| matches!(
            &frame.event.payload, EventPayload::TaskFinished { task_key, state: TaskState::Completed, .. }
                if task_key == &f.attempt.task_key
        )).count(), 1);
        assert_eq!(protected_frames.iter().filter(|frame| matches!(
            &frame.event.payload, EventPayload::Settled { attempt, .. } if attempt == &f.attempt
        )).count(), 1);
        withdraw_source(&mut f);
        reopen(&mut f);
        let replay = app
            .publish_source_relation_attempt(&f.job, &f.attempt, fixture_deadline(), false)
            .unwrap();
        assert_eq!(replay.1, plan.receipt.outputs);
        let after = f.job.inspect().unwrap();
        assert_eq!(after.attempts.len(), 1);
        assert_eq!(after.budget.dispatched_requests, 1);
        assert_eq!(after.budget.settled.requests, 0);
        assert_eq!(after.budget.outstanding.requests, 1);
        assert_eq!(
            after.budget.unknown_attempts,
            vec![f.attempt.attempt_id.clone()]
        );
        assert!(after.budget.known_costs.is_empty());
        let completed_frames = f
            .job
            .with(false, |_, loaded| Ok(loaded.frames.clone()))
            .unwrap();
        assert_eq!(completed_frames.len(), protected_frames.len() + 1);
        assert_eq!(
            &completed_frames[..protected_frames.len()],
            protected_frames.as_slice()
        );
        let completion = completed_frames.last().unwrap();
        let EventPayload::Checkpoint {
            completed_tasks,
            frontier,
            run_note,
        } = &completion.event.payload
        else {
            panic!("committed replay must append only its missing checkpoint");
        };
        assert_eq!(completed_tasks.count, 1);
        assert!(frontier.is_none());
        assert_eq!(after.run_note.as_ref(), Some(run_note));
        assert_eq!(run_note.record.record_id, f.attempt.run_id);
        assert_eq!(run_note.record.expected_kind, RecordKind::Run);
        assert_eq!(
            run_note.path,
            checkpoint::run_path(&f.attempt.run_id).unwrap()
        );
        checkpoint::output(&f.source.fs, run_note).unwrap();
        checkpoint::verify_summary_proof(
            &f.source.fs,
            run_note,
            completed_tasks,
            &completed_frames,
        )
        .unwrap();
        assert_eq!(
            completion.event.previous_checksum.as_ref(),
            Some(&protected_frames.last().unwrap().checksum)
        );
        assert_eq!(
            after.last_event,
            Some(crate::jobs::events::event_ref(completion))
        );
        let (authenticated, encoded) = crate::jobs::events::encode(&completion.event).unwrap();
        assert_eq!(&authenticated, completion);
        let mut expected_budget = protected_budget;
        expected_budget.journal_events_used += 1;
        expected_budget.journal_bytes_used += encoded.len() as u64;
        assert_eq!(after.budget, expected_budget);
        assert_eq!(after.tasks[&f.attempt.task_key].state, TaskState::Completed);

        // An additional reopen and committed replay must append no duplicate
        // task completion or settlement, and preserve the complete budget.
        reopen(&mut f);
        let again = app
            .publish_source_relation_attempt(&f.job, &f.attempt, fixture_deadline(), false)
            .unwrap();
        assert_eq!(again, replay);
        let replayed = f.job.inspect().unwrap();
        assert_eq!(replayed.budget, after.budget);
        assert_eq!(replayed.last_event, after.last_event);
        assert_eq!(
            f.job
                .with(false, |_, loaded| Ok(loaded.frames.clone()))
                .unwrap(),
            completed_frames
        );
        assert_eq!(
            after.attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
        assert_eq!(after.attempts[0].phase, AttemptPhase::Settled);
        assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn missing_unacknowledged_metadata_refuses_and_preserves_accounting_journal() {
    for normalized in [false, true] {
        let mut f = received(normalized);
        let app = application(&f);
        let plan = fresh(&f);
        let writer = WriterPermit::acquire(f.source.fs.root(), Duration::from_secs(1)).unwrap();
        publish_job_draft(&f.source.fs, &writer, &f.job, plan.draft.clone()).unwrap();
        drop(writer);
        let before = f.job.inspect().unwrap();
        assert_eq!(before.attempts[0].phase, AttemptPhase::Received);
        assert_eq!(before.budget.settled.requests, 0);
        assert!(before.budget.outstanding.requests > 0);
        let spool = before.attempts[0].spool.clone().unwrap();
        std::fs::remove_file(f.source.fs.root().resolve(&spool.metadata.path).unwrap()).unwrap();
        reopen(&mut f);
        let journal = f
            .source
            .fs
            .root()
            .path()
            .join(".wiki/state/jobs/run_relations/journal.bin");
        let retained = std::fs::read(&journal).unwrap();
        assert!(
            app.publish_source_relation_attempt(&f.job, &f.attempt, fixture_deadline(), false)
                .is_err()
        );
        assert_eq!(std::fs::read(&journal).unwrap(), retained);
        for output in &plan.receipt.outputs {
            assert!(checkpoint::output(&f.source.fs, output).is_ok());
        }
        assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn expired_publication_invocation_does_not_write_dispatch_or_acknowledge() {
    for normalized in [false, true] {
        let f = received(normalized);
        let app = application(&f);
        let before = f.job.inspect().unwrap();
        let tree = provider_fixture::tree(f.source.fs.root().path());
        let error = app
            .publish_source_relation_attempt(&f.job, &f.attempt, Instant::now(), false)
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert_eq!(provider_fixture::tree(f.source.fs.root().path()), tree);
        let after = f.job.inspect().unwrap();
        assert_eq!(after.attempts, before.attempts);
        assert_eq!(after.budget, before.budget);
        assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn retained_three_event_shape_cannot_authorize_substituted_or_missing_payloads() {
    let f = received(true);
    let plan = fresh(&f);
    let validated = f
        .job
        .with(false, |guard, loaded| {
            validate_locked(&f.job, guard, loaded, plan.draft.clone())
        })
        .unwrap();
    replay(
        &f.source.fs,
        &f.source.vault,
        &validated.operation,
        &validated.draft,
    )
    .unwrap();
    for index in 0..2 {
        let mut changed = validated.draft.clone();
        changed.operations[index]
            .proposed
            .as_mut()
            .unwrap()
            .extend_from_slice(b"\n<!-- substituted artifact bytes -->\n");
        assert!(
            replay(
                &f.source.fs,
                &f.source.vault,
                &validated.operation,
                &changed
            )
            .is_err()
        );
    }
    let mut wrong_path = validated.draft.clone();
    let mut wrong_operation = validated.operation.clone();
    let IndexedWriteOperation::JobBatch { records, .. } = &mut wrong_operation else {
        unreachable!()
    };
    let target = records
        .iter_mut()
        .find(|target| target.path == wrong_path.operations[0].target)
        .unwrap();
    target.path = VaultRelativePath::new(format!(
        "runs/{}/outputs/{}.md",
        f.attempt.run_id, target.id
    ))
    .unwrap();
    wrong_path.operations[0].target = target.path.clone();
    records.sort_by(|a, b| a.path.cmp(&b.path));
    assert!(wrong_operation.validate().is_err());
    assert!(replay(&f.source.fs, &f.source.vault, &wrong_operation, &wrong_path,).is_err());
    let mut missing = validated.draft.clone();
    missing.operations.remove(0);
    assert!(
        replay(
            &f.source.fs,
            &f.source.vault,
            &validated.operation,
            &missing
        )
        .is_err()
    );
    let mut extra = validated.draft.clone();
    extra.operations.push(extra.operations[0].clone());
    assert!(replay(&f.source.fs, &f.source.vault, &validated.operation, &extra).is_err());
    assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    for op in &validated.draft.operations {
        assert!(!f.source.fs.root().resolve(&op.target).unwrap().exists());
    }
}

#[test]
fn fresh_same_run_outputs_namespace_refuses_before_any_publication_write() {
    let f = received(true);
    let plan = fresh(&f);
    let mut wrong = plan.draft.clone();
    wrong.operations[0].target = VaultRelativePath::new(format!(
        "runs/{}/outputs/{}.md",
        f.attempt.run_id, plan.receipt.outputs[0].record.record_id
    ))
    .unwrap();
    assert!(
        f.job
            .with(false, |guard, loaded| validate_locked(
                &f.job, guard, loaded, wrong
            ))
            .is_err()
    );
    assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    for op in &plan.draft.operations {
        assert!(!f.source.fs.root().resolve(&op.target).unwrap().exists());
    }
}

#[test]
fn exact_roles_accept_all_operation_and_receipt_dependency_permutations() {
    let f = received(true);
    let plan = fresh(&f);
    let base = f
        .job
        .with(false, |guard, loaded| {
            validate_locked(&f.job, guard, loaded, plan.draft.clone())
        })
        .unwrap();
    let receipt_path = event_path(&f.attempt.run_id, &plan.receipt.receipt_id).unwrap();
    for order in OPERATION_PERMUTATIONS {
        for reverse_dependencies in [false, true] {
            let mut draft = permutation(&plan.draft, order);
            if reverse_dependencies {
                draft
                    .operations
                    .iter_mut()
                    .find(|op| op.target == receipt_path)
                    .unwrap()
                    .apply_after
                    .reverse();
            }
            let validated = f
                .job
                .with(false, |guard, loaded| {
                    validate_locked(&f.job, guard, loaded, draft.clone())
                })
                .unwrap();
            assert_eq!(validated.operation, base.operation);
            assert_eq!(
                proposed_receipt(&draft, &f.attempt.run_id).unwrap().outputs,
                plan.receipt.outputs
            );
            replay(&f.source.fs, &f.source.vault, &base.operation, &draft).unwrap();
        }
    }
    assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    for op in &plan.draft.operations {
        assert!(!f.source.fs.root().resolve(&op.target).unwrap().exists());
    }
}

#[test]
fn engine_target_sort_and_retained_manifest_reconstruction_preserve_typed_roles() {
    let f = received(true);
    let plan = fresh(&f);
    let validated = f
        .job
        .with(false, |guard, loaded| {
            validate_locked(&f.job, guard, loaded, permutation(&plan.draft, [2, 0, 1]))
        })
        .unwrap();
    let engine = ChangeEngine::new(f.source.fs.clone()).unwrap();
    let planned = engine.plan(&validated.draft).unwrap();
    assert!(
        planned
            .operations
            .windows(2)
            .all(|pair| pair[0].target < pair[1].target)
    );
    assert!(same_operations(
        &planned.operations,
        &validated.draft.operations
    ));
    let writer = WriterPermit::acquire(f.source.fs.root(), Duration::from_secs(1)).unwrap();
    let retained = engine.prepare(&writer, validated.draft.clone()).unwrap();
    drop(writer);
    let manifest = retained.manifest;
    assert!(
        manifest
            .operations
            .windows(2)
            .all(|pair| pair[0].target < pair[1].target)
    );
    let mut rebuilt = ChangeDraft {
        title: manifest.title.clone(),
        origin: manifest.origin.clone(),
        inverse_of: manifest.inverse_of.clone(),
        allocated_ids: manifest.allocated_ids.clone(),
        read_preconditions: manifest.read_preconditions.clone(),
        operations: vec![],
    };
    for (index, op) in manifest.operations.iter().enumerate() {
        rebuilt.operations.push(ExpectedWrite {
            target: op.target.clone(),
            expected: op.before.clone(),
            proposed: engine
                .verify_payload_with_limit(
                    &manifest.change_id,
                    index,
                    "proposed",
                    &op.target,
                    (&op.after, &op.after_payload),
                    store::MAX_ARTIFACT_BYTES,
                )
                .unwrap(),
            apply_after: op
                .apply_after
                .iter()
                .map(|index| manifest.operations[*index].target.clone())
                .collect(),
        });
    }
    assert!(same_operations(
        &rebuilt.operations,
        &validated.draft.operations
    ));
    replay(
        &f.source.fs,
        &f.source.vault,
        &validated.operation,
        &rebuilt,
    )
    .unwrap();
    assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    for op in &plan.draft.operations {
        assert!(!f.source.fs.root().resolve(&op.target).unwrap().exists());
    }
}

#[test]
fn every_role_order_rejects_changed_missing_extra_duplicate_and_wrong_dependency_sets() {
    let f = received(true);
    let plan = fresh(&f);
    let validated = f
        .job
        .with(false, |guard, loaded| {
            validate_locked(&f.job, guard, loaded, plan.draft.clone())
        })
        .unwrap();
    let raw_path = plan.receipt.outputs[0].path.clone();
    let annotation_path = plan.receipt.outputs[1].path.clone();
    let receipt_path = event_path(&f.attempt.run_id, &plan.receipt.receipt_id).unwrap();
    for order in OPERATION_PERMUTATIONS {
        let base = permutation(&plan.draft, order);
        let raw = base
            .operations
            .iter()
            .position(|op| op.target == raw_path)
            .unwrap();
        let annotation = base
            .operations
            .iter()
            .position(|op| op.target == annotation_path)
            .unwrap();
        let receipt = base
            .operations
            .iter()
            .position(|op| op.target == receipt_path)
            .unwrap();
        let mut invalid = Vec::new();
        for role in [raw, annotation, receipt] {
            let mut changed = base.clone();
            changed.operations[role]
                .proposed
                .as_mut()
                .unwrap()
                .extend_from_slice(b"\n<!-- substituted role bytes -->\n");
            invalid.push(changed);
        }
        let mut missing = base.clone();
        missing.operations.remove(annotation);
        invalid.push(missing);
        let mut extra = base.clone();
        extra.operations.push(base.operations[raw].clone());
        invalid.push(extra);
        let mut duplicate = base.clone();
        duplicate.operations[annotation] = base.operations[raw].clone();
        invalid.push(duplicate);
        for dependencies in [
            vec![],
            vec![raw_path.clone()],
            vec![raw_path.clone(), raw_path.clone()],
            vec![raw_path.clone(), receipt_path.clone()],
            vec![
                raw_path.clone(),
                annotation_path.clone(),
                receipt_path.clone(),
            ],
        ] {
            let mut changed = base.clone();
            changed.operations[receipt].apply_after = dependencies;
            invalid.push(changed);
        }
        for (leaf, dependency) in [
            (raw, annotation_path.clone()),
            (annotation, receipt_path.clone()),
        ] {
            let mut changed = base.clone();
            changed.operations[leaf].apply_after.push(dependency);
            invalid.push(changed);
        }
        let mut deleted = base.clone();
        deleted.operations[raw].proposed = None;
        invalid.push(deleted);
        let mut mutable = base.clone();
        mutable.operations[annotation].expected =
            ExpectedState::Hash(Blake3Hash::digest("existing event"));
        invalid.push(mutable);
        let mut swapped_outputs = base.clone();
        let bytes = swapped_outputs.operations[receipt]
            .proposed
            .as_ref()
            .unwrap();
        let fields = parse_note(bytes).fields.unwrap();
        let mut body: ReceiptBody = decode_fence(bytes, "lwiki.run-event.v1").unwrap();
        body.receipt.outputs.swap(0, 1);
        swapped_outputs.operations[receipt].proposed =
            Some(note(fields, "lwiki.run-event.v1", &body).unwrap());
        invalid.push(swapped_outputs);
        let mut duplicate_reference = base.clone();
        let bytes = duplicate_reference.operations[receipt]
            .proposed
            .as_ref()
            .unwrap();
        let fields = parse_note(bytes).fields.unwrap();
        let mut body: ReceiptBody = decode_fence(bytes, "lwiki.run-event.v1").unwrap();
        body.receipt.outputs[1] = body.receipt.outputs[0].clone();
        duplicate_reference.operations[receipt].proposed =
            Some(note(fields, "lwiki.run-event.v1", &body).unwrap());
        invalid.push(duplicate_reference);
        for (case, changed) in invalid.into_iter().enumerate() {
            assert!(
                f.job
                    .with(false, |guard, loaded| {
                        validate_locked(&f.job, guard, loaded, changed.clone())
                    })
                    .is_err(),
                "fresh order {order:?}, case {case}"
            );
            assert!(
                replay(
                    &f.source.fs,
                    &f.source.vault,
                    &validated.operation,
                    &changed
                )
                .is_err(),
                "replay order {order:?}, case {case}"
            );
        }
    }
    assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    for op in &plan.draft.operations {
        assert!(!f.source.fs.root().resolve(&op.target).unwrap().exists());
    }
}

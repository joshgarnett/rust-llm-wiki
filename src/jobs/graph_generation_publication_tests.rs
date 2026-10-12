//! Genuine mock Received responses exercise the typed publication authority.
use super::*;
use crate::providers::wire_tests::common as provider;
#[allow(
    clippy::duplicate_mod,
    reason = "Independent namespace for the shared public API fixture."
)]
#[path = "../../tests/fixtures/p18/common.rs"]
mod api_fixture;
use crate::{
    catalog::{CatalogOptions, PublicationCheckpoint, PublicationFault},
    jobs::{checkpoint, test_support},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct ReceivedFixture {
    source: api_fixture::Fixture,
    job: JobLedger,
    task: TaskSpec,
    attempt: AttemptRef,
    mock: Arc<api_fixture::Mock>,
}
fn received(normalized: bool) -> ReceivedFixture {
    let source = api_fixture::Fixture::new();
    if normalized {
        source.app.index_rebuild_normalized().unwrap();
    }
    let exported = source
        .app
        .graph_extract_agent(&source.request.export)
        .unwrap();
    let view = SourceView::from_fs_bounded(&source.fs, packet::SOURCE_CAP, 4096).unwrap();
    let packet = packet::load_packet(&view, &exported.packet.packet_id).unwrap();
    let planned =
        generation_cache::plan_task(&packet, &source.service, source.request.max_output_tokens)
            .unwrap();
    let task = planned.task.clone();
    let mut spec = test_support::spec(&source.fs, "run_api", 0);
    spec.created_at_utc_ms = source.request.created_at_utc_ms;
    spec.deadline_utc_ms = source.request.deadline_utc_ms;
    spec.tasks = vec![task.clone()];
    spec.limits = source.request.limits.clone();
    spec.scope.operation = "graph_extract_api".into();
    spec.scope.read_preconditions = vec![ReadDependency {
        path: packet.locator().path.clone(),
        expected: ExpectedState::Hash(packet.locator().observed_hash.clone()),
    }];
    spec.scope.input_records = vec![RecordRef {
        vault_id: spec.vault_id.clone(),
        record_id: packet.packet().packet_id.clone(),
        expected_kind: RecordKind::ExtractionPacket,
    }];
    let summary = source.service.summary();
    spec.scope.profile_fingerprints =
        BTreeMap::from([(summary.profile_id, summary.profile_fingerprint)]);
    spec.scope.scope_payload_hash = Some(packet.packet().packet_fingerprint.clone());
    spec.config_fingerprint = summary.config_fingerprint;
    spec.input_fingerprint = crate::jobs::tasks::input_fingerprint(&spec).unwrap();
    let job = source.ledger();
    let writer = WriterPermit::acquire(source.fs.root(), Duration::from_secs(2)).unwrap();
    generation_cache::retain_input(&source.fs, &writer, &planned).unwrap();
    job.create(&writer, spec).unwrap();
    drop(writer);
    job.start().unwrap();
    let mock = api_fixture::Mock::response(source.response());
    let output = source
        .dispatcher(mock.clone())
        .execute(
            &job,
            &source.service,
            &task.key,
            crate::providers::types::DispatchPurpose::Task,
        )
        .unwrap_or_else(|e| panic!("mock Received failed: {}", e.error.code));
    assert_eq!(
        job.inspect().unwrap().attempts[0].phase,
        AttemptPhase::Received
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    ReceivedFixture {
        source,
        job,
        task,
        attempt: output.attempt,
        mock,
    }
}
fn fresh(f: &ReceivedFixture) -> MaterializationPlan {
    f.job
        .with(false, |guard, loaded| {
            plan_locked(&f.job, guard, loaded, &f.attempt, phase_deadline())
        })
        .unwrap()
}
fn validate(f: &ReceivedFixture, draft: ChangeDraft) -> Result<ValidatedJobDraft> {
    f.job.with(false, |guard, loaded| {
        validate_locked(&f.job, guard, loaded, draft)
    })
}
fn assert_unpublished(f: &ReceivedFixture, plan: &MaterializationPlan) {
    for op in &plan.draft.operations {
        assert!(
            crate::changes::prepare::read_bounded(
                &f.source.fs,
                &op.target,
                crate::graph::MAX_ARTIFACT_BYTES
            )
            .unwrap()
            .is_none()
        );
    }
    assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.job.inspect().unwrap().attempts[0].phase,
        AttemptPhase::Received
    );
}

#[test]
fn exact_received_pair_accepts_both_operation_orders_and_only_extract_task() {
    for normalized in [false, true] {
        let f = received(normalized);
        let plan = fresh(&f);
        assert_eq!(plan.draft.operations.len(), 2);
        let seal = validate(&f, plan.draft.clone()).unwrap();
        replay(
            &f.source.fs,
            &api_fixture::id("vault_test"),
            &seal.operation,
            &seal.draft,
        )
        .unwrap();
        let mut reversed = plan.draft.clone();
        reversed.operations.reverse();
        validate(&f, reversed).unwrap();
        let mut wrong_stage = f.task.clone();
        wrong_stage.stage = TaskStage::PrepareSourceRelations;
        wrong_stage.key = crate::jobs::tasks::task_key(&wrong_stage).unwrap();
        let mut attempt = f.attempt.clone();
        attempt.task_key = wrong_stage.key.clone();
        assert!(require_task(&wrong_stage, &attempt).is_err());
        assert_unpublished(&f, &plan);
    }
}

#[test]
fn coherent_alternate_response_cannot_replace_authenticated_received_text() {
    let f = received(true);
    let plan = fresh(&f);
    let mut alternate = f.source.response();
    alternate["assertions"].as_array_mut().unwrap().pop();
    let text = serde_json::to_vec(&alternate).unwrap();
    let view = SourceView::from_fs_bounded(&f.source.fs, packet::SOURCE_CAP, 4096).unwrap();
    let verified = packet::load_packet(&view, &f.source.packet.packet_id).unwrap();
    wire::validate_response(&verified, &view, &text).unwrap();
    let received = f
        .job
        .with(false, |_, loaded| {
            Ok(loaded
                .frames
                .iter()
                .find(|f| matches!(&f.event.payload, EventPayload::Received { .. }))
                .unwrap()
                .event
                .clone())
        })
        .unwrap();
    let (output, write) = generation_cache::output_write(
        &api_fixture::id("vault_test"),
        &f.attempt,
        &verified,
        &text,
        &timestamp(received.occurred_at_utc_ms).unwrap(),
    )
    .unwrap();
    let mut forged = plan.draft.clone();
    let mut usage = plan.receipt.clone();
    usage.outputs = vec![output];
    forged.operations[0] = write;
    forged.operations[1].proposed = Some(usage_receipt_bytes(&usage, &received).unwrap());
    let budget = f.job.inspect().unwrap().budget;
    assert!(validate(&f, forged).is_err());
    assert_eq!(f.job.inspect().unwrap().budget, budget);
    assert_unpublished(&f, &plan);
}

#[test]
fn forged_accounting_guard_path_and_order_pairs_refuse_without_writes() {
    let f = received(true);
    let plan = fresh(&f);
    let budget = f.job.inspect().unwrap().budget;
    let received = f
        .job
        .with(false, |_, loaded| {
            Ok(loaded
                .frames
                .iter()
                .find(|f| matches!(&f.event.payload, EventPayload::Received { .. }))
                .unwrap()
                .event
                .clone())
        })
        .unwrap();
    let mut proposals = vec![];
    let mut wrong_usage = plan.draft.clone();
    let mut usage = plan.receipt.clone();
    usage.billing = BillingDisposition::KnownSettled;
    wrong_usage.operations[1].proposed = Some(usage_receipt_bytes(&usage, &received).unwrap());
    proposals.push(wrong_usage);
    let mut missing = plan.draft.clone();
    missing.read_preconditions.pop();
    proposals.push(missing);
    let mut duplicate = plan.draft.clone();
    duplicate
        .read_preconditions
        .push(duplicate.read_preconditions[0].clone());
    proposals.push(duplicate);
    let mut changed = plan.draft.clone();
    changed.read_preconditions[0].expected = ExpectedState::Hash(Blake3Hash::digest("forged"));
    proposals.push(changed);
    let mut extra = plan.draft.clone();
    extra.operations.push(extra.operations[0].clone());
    proposals.push(extra);
    let mut wrong_path = plan.draft.clone();
    wrong_path.operations[0].target = VaultRelativePath::new(format!(
        "runs/other/outputs/{}.md",
        plan.receipt.outputs[0].record.record_id
    ))
    .unwrap();
    proposals.push(wrong_path);
    let mut missing_edge = plan.draft.clone();
    missing_edge.operations[1].apply_after.clear();
    proposals.push(missing_edge);
    let mut reverse_edge = plan.draft.clone();
    reverse_edge.operations[0].apply_after = vec![reverse_edge.operations[1].target.clone()];
    proposals.push(reverse_edge);
    for draft in proposals {
        assert!(validate(&f, draft).is_err());
        assert_eq!(f.job.inspect().unwrap().budget, budget);
        assert_unpublished(&f, &plan);
    }
}

struct StopSql {
    point: PublicationCheckpoint,
    reached: AtomicBool,
}
impl PublicationFault for StopSql {
    fn check(&self, at: PublicationCheckpoint) -> Result<()> {
        if at == self.point {
            self.reached.store(true, Ordering::SeqCst);
            Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "graph generation reached SQL cut",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn ordinary_offline_api_retry_recovers_sql_cuts_and_reuses_without_second_request() {
    for cut in [
        PublicationCheckpoint::AfterPointer,
        PublicationCheckpoint::AfterCommit,
    ] {
        let f = received(true);
        let plan = fresh(&f);
        let fault = Arc::new(StopSql {
            point: cut,
            reached: AtomicBool::new(false),
        });
        let faulty = Catalog::with_options(
            f.source.fs.clone(),
            api_fixture::id("vault_test"),
            CatalogOptions {
                busy_timeout_ms: 1000,
                fault: Some(fault.clone()),
            },
        );
        let writer = WriterPermit::acquire(f.source.fs.root(), Duration::from_secs(2)).unwrap();
        let mut session =
            match checkpoint::prepare_job_draft(&faulty, &writer, &f.job, plan.draft.clone())
                .unwrap()
            {
                checkpoint::JobPreparation::Pending(session) => session,
                _ => panic!("fresh pair already committed"),
            };
        let expected_change = session.proof().change.clone();
        assert!(
            f.source
                .engine
                .apply_indexed_refresh(&writer, &mut session)
                .is_err()
        );
        assert!(
            fault.reached.load(Ordering::SeqCst),
            "intended SQL publication cut must be reached"
        );
        drop(session);
        drop(writer);
        // Canonical recovery must use the sealed pair, never expired transient
        // response bytes or the descriptor. Retain metadata for accounting ack.
        let body_path = VaultRelativePath::new(format!(
            ".wiki/state/requests/{}/response.bin",
            f.attempt.attempt_id
        ))
        .unwrap();
        std::fs::remove_file(f.source.fs.root().resolve(&body_path).unwrap()).unwrap();
        std::fs::remove_file(f.source.fs.root().resolve(&f.task.input.path).unwrap()).unwrap();
        let mut options = f.source.options.clone();
        options.policy.offline = true;
        let dispatcher = f.source.dispatcher(f.mock.clone());
        let first = f
            .source
            .app
            .graph_extract_api(
                &f.source.request,
                &f.source.service,
                &dispatcher,
                options.clone(),
            )
            .unwrap();
        assert!(first.reused);
        assert_eq!(first.output.as_ref(), Some(&plan.receipt.outputs[0]));
        let committed = committed_pair(&f.job, &f.attempt).unwrap().unwrap();
        assert_eq!(committed.change, expected_change);
        assert_eq!(committed.usage.billing, BillingDisposition::UnknownReserved);
        assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
        let again = f
            .source
            .app
            .graph_extract_api(&f.source.request, &f.source.service, &dispatcher, options)
            .unwrap();
        assert!(again.reused);
        assert_eq!(first.output, again.output);
        assert_eq!(first.receipt, again.receipt);
        assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
        assert!(!f.job.inspect().unwrap().budget.unknown_attempts.is_empty());
    }
}

#[test]
fn ordinary_received_retry_and_committed_reuse_keep_one_request_and_unknown_hold() {
    for normalized in [false, true] {
        let f = received(normalized);
        let dispatcher = f.source.dispatcher(f.mock.clone());
        let first = f
            .source
            .app
            .graph_extract_api(
                &f.source.request,
                &f.source.service,
                &dispatcher,
                f.source.options.clone(),
            )
            .unwrap();
        let repeated = f
            .source
            .app
            .graph_extract_api(
                &f.source.request,
                &f.source.service,
                &dispatcher,
                f.source.options.clone(),
            )
            .unwrap();
        assert!(repeated.reused);
        assert_eq!(first.output, repeated.output);
        assert_eq!(first.receipt, repeated.receipt);
        assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
        let committed = committed_pair(&f.job, &f.attempt).unwrap().unwrap();
        assert_eq!(committed.usage.billing, BillingDisposition::UnknownReserved);
        assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
        assert!(!f.job.inspect().unwrap().budget.unknown_attempts.is_empty());
        f.job
            .with(false, |guard, _| {
                assert!(
                    guard
                        .read_spool(&f.attempt, SpoolPart::Body, GENERATION_SPOOL_MAX_BYTES)?
                        .is_none()
                );
                Ok(())
            })
            .unwrap();
    }
}

struct StopLedger {
    point: LedgerCheckpoint,
    reached: AtomicBool,
}
impl LedgerFault for StopLedger {
    fn check(&self, at: LedgerCheckpoint) -> Result<()> {
        if at == self.point {
            self.reached.store(true, Ordering::SeqCst);
            Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "graph generation reached ledger cut",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn ordinary_api_retry_recovers_acknowledgement_and_settlement_cuts() {
    for cut in [
        LedgerCheckpoint::AfterOutputsCommitted,
        LedgerCheckpoint::AfterSettlement,
    ] {
        let f = received(true);
        let plan = fresh(&f);
        let mut options = f.source.options.clone();
        let fault = Arc::new(StopLedger {
            point: cut,
            reached: AtomicBool::new(false),
        });
        options.fault = Some(fault.clone());
        let interrupted = JobLedger::new(
            f.source.fs.clone(),
            api_fixture::id("vault_test"),
            f.attempt.run_id.clone(),
            options,
        )
        .unwrap();
        assert!(
            f.source
                .app
                .commit_generation_task(&interrupted, &plan, true)
                .is_err()
        );
        assert!(
            fault.reached.load(Ordering::SeqCst),
            "intended ledger publication cut must be reached"
        );
        let dispatcher = f.source.dispatcher(f.mock.clone());
        let first = f
            .source
            .app
            .graph_extract_api(
                &f.source.request,
                &f.source.service,
                &dispatcher,
                f.source.options.clone(),
            )
            .unwrap();
        let repeated = f
            .source
            .app
            .graph_extract_api(
                &f.source.request,
                &f.source.service,
                &dispatcher,
                f.source.options.clone(),
            )
            .unwrap();
        assert_eq!(first.output.as_ref(), Some(&plan.receipt.outputs[0]));
        assert_eq!(first.output, repeated.output);
        assert_eq!(first.receipt, repeated.receipt);
        assert!(repeated.reused);
        assert_eq!(f.mock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            committed_pair(&f.job, &f.attempt)
                .unwrap()
                .unwrap()
                .usage
                .billing,
            BillingDisposition::UnknownReserved
        );
        assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
    }
}

#[test]
fn missing_received_body_and_changed_packet_guard_cannot_grant_fresh_authority() {
    for missing_body in [false, true] {
        let f = received(true);
        let plan = fresh(&f);
        let budget = f.job.inspect().unwrap().budget;
        if missing_body {
            let body = VaultRelativePath::new(format!(
                ".wiki/state/requests/{}/response.bin",
                f.attempt.attempt_id
            ))
            .unwrap();
            std::fs::remove_file(f.source.fs.root().resolve(&body).unwrap()).unwrap();
        } else {
            let guard = &f.task.source_bindings[0];
            let path = f.source.fs.root().resolve(&guard.path).unwrap();
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(b'\n');
            std::fs::write(path, bytes).unwrap();
        }
        assert!(validate(&f, plan.draft.clone()).is_err());
        assert_eq!(f.job.inspect().unwrap().budget, budget);
        assert_unpublished(&f, &plan);
    }
}

fn project_actual(
    f: &ReceivedFixture,
    plan: &MaterializationPlan,
) -> (IndexedWriteOperation, ChangeDraft) {
    use crate::catalog::{
        query_types::QueryReadLimits, source_projection::RefreshProjectionLimits,
        write_projection::project_jobs,
    };
    let validated = validate(f, plan.draft.clone()).unwrap();
    let operation = validated.operation.clone();
    let reader = f
        .source
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let projected = project_jobs(
        &f.source.fs,
        &reader,
        validated,
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap();
    (operation, projected.draft().clone())
}

#[test]
fn actual_projected_target_witnesses_replay_but_cannot_grant_fresh_authority() {
    let f = received(true);
    let plan = fresh(&f);
    let (operation, projected) = project_actual(&f, &plan);
    for op in &projected.operations {
        assert_eq!(op.expected, ExpectedState::Absent);
        assert!(projected.read_preconditions.contains(&ReadDependency {
            path: op.target.clone(),
            expected: ExpectedState::Absent
        }));
    }
    replay(
        &f.source.fs,
        &api_fixture::id("vault_test"),
        &operation,
        &projected,
    )
    .unwrap();
    assert!(
        validate(&f, projected.clone()).is_err(),
        "projected before witnesses are never fresh source authority"
    );
    let mut bad = vec![];
    let mut unexpected = projected.clone();
    unexpected.read_preconditions.push(ReadDependency {
        path: VaultRelativePath::new("notes/unrelated.md").unwrap(),
        expected: ExpectedState::Absent,
    });
    bad.push(unexpected);
    let mut partial = projected.clone();
    partial
        .read_preconditions
        .retain(|d| d.path != projected.operations[0].target);
    bad.push(partial);
    let mut mismatched = projected.clone();
    mismatched
        .read_preconditions
        .iter_mut()
        .find(|d| d.path == projected.operations[0].target)
        .unwrap()
        .expected = ExpectedState::Hash(Blake3Hash::digest("invented existing output"));
    bad.push(mismatched);
    let mut source_missing = projected.clone();
    source_missing
        .read_preconditions
        .retain(|d| d.path != f.task.source_bindings[0].path);
    bad.push(source_missing);
    let mut duplicate = projected.clone();
    duplicate
        .read_preconditions
        .push(duplicate.read_preconditions[0].clone());
    bad.push(duplicate);
    let mut wrong_before = projected.clone();
    wrong_before.operations[0].expected =
        ExpectedState::Hash(Blake3Hash::digest("invented prior output"));
    bad.push(wrong_before);
    for draft in bad {
        assert!(
            replay(
                &f.source.fs,
                &api_fixture::id("vault_test"),
                &operation,
                &draft
            )
            .is_err()
        );
    }
    assert_unpublished(&f, &plan);
}

#[test]
fn actual_retained_manifest_replay_keeps_immutable_before_states_without_overlapping_guards() {
    let f = received(true);
    let plan = fresh(&f);
    let writer = WriterPermit::acquire(f.source.fs.root(), Duration::from_secs(2)).unwrap();
    let session =
        match checkpoint::prepare_job_draft(&f.source.catalog, &writer, &f.job, plan.draft.clone())
            .unwrap()
        {
            checkpoint::JobPreparation::Pending(session) => session,
            _ => panic!("fresh pair already committed"),
        };
    let proof = session.proof();
    let operation = proof.operation.as_ref().unwrap();
    let manifest = f
        .source
        .engine
        .inspect_history(&proof.change.change_id)
        .unwrap()
        .manifest;
    let mut retained = ChangeDraft {
        title: manifest.title.clone(),
        origin: manifest.origin.clone(),
        inverse_of: manifest.inverse_of.clone(),
        allocated_ids: manifest.allocated_ids.clone(),
        read_preconditions: manifest.read_preconditions.clone(),
        operations: vec![],
    };
    // Reconstruct exactly as source_refresh's retained JobBatch payload verifier.
    for (index, op) in manifest.operations.iter().enumerate() {
        let bytes = f
            .source
            .engine
            .verify_payload(
                &manifest.change_id,
                index,
                "proposed",
                &op.target,
                &op.after,
                &op.after_payload,
            )
            .unwrap()
            .unwrap();
        assert_eq!(op.before, ExpectedState::Absent);
        assert!(
            !retained
                .read_preconditions
                .iter()
                .any(|guard| guard.path == op.target)
        );
        retained.operations.push(ExpectedWrite {
            target: op.target.clone(),
            expected: op.before.clone(),
            proposed: Some(bytes),
            apply_after: op
                .apply_after
                .iter()
                .map(|index| manifest.operations[*index].target.clone())
                .collect(),
        });
    }
    for source in &f.task.source_bindings {
        assert!(retained.read_preconditions.contains(source));
    }
    replay(
        &f.source.fs,
        &api_fixture::id("vault_test"),
        operation,
        &retained,
    )
    .unwrap();
    let mut partial = retained.clone();
    partial.read_preconditions.push(ReadDependency {
        path: retained.operations[0].target.clone(),
        expected: ExpectedState::Absent,
    });
    assert!(
        replay(
            &f.source.fs,
            &api_fixture::id("vault_test"),
            operation,
            &partial
        )
        .is_err()
    );
    let mut source_missing = retained.clone();
    source_missing
        .read_preconditions
        .retain(|d| d.path != f.task.source_bindings[0].path);
    assert!(
        replay(
            &f.source.fs,
            &api_fixture::id("vault_test"),
            operation,
            &source_missing
        )
        .is_err()
    );
    drop(session);
    drop(writer);
    assert_unpublished(&f, &plan);
}

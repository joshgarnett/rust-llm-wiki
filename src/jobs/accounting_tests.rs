use crate as lwiki;
#[path = "../../tests/fixtures/p15/common.rs"]
mod common;
use super::{checkpoint, events, ledger, types::*};
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::ChangeEngine,
    domain::*,
    vault::WriterPermit,
};
use common::*;
use std::{sync::Arc, time::Duration};
fn authorize(job: &JobLedger, t: &TaskSpec) -> AttemptRef {
    let r = job.reserve(&t.key, bound(t)).unwrap();
    let p = job.dispatch_intent(r).unwrap();
    let send = job.begin_send(p).unwrap();
    let r = send.attempt().clone();
    assert_eq!(send.bound().wire_hash, r.request_hash);
    drop(send);
    r
}
fn response() -> ResponseSpoolInput {
    ResponseSpoolInput {
        bytes: b"invalid paid JSON response".to_vec(),
        metadata: ResponseMetadata {
            provider_request_id: Some("request-1".into()),
            returned_model: Some("mock-model".into()),
            status_code: Some(200),
            terminal_response: true,
            usage: KnownOrUnknown::Known(Usage {
                billable_units: [
                    (BillableClass::Input, 30),
                    (BillableClass::Output, 20),
                    (BillableClass::Reasoning, 10),
                ]
                .into_iter()
                .map(|(c, n)| (c, KnownOrUnknown::Known(n)))
                .collect(),
                request_bytes: 100,
                response_bytes: 26,
            }),
            computed_cost: KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 60)),
            failure_code: Some("MALFORMED".into()),
        },
    }
}
fn canonical_receipt(
    job: &JobLedger,
    fs: &crate::vault::VaultFs,
    r: &AttemptRef,
) -> DurableOutputRef {
    let plan = job.materialization_plan(r).unwrap();
    let receipt_id = plan.receipt.receipt_id.clone();
    let bytes = plan
        .draft
        .operations
        .last()
        .unwrap()
        .proposed
        .clone()
        .unwrap();
    let path = plan.draft.operations.last().unwrap().target.clone();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let prepared = engine.prepare(&writer, plan.draft).unwrap().prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(fs.clone(), id("vault_test")),
        )
        .unwrap();
    drop(writer);
    let output = DurableOutputRef {
        record: RecordRef {
            vault_id: id("vault_test"),
            record_id: receipt_id,
            expected_kind: RecordKind::RunEvent,
        },
        path,
        hash: Blake3Hash::digest(bytes),
    };
    job.outputs_committed(r, &prepared, output.clone(), vec![], vec![])
        .unwrap();
    output
}
#[test]
fn retry_gets_new_reservation_no_double_settlement() {
    let (_t, fs, job, spec, _) = fixture(1, |_| {});
    let r = authorize(&job, &spec.tasks[0]);
    job.record_response(&r, response()).unwrap();
    let receipt = canonical_receipt(&job, &fs, &r);
    let settled = job.settle(&r).unwrap();
    assert_eq!(job.settle(&r).unwrap(), settled);
    job.remove_spool_after_verified_commit(&r).unwrap();
    assert_eq!(job.inspect().unwrap().budget.settled.requests, 1);
    let retry = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    assert_ne!(retry.attempt().attempt_id, r.attempt_id);
    assert_eq!(retry.attempt().number, 2);
    assert_eq!(job.inspect().unwrap().budget.outstanding.requests, 1);
    assert!(fs.root().resolve(&receipt.path).unwrap().exists());
}
#[test]
fn unknown_retains_money_and_remote_exposure_until_reconciled() {
    let (_t, _fs, job, spec, _) = fixture(2, |s| s.limits.concurrency = 1);
    let r = authorize(&job, &spec.tasks[0]);
    job.outcome_unknown(&r, "TIMEOUT").unwrap();
    assert_eq!(job.inspect().unwrap().budget.remote_inflight, 1);
    assert!(
        job.reserve(&spec.tasks[1].key, bound(&spec.tasks[1]))
            .is_err()
    );
    job.reconcile(
        &r,
        true,
        KnownOrUnknown::Unknown,
        KnownOrUnknown::Unknown,
        "TERMINAL_CONFIRMED",
    )
    .unwrap();
    assert_eq!(job.inspect().unwrap().budget.remote_inflight, 0);
    assert_eq!(job.inspect().unwrap().budget.outstanding.requests, 1);
    job.reserve(&spec.tasks[1].key, bound(&spec.tasks[1]))
        .unwrap();
}
#[test]
fn checkpoint_requires_actual_committed_exact_run_and_events() {
    let (_t, fs, job, _spec, _) = fixture(1, |_| {});
    let draft = job.checkpoint_plan().unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let prepared = engine.prepare(&writer, draft).unwrap().prepared;
    assert!(job.checkpoint_committed(&prepared).is_err());
    engine
        .apply(
            &writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(fs.clone(), id("vault_test")),
        )
        .unwrap();
    drop(writer);
    job.checkpoint_committed(&prepared).unwrap();
    assert_eq!(job.inspect().unwrap().state, RunState::Running);
}
#[test]
fn dispatch_authority_is_one_use_and_replay_never_mints_it() {
    let (_t, _fs, job, spec, _) = fixture(1, |_| {});
    let r = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    let p = job.dispatch_intent(r).unwrap();
    let forged = DispatchPermit {
        attempt: p.attempt.clone(),
        bound: p.bound.clone(),
        intent_event: p.intent_event.clone(),
        genesis_hash: p.genesis_hash.clone(),
    };
    let send = job.begin_send(p).unwrap();
    assert_ne!(send.genesis_hash, hash("unassigned"));
    assert!(job.begin_send(forged).is_err());
    assert_eq!(
        job.replay().unwrap().inspection.budget.dispatched_requests,
        1
    );
    assert!(send.send_event.sequence > 0);
}
struct FailAt {
    point: LedgerCheckpoint,
    armed: std::sync::atomic::AtomicBool,
}
impl LedgerFault for FailAt {
    fn check(&self, p: LedgerCheckpoint) -> Result<()> {
        if p == self.point && self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            Err(WikiError::new(
                ErrorCode::Internal,
                "injected lifecycle checkpoint",
            ))
        } else {
            Ok(())
        }
    }
}
fn fault_job(
    fs: &crate::vault::VaultFs,
    spec: &RunSpec,
    clock: &Arc<Clock>,
    point: LedgerCheckpoint,
) -> JobLedger {
    let mut opts = options(clock.clone());
    opts.fault = Some(Arc::new(FailAt {
        point,
        armed: std::sync::atomic::AtomicBool::new(true),
    }));
    JobLedger::new(fs.clone(), spec.vault_id.clone(), spec.run_id.clone(), opts).unwrap()
}
struct PolicyChangesAfterSync {
    armed: std::sync::atomic::AtomicBool,
    clock: Arc<Clock>,
    utc: Option<i64>,
    cancel: Option<CancellationToken>,
}
impl LedgerFault for PolicyChangesAfterSync {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterJournalSync
            && self.armed.swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            if let Some(utc) = self.utc {
                self.clock.0.store(utc, std::sync::atomic::Ordering::SeqCst);
            }
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
        }
        Ok(())
    }
}
#[test]
fn final_durable_authority_gate_retains_accounting_without_returning_capsules() {
    for stage in 0..3 {
        for policy in 0..6 {
            let (_t, fs, job, spec, clock) = fixture(1, |_| {});
            let mut b = priced(&spec.tasks[0]);
            if policy == 3 || policy == 4 {
                let card = b.rate_card.as_mut().unwrap();
                card.validity = if policy == 3 {
                    PriceValidity::DispatchLocked {
                        valid_from_utc_ms: spec.created_at_utc_ms - 1000,
                        valid_until_utc_ms: spec.created_at_utc_ms + 1000,
                    }
                } else {
                    PriceValidity::EntireAttempt {
                        valid_from_utc_ms: spec.created_at_utc_ms - 1000,
                        valid_until_utc_ms: spec.created_at_utc_ms + b.timeout_ms as i64 + 1000,
                    }
                };
                card.fingerprint = super::budgets::rate_card_fingerprint(card).unwrap();
                b.bounds_fingerprint = super::budgets::bound_fingerprint(&b).unwrap();
            }
            let mut reservation = if stage > 0 {
                Some(job.reserve(&spec.tasks[0].key, b.clone()).unwrap())
            } else {
                None
            };
            let permit = if stage == 2 {
                Some(job.dispatch_intent(reservation.take().unwrap()).unwrap())
            } else {
                None
            };
            let mut opts = options(clock.clone());
            let utc = match policy {
                0 => None,
                1 => Some(spec.created_at_utc_ms - 1),
                2 => Some(spec.deadline_utc_ms),
                3 | 4 => Some(spec.created_at_utc_ms + 2000),
                _ => Some(spec.deadline_utc_ms - 1),
            };
            opts.fault = Some(Arc::new(PolicyChangesAfterSync {
                armed: std::sync::atomic::AtomicBool::new(true),
                clock: clock.clone(),
                utc,
                cancel: (policy == 0).then(|| opts.cancel.clone()),
            }));
            let fault = JobLedger::new(fs, spec.vault_id, spec.run_id, opts).unwrap();
            let error = match stage {
                0 => fault.reserve(&spec.tasks[0].key, b).err().unwrap(),
                1 => fault
                    .dispatch_intent(reservation.take().unwrap())
                    .err()
                    .unwrap(),
                _ => fault.begin_send(permit.unwrap()).err().unwrap(),
            };
            let expected_code = match policy {
                0 => ErrorCode::Cancelled,
                3 | 4 => ErrorCode::CapabilityUnavailable,
                _ => ErrorCode::BudgetExceeded,
            };
            assert_eq!(error.code, expected_code, "stage {stage}, policy {policy}");
            let replayed = job.replay().unwrap().inspection;
            assert_eq!(
                replayed.state,
                if policy == 0 {
                    RunState::Stopped
                } else {
                    RunState::Paused
                }
            );
            assert_eq!(replayed.attempts.len(), 1);
            let a = &replayed.attempts[0];
            assert_eq!(a.attempt.number, 1);
            assert_eq!(
                a.billing,
                if stage == 0 {
                    BillingDisposition::Reserved
                } else {
                    BillingDisposition::UnknownReserved
                }
            );
            assert_eq!(
                a.phase,
                if stage == 0 {
                    AttemptPhase::Reserved
                } else {
                    AttemptPhase::DispatchIntent
                }
            );
            assert_eq!(
                a.remote_exposure,
                if stage == 0 {
                    RemoteExposure::NotStarted
                } else {
                    RemoteExposure::PossiblyInFlight
                }
            );
            assert_eq!(replayed.budget.dispatched_requests, u64::from(stage > 0));
            assert!(
                job.reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
                    .is_err()
            );
        }
    }
}
fn priced(task: &TaskSpec) -> AttemptBound {
    common::priced_bound(task)
}
fn authorize_bound(job: &JobLedger, t: &TaskSpec, b: AttemptBound) -> AttemptRef {
    let r = job.reserve(&t.key, b).unwrap();
    let p = job.dispatch_intent(r).unwrap();
    let token = job.begin_send(p).unwrap();
    token.attempt().clone()
}
#[test]
fn unknown_usage_known_cost_overrun_and_currency_are_independent_violations() {
    for currency in ["USD", "EUR"] {
        let (_t, _fs, job, spec, _) = fixture(2, |s| {
            s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 1000))
        });
        let r = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
        let mut response = response();
        response.metadata.usage = KnownOrUnknown::Unknown;
        response.metadata.computed_cost =
            KnownOrUnknown::Known(Money::new(Currency::new(currency).unwrap(), 100));
        job.record_response(&r, response).unwrap();
        let i = job.inspect().unwrap();
        assert_eq!(i.state, RunState::Paused);
        assert!(!i.budget.guarantee_intact);
        assert_eq!(i.budget.known_costs[&Currency::new(currency).unwrap()], 100);
        assert_eq!(
            i.budget.outstanding.billable_units[&BillableClass::Input],
            100
        );
        assert!(
            job.reserve(&spec.tasks[1].key, priced(&spec.tasks[1]))
                .is_err()
        );
        assert!(
            job.reconcile(
                &r,
                true,
                KnownOrUnknown::Unknown,
                KnownOrUnknown::Known(Money::new(Currency::new(currency).unwrap(), 101)),
                "CORRECTED_COST"
            )
            .is_ok()
        );
        assert!(!job.inspect().unwrap().budget.guarantee_intact);
    }
}
#[test]
fn after_received_and_orphan_overrun_stop_before_another_admission() {
    for point in [
        LedgerCheckpoint::AfterReceived,
        LedgerCheckpoint::AfterSpoolMetadataSync,
    ] {
        let (_t, fs, job, spec, clock) = fixture(2, |s| {
            s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 1000))
        });
        let r = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
        let fault = fault_job(&fs, &spec, &clock, point);
        let mut paid = response();
        paid.metadata.usage = KnownOrUnknown::Unknown;
        paid.metadata.computed_cost =
            KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 100));
        assert!(fault.record_response(&r, paid).is_err());
        let report = job.replay().unwrap();
        assert_eq!(report.inspection.state, RunState::Paused);
        assert!(!report.inspection.budget.guarantee_intact);
        let mut again = response();
        again.metadata.usage = KnownOrUnknown::Unknown;
        again.metadata.computed_cost =
            KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 100));
        job.record_response(&r, again).unwrap();
        assert!(
            job.reserve(&spec.tasks[1].key, priced(&spec.tasks[1]))
                .is_err()
        );
        assert!(!job.replay().unwrap().inspection.budget.guarantee_intact);
    }
}
fn apply_receipt_plan(
    fs: &crate::vault::VaultFs,
    plan: MaterializationPlan,
) -> (crate::changes::PreparedChange, DurableOutputRef) {
    let receipt_id = plan.receipt.receipt_id;
    let path = checkpoint::event_path(&id("run_test"), &receipt_id).unwrap();
    let hash = Blake3Hash::digest(
        plan.draft
            .operations
            .iter()
            .find(|op| op.target == path)
            .unwrap()
            .proposed
            .as_ref()
            .unwrap(),
    );
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let prepared = engine.prepare(&writer, plan.draft).unwrap().prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(fs.clone(), id("vault_test")),
        )
        .unwrap();
    drop(writer);
    (
        prepared,
        DurableOutputRef {
            record: RecordRef {
                vault_id: id("vault_test"),
                record_id: receipt_id,
                expected_kind: RecordKind::RunEvent,
            },
            path,
            hash,
        },
    )
}
#[test]
fn interrupted_validated_output_ack_repairs_task_and_dependency_without_new_request() {
    for replay_first in [false, true] {
        let (_t, fs, job, spec, clock) = fixture(2, |s| {
            let predecessor = s.tasks[0].key.clone();
            s.tasks[1].dependencies.push(predecessor);
        });
        let r = authorize(&job, &spec.tasks[0]);
        let mut paid = response();
        paid.metadata.usage = KnownOrUnknown::Unknown;
        paid.metadata.computed_cost = KnownOrUnknown::Unknown;
        job.record_response(&r, paid).unwrap();
        let page=b"---\nwiki_schema: \"1\"\nwiki_id: output_page\nwiki_kind: page\ntitle: Output\nwiki_status: reviewed\n---\nValidated local mock adapter output.\n".to_vec();
        let output = DurableOutputRef {
            record: RecordRef {
                vault_id: id("vault_test"),
                record_id: id("output_page"),
                expected_kind: RecordKind::Page,
            },
            path: rel("pages/output.md"),
            hash: Blake3Hash::digest(&page),
        };
        let plan = checkpoint::receipt_plan(
            &job,
            &r,
            OutputDisposition::Validated,
            vec![output.clone()],
            vec![],
            vec![checkpoint::write(
                output.path.clone(),
                crate::vault::ExpectedState::Absent,
                page,
            )],
        )
        .unwrap();
        let (prepared, receipt) = apply_receipt_plan(&fs, plan);
        let fault = fault_job(&fs, &spec, &clock, LedgerCheckpoint::AfterOutputsCommitted);
        assert!(
            fault
                .outputs_committed(&r, &prepared, receipt.clone(), vec![output.clone()], vec![])
                .is_err()
        );
        assert_eq!(
            job.inspect().unwrap().tasks[&r.task_key].state,
            TaskState::Running
        );
        if replay_first {
            job.replay().unwrap();
        }
        job.outputs_committed(&r, &prepared, receipt, vec![output], vec![])
            .unwrap();
        job.settle(&r).unwrap();
        assert_eq!(
            job.inspect().unwrap().tasks[&r.task_key].state,
            TaskState::Completed
        );
        assert_eq!(job.ready_tasks().unwrap()[0].key, spec.tasks[1].key);
        assert!(job.reserve(&r.task_key, bound(&spec.tasks[0])).is_err());
        assert_eq!(
            job.inspect().unwrap().budget.unknown_attempts,
            vec![r.attempt_id]
        );
    }
}
#[test]
fn source_drift_keeps_paid_accounting_but_blocks_derived_activation_and_resume() {
    let (t, fs, job, spec, _) = fixture(1, |s| {
        s.scope
            .read_preconditions
            .push(crate::changes::ReadDependency {
                path: s.tasks[0].input.path.clone(),
                expected: crate::vault::ExpectedState::Hash(s.tasks[0].input.hash.clone()),
            })
    });
    let r = authorize(&job, &spec.tasks[0]);
    job.record_response(&r, response()).unwrap();
    std::fs::write(
        t.path().join(spec.tasks[0].input.path.as_str()),
        b"changed source/config input",
    )
    .unwrap();
    let derived = checkpoint::receipt_plan(
        &job,
        &r,
        OutputDisposition::Validated,
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    assert!(
        ChangeEngine::new(fs.clone())
            .unwrap()
            .prepare(&writer, derived.draft)
            .is_err()
    );
    drop(writer);
    let rejected = checkpoint::receipt_plan(
        &job,
        &r,
        OutputDisposition::Rejected,
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let (prepared, receipt) = apply_receipt_plan(&fs, rejected);
    job.outputs_committed(&r, &prepared, receipt, vec![], vec![])
        .unwrap();
    job.settle(&r).unwrap();
    assert_eq!(job.inspect().unwrap().budget.settled.requests, 1);
    job.pause(StopReason::InputsChanged).unwrap();
    assert!(job.resume(None).is_err());
    let draft = job.checkpoint_plan().unwrap();
    assert!(draft.read_preconditions.is_empty());
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let p = engine.prepare(&writer, draft).unwrap().prepared;
    engine
        .apply(
            &writer,
            &p,
            &CatalogGraphValidator,
            &Catalog::new(fs.clone(), id("vault_test")),
        )
        .unwrap();
    drop(writer);
    job.checkpoint_committed(&p).unwrap();
    assert!(job.replay().unwrap().reusable_outputs.is_empty());
    assert_eq!(job.inspect().unwrap().state, RunState::Paused);
}
#[test]
fn optional_corrections_do_not_spend_receipt_slots_and_work_after_cleanup() {
    let (_t, fs, job, spec, _) = fixture(2, |s| {
        s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 1000))
    });
    let r = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
    let persistence = job.inspect().unwrap().attempts[0].persistence.clone();
    for n in 0..20 {
        job.reconcile(
            &r,
            false,
            KnownOrUnknown::Unknown,
            KnownOrUnknown::Unknown,
            &format!("OBSERVATION_{n}"),
        )
        .unwrap();
    }
    assert_eq!(job.inspect().unwrap().attempts[0].persistence, persistence);
    let mut response = response();
    response.metadata.computed_cost =
        KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 1));
    job.record_response(&r, response).unwrap();
    canonical_receipt(&job, &fs, &r);
    job.settle(&r).unwrap();
    job.remove_spool_after_verified_commit(&r).unwrap();
    assert_eq!(
        job.inspect().unwrap().attempts[0].persistence.event_slots,
        0
    );
    job.reconcile(
        &r,
        true,
        KnownOrUnknown::Unknown,
        KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 100)),
        "LATE_COST_CORRECTION",
    )
    .unwrap();
    assert!(!job.inspect().unwrap().budget.guarantee_intact);
    assert_eq!(job.inspect().unwrap().state, RunState::Paused);
    assert_eq!(
        job.inspect().unwrap().budget.known_costs[&Currency::new("USD").unwrap()],
        100
    );
}
#[test]
fn undeclared_unknown_billable_class_breaks_completeness() {
    let (_t, fs, job, spec, _) = fixture(1, |_| {});
    let r = authorize(&job, &spec.tasks[0]);
    let mut response = response();
    let KnownOrUnknown::Known(usage) = &mut response.metadata.usage else {
        panic!()
    };
    usage
        .billable_units
        .insert(BillableClass::FetchByte, KnownOrUnknown::Unknown);
    job.record_response(&r, response).unwrap();
    assert!(!job.inspect().unwrap().budget.guarantee_intact);
    let receipt = canonical_receipt(&job, &fs, &r);
    let decoded = checkpoint::receipt(&fs, &receipt).unwrap();
    assert_eq!(decoded.billing, BillingDisposition::UnknownReserved);
    job.settle(&r).unwrap();
    assert_eq!(
        job.inspect().unwrap().budget.unknown_attempts,
        vec![r.attempt_id]
    );
}
#[test]
fn delayed_reservations_cannot_burst_actual_send_rate() {
    let (_t, _fs, job, spec, clock) = fixture(2, |s| {
        s.limits.requests_per_minute = Some(1);
        s.limits.concurrency = 2;
    });
    let a = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    clock.0.store(
        spec.created_at_utc_ms + 61000,
        std::sync::atomic::Ordering::SeqCst,
    );
    let b = job
        .reserve(&spec.tasks[1].key, bound(&spec.tasks[1]))
        .unwrap();
    clock.0.store(
        spec.created_at_utc_ms + 122000,
        std::sync::atomic::Ordering::SeqCst,
    );
    let p1 = job.dispatch_intent(a).unwrap();
    let p2 = job.dispatch_intent(b).unwrap();
    job.begin_send(p1).unwrap();
    assert_eq!(
        job.begin_send(p2).err().unwrap().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(job.inspect().unwrap().budget.dispatched_requests, 2);
    assert_eq!(job.inspect().unwrap().budget.remote_inflight, 2);
}
#[test]
fn replay_each_spool_output_receipt_settlement_boundary() {
    for point in [
        LedgerCheckpoint::AfterSpoolBytesSync,
        LedgerCheckpoint::AfterSpoolMetadataSync,
        LedgerCheckpoint::BeforeReceived,
        LedgerCheckpoint::AfterReceived,
        LedgerCheckpoint::BeforeOutputsCommitted,
        LedgerCheckpoint::AfterOutputsCommitted,
        LedgerCheckpoint::BeforeSettlement,
        LedgerCheckpoint::AfterSettlement,
        LedgerCheckpoint::BeforeSpoolRemove,
        LedgerCheckpoint::AfterSpoolRemove,
    ] {
        let (_t, fs, job, spec, clock) = fixture(1, |_| {});
        let r = authorize(&job, &spec.tasks[0]);
        let fault = fault_job(&fs, &spec, &clock, point);
        if matches!(
            point,
            LedgerCheckpoint::AfterSpoolBytesSync
                | LedgerCheckpoint::AfterSpoolMetadataSync
                | LedgerCheckpoint::BeforeReceived
                | LedgerCheckpoint::AfterReceived
        ) {
            assert!(fault.record_response(&r, response()).is_err());
            job.replay().unwrap();
            job.record_response(&r, response()).unwrap();
        } else {
            job.record_response(&r, response()).unwrap();
        }
        let plan = job.materialization_plan(&r).unwrap();
        let (prepared, receipt) = apply_receipt_plan(&fs, plan);
        if matches!(
            point,
            LedgerCheckpoint::BeforeOutputsCommitted | LedgerCheckpoint::AfterOutputsCommitted
        ) {
            assert!(
                fault
                    .outputs_committed(&r, &prepared, receipt.clone(), vec![], vec![])
                    .is_err()
            );
            job.replay().unwrap();
        }
        job.outputs_committed(&r, &prepared, receipt, vec![], vec![])
            .unwrap();
        if matches!(
            point,
            LedgerCheckpoint::BeforeSettlement | LedgerCheckpoint::AfterSettlement
        ) {
            assert!(fault.settle(&r).is_err());
        }
        job.settle(&r).unwrap();
        if matches!(
            point,
            LedgerCheckpoint::BeforeSpoolRemove | LedgerCheckpoint::AfterSpoolRemove
        ) {
            assert!(fault.remove_spool_after_verified_commit(&r).is_err());
        }
        job.remove_spool_after_verified_commit(&r).unwrap();
        let a = job.replay().unwrap();
        let b = job.replay().unwrap();
        assert_eq!(a.inspection, b.inspection, "{point:?}");
        assert_eq!(a.inspection.attempts.len(), 1);
        assert_eq!(a.inspection.budget.settled.requests, 1);
        assert_eq!(a.inspection.attempts[0].phase, AttemptPhase::Settled);
        assert!(a.inspection.attempts[0].spool.is_none());
        assert!(a.conflicting_paths.is_empty());
    }
}
#[test]
fn full_task_checkpoint_summary_is_bounded_and_exact() {
    let keys = (0..4096)
        .map(|n| hash(&format!("completed-{n}")))
        .collect::<Vec<_>>();
    let summary = events::completed_task_summary(keys.clone()).unwrap();
    assert_eq!(summary.count, 4096);
    let event = events::new_event(
        &id("run_test"),
        None,
        1_700_000_000_000,
        EventPayload::Checkpoint {
            completed_tasks: summary.clone(),
            frontier: None,
            run_note: DurableOutputRef {
                record: RecordRef {
                    vault_id: id("vault_test"),
                    record_id: id("run_test"),
                    expected_kind: RecordKind::Run,
                },
                path: rel("runs/run_test/run.md"),
                hash: hash("canonical"),
            },
        },
    )
    .unwrap();
    assert!(events::encode(&event).unwrap().1.len() < 1024);
    let mut reversed = keys;
    reversed.reverse();
    assert_eq!(events::completed_task_summary(reversed).unwrap(), summary);
    let (_t, fs, job, _spec, _) = fixture(1, |_| {});
    let draft = job.checkpoint_plan().unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let prepared = engine.prepare(&writer, draft).unwrap().prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(fs.clone(), id("vault_test")),
        )
        .unwrap();
    drop(writer);
    job.checkpoint_committed(&prepared).unwrap();
    job.with(true, |_g, l| {
        let EventPayload::Checkpoint {
            completed_tasks,
            run_note,
            ..
        } = &l.frames.last().unwrap().event.payload
        else {
            panic!()
        };
        let mut wrong = completed_tasks.clone();
        wrong.count += 1;
        assert!(checkpoint::verify_summary_proof(&fs, run_note, &wrong, &l.frames).is_err());
        Ok(())
    })
    .unwrap();
}
#[cfg(unix)]
struct KillAt {
    point: LedgerCheckpoint,
    ready: std::path::PathBuf,
}
#[cfg(unix)]
impl LedgerFault for KillAt {
    fn check(&self, p: LedgerCheckpoint) -> Result<()> {
        if p == self.point {
            std::fs::write(&self.ready, b"ready").unwrap();
            loop {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Ok(())
    }
}
#[cfg(unix)]
#[test]
fn native_sigkill_each_ledger_lifecycle_boundary() {
    use std::process::Command;
    for point in [
        LedgerCheckpoint::BeforeAppend,
        LedgerCheckpoint::AfterAppend,
        LedgerCheckpoint::AfterJournalSync,
        LedgerCheckpoint::AfterSpoolBytesSync,
        LedgerCheckpoint::AfterSpoolMetadataSync,
        LedgerCheckpoint::BeforeReceived,
        LedgerCheckpoint::AfterReceived,
        LedgerCheckpoint::BeforeOutputsCommitted,
        LedgerCheckpoint::AfterOutputsCommitted,
        LedgerCheckpoint::BeforeSettlement,
        LedgerCheckpoint::AfterSettlement,
        LedgerCheckpoint::BeforeSpoolRemove,
        LedgerCheckpoint::AfterSpoolRemove,
    ] {
        let (t, fs, job, _spec, _) = fixture(1, |_| {});
        let ready = t.path().join("kill-ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "jobs::accounting_tests::ledger_crash_child",
                "--nocapture",
            ])
            .env("LWIKI_P15_ROOT", t.path())
            .env("LWIKI_P15_POINT", format!("{point:?}"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        while !ready.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "child failed before {point:?}"
            );
            if started.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                panic!("child did not reach {point:?}")
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            Command::new("kill")
                .args(["-KILL", &child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert!(!child.wait().unwrap().success());
        let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
        let engine = ChangeEngine::new(fs.clone()).unwrap();
        engine
            .recover(
                &writer,
                &CatalogGraphValidator,
                &Catalog::new(fs.clone(), id("vault_test")),
            )
            .unwrap();
        drop(writer);
        let first = job.replay().unwrap();
        let second = job.replay().unwrap();
        assert_eq!(first.inspection, second.inspection, "{point:?}");
        assert_eq!(
            first.inspection.completeness,
            AccountingCompleteness::CompleteHistory
        );
        assert!(first.inspection.attempts.len() <= 1);
        if let Some(a) = first.inspection.attempts.first() {
            assert!(
                a.billing == BillingDisposition::KnownSettled
                    || first.inspection.budget.outstanding.requests == 1
            );
            if a.phase == AttemptPhase::DispatchIntent {
                assert_eq!(a.remote_exposure, RemoteExposure::PossiblyInFlight);
            }
            if let Some(receipt) = &a.receipt {
                assert!(fs.root().resolve(&receipt.path).unwrap().exists());
            }
        }
    }
}
#[cfg(unix)]
#[test]
#[ignore = "explicit native interruption child"]
fn ledger_crash_child() {
    let root = std::env::var("LWIKI_P15_ROOT").unwrap();
    let name = std::env::var("LWIKI_P15_POINT").unwrap();
    let point = [
        LedgerCheckpoint::BeforeAppend,
        LedgerCheckpoint::AfterAppend,
        LedgerCheckpoint::AfterJournalSync,
        LedgerCheckpoint::AfterSpoolBytesSync,
        LedgerCheckpoint::AfterSpoolMetadataSync,
        LedgerCheckpoint::BeforeReceived,
        LedgerCheckpoint::AfterReceived,
        LedgerCheckpoint::BeforeOutputsCommitted,
        LedgerCheckpoint::AfterOutputsCommitted,
        LedgerCheckpoint::BeforeSettlement,
        LedgerCheckpoint::AfterSettlement,
        LedgerCheckpoint::BeforeSpoolRemove,
        LedgerCheckpoint::AfterSpoolRemove,
    ]
    .into_iter()
    .find(|p| format!("{p:?}") == name)
    .unwrap();
    let fs = crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&root).unwrap());
    let clock = Arc::new(Clock(std::sync::atomic::AtomicI64::new(1_700_000_000_000)));
    let mut opts = options(clock);
    opts.fault = Some(Arc::new(KillAt {
        point,
        ready: std::path::PathBuf::from(root).join("kill-ready"),
    }));
    let job = JobLedger::new(fs.clone(), id("vault_test"), id("run_test"), opts).unwrap();
    let task = job.inspect().unwrap().spec.tasks[0].clone();
    let r = authorize(&job, &task);
    job.record_response(&r, response()).unwrap();
    canonical_receipt(&job, &fs, &r);
    job.settle(&r).unwrap();
    job.remove_spool_after_verified_commit(&r).unwrap();
    panic!("child missed interruption checkpoint");
}
struct FaultIo {
    steps: std::sync::atomic::AtomicUsize,
    fail: usize,
}
impl FaultIo {
    fn step(&self) -> std::io::Result<()> {
        let n = self.steps.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if n == self.fail {
            Err(std::io::Error::other(
                "injected actual durable I/O boundary",
            ))
        } else {
            Ok(())
        }
    }
    fn call<T>(&self, f: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
        self.step()?;
        let result = f();
        self.step()?;
        result
    }
}
impl crate::vault::DurableIo for FaultIo {
    fn create_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        self.call(|| crate::vault::NativeIo.create_stage(p))
    }
    fn create_private_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        self.call(|| crate::vault::NativeIo.create_private_stage(p))
    }
    fn create_private_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        self.call(|| crate::vault::NativeIo.create_private_directory(p))
    }
    fn open_append(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        self.call(|| crate::vault::NativeIo.open_append(p))
    }
    fn truncate_file(&self, f: &std::fs::File, l: u64) -> std::io::Result<()> {
        self.call(|| crate::vault::NativeIo.truncate_file(f, l))
    }
    fn write_stage(&self, f: &mut std::fs::File, b: &[u8]) -> std::io::Result<()> {
        self.call(|| crate::vault::NativeIo.write_stage(f, b))
    }
    fn sync_file(&self, f: &std::fs::File) -> std::io::Result<()> {
        self.call(|| crate::vault::NativeIo.sync_file(f))
    }
    fn replace(&self, s: &std::path::Path, t: &std::path::Path) -> std::io::Result<()> {
        self.call(|| crate::vault::NativeIo.replace(s, t))
    }
    fn remove(&self, p: &std::path::Path) -> std::io::Result<()> {
        self.call(|| crate::vault::NativeIo.remove(p))
    }
    fn remove_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        self.call(|| crate::vault::NativeIo.remove_directory(p))
    }
    fn create_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        self.call(|| crate::vault::NativeIo.create_directory(p))
    }
    fn sync_directory(&self, p: &std::path::Path) -> std::io::Result<crate::vault::DirectorySync> {
        self.call(|| crate::vault::NativeIo.sync_directory(p))
    }
}
fn durable_workflow(job: &JobLedger, fs: &crate::vault::VaultFs, task: &TaskSpec) -> Result<()> {
    let reservation = job.reserve(&task.key, bound(task))?;
    let permit = job.dispatch_intent(reservation)?;
    let sent = job.begin_send(permit)?;
    let r = sent.attempt().clone();
    drop(sent);
    job.record_response(&r, response())?;
    let plan = job.materialization_plan(&r)?;
    let receipt_id = plan.receipt.receipt_id;
    let op = plan.draft.operations.last().unwrap();
    let receipt = DurableOutputRef {
        record: RecordRef {
            vault_id: id("vault_test"),
            record_id: receipt_id,
            expected_kind: RecordKind::RunEvent,
        },
        path: op.target.clone(),
        hash: Blake3Hash::digest(op.proposed.as_ref().unwrap()),
    };
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1))?;
    let engine = ChangeEngine::new(fs.clone())?;
    let prepared = engine.prepare(&writer, plan.draft)?.prepared;
    engine.apply(
        &writer,
        &prepared,
        &CatalogGraphValidator,
        &Catalog::new(fs.clone(), id("vault_test")),
    )?;
    drop(writer);
    job.outputs_committed(&r, &prepared, receipt, vec![], vec![])?;
    job.settle(&r)?;
    job.remove_spool_after_verified_commit(&r)?;
    Ok(())
}
#[test]
fn every_actual_ledger_io_boundary_replays_without_duplicate_charge() {
    let (_t, fs, _job, spec, clock) = fixture(1, |_| {});
    let counter = Arc::new(FaultIo {
        steps: std::sync::atomic::AtomicUsize::new(0),
        fail: usize::MAX,
    });
    let counted = crate::vault::VaultFs::with_io(fs.root().clone(), counter.clone());
    let job = JobLedger::new(counted.clone(), spec.vault_id, spec.run_id, options(clock)).unwrap();
    durable_workflow(&job, &counted, &spec.tasks[0]).unwrap();
    let boundaries = counter.steps.load(std::sync::atomic::Ordering::SeqCst);
    assert!(boundaries > 100);
    eprintln!(
        "P15 actual ledger workflow: {} durable calls / {} before-after fault points",
        boundaries / 2,
        boundaries
    );
    for fail in 1..=boundaries {
        if fail % 100 == 0 {
            eprintln!("P15 ledger native fault {fail}/{boundaries}");
        }
        let (_t, fs, _job, spec, clock) = fixture(1, |_| {});
        let io = Arc::new(FaultIo {
            steps: std::sync::atomic::AtomicUsize::new(0),
            fail,
        });
        let faulted = crate::vault::VaultFs::with_io(fs.root().clone(), io.clone());
        let job = JobLedger::new(
            faulted.clone(),
            spec.vault_id.clone(),
            spec.run_id.clone(),
            options(clock),
        )
        .unwrap();
        let _outcome = durable_workflow(&job, &faulted, &spec.tasks[0]);
        assert!(
            io.steps.load(std::sync::atomic::Ordering::SeqCst) >= fail,
            "boundary {fail} not reached"
        );
        let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
        let engine = ChangeEngine::new(fs.clone()).unwrap();
        engine
            .recover(
                &writer,
                &CatalogGraphValidator,
                &Catalog::new(fs.clone(), id("vault_test")),
            )
            .unwrap();
        drop(writer);
        let report = job.replay().unwrap();
        let again = job.replay().unwrap();
        assert_eq!(report.inspection, again.inspection, "fault {fail}");
        assert_eq!(
            report.inspection.completeness,
            AccountingCompleteness::CompleteHistory,
            "fault {fail}"
        );
        assert!(report.inspection.attempts.len() <= 1, "fault {fail}");
        if let Some(a) = report.inspection.attempts.first() {
            if a.billing == BillingDisposition::KnownSettled {
                assert_eq!(report.inspection.budget.settled.requests, 1);
            } else {
                assert_eq!(
                    report.inspection.budget.outstanding.requests, 1,
                    "fault {fail}"
                );
            }
            assert!(report.inspection.budget.dispatched_requests <= 1);
        }
    }
}

#[test]
fn canonical_run_drift_after_send_preserves_operational_paid_cost_without_overwrite() {
    let (t, fs, job, spec, _) = fixture(2, |_| {});
    let r = authorize(&job, &spec.tasks[0]);
    let path = checkpoint::run_path(&spec.run_id).unwrap();
    let foreign = b"foreign canonical run edit\n";
    std::fs::write(t.path().join(path.as_str()), foreign).unwrap();
    job.record_response(&r, response()).unwrap();
    job.reconcile(
        &r,
        true,
        KnownOrUnknown::Unknown,
        KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 65)),
        "PAID_DRIFT",
    )
    .unwrap();
    let inspection = job.inspect().unwrap();
    assert_eq!(
        inspection.completeness,
        AccountingCompleteness::CompleteHistory
    );
    assert_eq!(
        inspection.budget.known_costs[&Currency::new("USD").unwrap()],
        65
    );
    assert!(!inspection.warnings.is_empty());
    assert!(
        job.reserve(&spec.tasks[1].key, bound(&spec.tasks[1]))
            .is_err()
    );
    assert!(job.materialization_plan(&r).is_err());
    let replay = job.replay().unwrap();
    assert!(replay.conflicting_paths.contains(&path));
    assert_eq!(
        std::fs::read(t.path().join(path.as_str())).unwrap(),
        foreign
    );
    assert_eq!(
        replay.inspection.budget.known_costs[&Currency::new("USD").unwrap()],
        65
    );
    // Retention remains independently bound to the full operational genesis/head/attempt.
    job.with(false, |g, l| {
        let spool = l.state.inspection.attempts[0].spool.as_ref().unwrap();
        let body = g
            .read_spool(&r, crate::vault::operational::SpoolPart::Body, 1000)?
            .unwrap();
        assert_eq!(body.hash, spool.response.hash);
        Ok(())
    })
    .unwrap();
    let _ = fs;
}

#[test]
fn stopped_control_headroom_stays_released_for_mandatory_receipt_lifecycle() {
    let maximum = 20;
    let mut used = 10;
    let mut reserved = PersistenceAllowance {
        event_slots: 8,
        journal_bytes: 0,
    };
    assert!(events::history_capacity(
        0,
        used,
        &reserved,
        RunState::Running,
        true,
        JOURNAL_MAX_BYTES,
        maximum
    ));
    // Urgent stop spends a control slot. Subsequent paid lifecycle events consume reserved slots.
    used += 1;
    assert!(events::history_capacity(
        0,
        used,
        &reserved,
        RunState::Stopped,
        true,
        JOURNAL_MAX_BYTES,
        maximum
    ));
    for _ in 0..8 {
        used += 1;
        reserved.event_slots -= 1;
        assert!(events::history_capacity(
            0,
            used,
            &reserved,
            RunState::Stopped,
            true,
            JOURNAL_MAX_BYTES,
            maximum
        ));
        assert!(events::history_capacity(
            0,
            used,
            &reserved,
            RunState::Paused,
            false,
            JOURNAL_MAX_BYTES,
            maximum
        ));
        assert!(!events::history_capacity(
            0,
            used,
            &reserved,
            RunState::Running,
            true,
            JOURNAL_MAX_BYTES,
            maximum
        ));
    }
}

fn commit_checkpoint(
    fs: &crate::vault::VaultFs,
    job: &JobLedger,
    draft: crate::changes::ChangeDraft,
) {
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let prepared = engine.prepare(&writer, draft).unwrap().prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(fs.clone(), id("vault_test")),
        )
        .unwrap();
    drop(writer);
    job.checkpoint_committed(&prepared).unwrap();
}
#[test]
fn bounded_checkpoint_batches_advance_exact_prefix_without_resetting_lifetime_counts() {
    let (_t, fs, job, spec, _) = fixture(1, |_| {});
    let r = authorize(&job, &spec.tasks[0]);
    let target = job.inspect().unwrap().last_event.unwrap().sequence;
    let before = job.inspect().unwrap().budget.dispatched_requests;
    let first = job
        .with(true, |_, l| {
            checkpoint::checkpoint_plan_bounded(&fs, l, 1, 1024 * 1024)
        })
        .unwrap();
    let first_run = first
        .operations
        .iter()
        .find(|op| op.target == checkpoint::run_path(&spec.run_id).unwrap())
        .unwrap();
    let first_through = checkpoint::decode_run(first_run.proposed.as_ref().unwrap())
        .unwrap()
        .checkpoint
        .unwrap()
        .sequence;
    assert!(first_through < target);
    assert_eq!(first.operations.len(), 2);
    commit_checkpoint(&fs, &job, first);
    let second = job
        .with(true, |_, l| {
            checkpoint::checkpoint_plan_bounded(&fs, l, 256, 1024 * 1024)
        })
        .unwrap();
    let second_run = second
        .operations
        .iter()
        .find(|op| op.target == checkpoint::run_path(&spec.run_id).unwrap())
        .unwrap();
    assert!(
        checkpoint::decode_run(second_run.proposed.as_ref().unwrap())
            .unwrap()
            .checkpoint
            .unwrap()
            .sequence
            >= target
    );
    commit_checkpoint(&fs, &job, second);
    assert_eq!(job.inspect().unwrap().budget.dispatched_requests, before);
    assert_eq!(job.replay().unwrap().inspection.attempts[0].attempt, r);
}

fn fence_json(bytes: &[u8], fence: &str) -> serde_json::Value {
    let text = std::str::from_utf8(bytes).unwrap();
    let json = text
        .split(&format!("```{fence}\n"))
        .nth(1)
        .unwrap()
        .split("\n```")
        .next()
        .unwrap();
    serde_json::from_str(json).unwrap()
}
#[test]
fn produced_canonical_run_events_and_receipts_obey_authoritative_strict_schemas() {
    let (_t, fs, job, spec, _) = fixture(1, |_| {});
    let r = authorize(&job, &spec.tasks[0]);
    job.record_response(&r, response()).unwrap();
    let receipt = canonical_receipt(&job, &fs, &r);
    job.settle(&r).unwrap();
    job.remove_spool_after_verified_commit(&r).unwrap();
    commit_checkpoint(&fs, &job, job.checkpoint_plan().unwrap());
    let run_schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/run-v1.json")).unwrap();
    let event_schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/run-event-v1.json")).unwrap();
    let receipt_schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/usage-receipt-v1.json")).unwrap();
    let run_validator = jsonschema::validator_for(&run_schema).unwrap();
    let event_validator = jsonschema::validator_for(&event_schema).unwrap();
    let receipt_validator = jsonschema::validator_for(&receipt_schema).unwrap();
    let run = fence_json(
        &ledger::read(&fs, &checkpoint::run_path(&spec.run_id).unwrap())
            .unwrap()
            .unwrap(),
        "lwiki.run-plan.v1",
    );
    run_validator.validate(&run).unwrap();
    for key in ["version", "spec_hash", "unknown"] {
        let mut bad = run.clone();
        bad[key] = serde_json::json!("wrong");
        assert!(!run_validator.is_valid(&bad));
    }
    let usage = fence_json(
        &ledger::read(&fs, &receipt.path).unwrap().unwrap(),
        "lwiki.run-event.v1",
    );
    receipt_validator.validate(&usage).unwrap();
    let mut bad_usage = usage.clone();
    bad_usage["receipt"]["computed_cost"] = serde_json::json!({"state":"unknown", "value":123});
    assert!(!receipt_validator.is_valid(&bad_usage));
    job.with(true, |_, l| {
        for frame in &l.frames {
            let value = fence_json(
                &checkpoint::event_bytes(&frame.event)?,
                "lwiki.run-event.v1",
            );
            event_validator.validate(&value).unwrap();
            let mut bad = value.clone();
            bad["event"]["unexpected"] = true.into();
            assert!(!event_validator.is_valid(&bad));
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn historical_checkpoint_proof_loss_cannot_block_post_send_paid_retention() {
    let (t, fs, job, spec, _) = fixture(2, |_| {});
    commit_checkpoint(&fs, &job, job.checkpoint_plan().unwrap());
    let r = authorize(&job, &spec.tasks[0]);
    let path = checkpoint::run_path(&spec.run_id).unwrap();
    std::fs::remove_file(t.path().join(path.as_str())).unwrap();
    std::fs::remove_dir_all(t.path().join("changes")).unwrap();
    job.record_response(&r, response()).unwrap();
    job.reconcile(
        &r,
        true,
        KnownOrUnknown::Unknown,
        KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 75)),
        "PAID_PROOF_LOST",
    )
    .unwrap();
    let inspection = job.inspect().unwrap();
    assert_eq!(
        inspection.completeness,
        AccountingCompleteness::CompleteHistory
    );
    assert_eq!(
        inspection.budget.known_costs[&Currency::new("USD").unwrap()],
        75
    );
    assert!(
        inspection
            .warnings
            .iter()
            .any(|w| w.contains("operational accounting only"))
    );
    assert!(
        job.reserve(&spec.tasks[1].key, bound(&spec.tasks[1]))
            .is_err()
    );
    assert!(job.materialization_plan(&r).is_err());
    let replay = job.replay().unwrap();
    assert!(replay.conflicting_paths.contains(&path));
    assert!(!t.path().join(path.as_str()).exists());
    job.with(false, |g, _| {
        assert_eq!(
            g.read_spool(&r, crate::vault::operational::SpoolPart::Body, 1000)?
                .unwrap()
                .bytes,
            response().bytes
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn every_bootstrap_genesis_head_and_canonical_create_io_boundary_is_retryable() {
    let (_t, fs, _job, _spec, clock) = fixture(1, |_| {});
    let spec = common::spec(&fs, "run_bootstrap", 1);
    let counter = Arc::new(FaultIo {
        steps: std::sync::atomic::AtomicUsize::new(0),
        fail: usize::MAX,
    });
    let counted = crate::vault::VaultFs::with_io(fs.root().clone(), counter.clone());
    let job = JobLedger::new(
        counted.clone(),
        spec.vault_id.clone(),
        spec.run_id.clone(),
        options(clock),
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    job.create(&writer, spec).unwrap();
    drop(writer);
    let boundaries = counter.steps.load(std::sync::atomic::Ordering::SeqCst);
    assert!(boundaries > 50);
    eprintln!(
        "P15 actual bootstrap workflow: {} durable calls / {} before-after fault points",
        boundaries / 2,
        boundaries
    );
    for fail in 1..=boundaries {
        if fail % 100 == 0 {
            eprintln!("P15 bootstrap native fault {fail}/{boundaries}");
        }
        let (_t, fs, _job, _spec, clock) = fixture(1, |_| {});
        let spec = common::spec(&fs, "run_bootstrap", 1);
        let io = Arc::new(FaultIo {
            steps: std::sync::atomic::AtomicUsize::new(0),
            fail,
        });
        let faulted = crate::vault::VaultFs::with_io(fs.root().clone(), io.clone());
        let job = JobLedger::new(
            faulted,
            spec.vault_id.clone(),
            spec.run_id.clone(),
            options(clock),
        )
        .unwrap();
        let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
        let _ = job.create(&writer, spec.clone());
        assert!(
            io.steps.load(std::sync::atomic::Ordering::SeqCst) >= fail,
            "bootstrap fault {fail} not reached"
        );
        ChangeEngine::new(fs.clone())
            .unwrap()
            .recover(
                &writer,
                &CatalogGraphValidator,
                &Catalog::new(fs.clone(), id("vault_test")),
            )
            .unwrap();
        let created = job
            .create(&writer, spec)
            .unwrap_or_else(|e| panic!("bootstrap fault {fail} retry failed: {e:?}"));
        drop(writer);
        assert_eq!(created.state, RunState::Planned, "bootstrap fault {fail}");
        let replay = job.replay().unwrap();
        assert_eq!(
            replay.inspection.completeness,
            AccountingCompleteness::CompleteHistory
        );
        assert!(replay.inspection.attempts.is_empty());
        job.with(true, |_, loaded| {
            assert_eq!(
                loaded
                    .frames
                    .iter()
                    .filter(|f| matches!(f.event.payload, EventPayload::Genesis { .. }))
                    .count(),
                1
            );
            Ok(())
        })
        .unwrap();
    }
}

#[test]
fn oversized_paid_body_records_actual_cost_and_breach_without_spooling() {
    let (_t, _fs, job, spec, _) = fixture(2, |_| {});
    let r = authorize(&job, &spec.tasks[0]);
    let mut paid = response();
    paid.bytes = vec![b'x'; 1001];
    paid.metadata.usage = KnownOrUnknown::Unknown;
    assert_eq!(
        job.record_response(&r, paid).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    let inspection = job.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Paused);
    assert!(!inspection.budget.guarantee_intact);
    assert_eq!(
        inspection.budget.known_costs[&Currency::new("USD").unwrap()],
        60
    );
    assert_eq!(inspection.budget.outstanding.response_bytes, 1001);
    assert!(inspection.attempts[0].spool.is_none());
    assert_eq!(
        inspection.budget.unknown_attempts,
        vec![r.attempt_id.clone()]
    );
    assert!(
        job.reserve(&spec.tasks[1].key, bound(&spec.tasks[1]))
            .is_err()
    );
    job.with(false, |g, _| {
        assert!(
            g.read_spool(&r, crate::vault::operational::SpoolPart::Body, 1001)?
                .is_none()
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn stopped_run_finishes_only_already_planned_ready_local_partial_work() {
    let (_t, _fs, job, spec, _) = fixture(1, |s| {
        s.tasks[0].capability = None;
        s.tasks[0].key = crate::jobs::tasks::task_key(&s.tasks[0]).unwrap();
    });
    job.stop().unwrap();
    assert!(job.add_tasks(vec![spec.tasks[0].clone()]).is_err());
    job.finish_local_task(&spec.tasks[0].key, vec![], vec![])
        .unwrap();
    assert_eq!(
        job.inspect().unwrap().tasks[&spec.tasks[0].key].state,
        TaskState::Completed
    );
    assert_eq!(job.inspect().unwrap().budget.dispatched_requests, 0);
    assert!(
        job.reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
            .is_err()
    );
}

#[test]
fn proven_not_sent_refunds_once_but_old_authorization_and_attempt_number_cannot_reset() {
    let (_t, _fs, job, spec, _) = fixture(1, |s| s.limits.requests = 1);
    let reserved = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    let r = reserved.attempt().clone();
    let permit = job.dispatch_intent(reserved).unwrap();
    let released = job
        .release_not_sent(
            &r,
            NotSentObservation {
                reason: NotSentReason::TransportNotEntered,
            },
        )
        .unwrap();
    assert_eq!(
        job.release_not_sent(
            &r,
            NotSentObservation {
                reason: NotSentReason::TransportNotEntered
            }
        )
        .unwrap(),
        released
    );
    assert!(job.begin_send(permit).is_err());
    let inspection = job.inspect().unwrap();
    assert_eq!(inspection.budget.dispatched_requests, 0);
    assert_eq!(inspection.budget.outstanding.requests, 0);
    assert_eq!(inspection.budget.remote_inflight, 0);
    let retry = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    assert_eq!(retry.attempt().number, 2);
    assert_ne!(retry.attempt().attempt_id, r.attempt_id);
    assert_eq!(job.inspect().unwrap().budget.outstanding.requests, 1);
}

fn unknown_http_response(status: Option<u16>, terminal: bool) -> ResponseSpoolInput {
    let mut paid = response();
    paid.metadata.status_code = status;
    paid.metadata.terminal_response = terminal;
    paid.metadata.usage = KnownOrUnknown::Unknown;
    paid.metadata.computed_cost = KnownOrUnknown::Unknown;
    if let Some(status) = status.filter(|status| *status != 200) {
        paid.metadata.failure_code = Some(format!("HTTP_{status}"));
    }
    paid
}

fn reopen_job(fs: &crate::vault::VaultFs, spec: &RunSpec, clock: &Arc<Clock>) -> JobLedger {
    JobLedger::new(
        fs.clone(),
        spec.vault_id.clone(),
        spec.run_id.clone(),
        options(clock.clone()),
    )
    .unwrap()
}

#[test]
fn terminal_http_unknown_billing_retries_keep_full_charges_after_reopen() {
    for status in [401, 429, 503] {
        let (_t, fs, job, spec, clock) = fixture(1, |s| {
            s.limits.concurrency = 1;
            s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 2));
        });
        let r = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
        job.record_response(&r, unknown_http_response(Some(status), true))
            .unwrap();
        let receipt = canonical_receipt(&job, &fs, &r);
        job.settle(&r).unwrap();
        let job = reopen_job(&fs, &spec, &clock);
        let before = job.inspect().unwrap();
        assert_eq!(
            before.attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
        assert_eq!(before.attempts[0].phase, AttemptPhase::Settled);
        assert_eq!(
            before.budget.outstanding.cost.as_ref().unwrap().nanounits(),
            1
        );
        assert_eq!(before.budget.concurrency_admitted, 0);
        assert_eq!(before.budget.settled.requests, 0);
        assert!(fs.root().resolve(&receipt.path).unwrap().exists());
        let second = job
            .reserve(&spec.tasks[0].key, priced(&spec.tasks[0]))
            .unwrap();
        assert_eq!(second.attempt().number, 2);
        assert_ne!(second.attempt().attempt_id, r.attempt_id);
        let after = job.inspect().unwrap();
        assert_eq!(after.budget.dispatched_requests, 1);
        assert_eq!(after.budget.outstanding_requests, 1);
        assert_eq!(after.budget.outstanding.requests, 2);
        assert_eq!(after.budget.outstanding.request_bytes, 200);
        assert_eq!(after.budget.outstanding.response_bytes, 2000);
        assert_eq!(
            after.budget.outstanding.billable_units[&BillableClass::Reasoning],
            100
        );
        assert_eq!(
            after.budget.outstanding.cost.as_ref().unwrap().nanounits(),
            2
        );
        assert_eq!(after.budget.concurrency_admitted, 1);
        assert_eq!(after.budget.unknown_attempts, vec![r.attempt_id]);
        let second = job
            .begin_send(job.dispatch_intent(second).unwrap())
            .unwrap();
        let second = second.attempt().clone();
        job.record_response(&second, unknown_http_response(Some(503), true))
            .unwrap();
        canonical_receipt(&job, &fs, &second);
        job.settle(&second).unwrap();
        let job = reopen_job(&fs, &spec, &clock);
        assert_eq!(job.inspect().unwrap().budget.concurrency_admitted, 0);
        assert_eq!(
            job.reserve(&spec.tasks[0].key, priced(&spec.tasks[0]))
                .err()
                .unwrap()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(job.inspect().unwrap().attempts.len(), 2);
    }
}

#[test]
fn terminal_http_third_retry_needs_every_prior_status_spool() {
    for remove_first in [false, true] {
        let (_t, fs, job, spec, clock) = fixture(1, |s| {
            s.limits.concurrency = 1;
            s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 3));
        });
        let first = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
        job.record_response(&first, unknown_http_response(Some(429), true))
            .unwrap();
        canonical_receipt(&job, &fs, &first);
        job.settle(&first).unwrap();
        let second = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
        job.record_response(&second, unknown_http_response(Some(503), true))
            .unwrap();
        canonical_receipt(&job, &fs, &second);
        job.settle(&second).unwrap();
        if remove_first {
            job.remove_spool_after_verified_commit(&first).unwrap();
        }
        let job = reopen_job(&fs, &spec, &clock);
        let result = job.reserve(&spec.tasks[0].key, priced(&spec.tasks[0]));
        if remove_first {
            assert_eq!(result.err().unwrap().code, ErrorCode::RecoveryRequired);
            assert_eq!(job.inspect().unwrap().attempts.len(), 2);
        } else {
            assert_eq!(result.unwrap().attempt().number, 3);
            let inspection = job.inspect().unwrap();
            assert_eq!(inspection.budget.outstanding.requests, 3);
            assert_eq!(
                inspection
                    .budget
                    .outstanding
                    .cost
                    .as_ref()
                    .unwrap()
                    .nanounits(),
                3
            );
            assert_eq!(inspection.budget.unknown_attempts.len(), 2);
        }
    }
}

#[test]
fn terminal_http_retry_does_not_hide_an_older_possible_send() {
    let (_t, fs, job, spec, clock) = fixture(1, |_| {});
    let first = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
    job.outcome_unknown(&first, "TIMEOUT").unwrap();
    let mut explicit = options(clock.clone());
    explicit.policy.retry_uncertain = true;
    let explicit = JobLedger::new(
        fs.clone(),
        spec.vault_id.clone(),
        spec.run_id.clone(),
        explicit,
    )
    .unwrap();
    let second = authorize_bound(&explicit, &spec.tasks[0], priced(&spec.tasks[0]));
    explicit
        .record_response(&second, unknown_http_response(Some(503), true))
        .unwrap();
    canonical_receipt(&explicit, &fs, &second);
    explicit.settle(&second).unwrap();
    let job = reopen_job(&fs, &spec, &clock);
    assert_eq!(
        job.reserve(&spec.tasks[0].key, priced(&spec.tasks[0]))
            .err()
            .unwrap()
            .code,
        ErrorCode::RecoveryRequired
    );
    let inspection = job.inspect().unwrap();
    assert_eq!(inspection.attempts.len(), 2);
    assert_eq!(inspection.budget.remote_inflight, 1);
    assert_eq!(
        inspection
            .budget
            .outstanding
            .cost
            .as_ref()
            .unwrap()
            .nanounits(),
        2
    );
}

#[test]
fn terminal_http_retry_requires_received_status_not_public_reconciliation() {
    let (_t, fs, job, spec, clock) = fixture(1, |_| {});
    let first = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
    job.outcome_unknown(&first, "TIMEOUT").unwrap();
    job.reconcile(
        &first,
        true,
        KnownOrUnknown::Unknown,
        KnownOrUnknown::Unknown,
        "TERMINAL_CONFIRMED",
    )
    .unwrap();
    let job = reopen_job(&fs, &spec, &clock);
    let inspection = job.inspect().unwrap();
    assert_eq!(
        inspection.attempts[0].remote_exposure,
        RemoteExposure::TerminalConfirmed
    );
    assert_eq!(inspection.budget.concurrency_admitted, 0);
    assert_eq!(
        job.reserve(&spec.tasks[0].key, priced(&spec.tasks[0]))
            .err()
            .unwrap()
            .code,
        ErrorCode::RecoveryRequired
    );
    assert_eq!(
        job.inspect()
            .unwrap()
            .budget
            .outstanding
            .cost
            .as_ref()
            .unwrap()
            .nanounits(),
        1
    );
}

#[test]
fn terminal_http_default_retry_refuses_invalid_status_or_missing_corrupt_proof() {
    for (status, terminal) in [
        (Some(200), true),
        (Some(400), true),
        (Some(403), true),
        (None, true),
        (Some(503), false),
    ] {
        let (_t, fs, job, spec, clock) = fixture(1, |_| {});
        let first = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
        job.record_response(&first, unknown_http_response(status, terminal))
            .unwrap();
        canonical_receipt(&job, &fs, &first);
        job.settle(&first).unwrap();
        let job = reopen_job(&fs, &spec, &clock);
        assert_eq!(
            job.reserve(&spec.tasks[0].key, priced(&spec.tasks[0]))
                .err()
                .unwrap()
                .code,
            ErrorCode::RecoveryRequired
        );
        assert_eq!(
            job.inspect()
                .unwrap()
                .budget
                .outstanding
                .cost
                .as_ref()
                .unwrap()
                .nanounits(),
            1
        );
    }
    for corrupt in [false, true] {
        use crate::vault::operational::{RunFile, SpoolPart};
        let (_t, fs, job, spec, clock) = fixture(1, |_| {});
        let first = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
        job.record_response(&first, unknown_http_response(Some(503), true))
            .unwrap();
        canonical_receipt(&job, &fs, &first);
        job.settle(&first).unwrap();
        job.with(false, |g, l| {
            let spool = l.state.inspection.attempts[0].spool.as_ref().unwrap();
            if corrupt {
                let bytes =
                    serde_json::to_vec(&unknown_http_response(Some(200), true).metadata).unwrap();
                g.secure_replace(
                    RunFile::Spool {
                        attempt: &first,
                        part: SpoolPart::Metadata,
                    },
                    crate::vault::ExpectedState::Hash(spool.metadata.hash.clone()),
                    &bytes,
                )?;
            } else {
                g.remove_spool_file(&first, SpoolPart::Metadata, &spool.metadata.hash)?;
            }
            Ok(())
        })
        .unwrap();
        let job = reopen_job(&fs, &spec, &clock);
        let error = job
            .reserve(&spec.tasks[0].key, priced(&spec.tasks[0]))
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::RecoveryRequired);
    }
}

#[test]
fn wire_contract_violation_survives_received_interrupt_and_cannot_resume() {
    for unknown in [false, true] {
        for interrupt_received in [false, true] {
            let (_t, fs, job, spec, clock) = fixture(2, |s| {
                s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 2));
            });
            let r = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
            let mut paid = response();
            paid.metadata.failure_code = Some("wire_contract_violation".into());
            if unknown {
                paid.metadata.usage = KnownOrUnknown::Unknown;
                paid.metadata.computed_cost = KnownOrUnknown::Unknown;
            } else {
                // Every actual unit and the cost fits admission. The breach is
                // the sealed adapter's semantic observation, not numeric excess.
                paid.metadata.computed_cost =
                    KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 1));
            }
            let expected_metadata = paid.metadata.clone();
            if interrupt_received {
                let interrupted = fault_job(&fs, &spec, &clock, LedgerCheckpoint::AfterReceived);
                assert_eq!(
                    interrupted.record_response(&r, paid).unwrap_err().code,
                    ErrorCode::Internal
                );
            } else {
                job.record_response(&r, paid).unwrap();
            }
            let job = reopen_job(&fs, &spec, &clock);
            job.with(false, |g, l| {
                let spool = l.state.inspection.attempts[0].spool.as_ref().unwrap();
                let metadata = g.read_spool(&r, crate::vault::operational::SpoolPart::Metadata, 64 * 1024)?.unwrap();
                assert_eq!(metadata.hash, spool.metadata.hash);
                assert_eq!(metadata.bytes.len() as u64, spool.metadata.byte_len);
                assert_eq!(serde_json::from_slice::<ResponseMetadata>(&metadata.bytes).unwrap(), expected_metadata);
                assert!(l.frames.iter().any(|f| matches!(&f.event.payload, EventPayload::Received { spool } if spool.attempt == r)));
                assert_eq!(l.frames.iter().filter(|f| matches!(&f.event.payload, EventPayload::BoundViolated { attempt, .. } if attempt == &r)).count(), usize::from(!interrupt_received));
                Ok(())
            }).unwrap();
            let inspection = job.inspect().unwrap();
            assert_eq!(inspection.state, RunState::Paused);
            assert!(!inspection.budget.guarantee_intact);
            assert_eq!(inspection.attempts[0].phase, AttemptPhase::Received);
            assert_eq!(inspection.budget.dispatched_requests, 1);
            assert_eq!(inspection.budget.outstanding.requests, 1);
            assert_eq!(
                inspection
                    .budget
                    .outstanding
                    .cost
                    .as_ref()
                    .unwrap()
                    .nanounits(),
                1
            );
            assert_eq!(inspection.budget.concurrency_admitted, 0);
            let replayed = job.replay().unwrap();
            assert!(!replayed.inspection.budget.guarantee_intact);
            job.replay().unwrap();
            job.with(false, |_, l| {
                assert_eq!(l.frames.iter().filter(|f| matches!(&f.event.payload, EventPayload::BoundViolated { attempt, .. } if attempt == &r)).count(), 1);
                assert!(l.frames.iter().any(|f| matches!(&f.event.payload, EventPayload::Reconciled { attempt, .. } if attempt == &r)));
                Ok(())
            }).unwrap();
            // Paid receipt retention is still possible after authority stops.
            let receipt = canonical_receipt(&job, &fs, &r);
            let receipt_json = fence_json(
                &ledger::read(&fs, &receipt.path).unwrap().unwrap(),
                "lwiki.run-event.v1",
            );
            assert_eq!(
                receipt_json["receipt"]["failure_code"],
                "wire_contract_violation"
            );
            job.settle(&r).unwrap();
            let job = reopen_job(&fs, &spec, &clock);
            assert!(
                job.reserve(&spec.tasks[1].key, priced(&spec.tasks[1]))
                    .is_err()
            );
            assert_eq!(
                job.resume(None).unwrap_err().code,
                ErrorCode::BudgetExceeded
            );
            let mut higher_limits = spec.limits.clone();
            higher_limits.requests += 1;
            higher_limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 3));
            assert_eq!(
                job.resume(Some(LimitAmendment {
                    requested_at_utc_ms: clock.read().unwrap().utc_ms,
                    reason: "raise limits after retained wire breach".into(),
                    limits: higher_limits,
                    deadline_utc_ms: spec.deadline_utc_ms,
                }))
                .unwrap_err()
                .code,
                ErrorCode::BudgetExceeded
            );
            let inspection = job.inspect().unwrap();
            assert!(!inspection.budget.guarantee_intact);
            assert_eq!(inspection.state, RunState::Paused);
            assert_eq!(inspection.attempts.len(), 1);
            assert_eq!(inspection.budget.dispatched_requests, 1);
            if unknown {
                assert_eq!(
                    inspection.attempts[0].billing,
                    BillingDisposition::UnknownReserved
                );
                assert_eq!(
                    inspection
                        .budget
                        .outstanding
                        .cost
                        .as_ref()
                        .unwrap()
                        .nanounits(),
                    1
                );
            } else {
                assert_eq!(
                    inspection.attempts[0].billing,
                    BillingDisposition::KnownSettled
                );
                assert_eq!(
                    inspection.budget.settled.cost.as_ref().unwrap().nanounits(),
                    1
                );
            }
            assert!(
                job.reserve(&spec.tasks[1].key, priced(&spec.tasks[1]))
                    .is_err()
            );
        }
    }
}

#[test]
fn wire_contract_ordinary_paid_output_failures_do_not_invalidate_bounds() {
    for label in ["MALFORMED", "REFUSAL", "wire_contract_violation_suffix"] {
        for unknown in [false, true] {
            let (_t, fs, job, spec, clock) = fixture(2, |s| {
                s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 2));
            });
            let r = authorize_bound(&job, &spec.tasks[0], priced(&spec.tasks[0]));
            let mut paid = response();
            paid.metadata.failure_code = Some(label.into());
            if unknown {
                paid.metadata.usage = KnownOrUnknown::Unknown;
                paid.metadata.computed_cost = KnownOrUnknown::Unknown;
            } else {
                paid.metadata.computed_cost =
                    KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 1));
            }
            job.record_response(&r, paid).unwrap();
            canonical_receipt(&job, &fs, &r);
            job.settle(&r).unwrap();
            let job = reopen_job(&fs, &spec, &clock);
            let inspection = job.replay().unwrap().inspection;
            assert!(inspection.budget.guarantee_intact);
            assert_eq!(inspection.state, RunState::Running);
            job.with(false, |_, l| {
                assert!(!l.frames.iter().any(|f| matches!(&f.event.payload, EventPayload::BoundViolated { attempt, .. } if attempt == &r)));
                Ok(())
            }).unwrap();
            // A separately ready task remains admissible. Invalid knowledge did
            // not become a free response or a fabricated guarantee violation.
            job.reserve(&spec.tasks[1].key, priced(&spec.tasks[1]))
                .unwrap();
            let inspection = job.inspect().unwrap();
            assert_eq!(inspection.attempts.len(), 2);
            assert_eq!(inspection.budget.dispatched_requests, 1);
            assert_eq!(inspection.budget.outstanding_requests, 1);
            if unknown {
                assert_eq!(
                    inspection
                        .budget
                        .outstanding
                        .cost
                        .as_ref()
                        .unwrap()
                        .nanounits(),
                    2
                );
            } else {
                assert_eq!(
                    inspection.budget.settled.cost.as_ref().unwrap().nanounits(),
                    1
                );
                assert_eq!(
                    inspection
                        .budget
                        .outstanding
                        .cost
                        .as_ref()
                        .unwrap()
                        .nanounits(),
                    1
                );
            }
        }
    }
}

#[test]
fn begin_send_rejects_utc_regression_below_unpersisted_before_flush_sample() {
    use std::sync::Mutex;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Phase {
        Idle,
        BeforeFlush,
        AppendTimestamp,
        Closing,
    }
    struct State {
        phase: Phase,
        sampled: Vec<i64>,
        syncs: usize,
    }
    struct ClockAndFault {
        baseline: i64,
        state: Mutex<State>,
    }
    impl JobClock for ClockAndFault {
        fn read(&self) -> Result<ClockReading> {
            let mut s = self.state.lock().unwrap();
            let delta = match s.phase {
                Phase::Idle => 0,
                Phase::BeforeFlush => {
                    s.phase = Phase::AppendTimestamp;
                    100
                }
                Phase::AppendTimestamp => 90,
                Phase::Closing => 95,
            };
            let utc = self.baseline + delta;
            if s.phase != Phase::Idle {
                s.sampled.push(utc);
            }
            Ok(ClockReading {
                utc_ms: utc,
                // Exactly stable throughout; a monotonic-regression check
                // cannot explain the required failure.
                monotonic_ms: 10,
            })
        }
    }
    impl LedgerFault for ClockAndFault {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            let mut s = self.state.lock().unwrap();
            if point == LedgerCheckpoint::BeforeSendAuthorityFlush {
                assert!(s.phase == Phase::Idle, "send-boundary hook fired twice");
                s.phase = Phase::BeforeFlush;
            } else if point == LedgerCheckpoint::AfterJournalSync
                && s.phase == Phase::AppendTimestamp
            {
                // SendAuthorized's timestamp was sampled before its append.
                // Only now move to the closing reading. Later Pause event
                // flushes leave the clock stable at T+95.
                assert_eq!(s.sampled, vec![self.baseline + 100, self.baseline + 90]);
                s.syncs += 1;
                s.phase = Phase::Closing;
            }
            Ok(())
        }
    }

    let (_temp, fs, job, spec, fixture_clock) = fixture(1, |_| {});
    let attempt_bound = priced(&spec.tasks[0]);
    let reservation = job
        .reserve(&spec.tasks[0].key, attempt_bound.clone())
        .unwrap();
    let permit = job.dispatch_intent(reservation).unwrap();
    let attempt = permit.attempt().clone();
    let baseline = job.inspect().unwrap().utc_high_water_ms;
    assert_eq!(baseline, spec.created_at_utc_ms);

    let injected = Arc::new(ClockAndFault {
        baseline,
        state: Mutex::new(State {
            phase: Phase::Idle,
            sampled: Vec::new(),
            syncs: 0,
        }),
    });
    let mut opts = options(fixture_clock.clone());
    opts.clock = injected.clone();
    opts.fault = Some(injected.clone());
    let sender =
        JobLedger::new(fs.clone(), spec.vault_id.clone(), spec.run_id.clone(), opts).unwrap();

    // Positive control: rate interval/deadline admission at closing T+95 is
    // valid. The closing UTC exceeds persisted SendAuthorized timestamp T+90.
    super::budgets::quote_bound(
        &attempt_bound,
        &spec.limits,
        baseline + 95,
        spec.deadline_utc_ms,
    )
    .unwrap();
    let error = sender
        .begin_send(permit)
        .err()
        .expect("UTC below the unpersisted before-flush sample must withhold authority");
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    {
        let s = injected.state.lock().unwrap();
        assert_eq!(s.syncs, 1);
        assert!(s.sampled.len() >= 3);
        assert_eq!(
            &s.sampled[..3],
            &[baseline + 100, baseline + 90, baseline + 95]
        );
        assert!(s.sampled[2..].iter().all(|utc| *utc == baseline + 95));
    }

    // Inspect actual durable events through the existing private helper.
    // Send authorization was durably recorded, but no capsule escaped; its
    // uncertainty must not be refunded merely because the closing gate refused.
    sender
        .with(false, |_, loaded| {
            let sends: Vec<_> = loaded.frames.iter().filter(|f| {
                matches!(&f.event.payload, EventPayload::SendAuthorized { attempt: a } if a == &attempt)
            }).collect();
            assert_eq!(sends.len(), 1);
            assert_eq!(sends[0].event.occurred_at_utc_ms, baseline + 90);
            let prior_high = loaded.frames.iter()
                .take_while(|f| f.event.sequence < sends[0].event.sequence)
                .map(|f| f.event.occurred_at_utc_ms)
                .max().unwrap();
            assert_eq!(prior_high, baseline);
            assert!(loaded.frames.iter().any(|f| {
                f.event.occurred_at_utc_ms == baseline + 95
                    && matches!(&f.event.payload, EventPayload::RunTransition {
                        from: RunState::Running,
                        to: RunState::Paused,
                        reason: StopReason::ClockRegression,
                    })
            }));
            Ok(())
        })
        .unwrap();

    fixture_clock
        .0
        .store(baseline + 95, std::sync::atomic::Ordering::SeqCst);
    let reopened = JobLedger::new(fs, spec.vault_id, spec.run_id, options(fixture_clock)).unwrap();
    let replayed = reopened.replay().unwrap().inspection;
    assert_eq!(replayed.state, RunState::Paused);
    assert_eq!(replayed.attempts.len(), 1);
    assert_eq!(replayed.attempts[0].attempt, attempt);
    assert_eq!(replayed.attempts[0].phase, AttemptPhase::DispatchIntent);
    assert_eq!(
        replayed.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    assert_eq!(
        replayed.attempts[0].remote_exposure,
        RemoteExposure::PossiblyInFlight
    );
    assert_eq!(replayed.budget.dispatched_requests, 1);
    assert_eq!(replayed.budget.outstanding.requests, 1);
}

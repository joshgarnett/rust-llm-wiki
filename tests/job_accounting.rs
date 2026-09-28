extern crate lwiki;
#[path = "fixtures/p15/common.rs"]
mod common;
use common::*;
use lwiki::{
    domain::*,
    jobs::{self, *},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::{process::Command, sync::Arc, time::Duration};
#[test]
fn checked_money_round_up_and_unknown_cost_ceiling_rejected() {
    let usd = Currency::new("USD").unwrap();
    assert_eq!(
        Money::parse_decimal(usd.clone(), "0.000000001")
            .unwrap()
            .nanounits(),
        1
    );
    assert_eq!(
        Rate {
            price_nanounits: 1,
            per_units: std::num::NonZeroU64::new(3).unwrap()
        }
        .allowance_nanounits(1)
        .unwrap(),
        1
    );
    assert!(Money::parse_decimal(usd.clone(), "1e6").is_err());
    assert!(Money::parse_decimal(usd.clone(), "18446744074").is_err());
    assert!(Currency::new("usd").is_err());
    let (_t, _fs, job, spec, _clock) = fixture(1, |s| {
        s.limits.max_cost = Some(Money::new(usd.clone(), 1000))
    });
    let mut b = bound(&spec.tasks[0]);
    b.billable_bounds
        .insert(BillableClass::Reasoning, TokenBound::Unknown);
    b.bounds_fingerprint = jobs::budgets::bound_fingerprint(&b).unwrap();
    assert_eq!(
        job.check_bound(&spec.tasks[0].key, &b).unwrap_err().code,
        ErrorCode::CapabilityUnavailable
    );
    assert!(job.inspect().unwrap().attempts.is_empty());
}
#[test]
fn dispatch_intent_crash_keeps_unknown_charge() {
    let (_t, fs, job, spec, clock) = fixture(1, |_| {});
    let r = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    let attempt = r.attempt().clone();
    let _lost_permission = job.dispatch_intent(r).unwrap();
    drop(job);
    let restored = JobLedger::new(fs, spec.vault_id, spec.run_id, options(clock)).unwrap();
    let before = restored.replay().unwrap().inspection;
    assert_eq!(before.attempts[0].attempt, attempt);
    assert_eq!(
        before.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    assert_eq!(before.budget.remote_inflight, 1);
    assert_eq!(before.budget.outstanding_requests, 0);
    assert_eq!(before.budget.dispatched_requests, 1);
    assert_eq!(restored.replay().unwrap().inspection, before);
}
#[test]
fn markdown_restore_cannot_resume_old_hard_budget_without_accounting() {
    let (t, fs, job, spec, clock) = fixture(1, |_| {});
    let _r = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    std::fs::remove_dir_all(t.path().join(".wiki/state/jobs")).unwrap();
    let i = job.inspect().unwrap();
    assert_eq!(i.completeness, AccountingCompleteness::MarkdownOnly);
    assert!(!i.budget.guarantee_intact);
    assert!(job.resume(None).is_err());
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    assert!(job.create(&writer, spec.clone()).is_err());
    drop(writer);
    let mut next = common::spec(&fs, "run_next", 1);
    let new = JobLedger::new(
        fs.clone(),
        next.vault_id.clone(),
        next.run_id.clone(),
        options(clock),
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    assert!(new.create(&writer, next.clone()).is_err());
    next.prior_accounting = PriorAccounting::Unknown {
        prior_run_ids: vec![spec.run_id],
        reason: "Markdown-only restore; spend unknown".into(),
    };
    new.create(&writer, next).unwrap();
}
#[test]
fn valid_suffix_truncation_is_not_complete_history() {
    let (t, _fs, job, spec, _) = fixture(1, |_| {});
    let path = t.path().join(".wiki/state/jobs/run_test/journal.bin");
    let length = std::fs::metadata(&path).unwrap().len();
    let _ = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(length)
        .unwrap();
    assert_eq!(
        job.inspect().unwrap().completeness,
        AccountingCompleteness::CorruptHistory
    );
    assert!(
        job.reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
            .is_err()
    );
}
#[test]
fn two_processes_final_slot_only_one_admitted() {
    for mode in ["requests", "money", "concurrency"] {
        let (t, _fs, job, spec, _) = fixture(2, |s| match mode {
            "requests" => s.limits.requests = 1,
            "concurrency" => s.limits.concurrency = 1,
            "money" => s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 1)),
            _ => unreachable!(),
        });
        drop(job);
        let exe = std::env::current_exe().unwrap();
        let mut children = vec![];
        for n in 0..2 {
            children.push(
                Command::new(&exe)
                    .args(["--ignored", "--exact", "admission_child", "--nocapture"])
                    .env("LWIKI_P15_ROOT", t.path())
                    .env("LWIKI_P15_TASK", n.to_string())
                    .env("LWIKI_P15_MODE", mode)
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        }
        std::fs::write(t.path().join("race-start"), b"start").unwrap();
        let codes = children
            .iter_mut()
            .map(|c| c.wait().unwrap().code().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(codes.iter().filter(|c| **c == 0).count(), 1, "{mode}");
        assert_eq!(codes.iter().filter(|c| **c == 7).count(), 1, "{mode}");
        let fs = VaultFs::new(VaultRoot::explicit(t.path()).unwrap());
        let ledger = JobLedger::new(
            fs,
            spec.vault_id,
            spec.run_id,
            options(Arc::new(Clock(std::sync::atomic::AtomicI64::new(
                spec.created_at_utc_ms,
            )))),
        )
        .unwrap();
        assert_eq!(ledger.inspect().unwrap().attempts.len(), 1, "{mode}");
    }
}
#[test]
#[ignore = "explicit subprocess race helper"]
fn admission_child() {
    let path = std::env::var("LWIKI_P15_ROOT").unwrap();
    let started = std::time::Instant::now();
    while !std::path::Path::new(&path).join("race-start").exists() {
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(1));
    }
    let fs = VaultFs::new(VaultRoot::explicit(path).unwrap());
    let job = JobLedger::new(
        fs,
        id("vault_test"),
        id("run_test"),
        options(Arc::new(Clock(std::sync::atomic::AtomicI64::new(
            1_700_000_000_000,
        )))),
    )
    .unwrap();
    let i = job.inspect().unwrap();
    let n = std::env::var("LWIKI_P15_TASK")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let task = i.spec.tasks.get(n).unwrap();
    let bound = if std::env::var("LWIKI_P15_MODE").unwrap() == "money" {
        priced_bound(task)
    } else {
        bound(task)
    };
    match job.reserve(&task.key, bound) {
        Ok(_) => std::process::exit(0),
        Err(e) => std::process::exit(i32::from(e.exit_code())),
    }
}
#[test]
fn offline_dry_cancel_deadline_and_clock_keep_tree_and_allowances() {
    let (t, fs, job, spec, clock) = fixture(1, |_| {});
    let baseline = fixture_tree(t.path());
    let mut opts = options(clock.clone());
    opts.policy.offline = true;
    let offline =
        JobLedger::new(fs.clone(), spec.vault_id.clone(), spec.run_id.clone(), opts).unwrap();
    assert!(
        offline
            .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
            .is_err()
    );
    let mut opts = options(clock.clone());
    opts.policy.dry_run = true;
    let dry = JobLedger::new(fs.clone(), spec.vault_id.clone(), spec.run_id.clone(), opts).unwrap();
    assert!(dry.stop().is_err());
    dry.inspect().unwrap();
    dry.replay().unwrap();
    assert_eq!(baseline, fixture_tree(t.path()));
    let opts = options(clock.clone());
    opts.cancel.cancel();
    let cancelled =
        JobLedger::new(fs.clone(), spec.vault_id.clone(), spec.run_id.clone(), opts).unwrap();
    assert_eq!(
        cancelled
            .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
            .err()
            .unwrap()
            .code,
        ErrorCode::Cancelled
    );
    assert_eq!(baseline, fixture_tree(t.path()));
    clock.0.store(
        spec.created_at_utc_ms - 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    assert!(
        job.reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
            .is_err()
    );
    assert_eq!(job.inspect().unwrap().state, RunState::Paused);
}
#[test]
fn truncated_prior_head_requires_explicit_unknown_disclosure() {
    let (t, fs, job, spec, clock) = fixture(1, |_| {});
    let path = t.path().join(".wiki/state/jobs/run_test/journal.bin");
    let old = std::fs::metadata(&path).unwrap().len();
    job.reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(old)
        .unwrap();
    let mut next = common::spec(&fs, "run_next", 1);
    let ledger = JobLedger::new(
        fs.clone(),
        next.vault_id.clone(),
        next.run_id.clone(),
        options(clock),
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    assert_eq!(
        ledger.create(&writer, next.clone()).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    next.prior_accounting = PriorAccounting::Unknown {
        prior_run_ids: vec![spec.run_id],
        reason: "valid suffix lost beneath durable head".into(),
    };
    ledger.create(&writer, next).unwrap();
}
#[test]
fn amendments_do_not_invent_historical_class_or_rate_guarantees() {
    let (_t, _fs, job, spec, _) = fixture(1, |_| {});
    let mut b = bound(&spec.tasks[0]);
    b.billable_bounds
        .insert(BillableClass::Input, TokenBound::Unknown);
    b.bounds_fingerprint = jobs::budgets::bound_fingerprint(&b).unwrap();
    job.reserve(&spec.tasks[0].key, b).unwrap();
    job.pause(StopReason::User("test".into())).unwrap();
    let mut limits = spec.limits.clone();
    limits.billable_units.insert(BillableClass::Input, 0);
    assert!(
        job.resume(Some(LimitAmendment {
            requested_at_utc_ms: spec.created_at_utc_ms,
            reason: "not enough prior proof".into(),
            limits,
            deadline_utc_ms: spec.deadline_utc_ms
        }))
        .is_err()
    );
    let mut limits = spec.limits.clone();
    limits.requests_per_minute = Some(1);
    assert!(
        job.resume(Some(LimitAmendment {
            requested_at_utc_ms: spec.created_at_utc_ms,
            reason: "cannot invent rate history".into(),
            limits,
            deadline_utc_ms: spec.deadline_utc_ms
        }))
        .is_err()
    );
    assert_eq!(job.inspect().unwrap().attempts.len(), 1);
}

fn fixture_tree(
    root: &std::path::Path,
) -> std::collections::BTreeMap<String, (bool, Vec<u8>, std::time::SystemTime)> {
    fn walk(
        root: &std::path::Path,
        directory: &std::path::Path,
        out: &mut std::collections::BTreeMap<String, (bool, Vec<u8>, std::time::SystemTime)>,
    ) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = entry.metadata().unwrap();
            let directory = metadata.is_dir();
            out.insert(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                (
                    directory,
                    if directory {
                        vec![]
                    } else {
                        std::fs::read(&path).unwrap()
                    },
                    metadata.modified().unwrap(),
                ),
            );
            if directory {
                walk(root, &path, out);
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}
#[test]
fn expired_deadline_requires_explicit_extension_without_lifetime_reset() {
    let (_t, _fs, job, spec, clock) = fixture(1, |_| {});
    clock.0.store(
        spec.deadline_utc_ms + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    assert!(
        job.reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
            .is_err()
    );
    assert_eq!(job.inspect().unwrap().state, RunState::Paused);
    assert!(job.resume(None).is_err());
}

#[test]
fn checked_rate_snapshot_versions_classes_validity_and_overflow() {
    let (_t, _fs, job, spec, _) = fixture(1, |s| {
        s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 1000));
    });
    let mut b = priced_bound(&spec.tasks[0]);
    b.rate_card.as_mut().unwrap().version = 2;
    b.rate_card.as_mut().unwrap().fingerprint =
        jobs::budgets::rate_card_fingerprint(b.rate_card.as_ref().unwrap()).unwrap();
    b.bounds_fingerprint = jobs::budgets::bound_fingerprint(&b).unwrap();
    assert_eq!(
        job.check_bound(&spec.tasks[0].key, &b)
            .unwrap()
            .cost
            .unwrap()
            .nanounits(),
        1
    );
    let mut zero = b.clone();
    zero.rate_card.as_mut().unwrap().version = 0;
    zero.rate_card.as_mut().unwrap().fingerprint =
        jobs::budgets::rate_card_fingerprint(zero.rate_card.as_ref().unwrap()).unwrap();
    zero.bounds_fingerprint = jobs::budgets::bound_fingerprint(&zero).unwrap();
    assert!(job.check_bound(&spec.tasks[0].key, &zero).is_err());
    let mut incomplete = b.clone();
    incomplete
        .rate_card
        .as_mut()
        .unwrap()
        .rates
        .remove(&BillableClass::Reasoning);
    incomplete.rate_card.as_mut().unwrap().fingerprint =
        jobs::budgets::rate_card_fingerprint(incomplete.rate_card.as_ref().unwrap()).unwrap();
    incomplete.bounds_fingerprint = jobs::budgets::bound_fingerprint(&incomplete).unwrap();
    assert_eq!(
        job.check_bound(&spec.tasks[0].key, &incomplete)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    let mut interval = b.clone();
    interval.rate_card.as_mut().unwrap().validity = PriceValidity::EntireAttempt {
        valid_from_utc_ms: spec.created_at_utc_ms,
        valid_until_utc_ms: spec.created_at_utc_ms + 1,
    };
    interval.rate_card.as_mut().unwrap().fingerprint =
        jobs::budgets::rate_card_fingerprint(interval.rate_card.as_ref().unwrap()).unwrap();
    interval.bounds_fingerprint = jobs::budgets::bound_fingerprint(&interval).unwrap();
    assert_eq!(
        job.check_bound(&spec.tasks[0].key, &interval)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    assert!(
        Rate {
            price_nanounits: u64::MAX,
            per_units: std::num::NonZeroU64::new(1).unwrap()
        }
        .allowance_nanounits(2)
        .is_err()
    );
    assert!(
        Money::new(Currency::new("USD").unwrap(), u64::MAX)
            .checked_add(&Money::new(Currency::new("USD").unwrap(), 1))
            .is_err()
    );
    assert!(
        Money::new(Currency::new("USD").unwrap(), 1)
            .checked_add(&Money::new(Currency::new("EUR").unwrap(), 1))
            .is_err()
    );
}

#[test]
fn complete_prior_history_with_possible_paid_charge_requires_unknown_disclosure() {
    let (_t, fs, job, spec, clock) = fixture(1, |_| {});
    let reservation = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    drop(job.dispatch_intent(reservation).unwrap());
    assert_eq!(
        job.inspect().unwrap().completeness,
        AccountingCompleteness::CompleteHistory
    );
    assert_eq!(job.inspect().unwrap().budget.unknown_attempts.len(), 1);
    let mut next = common::spec(&fs, "run_next", 1);
    let new = JobLedger::new(
        fs.clone(),
        next.vault_id.clone(),
        next.run_id.clone(),
        options(clock),
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    assert_eq!(
        new.create(&writer, next.clone()).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    next.prior_accounting = PriorAccounting::Unknown {
        prior_run_ids: vec![spec.run_id],
        reason: "prior dispatch intent may have incurred unknown spend".into(),
    };
    let created = new.create(&writer, next).unwrap();
    assert_eq!(created.budget.dispatched_requests, 0);
    assert_eq!(job.inspect().unwrap().budget.dispatched_requests, 1);
}

#[test]
fn deleted_canonical_prior_run_does_not_hide_operational_unknown_spend() {
    let (t, fs, job, spec, clock) = fixture(1, |_| {});
    let reserved = job
        .reserve(&spec.tasks[0].key, bound(&spec.tasks[0]))
        .unwrap();
    drop(job.dispatch_intent(reserved).unwrap());
    std::fs::remove_file(t.path().join("runs/run_test/run.md")).unwrap();
    let mut next = common::spec(&fs, "run_next", 1);
    let new = JobLedger::new(
        fs.clone(),
        next.vault_id.clone(),
        next.run_id.clone(),
        options(clock),
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    assert_eq!(
        new.create(&writer, next.clone()).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    next.prior_accounting = PriorAccounting::Unknown {
        prior_run_ids: vec![spec.run_id],
        reason: "canonical note lost; retained operational intent has unknown spend".into(),
    };
    new.create(&writer, next).unwrap();
}

#[test]
fn malformed_or_future_owned_run_restore_requires_explicit_unknown_accounting() {
    for bytes in [b"malformed restored run\n".as_slice(), b"---\nwiki_schema: \"999\"\nwiki_id: run_test\nwiki_kind: run\ntitle: Future run\n---\nUnknown saved history.\n".as_slice()] {
        let (t, fs, _job, spec, clock) = fixture(1, |_| {});
        std::fs::remove_dir_all(t.path().join(".wiki/state/jobs/run_test")).unwrap();
        std::fs::write(t.path().join("runs/run_test/run.md"), bytes).unwrap();
        let mut next = common::spec(&fs, "run_next", 1);
        let new = JobLedger::new(fs.clone(), next.vault_id.clone(), next.run_id.clone(), options(clock)).unwrap();
        let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
        assert_eq!(new.create(&writer, next.clone()).unwrap_err().code, ErrorCode::RecoveryRequired);
        next.prior_accounting = PriorAccounting::Unknown { prior_run_ids:vec![spec.run_id], reason:"unusable owned run restore has unknown prior spend".into() };
        new.create(&writer, next).unwrap();
    }
}

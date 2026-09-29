//! Research invariant tests use disposable vaults and the real accounting journal.
use super::test_support as common;
use super::{research, tasks, types::*};
use crate::{
    domain::*,
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use common::*;
use std::{
    sync::{Arc, atomic::AtomicI64},
    time::Duration,
};

fn fixture_research(
    count: usize,
    local: bool,
) -> (tempfile::TempDir, VaultFs, JobLedger, RunSpec, Arc<Clock>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("WIKI.md"),
        b"---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Research\n---\n",
    )
    .unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(dir.path()).unwrap());
    let mut spec = spec(&fs, "run_research", count);
    std::fs::write(dir.path().join("scope.json"), b"{\"question\":\"bounded\"}").unwrap();
    let scope_hash = Blake3Hash::digest(b"{\"question\":\"bounded\"}");
    spec.scope.scope_payload_hash = Some(scope_hash.clone());
    for task in &mut spec.tasks {
        if local {
            task.capability = None;
            task.stage = TaskStage::InspectExisting;
        } else {
            task.settings_hash = Blake3Hash::digest(hash("profile").as_str());
        }
        task.key = tasks::task_key(task).unwrap();
    }
    spec.scope.research = Some(ResearchGenesisV1 {
        version: 1,
        scope: BoundedPayloadRef {
            path: rel("scope.json"),
            hash: scope_hash,
            byte_len: 22,
        },
        limits: ResearchAdmissionLimits {
            rounds: 3,
            sources: 2,
        },
        initial_binding: BindingEpochV1 {
            version: 1,
            number: 0,
            config_fingerprint: spec.config_fingerprint.clone(),
            source_snapshot: None,
            input_records: vec![],
            read_preconditions: vec![],
            services: if local {
                vec![]
            } else {
                vec![ServiceBindingV1 {
                    profile_id: "mock".into(),
                    capability: Capability::Generate,
                    profile_fingerprint: hash("profile"),
                    endpoint_fingerprint: hash("endpoint"),
                }]
            },
        },
    });
    spec.input_fingerprint = tasks::input_fingerprint(&spec).unwrap();
    let clock = Arc::new(Clock(AtomicI64::new(spec.created_at_utc_ms)));
    let job = JobLedger::new(
        fs.clone(),
        spec.vault_id.clone(),
        spec.run_id.clone(),
        options(clock.clone()),
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    job.create(&writer, spec.clone()).unwrap();
    drop(writer);
    job.start().unwrap();
    (dir, fs, job, spec, clock)
}
fn research_bound(task: &TaskSpec) -> AttemptBound {
    let mut b = bound(task);
    b.profile_fingerprint = Some(hash("profile"));
    b.bounds_fingerprint = super::budgets::bound_fingerprint(&b).unwrap();
    b
}
fn event() -> EventRef {
    EventRef {
        event_id: id("event_research"),
        sequence: 3,
        checksum: hash("event"),
    }
}
fn frontier(i: &LedgerInspection, id_name: &str, round: u32, urls: &[&str]) -> EventPayload {
    let r = i.research.as_ref().unwrap();
    EventPayload::ResearchFrontierAdmitted {
        version: 1,
        epoch: r.binding.number,
        prior_revision: r.frontier_revision,
        admission_id: hash(id_name),
        round,
        origins: urls
            .iter()
            .map(|u| ResearchOriginV1 {
                key: hash(u),
                url: (*u).into(),
                round,
            })
            .collect(),
        tasks: vec![],
        task_origins: vec![],
        parent_outputs: vec![],
    }
}
fn rebound(i: &LedgerInspection, active: Vec<Blake3Hash>) -> EventPayload {
    let r = i.research.as_ref().unwrap();
    let mut binding = r.binding.clone();
    binding.number += 1;
    EventPayload::ResearchRebound {
        version: 1,
        expected_epoch: r.binding.number,
        prior_revision: r.frontier_revision,
        amendment_id: hash(&format!("rebind-{}", r.binding.number)),
        binding,
        active_tasks: active,
        tasks: vec![],
        reason: "caller changed source".into(),
    }
}
#[test]
fn published_research_schemas_match_durable_plan_and_event_encodings() {
    let (_dir, fs, job, spec, _) = fixture_research(0, true);
    let initial = job.inspect().unwrap();
    job.admit_research_frontier(frontier(
        &initial,
        "schema-frontier",
        1,
        &["https://example.org/source"],
    ))
    .unwrap();
    let inspection = job.inspect().unwrap();
    let bytes = crate::changes::prepare::read_bounded(
        &fs,
        &super::checkpoint::run_path(&spec.run_id).unwrap(),
        crate::changes::prepare::MAX_PAYLOAD_BYTES,
    )
    .unwrap()
    .unwrap();
    let durable = super::checkpoint::decode_run(&bytes).unwrap();
    let value = serde_json::to_value(durable).unwrap();
    for text in [
        include_str!("../../schemas/run-v1.json"),
        include_str!("../../schemas/research-run-plan-v1.json"),
    ] {
        let schema: serde_json::Value = serde_json::from_str(text).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        validator.validate(&value).unwrap();
        let mut invalid = value.clone();
        invalid["research"]["binding"]["version"] = 2.into();
        assert!(!validator.is_valid(&invalid));
        let mut invalid = value.clone();
        invalid["spec"]["scope"]["research"]["invented_limit"] = 999.into();
        assert!(!validator.is_valid(&invalid));
    }
    let output = DurableOutputRef {
        record: RecordRef {
            vault_id: spec.vault_id.clone(),
            record_id: id("run_event_output"),
            expected_kind: RecordKind::RunEvent,
        },
        path: rel("runs/run_research/output.md"),
        hash: hash("output"),
    };
    let payloads = [
        frontier(&inspection, "next-frontier", 2, &[]),
        rebound(&inspection, vec![]),
        EventPayload::ResearchRoundAssessed {
            version: 1,
            epoch: 0,
            prior_revision: 1,
            assessment_id: hash("assessment"),
            round: 1,
            task_key: hash("task"),
            output,
            citations: vec![],
            support_groups: vec![],
        },
        EventPayload::ResearchRetryScheduled {
            version: 1,
            epoch: 0,
            task_key: hash("task"),
            attempt: AttemptRef {
                run_id: spec.run_id.clone(),
                task_key: hash("task"),
                attempt_id: id("attempt_schema"),
                number: 1,
                request_hash: hash("request"),
            },
            not_before_utc_ms: spec.created_at_utc_ms + 1,
        },
    ];
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/run-event-v1.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for payload in payloads {
        let event = super::events::new_event(
            &spec.run_id,
            inspection.last_event.as_ref(),
            spec.created_at_utc_ms,
            payload,
        )
        .unwrap();
        let (encoded, _) = super::events::encode(&event).unwrap();
        let value = serde_json::json!({"version": 1, "event": encoded.event});
        validator.validate(&value).unwrap();
        let mut invalid = value;
        invalid["event"]["payload"]["version"] = 2.into();
        assert!(!validator.is_valid(&invalid));
    }
}
#[test]
fn research_genesis_versions_and_legacy_absence_are_strict() {
    let (_dir, _fs, job, spec, _) = fixture_research(0, true);
    let mut legacy = spec.clone();
    legacy.scope.research = None;
    assert!(research::initial(&legacy).unwrap().is_none());
    assert!(
        !serde_json::to_value(&legacy.scope)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("research")
    );
    let mut changed = spec;
    changed.scope.research.as_mut().unwrap().version = 2;
    assert!(research::initial(&changed).is_err());
    changed.scope.research.as_mut().unwrap().version = 1;
    changed.scope.research.as_mut().unwrap().limits.rounds = 17;
    assert!(research::initial(&changed).is_err());
    let mut i = job.inspect().unwrap();
    let mut payload = frontier(&i, "unknown", 0, &[]);
    if let EventPayload::ResearchFrontierAdmitted { version, .. } = &mut payload {
        *version = 2;
    }
    assert!(research::apply(&mut i, &payload, &event()).is_err());
}
#[test]
fn research_frontier_is_exactly_idempotent_and_cas_guarded() {
    let (_dir, _fs, job, _, _) = fixture_research(0, true);
    let i = job.inspect().unwrap();
    let payload = frontier(&i, "first", 1, &["https://example.com/?q=1"]);
    let first = job.admit_research_frontier(payload.clone()).unwrap();
    assert_eq!(job.admit_research_frontier(payload.clone()).unwrap(), first);
    let mut conflict = payload;
    if let EventPayload::ResearchFrontierAdmitted { origins, .. } = &mut conflict {
        origins.clear();
    }
    assert!(job.admit_research_frontier(conflict).is_err());
    assert!(
        job.admit_research_frontier(frontier(&i, "stale", 1, &["https://example.org/"]))
            .is_err()
    );
    let i = job.inspect().unwrap();
    assert_eq!(i.research.as_ref().unwrap().origins.len(), 1);
    assert_eq!(i.research.as_ref().unwrap().rounds_started, 1);
    assert_eq!(i.budget.dispatched_requests, 0);
    assert!(job.add_tasks(vec![]).is_err());
}
#[test]
fn research_last_source_and_round_race_admits_one_revision() {
    let (_dir, fs, job, _, clock) = fixture_research(0, true);
    job.admit_research_frontier(frontier(
        &job.inspect().unwrap(),
        "initial",
        1,
        &["https://first.example/"],
    ))
    .unwrap();
    let i = job.inspect().unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = ["https://a.example/", "https://b.example/"]
        .into_iter()
        .map(|url| {
            let other = JobLedger::new(
                fs.clone(),
                i.spec.vault_id.clone(),
                i.spec.run_id.clone(),
                options(clock.clone()),
            )
            .unwrap();
            let payload = frontier(&i, url, 1, &[url]);
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                other.admit_research_frontier(payload).is_ok()
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .filter_map(|h| h.join().unwrap().then_some(()))
            .count(),
        1
    );
    let i = job.inspect().unwrap();
    assert_eq!(i.research.as_ref().unwrap().origins.len(), 2);
    assert!(
        job.admit_research_frontier(frontier(&i, "round-race", 2, &[]))
            .is_err()
    );
}
#[test]
fn research_retirement_keeps_genesis_task_definitions_and_counters() {
    let (_dir, _fs, job, spec, _) = fixture_research(2, true);
    job.pause(StopReason::User("paused".into())).unwrap();
    let before = job.inspect().unwrap();
    let old_budget = before.budget.clone();
    let mut payload = rebound(&before, vec![spec.tasks[1].key.clone()]);
    if let EventPayload::ResearchRebound { binding, .. } = &mut payload {
        binding.config_fingerprint = hash("changed-model-config");
    }
    let mut after = before.clone();
    research::apply(&mut after, &payload, &event()).unwrap();
    assert_eq!(after.spec_hash, before.spec_hash);
    assert_eq!(after.spec, before.spec);
    assert_eq!(after.budget, old_budget);
    assert_eq!(after.tasks, before.tasks);
    assert!(!research::task_is_active(&after, &spec.tasks[0].key));
    assert!(research::task_is_active(&after, &spec.tasks[1].key));
    assert_eq!(
        after.effective_deadline_utc_ms,
        before.effective_deadline_utc_ms
    );
}
#[test]
fn research_rebind_blocks_reserved_and_unknown_authority() {
    let (_dir, _fs, job, spec, _) = fixture_research(1, false);
    let reservation = job
        .reserve(&spec.tasks[0].key, research_bound(&spec.tasks[0]))
        .unwrap();
    job.pause(StopReason::User("paused".into())).unwrap();
    let mut i = job.inspect().unwrap();
    let payload = rebound(&i, vec![]);
    assert!(research::apply(&mut i, &payload, &event()).is_err());
    job.release_not_sent(
        reservation.attempt(),
        NotSentObservation {
            reason: NotSentReason::TransportNotEntered,
        },
    )
    .unwrap();
    let mut i = job.inspect().unwrap();
    let payload = rebound(&i, vec![]);
    research::apply(&mut i, &payload, &event()).unwrap();
    let a = i.attempts.last_mut().unwrap();
    a.billing = BillingDisposition::UnknownReserved;
    a.remote_exposure = RemoteExposure::PossiblyInFlight;
    let payload = rebound(&i, vec![]);
    assert!(research::apply(&mut i, &payload, &event()).is_err());
    i.attempts.last_mut().unwrap().remote_exposure = RemoteExposure::TerminalConfirmed;
    let old = i.budget.clone();
    research::apply(&mut i, &payload, &event()).unwrap();
    assert_eq!(i.budget, old);
}
#[test]
fn research_dual_role_profile_proofs_do_not_collide() {
    let (_dir, _fs, job, spec, _) = fixture_research(1, false);
    let mut i = job.inspect().unwrap();
    i.research
        .as_mut()
        .unwrap()
        .binding
        .services
        .push(ServiceBindingV1 {
            profile_id: "mock".into(),
            capability: Capability::Search,
            profile_fingerprint: hash("search-profile"),
            endpoint_fingerprint: hash("search-endpoint"),
        });
    let bound = research_bound(&spec.tasks[0]);
    assert!(research::bound_is_current(&i, &spec.tasks[0].key, &bound));
    let mut bad = bound.clone();
    bad.profile_fingerprint = Some(hash("search-profile"));
    assert!(!research::bound_is_current(&i, &spec.tasks[0].key, &bad));
    bad = bound.clone();
    bad.endpoint_fingerprint = hash("search-endpoint");
    assert!(!research::bound_is_current(&i, &spec.tasks[0].key, &bad));
    bad = bound;
    bad.capability = Capability::Search;
    assert!(!research::bound_is_current(&i, &spec.tasks[0].key, &bad));
}
#[test]
fn research_duplicate_task_requires_priority_and_dependencies_equality() {
    let (_dir, _fs, job, spec, _) = fixture_research(1, true);
    let mut i = job.inspect().unwrap();
    let mut payload = frontier(&i, "duplicate", 0, &[]);
    if let EventPayload::ResearchFrontierAdmitted { tasks, .. } = &mut payload {
        let mut t = spec.tasks[0].clone();
        t.priority += 1;
        tasks.push(t);
    }
    let prior = i.clone();
    assert!(research::apply(&mut i, &payload, &event()).is_err());
    assert_eq!(i, prior);
}
#[test]
fn research_stable_batch_preserves_the_final_request_prefix() {
    let (_dir, _fs, job, spec, _) = fixture(3, |s| {
        s.limits.requests = 1;
    });
    assert!(
        job.reserve_ready_batch(vec![(spec.tasks[1].key.clone(), bound(&spec.tasks[1]))])
            .is_err()
    );
    let wave = job
        .reserve_ready_batch(
            spec.tasks
                .iter()
                .map(|t| (t.key.clone(), priced_bound(t)))
                .collect(),
        )
        .unwrap();
    assert_eq!(wave.reservations.len(), 1);
    assert!(wave.stop.is_some());
    assert_eq!(wave.reservations[0].attempt().task_key, spec.tasks[0].key);
    assert_eq!(job.inspect().unwrap().budget.outstanding_requests, 1);
}
#[test]
fn research_scope_bytes_are_checked_and_dry_run_has_no_journal_write() {
    let (dir, fs, job, spec, clock) = fixture_research(0, true);
    let before = job.inspect().unwrap();
    let mut opt = options(clock);
    opt.policy.dry_run = true;
    let dry = JobLedger::new(fs, spec.vault_id, spec.run_id, opt).unwrap();
    assert!(
        dry.admit_research_frontier(frontier(&before, "dry", 1, &[]))
            .is_err()
    );
    assert_eq!(job.inspect().unwrap(), before);
    std::fs::write(dir.path().join("scope.json"), b"changed").unwrap();
    assert!(
        job.admit_research_frontier(frontier(&before, "changed", 1, &[]))
            .is_err()
    );
}
#[test]
fn research_origins_preserve_queries_and_reject_fragment_aliases() {
    let (_dir, _fs, job, _, _) = fixture_research(0, true);
    let mut i = job.inspect().unwrap();
    let payload = frontier(
        &i,
        "queries",
        1,
        &["https://example.com/?a=1", "https://example.com/?a=2"],
    );
    research::apply(&mut i, &payload, &event()).unwrap();
    assert_eq!(i.research.as_ref().unwrap().origins.len(), 2);
    let before = i.clone();
    let payload = frontier(&i, "fragment", 1, &["https://example.com/#fragment"]);
    assert!(research::apply(&mut i, &payload, &event()).is_err());
    assert_eq!(i, before);
}

#[test]
fn research_no_progress_and_mirrored_support_survive_epochs() {
    let (_dir, _fs, job, spec, _) = fixture_research(1, false);
    job.reserve(&spec.tasks[0].key, research_bound(&spec.tasks[0]))
        .unwrap();
    let mut i = job.inspect().unwrap();
    let key = spec.tasks[0].key.clone();
    let output = DurableOutputRef {
        record: RecordRef {
            vault_id: i.spec.vault_id.clone(),
            record_id: id("assessment_output"),
            expected_kind: RecordKind::RunEvent,
        },
        path: rel("runs/assessment.md"),
        hash: hash("output"),
    };
    // This test isolates pure replay; actual receipt/source bytes have separate
    // live-method validation, and are not represented as filesystem evidence here.
    i.tasks.get_mut(&key).unwrap().spec.stage = TaskStage::AssessGaps;
    let new_key = tasks::task_key(&i.tasks[&key].spec).unwrap();
    let mut task = i.tasks.remove(&key).unwrap();
    task.spec.key = new_key.clone();
    task.state = TaskState::Completed;
    task.outputs = vec![output.clone()];
    i.tasks.insert(new_key.clone(), task);
    i.research.as_mut().unwrap().active_tasks = [new_key.clone()].into_iter().collect();
    let a = &mut i.attempts[0];
    a.attempt.task_key = new_key.clone();
    a.phase = AttemptPhase::Settled;
    a.remote_exposure = RemoteExposure::TerminalConfirmed;
    a.outputs = vec![output.clone()];
    a.receipt = Some(output.clone());
    let quote = hash("identical mirrored bytes");
    let citations: Vec<_> = ["source_a", "source_b"]
        .into_iter()
        .map(|source| {
            CitationRef::Source(SourceSpanRef {
                source_id: id(source),
                source_revision: id("revision_1"),
                span: ByteSpan::new(0, 24).unwrap(),
                quote_hash: quote.clone(),
            })
        })
        .collect();
    for round in 1..=3 {
        let payload = frontier(&i, &format!("round-{round}"), round, &[]);
        research::apply(&mut i, &payload, &event()).unwrap();
        let r = i.research.as_ref().unwrap();
        let payload = EventPayload::ResearchRoundAssessed {
            version: 1,
            epoch: r.binding.number,
            prior_revision: r.frontier_revision,
            assessment_id: hash(&format!("assessment-{round}")),
            round,
            task_key: new_key.clone(),
            output: output.clone(),
            citations: citations.clone(),
            support_groups: vec![quote.clone()],
        };
        research::apply(&mut i, &payload, &event()).unwrap();
        if round == 2 {
            i.state = RunState::Paused;
            let payload = rebound(&i, vec![new_key.clone()]);
            research::apply(&mut i, &payload, &event()).unwrap();
            assert_eq!(i.research.as_ref().unwrap().no_progress_rounds, 1);
            i.state = RunState::Running;
        }
    }
    let r = i.research.as_ref().unwrap();
    assert_eq!(r.support_groups.len(), 1);
    assert_eq!(r.rounds[0].added_groups, 1);
    assert_eq!(r.rounds[1].added_groups, 0);
    assert_eq!(r.no_progress_rounds, 2);
    i.spec.scope.research.as_mut().unwrap().limits.rounds = 16;
    let payload = frontier(&i, "no-fourth", 4, &[]);
    assert!(research::apply(&mut i, &payload, &event()).is_err());
}
#[test]
fn research_fetch_admission_verifies_actual_descriptor_origin() {
    let (dir, _fs, job, _, _) = fixture_research(0, true);
    let input = crate::providers::types::RemoteInput {
        version: 1,
        operation: crate::providers::types::RemoteOperation::Fetch {
            url: "https://different.example/".into(),
            limits: crate::providers::public_fetch::FetchLimits::default(),
        },
    };
    let bytes = crate::graph::packet::canonical_json(&input).unwrap();
    std::fs::write(dir.path().join("capture.json"), &bytes).unwrap();
    let mut task = TaskSpec {
        key: hash("temp"),
        stage: TaskStage::Capture,
        capability: Some(Capability::Fetch),
        priority: 1,
        dependencies: vec![],
        input_hash: Blake3Hash::digest(&bytes),
        prompt_hash: None,
        schema_hash: None,
        model_hash: None,
        settings_hash: crate::providers::public_fetch::settings_fingerprint(),
        source_bindings: vec![],
        input: BoundedPayloadRef {
            path: rel("capture.json"),
            hash: Blake3Hash::digest(&bytes),
            byte_len: bytes.len() as u64,
        },
    };
    task.key = tasks::task_key(&task).unwrap();
    let i = job.inspect().unwrap();
    let mut payload = frontier(&i, "bad-origin", 1, &["https://origin.example/"]);
    if let EventPayload::ResearchFrontierAdmitted {
        tasks,
        task_origins,
        ..
    } = &mut payload
    {
        task_origins.push(ResearchTaskOriginV1 {
            task_key: task.key.clone(),
            origin_key: hash("https://origin.example/"),
            parent_capture: None,
        });
        tasks.push(task);
    }
    assert!(job.admit_research_frontier(payload).is_err());
    assert_eq!(job.inspect().unwrap(), i);
}
#[test]
fn research_batch_flush_deadline_returns_already_reserved_authority() {
    struct CrossDeadline(Arc<Clock>, i64);
    impl LedgerFault for CrossDeadline {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            if point == LedgerCheckpoint::AfterJournalSync {
                self.0.0.store(self.1, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
    }
    let (_dir, fs, job, spec, clock) = fixture(2, |_| {});
    let mut opt = options(clock.clone());
    opt.fault = Some(Arc::new(CrossDeadline(clock, spec.deadline_utc_ms)));
    let racing = JobLedger::new(fs, spec.vault_id, spec.run_id, opt).unwrap();
    let wave = racing
        .reserve_ready_batch(
            spec.tasks
                .iter()
                .map(|t| (t.key.clone(), bound(t)))
                .collect(),
        )
        .unwrap();
    assert_eq!(wave.reservations.len(), 1);
    assert!(wave.stop.is_some());
    let reservation = wave.reservations.into_iter().next().unwrap();
    assert!(racing.dispatch_intent(reservation).is_err());
    let i = job.inspect().unwrap();
    assert_eq!(i.attempts.len(), 1);
    assert_eq!(i.attempts[0].phase, AttemptPhase::Reserved);
}
#[test]
fn research_live_rebind_is_idempotent_and_replay_preserves_retired_specs() {
    let (_dir, _fs, job, spec, _) = fixture_research(2, true);
    job.pause(StopReason::User("rebind requested".into()))
        .unwrap();
    let i = job.inspect().unwrap();
    let payload = rebound(&i, vec![spec.tasks[1].key.clone()]);
    let first = job.rebind_research(payload.clone(), &[]).unwrap();
    assert_eq!(job.rebind_research(payload, &[]).unwrap(), first);
    let after = job.inspect().unwrap();
    assert_eq!(after.spec, i.spec);
    assert_eq!(after.spec_hash, i.spec_hash);
    assert_eq!(after.tasks, i.tasks);
    assert_eq!(after.attempts, i.attempts);
    assert_eq!(after.research.as_ref().unwrap().binding.number, 1);
    job.resume(None).unwrap();
    assert_eq!(
        job.ready_tasks()
            .unwrap()
            .iter()
            .map(|t| t.key.clone())
            .collect::<Vec<_>>(),
        vec![spec.tasks[1].key.clone()]
    );
}
#[test]
fn research_changed_source_retires_only_dependent_work() {
    let (dir, _fs, job, spec, _) = fixture_research(1, true);
    std::fs::write(dir.path().join("source.txt"), b"old source").unwrap();
    let mut old = spec.tasks[0].clone();
    old.source_bindings.push(crate::changes::ReadDependency {
        path: rel("source.txt"),
        expected: crate::vault::ExpectedState::Hash(hash("old source")),
    });
    old.key = tasks::task_key(&old).unwrap();
    let mut payload = frontier(&job.inspect().unwrap(), "dependent", 0, &[]);
    if let EventPayload::ResearchFrontierAdmitted { tasks, .. } = &mut payload {
        tasks.push(old.clone());
    }
    job.admit_research_frontier(payload).unwrap();
    job.pause(StopReason::InputsChanged).unwrap();
    std::fs::write(dir.path().join("source.txt"), b"edited source").unwrap();
    assert!(job.resume(None).is_err());
    let before = job.inspect().unwrap();
    let payload = rebound(&before, vec![spec.tasks[0].key.clone()]);
    job.rebind_research(payload, &[]).unwrap();
    job.resume(None).unwrap();
    assert_eq!(job.ready_tasks().unwrap().len(), 1);
    assert!(job.finish_local_task(&old.key, vec![], vec![]).is_err());
    let after = job.inspect().unwrap();
    assert_eq!(after.tasks[&old.key].spec, old);
    assert_eq!(after.spec_hash, before.spec_hash);
}
#[test]
fn research_retry_not_before_is_durable_and_cannot_skip_priority() {
    let (_dir, _fs, job, spec, clock) = fixture_research(1, false);
    let reservation = job
        .reserve(&spec.tasks[0].key, research_bound(&spec.tasks[0]))
        .unwrap();
    let attempt = reservation.attempt().clone();
    job.release_not_sent(
        &attempt,
        NotSentObservation {
            reason: NotSentReason::ConnectFailedBeforeWrite,
        },
    )
    .unwrap();
    let due = spec.created_at_utc_ms + 1000;
    let payload = EventPayload::ResearchRetryScheduled {
        version: 1,
        epoch: 0,
        task_key: spec.tasks[0].key.clone(),
        attempt,
        not_before_utc_ms: due,
    };
    let event = job.schedule_research_retry(payload.clone()).unwrap();
    assert_eq!(job.schedule_research_retry(payload).unwrap(), event);
    assert!(
        job.reserve(&spec.tasks[0].key, research_bound(&spec.tasks[0]))
            .is_err()
    );
    assert!(
        job.reserve_ready_batch(vec![(
            spec.tasks[0].key.clone(),
            research_bound(&spec.tasks[0])
        )])
        .is_err()
    );
    clock.0.store(due, std::sync::atomic::Ordering::SeqCst);
    let wave = job
        .reserve_ready_batch(vec![(
            spec.tasks[0].key.clone(),
            research_bound(&spec.tasks[0]),
        )])
        .unwrap();
    assert_eq!(wave.reservations.len(), 1);
    assert!(wave.stop.is_none());
    assert!(
        !job.inspect()
            .unwrap()
            .research
            .unwrap()
            .retry_not_before
            .contains_key(&spec.tasks[0].key)
    );
}

#[test]
fn research_individual_reserve_enforces_prefix_and_retains_postflush_token() {
    struct CrossDeadline(Arc<Clock>, i64);
    impl LedgerFault for CrossDeadline {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            if point == LedgerCheckpoint::AfterJournalSync {
                self.0.0.store(self.1, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
    }
    let (_dir, fs, job, spec, clock) = fixture_research(2, false);
    let before = job.inspect().unwrap();
    assert!(
        job.reserve(&spec.tasks[1].key, research_bound(&spec.tasks[1]))
            .is_err()
    );
    assert_eq!(job.inspect().unwrap(), before);
    let mut opt = options(clock.clone());
    opt.fault = Some(Arc::new(CrossDeadline(clock, spec.deadline_utc_ms)));
    let racing = JobLedger::new(fs, spec.vault_id, spec.run_id, opt).unwrap();
    let reservation = racing
        .reserve(&spec.tasks[0].key, research_bound(&spec.tasks[0]))
        .unwrap();
    let attempt = reservation.attempt().clone();
    assert_eq!(attempt.task_key, spec.tasks[0].key);
    assert_eq!(job.inspect().unwrap().budget.outstanding_requests, 1);
    assert!(racing.dispatch_intent(reservation).is_err());
    job.release_not_sent(
        &attempt,
        NotSentObservation {
            reason: NotSentReason::TransportNotEntered,
        },
    )
    .unwrap();
    let after = job.inspect().unwrap();
    assert_eq!(after.budget.outstanding_requests, 0);
    assert_eq!(
        after.attempts[0].billing,
        BillingDisposition::ReleasedNotSent
    );
}

#[test]
fn research_rebind_cannot_admit_new_acquisition_outside_rounds() {
    let (_dir, _fs, job, spec, _) = fixture_research(1, true);
    job.pause(StopReason::User("rebind requested".into()))
        .unwrap();
    for capability in [Capability::Search, Capability::Fetch] {
        let mut before = job.inspect().unwrap();
        // Exercise both an unused run and a no-progress stop: rebinding is not
        // a replacement for the separate frontier/round admission event.
        for stopped_for_progress in [false, true] {
            before.research.as_mut().unwrap().no_progress_rounds =
                if stopped_for_progress { 2 } else { 0 };
            let mut task = spec.tasks[0].clone();
            task.capability = Some(capability);
            task.stage = if capability == Capability::Search {
                TaskStage::Discover
            } else {
                TaskStage::Capture
            };
            task.prompt_hash = None;
            task.schema_hash = None;
            task.model_hash = None;
            task.settings_hash = if capability == Capability::Search {
                Blake3Hash::digest(hash("search-profile").as_str())
            } else {
                crate::providers::public_fetch::settings_fingerprint()
            };
            task.key = tasks::task_key(&task).unwrap();
            let mut payload = rebound(&before, vec![task.key.clone()]);
            if let EventPayload::ResearchRebound { binding, tasks, .. } = &mut payload {
                if capability == Capability::Search {
                    binding.services.push(ServiceBindingV1 {
                        profile_id: "mock".into(),
                        capability,
                        profile_fingerprint: hash("search-profile"),
                        endpoint_fingerprint: hash("search-endpoint"),
                    });
                }
                tasks.push(task);
            }
            let mut after = before.clone();
            let error = research::apply(&mut after, &payload, &event()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("new acquisition requires atomic frontier admission")
            );
            assert_eq!(after, before);
        }
    }
}

fn research_authorize(job: &JobLedger, task: &TaskSpec) -> AttemptRef {
    let reservation = job.reserve(&task.key, research_bound(task)).unwrap();
    let permit = job.dispatch_intent(reservation).unwrap();
    job.begin_send(permit).unwrap().attempt().clone()
}

fn research_settle_unknown(
    job: &JobLedger,
    fs: &VaultFs,
    attempt: &AttemptRef,
    terminal: bool,
    disposition: OutputDisposition,
) {
    job.record_response(
        attempt,
        ResponseSpoolInput {
            bytes: b"{}".to_vec(),
            metadata: ResponseMetadata {
                acquisition: None,
                provider_request_id: None,
                returned_model: None,
                status_code: terminal.then_some(200),
                terminal_response: terminal,
                usage: KnownOrUnknown::Unknown,
                computed_cost: KnownOrUnknown::Unknown,
                failure_code: (!terminal).then(|| "incomplete_response".into()),
            },
        },
    )
    .unwrap();
    let plan =
        super::checkpoint::receipt_plan(job, attempt, disposition, vec![], vec![], vec![]).unwrap();
    let receipt_id = plan.receipt.receipt_id.clone();
    let operation = plan.draft.operations.last().unwrap();
    let receipt = DurableOutputRef {
        record: RecordRef {
            vault_id: id("vault_test"),
            record_id: receipt_id,
            expected_kind: RecordKind::RunEvent,
        },
        path: operation.target.clone(),
        hash: Blake3Hash::digest(operation.proposed.as_ref().unwrap()),
    };
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    let engine = crate::changes::ChangeEngine::new(fs.clone()).unwrap();
    let prepared = engine.prepare(&writer, plan.draft).unwrap().prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &crate::catalog::CatalogGraphValidator,
            &crate::catalog::Catalog::new(fs.clone(), id("vault_test")),
        )
        .unwrap();
    drop(writer);
    job.outputs_committed(attempt, &prepared, receipt, vec![], vec![])
        .unwrap();
    job.settle(attempt).unwrap();
}

#[test]
fn research_completion_requires_terminal_history_and_preserves_unknown_holds() {
    let (_dir, fs, _, spec, clock) = fixture_research(1, false);
    let mut opt = options(clock);
    opt.policy.retry_uncertain = true;
    let job = JobLedger::new(fs.clone(), spec.vault_id, spec.run_id, opt).unwrap();
    let task = &spec.tasks[0];
    let first = research_authorize(&job, task);
    research_settle_unknown(&job, &fs, &first, false, OutputDisposition::Unknown);
    job.schedule_research_retry_after(&task.key, &first, 0)
        .unwrap();
    let second = research_authorize(&job, task);
    research_settle_unknown(&job, &fs, &second, true, OutputDisposition::Validated);
    job.finish_remote_task(&task.key, vec![], vec![], |_| Ok(false))
        .unwrap();
    let before = job.inspect().unwrap();
    assert!(
        before
            .attempts
            .iter()
            .all(|a| a.phase == AttemptPhase::Settled)
    );
    assert_eq!(before.tasks[&task.key].state, TaskState::Completed);
    assert_eq!(before.budget.remote_inflight, 1);
    assert_eq!(
        job.complete_run().unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    // The replay/append guard independently refuses a forged completion event.
    assert!(
        job.with(false, |g, loaded| job.append(
            g,
            loaded,
            EventPayload::RunTransition {
                from: RunState::Running,
                to: RunState::Completed,
                reason: StopReason::Completed
            }
        ))
        .is_err()
    );
    assert_eq!(job.inspect().unwrap(), before);
    job.reconcile(
        &first,
        true,
        KnownOrUnknown::Unknown,
        KnownOrUnknown::Unknown,
        "terminal_observed",
    )
    .unwrap();
    job.complete_run().unwrap();
    let after = job.inspect().unwrap();
    assert_eq!(after.state, RunState::Completed);
    assert_eq!(after.budget.remote_inflight, 0);
    assert_eq!(after.budget.outstanding, before.budget.outstanding);
    assert_eq!(
        after.budget.unknown_attempts,
        before.budget.unknown_attempts
    );
    assert_eq!(after.budget.dispatched_requests, 2);
}

#[test]
fn research_failed_gap_requires_terminal_settlement_without_refunding_hold() {
    let (_dir, fs, job, spec, _) = fixture_research(1, false);
    let key = &spec.tasks[0].key;
    let reservation = job.reserve(key, research_bound(&spec.tasks[0])).unwrap();
    assert!(job.fail_research_task(key, "failed_origin").is_err());
    let permit = job.dispatch_intent(reservation).unwrap();
    let attempt = job.begin_send(permit).unwrap().attempt().clone();
    research_settle_unknown(&job, &fs, &attempt, false, OutputDisposition::Unknown);
    let before = job.inspect().unwrap();
    assert!(job.fail_research_task(key, "failed_origin").is_err());
    job.reconcile(
        &attempt,
        true,
        KnownOrUnknown::Unknown,
        KnownOrUnknown::Unknown,
        "terminal_observed",
    )
    .unwrap();
    let failed = job.fail_research_task(key, "failed_origin").unwrap();
    assert_eq!(
        job.fail_research_task(key, "failed_origin").unwrap(),
        failed
    );
    assert!(job.complete_run().is_err());
    let after = job.inspect().unwrap();
    assert_eq!(after.tasks[key].state, TaskState::Failed);
    assert_eq!(after.budget.outstanding, before.budget.outstanding);
    assert_eq!(
        after.budget.unknown_attempts,
        before.budget.unknown_attempts
    );
}

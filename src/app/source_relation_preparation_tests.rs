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
    app::{OperationOptions, source_relation_inputs::tests::Fixture},
    config::providers::ProviderConfig,
    providers::{
        credentials::{CredentialBroker, CredentialOptions},
        dispatcher::Dispatcher,
        types::*,
    },
    records::parse_note,
    sources::revision::record_bytes,
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
pub(crate) struct Mock {
    pub(crate) calls: AtomicUsize,
    pub(crate) response_override: std::sync::Mutex<Option<String>>,
    invalid_span: AtomicBool,
    malformed_provider: AtomicBool,
}
impl Transport for Mock {
    fn execute<'a>(
        &'a self,
        request: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        assert_eq!(request.summary().role, ServiceRole::Generate);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text=serde_json::to_string(&json!({"schema":"lwiki.source-relation-response.v1","nodes":[{"id":"notice","kind":"rule","scope":"fictional notice","span_id":if self.invalid_span.load(Ordering::SeqCst) {"span_missing"} else {"span_000000"}}],"edges":[]})).unwrap();
        let text = self
            .response_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(text);
        let body=serde_json::to_vec(&json!({"id":"response_fictional","object":"response","status":"completed","model":"fictional-model","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}],"usage":{"input_tokens":10,"output_tokens":10,"total_tokens":20}})).unwrap();
        let body = if self.malformed_provider.load(Ordering::SeqCst) {
            b"{malformed response".to_vec()
        } else {
            body
        };
        Box::pin(async move { Ok(TransportReply::new(200, vec![], body).unwrap()) })
    }
}
pub(crate) struct Setup {
    pub(crate) source: Fixture,
    _config: tempfile::TempDir,
    pub(crate) app: OfflineApp,
    pub(crate) runtime: RemoteRuntime,
    pub(crate) mock: Arc<Mock>,
    pub(crate) inputs: Vec<(SelectedSourceBinding, BTreeMap<VaultRelativePath, Vec<u8>>)>,
}
fn selected_input(
    f: &Fixture,
    n: usize,
) -> (SelectedSourceBinding, BTreeMap<VaultRelativePath, Vec<u8>>) {
    let mut binding = f.authenticate().unwrap().selected_binding;
    let mut captured = BTreeMap::new();
    let source = RecordId::new(format!("fictional_source_{n}")).unwrap();
    let revision = RecordId::new(format!("fictional_revision_{n}")).unwrap();
    let prefix = format!("sources/{source}/revisions/{revision}");
    for (path, bytes) in &f.captured {
        let target = if path.as_str() == "WIKI.md" {
            path.clone()
        } else if path == &binding.source_path {
            VaultRelativePath::new(format!("sources/{source}/source.md")).unwrap()
        } else {
            VaultRelativePath::new(format!(
                "{prefix}/{}",
                path.as_str().rsplit('/').next().unwrap()
            ))
            .unwrap()
        };
        let bytes = if path == &binding.source_path || path == &binding.revision_path {
            let mut fields = parse_note(bytes).fields.unwrap();
            if path == &binding.source_path {
                fields.insert("wiki_id".into(), json!(source));
                fields.insert("wiki_current_revision".into(), json!(revision));
                fields.insert("wiki_revisions".into(), json!([revision]));
            } else {
                fields.insert("wiki_id".into(), json!(revision));
                fields.insert("wiki_source_id".into(), json!(source));
            }
            record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap()
        } else {
            bytes.clone()
        };
        captured.insert(target, bytes);
    }
    binding.source_id = source.clone();
    binding.revision_id = revision.clone();
    binding.source_path = VaultRelativePath::new(format!("sources/{source}/source.md")).unwrap();
    binding.revision_path = VaultRelativePath::new(format!("{prefix}/revision.md")).unwrap();
    binding.content_path = VaultRelativePath::new(format!("{prefix}/content.md")).unwrap();
    // Distinct complete Source bytes give independently missing descriptor caches.
    let mut text = captured[&binding.content_path].clone();
    text.extend_from_slice(format!("\nFictional complete Source {n}.\n").as_bytes());
    let content_hash = Blake3Hash::digest(&text);
    captured.insert(binding.content_path.clone(), text.clone());
    captured.insert(
        VaultRelativePath::new(format!("{prefix}/original.bin")).unwrap(),
        text,
    );
    let mut fields = parse_note(&captured[&binding.revision_path])
        .fields
        .unwrap();
    fields.insert("wiki_original_hash".into(), json!(content_hash));
    fields.insert("wiki_content_hash".into(), json!(content_hash));
    captured.insert(
        binding.revision_path.clone(),
        record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap(),
    );
    binding.content_hash = content_hash;
    binding.source_hash = Blake3Hash::digest(&captured[&binding.source_path]);
    binding.revision_hash = Blake3Hash::digest(&captured[&binding.revision_path]);
    (binding, captured)
}
pub(crate) fn setup(count: usize) -> Setup {
    let source = Fixture::new(false);
    let inputs: Vec<_> = (0..count).map(|n| selected_input(&source, n)).collect();
    for (_, captured) in &inputs {
        for (path, bytes) in captured {
            let p = source.fs.root().resolve(path).unwrap();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
    }
    let config = tempfile::tempdir().unwrap();
    let path = config.path().join("providers.toml");
    let root = serde_json::to_string(source.fs.root().path().to_str().unwrap()).unwrap();
    provider_fixture::private(
        &path,
        format!(
            "version=1\n[profiles.primary]\ngeneration=\"generate\"\n[services.generate]\nadapter=\"responses-v1\"\nurl=\"https://fictional.example/v1/responses\"\nmodel=\"fictional-model\"\nrevision=\"r1\"\nmax_output_tokens=4096\ntimeout_seconds=1\nconnect_timeout_seconds=1\n[services.generate.auth]\nkind=\"static\"\nkey_env=\"FICTIONAL_MOCK_ONLY\"\n[vault_bindings.main]\nroot={root}\nwiki_id=\"fictional_vault\"\nallowed_profiles=[\"primary\"]\n"
        ),
    );
    let service = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&source.fs, &source.vault, "primary", Capability::Generate)
        .unwrap();
    let clock = Arc::new(provider_fixture::Clock::new());
    let now = clock.read().unwrap().utc_ms;
    let broker = Arc::new(CredentialBroker::new(CredentialOptions {
        clock: clock.clone(),
        inputs: Arc::new(provider_fixture::Inputs(AtomicUsize::new(0))),
        runner: Arc::new(provider_fixture::Runner {
            calls: AtomicUsize::new(0),
            hook: None,
        }),
    }));
    let mock = Arc::new(Mock {
        response_override: std::sync::Mutex::new(None),
        calls: AtomicUsize::new(0),
        invalid_span: AtomicBool::new(false),
        malformed_provider: AtomicBool::new(false),
    });
    let dispatcher = Dispatcher::new(
        source.fs.clone(),
        DispatchOptions {
            broker,
            transport: mock.clone(),
            jitter: Arc::new(provider_fixture::ZeroJitter),
        },
    );
    let runtime = RemoteRuntime {
        service,
        dispatcher,
        job_options: JobOptions {
            clock,
            fault: None,
            cancel: CancellationToken::default(),
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        },
        limits: LifetimeLimits::default(),
        created_at_utc_ms: now,
        deadline_utc_ms: now + 900_000,
        requested_limits: None,
    };
    let app = OfflineApp::new(source.fs.clone(), OperationOptions::default()).unwrap();
    Setup {
        source,
        _config: config,
        app,
        runtime,
        mock,
        inputs,
    }
}
pub(crate) fn until() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn manifest(s: &Setup) -> Vec<u8> {
    plan_manifest(
        &s.source.fs,
        &s.source.vault,
        "primary",
        &s.runtime.service,
        1024,
        &s.inputs,
        until(),
    )
    .unwrap()
}
pub(crate) fn run_id() -> RecordId {
    RecordId::new("run_relations_preparation").unwrap()
}
#[test]
fn manifest_is_deterministic_complete_and_planning_has_no_side_effects() {
    let s = setup(2);
    let before = provider_fixture::tree(s.source.fs.root().path());
    let a = manifest(&s);
    let mut reversed = s.inputs.clone();
    reversed.reverse();
    let b = plan_manifest(
        &s.source.fs,
        &s.source.vault,
        "primary",
        &s.runtime.service,
        1024,
        &reversed,
        until(),
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(provider_fixture::tree(s.source.fs.root().path()), before);
    let m: SourceRelationManifest = strict_json(&a).unwrap();
    assert_eq!(m.entries.len(), 2);
    for entry in &m.entries {
        assert_eq!(entry.task.source_bindings.len(), 5);
        assert_eq!(entry.task.stage, TaskStage::PrepareSourceRelations);
    }
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn dry_run_and_new_offline_run_have_no_job_descriptor_or_provider_side_effects() {
    let mut s = setup(1);
    let bytes = manifest(&s);
    let before = provider_fixture::tree(s.source.fs.root().path());
    s.app.options.dry_run = true;
    assert!(
        s.app
            .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
            .unwrap()
            .dry_run
    );
    assert_eq!(provider_fixture::tree(s.source.fs.root().path()), before);
    s.app.options.dry_run = false;
    s.app.options.offline = true;
    assert_eq!(
        s.app
            .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
            .err()
            .unwrap()
            .code,
        ErrorCode::OfflineUnavailable
    );
    assert_eq!(provider_fixture::tree(s.source.fs.root().path()), before);
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn same_run_continues_across_partition_boundary_without_premature_completion() {
    let s = setup(17);
    s.app.index_rebuild_normalized().unwrap();
    assert!(s.app.catalog().operation_state().unwrap().is_some());
    let source_ids: Vec<_> = s
        .inputs
        .iter()
        .map(|(input, _)| input.source_id.clone())
        .collect();
    let bytes = s
        .app
        .prepare_source_relation_manifest(&source_ids, &s.runtime, 1024, until())
        .unwrap();
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 0);
    let first = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    assert_eq!(first.completed, 1);
    assert_eq!(first.pending, 16);
    assert_eq!(first.admitted_tasks, 1);
    let ledger = JobLedger::new(
        s.source.fs.clone(),
        s.source.vault.clone(),
        run_id(),
        s.runtime.job_options.clone(),
    )
    .unwrap();
    let initial = ledger.inspect().unwrap();
    assert_eq!(initial.state, RunState::Running);
    assert!(initial.spec.tasks.is_empty());
    assert!(initial.spec.scope.read_preconditions.is_empty());
    assert_eq!(
        initial.spec.scope.scope_payload_hash,
        Some(Blake3Hash::digest(&bytes))
    );
    let second = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 8)
        .unwrap();
    assert_eq!(second.completed, 9);
    assert_eq!(second.pending, 8);
    assert_eq!(second.admitted_tasks, 9);
    assert_eq!(ledger.inspect().unwrap().state, RunState::Running);
    // This functional gate covers the normalized public frontend's frozen tasks.
    // The legacy 1+16 and 1+8+8 deadline failures remain separate evidence;
    // this fixed schedule and budget do not qualify legacy or production throughput.
    let third = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 8)
        .unwrap();
    assert_eq!(third.completed, 17);
    assert_eq!(third.pending, 0);
    assert_eq!(third.admitted_tasks, 17);
    let final_state = ledger.inspect().unwrap();
    assert_eq!(final_state.state, RunState::Completed);
    assert_eq!(final_state.spec_hash, initial.spec_hash);
    assert_eq!(final_state.tasks.len(), 17);
    assert_eq!(final_state.attempts.len(), 17);
    assert_eq!(
        final_state
            .attempts
            .iter()
            .map(|attempt| &attempt.attempt.attempt_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        17
    );
    assert_eq!(final_state.budget.dispatched_requests, 17);
    assert_eq!(final_state.budget.settled.requests, 0);
    assert_eq!(final_state.budget.outstanding.requests, 17);
    assert_eq!(final_state.budget.unknown_attempts.len(), 17);
    let outputs: std::collections::BTreeSet<_> = final_state
        .attempts
        .iter()
        .flat_map(|attempt| attempt.outputs.iter().map(|output| &output.path))
        .collect();
    assert_eq!(
        outputs.len(),
        34,
        "each task retains its own raw/annotation pair"
    );
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 17);
}
#[test]
fn reformatted_manifest_refuses_exact_scope_before_resume_and_full_task_membership_refuses_priority_forgery()
 {
    let s = setup(2);
    let bytes = manifest(&s);
    s.app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    let m: SourceRelationManifest = strict_json(&bytes).unwrap();
    let reformatted = serde_json::to_vec_pretty(&m).unwrap();
    assert_eq!(
        s.app
            .run_source_relation_preparation(&reformatted, &run_id(), &s.runtime, until(), 1)
            .err()
            .unwrap()
            .code,
        ErrorCode::SourceIntegrity
    );
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
    let ledger = JobLedger::new(
        s.source.fs.clone(),
        s.source.vault.clone(),
        run_id(),
        s.runtime.job_options.clone(),
    )
    .unwrap();
    let mut state = ledger.inspect().unwrap();
    state.tasks.values_mut().next().unwrap().spec.priority = 1;
    assert_eq!(
        membership(&state, &m, &Blake3Hash::digest(&bytes), &s.runtime.service)
            .err()
            .unwrap()
            .code,
        ErrorCode::SourceIntegrity
    );
}
#[test]
fn completed_source_survives_changed_source_and_descriptor_loss_while_pending_source_remains_frozen()
 {
    let s = setup(2);
    let bytes = manifest(&s);
    s.app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    let m: SourceRelationManifest = strict_json(&bytes).unwrap();
    let completed = &m.entries[0];
    std::fs::remove_file(
        s.source
            .fs
            .root()
            .resolve(&completed.task.input.path)
            .unwrap(),
    )
    .unwrap();
    let source_path = s
        .source
        .fs
        .root()
        .resolve(&completed.source.source_path)
        .unwrap();
    let mut fields = parse_note(&std::fs::read(&source_path).unwrap())
        .fields
        .unwrap();
    fields.insert("wiki_status".into(), json!("withdrawn"));
    std::fs::write(
        source_path,
        record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap(),
    )
    .unwrap();
    let report = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    assert_eq!(report.completed, 2);
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 2);
    let s = setup(1);
    let bytes = manifest(&s);
    let mut m: SourceRelationManifest = strict_json(&bytes).unwrap();
    std::fs::write(
        s.source
            .fs
            .root()
            .resolve(&m.entries[0].source.content_path)
            .unwrap(),
        b"different content",
    )
    .unwrap();
    assert_eq!(
        reconstruct(
            &s.source.fs,
            &m.entries.remove(0),
            &strict_json::<SourceRelationManifest>(&bytes).unwrap(),
            &s.runtime.service,
            until()
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::FreshnessConflict
    );
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn duplicate_source_incomplete_oversized_and_expired_preflight_refuse_without_paid_work() {
    let s = setup(1);
    let mut duplicate = s.inputs.clone();
    duplicate.push(s.inputs[0].clone());
    assert!(
        plan_manifest(
            &s.source.fs,
            &s.source.vault,
            "primary",
            &s.runtime.service,
            1024,
            &duplicate,
            until()
        )
        .is_err()
    );
    let mut missing = s.inputs.clone();
    let content = missing[0].0.content_path.clone();
    missing[0].1.remove(&content);
    assert!(
        plan_manifest(
            &s.source.fs,
            &s.source.vault,
            "primary",
            &s.runtime.service,
            1024,
            &missing,
            until()
        )
        .is_err()
    );
    let mut large = s.inputs.clone();
    large[0]
        .1
        .insert(content, vec![b'x'; MAX_CONTENT_BYTES + 1]);
    assert_eq!(
        plan_manifest(
            &s.source.fs,
            &s.source.vault,
            "primary",
            &s.runtime.service,
            1024,
            &large,
            until()
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::BudgetExceeded
    );
    let bytes = manifest(&s);
    let before = provider_fixture::tree(s.source.fs.root().path());
    assert_eq!(
        s.app
            .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, Instant::now(), 1)
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(provider_fixture::tree(s.source.fs.root().path()), before);
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn paid_semantic_failure_keeps_diagnostic_full_spool_and_same_run_without_resend() {
    let s = setup(1);
    let bytes = manifest(&s);
    s.mock.invalid_span.store(true, Ordering::SeqCst);
    let first = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(first.completed, 0);
    assert_eq!(first.pending, 0);
    let ledger = JobLedger::new(
        s.source.fs.clone(),
        s.source.vault.clone(),
        run_id(),
        s.runtime.job_options.clone(),
    )
    .unwrap();
    let state = ledger.inspect().unwrap();
    let attempt = &state.attempts[0];
    assert_eq!(attempt.phase, AttemptPhase::Settled);
    assert!(attempt.spool.is_some());
    assert_eq!(attempt.outputs.len(), 0);
    assert_eq!(attempt.billing, BillingDisposition::UnknownReserved);
    assert!(
        state
            .budget
            .unknown_attempts
            .contains(&attempt.attempt.attempt_id)
    );
    let second = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    assert_eq!(second.failed, 1);
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
    assert!(!semantic(&WikiError::new(
        ErrorCode::BudgetExceeded,
        "expired phase"
    )));
    assert!(!semantic(&WikiError::new(
        ErrorCode::Internal,
        "temporary I/O"
    )));
}
#[test]
fn completed_offline_replay_uses_no_new_invocation_or_provider_allowance() {
    let mut s = setup(1);
    let bytes = manifest(&s);
    s.app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    let m: SourceRelationManifest = strict_json(&bytes).unwrap();
    std::fs::remove_file(
        s.source
            .fs
            .root()
            .resolve(&m.entries[0].task.input.path)
            .unwrap(),
    )
    .unwrap();
    let path = s
        .source
        .fs
        .root()
        .resolve(&m.entries[0].source.source_path)
        .unwrap();
    let mut fields = parse_note(&std::fs::read(&path).unwrap()).fields.unwrap();
    fields.insert("wiki_status".into(), json!("withdrawn"));
    std::fs::write(
        path,
        record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap(),
    )
    .unwrap();
    s.app.options.offline = true;
    // Invocation UTC expiry blocks future sends, but cannot erase authenticated
    // historical completion; local replay retains the original Run accounting.
    s.runtime.deadline_utc_ms = s.runtime.created_at_utc_ms;
    let replay = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    assert_eq!(replay.completed, 1);
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn oversized_tasks_added_partition_shrinks_with_actual_event_encoder_and_keeps_order() {
    let s = setup(1);
    let bytes = manifest(&s);
    let m: SourceRelationManifest = strict_json(&bytes).unwrap();
    let mut entries = Vec::new();
    // Pure size fixture: deliberately long guard paths, without pretending this
    // mutated task has Source/publication authority. Only frame sizing is tested.
    for index in 0..17 {
        let mut entry = m.entries[0].clone();
        for (n, dep) in entry.task.source_bindings.iter_mut().enumerate() {
            dep.path =
                VaultRelativePath::new(format!("size/{index}/{n}/{}", "p".repeat(4000))).unwrap();
        }
        entry.task.key = jobs::tasks::task_key(&entry.task).unwrap();
        entries.push(entry);
    }
    // Establish the actual encoder limit before testing adaptive shrinkage.
    assert_eq!(
        sized_event(
            &run_id(),
            s.runtime.created_at_utc_ms,
            EventPayload::TasksAdded {
                tasks: entries[..PARTITION_TASKS]
                    .iter()
                    .map(|entry| entry.task.clone())
                    .collect(),
            },
        )
        .expect_err("the full16-task frame must exceed the encoded byte ceiling")
        .code,
        ErrorCode::BudgetExceeded
    );
    let groups = partition(&entries, &run_id(), s.runtime.created_at_utc_ms).unwrap();
    assert!(groups.len() >= 2);
    assert_eq!(
        groups.iter().flatten().cloned().collect::<Vec<_>>(),
        entries.iter().map(|e| e.task.clone()).collect::<Vec<_>>()
    );
    for tasks in groups {
        assert!(tasks.len() < PARTITION_TASKS);
        assert!(
            sized_event(
                &run_id(),
                s.runtime.created_at_utc_ms,
                EventPayload::TasksAdded { tasks }
            )
            .unwrap()
                <= EVENT_MAX_BYTES
        );
    }
}

struct ReceivedCut {
    reached: Arc<AtomicBool>,
}
impl LedgerFault for ReceivedCut {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterReceived {
            self.reached.store(true, Ordering::SeqCst);
            return Err(WikiError::new(
                ErrorCode::Internal,
                "fixture crash after Received",
            ));
        }
        Ok(())
    }
}
fn inspect_ledger(s: &Setup) -> JobLedger {
    JobLedger::new(
        s.source.fs.clone(),
        s.source.vault.clone(),
        run_id(),
        s.runtime.job_options.clone(),
    )
    .unwrap()
}
#[test]
fn malformed_received_replay_rejects_through_sealed_decoder_without_resend() {
    let mut s = setup(1);
    let bytes = manifest(&s);
    s.mock.malformed_provider.store(true, Ordering::SeqCst);
    let reached = Arc::new(AtomicBool::new(false));
    s.runtime.job_options.fault = Some(Arc::new(ReceivedCut {
        reached: reached.clone(),
    }));
    assert!(
        s.app
            .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
            .is_err()
    );
    assert!(
        reached.load(Ordering::SeqCst),
        "fixture did not reach AfterReceived"
    );
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
    let state = inspect_ledger(&s).inspect().unwrap();
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.attempts[0].phase, AttemptPhase::Received);
    assert!(state.attempts[0].receipt.is_none());
    s.runtime.job_options.fault = None;
    let report = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(report.completed, 0);
    let state = inspect_ledger(&s).inspect().unwrap();
    assert_eq!(state.attempts[0].phase, AttemptPhase::Settled);
    assert!(state.attempts[0].outputs.is_empty());
    assert!(state.attempts[0].spool.is_some());
    assert_eq!(
        state.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
    assert!(!semantic(&WikiError::new(
        ErrorCode::ProviderResponse,
        "unclassified provider error"
    )));
}
#[test]
fn all_admitted_pending_descriptors_are_restored_before_active_or_paused_binding() {
    for paused in [false, true] {
        let s = setup(2);
        let bytes = manifest(&s);
        let m: SourceRelationManifest = strict_json(&bytes).unwrap();
        assert_ne!(m.entries[0].task.input.path, m.entries[1].task.input.path);
        let mut spec = tiny_spec(&m, &Blake3Hash::digest(&bytes), &run_id(), &s.runtime).unwrap();
        spec.prior_accounting = s.app.prior_accounting_for_new_run().unwrap();
        preflight_run(&s.source.fs, &spec, &m, until()).unwrap();
        for entry in &m.entries {
            let plan = reconstruct(&s.source.fs, entry, &m, &s.runtime.service, until()).unwrap();
            retain_descriptor(&s.app, &plan, until()).unwrap();
        }
        let ledger = inspect_ledger(&s);
        let writer = WriterPermit::acquire(s.source.fs.root(), Duration::from_secs(5)).unwrap();
        ledger.create(&writer, spec).unwrap();
        drop(writer);
        ledger.start().unwrap();
        ledger
            .add_tasks(m.entries.iter().map(|e| e.task.clone()).collect())
            .unwrap();
        if paused {
            ledger
                .pause(StopReason::User(
                    "fixture interruption after TasksAdded".into(),
                ))
                .unwrap();
        }
        for entry in &m.entries {
            std::fs::remove_file(s.source.fs.root().resolve(&entry.task.input.path).unwrap())
                .unwrap();
        }
        assert!(ledger.inspect().unwrap().attempts.is_empty());
        let result = s
            .app
            .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 2)
            .unwrap();
        assert_eq!(result.completed, 2);
        assert_eq!(result.failed, 0);
        assert_eq!(s.mock.calls.load(Ordering::SeqCst), 2);
        assert_eq!(ledger.inspect().unwrap().state, RunState::Completed);
    }
}
#[test]
fn mixed_failed_run_replays_completed_sibling_without_source_or_cache() {
    let mut s = setup(2);
    let bytes = manifest(&s);
    s.app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    s.mock.invalid_span.store(true, Ordering::SeqCst);
    let result = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    assert_eq!((result.completed, result.failed), (1, 1));
    assert_eq!(
        inspect_ledger(&s).inspect().unwrap().state,
        RunState::Failed
    );
    let m: SourceRelationManifest = strict_json(&bytes).unwrap();
    let completed = &m.entries[0];
    std::fs::remove_file(
        s.source
            .fs
            .root()
            .resolve(&completed.task.input.path)
            .unwrap(),
    )
    .unwrap();
    let path = s
        .source
        .fs
        .root()
        .resolve(&completed.source.source_path)
        .unwrap();
    let mut fields = parse_note(&std::fs::read(&path).unwrap()).fields.unwrap();
    fields.insert("wiki_status".into(), json!("withdrawn"));
    std::fs::write(
        path,
        record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap(),
    )
    .unwrap();
    for offline in [false, true] {
        s.app.options.offline = offline;
        let result = s
            .app
            .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 2)
            .unwrap();
        assert_eq!((result.completed, result.failed, result.pending), (1, 1, 0));
        assert_eq!(
            inspect_ledger(&s).inspect().unwrap().state,
            RunState::Failed
        );
        assert_eq!(s.mock.calls.load(Ordering::SeqCst), 2);
    }
}
#[test]
fn new_run_preserves_prior_retained_run_accounting_disclosure() {
    let s = setup(1);
    let bytes = manifest(&s);
    let prior = RecordId::new("run_fictional_prior_uncertainty").unwrap();
    let path = s
        .source
        .fs
        .root()
        .path()
        .join(format!("runs/{prior}/run.md"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        b"retained prior operational Run requiring accounting discovery",
    )
    .unwrap();
    s.app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    match inspect_ledger(&s).inspect().unwrap().spec.prior_accounting {
        PriorAccounting::Unknown { prior_run_ids, .. } => assert!(prior_run_ids.contains(&prior)),
        _ => panic!("new explicit Run lost prior retained accounting disclosure"),
    }
}

#[test]
fn corrupt_received_spool_remains_recovery_failure_with_protected_hold() {
    let mut s = setup(1);
    let bytes = manifest(&s);
    let reached = Arc::new(AtomicBool::new(false));
    s.runtime.job_options.fault = Some(Arc::new(ReceivedCut {
        reached: reached.clone(),
    }));
    assert!(
        s.app
            .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
            .is_err()
    );
    assert!(
        reached.load(Ordering::SeqCst),
        "fixture did not reach AfterReceived"
    );
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
    let state = inspect_ledger(&s).inspect().unwrap();
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.attempts[0].phase, AttemptPhase::Received);
    assert!(state.attempts[0].receipt.is_none());
    let response = &state.attempts[0].spool.as_ref().unwrap().response;
    std::fs::write(
        s.source.fs.root().resolve(&response.path).unwrap(),
        b"different retained bytes",
    )
    .unwrap();
    s.runtime.job_options.fault = None;
    assert!(
        s.app
            .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
            .is_err()
    );
    let after = inspect_ledger(&s).inspect().unwrap();
    assert_eq!(after.attempts[0].phase, AttemptPhase::Received);
    assert!(after.attempts[0].receipt.is_none());
    assert_eq!(after.budget.outstanding, state.budget.outstanding);
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn immediate_wire_invalid_response_settles_exact_named_rejection_in_normalized_vault() {
    let mut s = setup(1);
    s.app.index_rebuild_normalized().unwrap();
    assert!(s.app.catalog().operation_state().unwrap().is_some());
    let bytes = manifest(&s);
    s.mock.malformed_provider.store(true, Ordering::SeqCst);
    // No fault, manual Received event, receipt or internal trusted output:
    // ordinary execute_with_invocation traverses the sealed mock transport codec.
    let report = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    assert_eq!((report.completed, report.failed, report.pending), (0, 1, 0));
    let ledger = inspect_ledger(&s);
    let state = ledger.inspect().unwrap();
    assert_eq!(state.state, RunState::Failed);
    assert_eq!(state.attempts.len(), 1);
    let attempt = &state.attempts[0];
    assert_eq!(attempt.phase, AttemptPhase::Settled);
    assert_eq!(attempt.billing, BillingDisposition::UnknownReserved);
    assert!(attempt.outputs.is_empty());
    assert!(attempt.cache_outputs.is_empty());
    let named = attempt
        .receipt
        .as_ref()
        .expect("exact rejected receipt acknowledged");
    let receipt = jobs::checkpoint::receipt(&s.source.fs, named).unwrap();
    assert_eq!(receipt.receipt_id, named.record.record_id);
    assert_eq!(receipt.attempt, attempt.attempt);
    assert_eq!(receipt.output_disposition, OutputDisposition::Rejected);
    assert_eq!(receipt.billing, attempt.billing);
    assert!(receipt.outputs.is_empty());
    assert!(receipt.cache_outputs.is_empty());
    assert_eq!(
        state.tasks[&attempt.attempt.task_key].state,
        TaskState::Failed
    );
    assert!(
        state
            .budget
            .unknown_attempts
            .contains(&attempt.attempt.attempt_id)
    );
    assert!(state.budget.outstanding.requests > 0);
    let spool = attempt
        .spool
        .as_ref()
        .expect("rejected full response spool retained");
    for payload in [&spool.response, &spool.metadata] {
        let bytes = read_bounded(&s.source.fs, &payload.path, payload.byte_len as usize)
            .unwrap()
            .unwrap();
        assert_eq!(Blake3Hash::digest(&bytes), payload.hash);
    }
    // Completion itself proves exact committed zero-output accounting. Replay
    // must preserve the same named receipt, protected allowance and full spool.
    ledger
        .finish_rejected_remote_task(&attempt.attempt.task_key)
        .unwrap();
    for offline in [false, true] {
        s.app.options.offline = offline;
        let report = s
            .app
            .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
            .unwrap();
        assert_eq!((report.completed, report.failed, report.pending), (0, 1, 0));
        let after = inspect_ledger(&s).inspect().unwrap();
        assert_eq!(after.state, RunState::Failed);
        assert_eq!(
            after.spec.scope.scope_payload_hash,
            Some(Blake3Hash::digest(&bytes))
        );
        assert_eq!(after.attempts, state.attempts);
        assert_eq!(after.budget.outstanding, state.budget.outstanding);
        assert_eq!(after.budget.unknown_attempts, state.budget.unknown_attempts);
        assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
    }
}

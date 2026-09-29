use lwiki as library;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod provider;
use lwiki::{
    app::{OfflineApp, OperationOptions},
    config::providers::{ProviderConfig, TrustedService},
    domain::*,
    jobs::*,
    providers::{
        credentials::{CredentialBroker, CredentialOptions},
        dispatcher::Dispatcher,
        public_fetch::*,
        types::*,
    },
    research::{
        self, ResearchLimits, ResearchResumeAmendment, ResearchRuntime, ResearchScope, inspection,
        plan, runner,
    },
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{VaultFs, VaultRoot},
};
use serde_json::{Value, json};
use std::{
    net::IpAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}

type StageHook = (TaskStage, Box<dyn FnOnce() + Send>);

struct Model {
    fs: VaultFs,
    options: JobOptions,
    stages: Mutex<Vec<TaskStage>>,
    stop: bool,
    invalid_evidence: bool,
    update_pages: AtomicBool,
    cancel_stage: Mutex<Option<TaskStage>>,
    hook: Mutex<Option<StageHook>>,
}
impl Transport for Model {
    fn execute<'a>(
        &'a self,
        request: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        let summary = request.summary();
        let ledger = JobLedger::new(
            self.fs.clone(),
            id("vault_test"),
            summary.attempt.run_id.clone(),
            self.options.clone(),
        )
        .unwrap();
        let inspection = ledger.inspect().unwrap();
        let task = &inspection.tasks[&summary.attempt.task_key].spec;
        if self.cancel_stage.lock().unwrap().as_ref() == Some(&task.stage) {
            self.options.cancel.cancel();
        }
        let hook = {
            let mut hook = self.hook.lock().unwrap();
            if hook.as_ref().is_some_and(|(stage, _)| *stage == task.stage) {
                hook.take().map(|(_, hook)| hook)
            } else {
                None
            }
        };
        if let Some(hook) = hook {
            hook();
        }
        self.stages.lock().unwrap().push(task.stage);
        let bytes = std::fs::read(self.fs.root().path().join(task.input.path.as_str())).unwrap();
        let input: RemoteInput = serde_json::from_slice(&bytes).unwrap();
        let RemoteOperation::Generate { data, .. } = input.operation else {
            panic!("unexpected provider operation")
        };
        let data: Value = serde_json::from_str(&data).unwrap();
        let response = match task.stage {
            TaskStage::PlanFrontier => {
                json!({"queries":[],"urls":[],"reason":"Use caller-supplied explicit sources."})
            }
            TaskStage::Extract => {
                json!({"schema":"lwiki.extraction.v1","packet_id":data["packet_id"],"packet_fingerprint":data["packet_fingerprint"],"mentions":[],"assertions":[],"unresolved":[]})
            }
            TaskStage::AssessGaps => {
                json!({"covered_evidence_ids":if self.invalid_evidence { vec!["evidence_invented"] } else { vec![] },"gaps":["Entailment remains unassessed."],"next_queries":[],"next_urls":[],"stop":self.stop})
            }
            TaskStage::Synthesize => {
                let citations = data["binding"]["citations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .take(1)
                    .cloned()
                    .collect::<Vec<_>>();
                let proposals = if self.update_pages.load(Ordering::SeqCst) {
                    data["binding"]["current_records"].as_array().unwrap().iter()
                        .filter(|record| record["expected_kind"] == "page")
                        .map(|record| json!({"kind":"update_page","record":record,"body":format!("Unassessed update for {}.", record["record_id"].as_str().unwrap()),"citations":citations}))
                        .collect::<Vec<_>>()
                } else {
                    vec![]
                };
                json!({"sections":[{"heading":"Unassessed findings","claims":[{"text":"A preliminary model-derived claim.","citations":citations}]}],"unanswered_questions":["Does the source entail the preliminary claim?"],"proposed_changes":proposals})
            }
            _ => panic!("unexpected research stage"),
        };
        let body = json!({"model":"fixture-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":serde_json::to_string(&response).unwrap()}}],"usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0}}});
        Box::pin(async move {
            Ok(TransportReply::new(200, vec![], serde_json::to_vec(&body).unwrap()).unwrap())
        })
    }
}
struct Resolver;
impl PublicResolver for Resolver {
    fn resolve<'a>(&'a self, _: &'a str, _: u16) -> ResolveFuture<'a> {
        Box::pin(async { Ok(vec!["8.8.8.8".parse::<IpAddr>().unwrap()]) })
    }
}
struct Connector {
    calls: AtomicUsize,
    fail_url: Mutex<Option<String>>,
    reject_url: Mutex<Option<String>>,
}
impl PublicConnector for Connector {
    fn connect<'a>(&'a self, request: PinnedRequest) -> Result<ConnectionFuture<'a>> {
        assert!(request.addresses().iter().all(|a| public_ip(a.ip())));
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.reject_url.lock().unwrap().as_deref() == Some(request.url()) {
            return Err(WikiError::new(
                ErrorCode::ProviderUnavailable,
                "Fixture refused connection before entering transport",
            ));
        }
        if self.fail_url.lock().unwrap().as_deref() == Some(request.url()) {
            return Ok(Box::pin(async {
                Ok(PublicReply::new(
                    404,
                    vec![],
                    b"Source unavailable".to_vec(),
                ))
            }));
        }
        Ok(Box::pin(async {
            Ok(PublicReply::new(200, vec![("content-type".into(), "text/html; charset=utf-8".into())], b"<html><body><main><p>Alpha supports a preliminary source finding.</p></main></body></html>".to_vec()))
        }))
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    fs: VaultFs,
    app: OfflineApp,
    generation: TrustedService,
    clock: Arc<provider::Clock>,
    options: JobOptions,
    dispatcher: Dispatcher,
    model: Arc<Model>,
    connector: Arc<Connector>,
    public: PublicFetchOptions,
    inputs: Arc<provider::Inputs>,
}
impl Fixture {
    fn new(stop: bool, invalid_evidence: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Research fixture\n---\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let config = temp.path().join("providers.toml");
        provider::private(
            &config,
            format!(
                "version=1\n[profiles.primary]\ngeneration=\"generation\"\n[services.generation]\nadapter=\"chat-completions-v1\"\nurl=\"https://fixture.example/v1/chat/completions\"\nmodel=\"fixture-model\"\nrevision=\"r1\"\n[services.generation.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\n[vault_bindings.test]\nroot={}\nwiki_id=\"vault_test\"\nallowed_profiles=[\"primary\"]\n",
                provider::quote(temp.path().to_str().unwrap())
            ),
        );
        let generation = ProviderConfig::load(&config)
            .unwrap()
            .authorize(&fs, &id("vault_test"), "primary", Capability::Generate)
            .unwrap();
        let clock = Arc::new(provider::Clock::new());
        let options = JobOptions {
            clock: clock.clone(),
            cancel: CancellationToken::default(),
            fault: None,
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        };
        let app = OfflineApp::new(
            fs.clone(),
            OperationOptions {
                lock_timeout_ms: 5000,
                ..Default::default()
            },
        )
        .unwrap();
        let inputs = Arc::new(provider::Inputs(AtomicUsize::new(0)));
        let broker = Arc::new(CredentialBroker::new(CredentialOptions {
            clock: options.clock.clone(),
            inputs: inputs.clone(),
            runner: Arc::new(provider::Runner {
                calls: AtomicUsize::new(0),
                hook: None,
            }),
        }));
        let model = Arc::new(Model {
            fs: fs.clone(),
            options: options.clone(),
            stages: Mutex::new(vec![]),
            stop,
            invalid_evidence,
            update_pages: AtomicBool::new(false),
            cancel_stage: Mutex::new(None),
            hook: Mutex::new(None),
        });
        let dispatcher = Dispatcher::new(
            fs.clone(),
            DispatchOptions {
                broker,
                transport: model.clone(),
                jitter: Arc::new(provider::ZeroJitter),
            },
        );
        let connector = Arc::new(Connector {
            calls: AtomicUsize::new(0),
            fail_url: Mutex::new(None),
            reject_url: Mutex::new(None),
        });
        let public = PublicFetchOptions {
            resolver: Arc::new(Resolver),
            connector: connector.clone(),
        };
        Self {
            temp,
            fs,
            app,
            generation,
            clock,
            options,
            dispatcher,
            model,
            connector,
            public,
            inputs,
        }
    }
    fn scope(&self) -> ResearchScope {
        ResearchScope {
            version: 1,
            question: "Alpha preliminary finding".into(),
            exclusions: vec![],
            explicit_urls: vec!["https://example.org/research-source".into()],
            generation_profile: "primary".into(),
            search_profile: None,
            limits: ResearchLimits::default(),
            apply: false,
        }
    }
    fn create(&self, limits: LifetimeLimits) -> JobLedger {
        self.create_scope(self.scope(), limits)
    }
    fn create_scope(&self, scope: ResearchScope, limits: LifetimeLimits) -> JobLedger {
        let inspected = inspection::inspect(&self.fs, self.app.vault_id(), &scope).unwrap();
        let summary = self.generation.summary();
        let binding = BindingEpochV1 {
            version: 1,
            number: 0,
            config_fingerprint: summary.config_fingerprint,
            source_snapshot: Some(inspected.snapshot),
            input_records: inspected.records,
            read_preconditions: inspected.dependencies,
            services: vec![ServiceBindingV1 {
                profile_id: summary.profile_id,
                capability: summary.capability,
                profile_fingerprint: summary.profile_fingerprint,
                endpoint_fingerprint: summary.endpoint_fingerprint,
            }],
        };
        let now = self.options.clock.read().unwrap().utc_ms;
        let planned = plan::plan(
            scope,
            id("vault_test"),
            id("run_workflow"),
            binding,
            &self.generation,
            &inspected.passages,
            limits,
            now,
            now + 900_000,
        )
        .unwrap();
        runner::create(&self.fs, &planned, self.options.clone()).unwrap()
    }
    fn runtime(&self) -> ResearchRuntime<'_> {
        ResearchRuntime {
            generation: &self.generation,
            search: None,
            dispatcher: &self.dispatcher,
            public_fetch: &self.public,
            job_options: self.options.clone(),
        }
    }
}

#[test]
fn explicit_url_research_uses_one_ledger_and_exact_paid_receipts_without_search() {
    let f = Fixture::new(true, false);
    let ledger = f.create(LifetimeLimits::default());
    let genesis = ledger.inspect().unwrap().spec_hash;
    let outcome = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Completed);
    assert_eq!(inspection.spec_hash, genesis);
    assert_eq!(inspection.attempts.len(), 5);
    assert!(
        inspection
            .attempts
            .iter()
            .all(|a| a.phase == AttemptPhase::Settled && a.receipt.is_some())
    );
    assert_eq!(inspection.budget.dispatched_requests, 5);
    assert_eq!(inspection.research.as_ref().unwrap().origins.len(), 1);
    assert_eq!(inspection.research.as_ref().unwrap().rounds_started, 1);
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 1);
    assert!(
        !f.model
            .stages
            .lock()
            .unwrap()
            .contains(&TaskStage::Discover)
    );
    let report = outcome.report.unwrap();
    assert!(!report.partial);
    assert!(!report.passages.is_empty());
    assert!(!report.proposed_changes.is_empty());
    assert!(
        report
            .claim_assessments
            .iter()
            .all(|claim| claim.status == research::synthesis::ClaimStatus::Unassessed)
    );
    assert!(outcome.network_used);
    let requests = inspection.budget.dispatched_requests;
    let again = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert!(!again.network_used);
    assert_eq!(
        ledger.inspect().unwrap().budget.dispatched_requests,
        requests
    );
}

#[test]
fn request_budget_stop_keeps_partial_report_and_skips_final_model_call() {
    let f = Fixture::new(true, false);
    let ledger = f.create(LifetimeLimits {
        requests: 1,
        ..Default::default()
    });
    let before = ledger.inspect().unwrap().spec_hash;
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.spec_hash, before);
    assert_eq!(inspection.spec.limits.requests, 1);
    assert_eq!(inspection.budget.dispatched_requests, 1);
    assert_eq!(inspection.research.unwrap().origins.len(), 1);
    assert!(result.report.unwrap().partial);
    assert_eq!(
        *f.model.stages.lock().unwrap(),
        vec![TaskStage::PlanFrontier]
    );
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn invented_evidence_id_is_rejected_with_paid_receipt_and_partial_report() {
    let f = Fixture::new(true, true);
    let ledger = f.create(LifetimeLimits::default());
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert!(result.report.unwrap().partial);
    let inspection = ledger.inspect().unwrap();
    let assessment = inspection
        .tasks
        .values()
        .find(|task| task.spec.stage == TaskStage::AssessGaps)
        .unwrap();
    assert_eq!(assessment.state, TaskState::Failed);
    let paid = inspection
        .attempts
        .iter()
        .find(|attempt| attempt.attempt.task_key == assessment.spec.key)
        .unwrap();
    assert_eq!(paid.phase, AttemptPhase::Settled);
    assert!(paid.receipt.is_some());
    assert!(paid.outputs.is_empty());
    assert!(
        !f.model
            .stages
            .lock()
            .unwrap()
            .contains(&TaskStage::Synthesize)
    );
}

#[test]
fn two_rounds_without_new_support_stop_at_lifetime_round_history() {
    let f = Fixture::new(false, false);
    let ledger = f.create(LifetimeLimits::default());
    runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    let inspection = ledger.inspect().unwrap();
    let state = inspection.research.unwrap();
    assert_eq!(state.rounds_started, 3);
    assert_eq!(state.rounds.len(), 3);
    assert_eq!(state.no_progress_rounds, 2);
    assert_eq!(state.origins.len(), 1);
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.model
            .stages
            .lock()
            .unwrap()
            .iter()
            .filter(|stage| **stage == TaskStage::PlanFrontier)
            .count(),
        3
    );
}

#[test]
fn dry_run_and_offline_do_not_resolve_credentials_or_dispatch() {
    let f = Fixture::new(true, false);
    let ledger = f.create(LifetimeLimits::default());
    let before = provider::tree(f.temp.path());
    let mut options = f.options.clone();
    options.policy.dry_run = true;
    let readonly = JobLedger::new(
        f.fs.clone(),
        id("vault_test"),
        id("run_workflow"),
        options.clone(),
    )
    .unwrap();
    let mut runtime = f.runtime();
    runtime.job_options = options;
    runner::run(&f.app, &readonly, &runtime).unwrap();
    assert_eq!(provider::tree(f.temp.path()), before);
    assert_eq!(f.inputs.0.load(Ordering::SeqCst), 0);
    let offline = OfflineApp::new(
        f.fs.clone(),
        OperationOptions {
            offline: true,
            lock_timeout_ms: 5000,
            ..Default::default()
        },
    )
    .unwrap();
    let result = runner::run(&offline, &ledger, &f.runtime()).unwrap();
    assert!(result.report.unwrap().partial);
    assert_eq!(f.inputs.0.load(Ordering::SeqCst), 0);
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn entered_cancellation_retains_paid_outcome_and_local_partial_report() {
    let f = Fixture::new(true, false);
    *f.model.cancel_stage.lock().unwrap() = Some(TaskStage::PlanFrontier);
    let ledger = f.create(LifetimeLimits::default());
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert!(result.report.unwrap().partial);
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.budget.dispatched_requests, 1);
    assert!(inspection.attempts[0].receipt.is_some() || inspection.attempts[0].spool.is_some());
    assert_eq!(
        *f.model.stages.lock().unwrap(),
        vec![TaskStage::PlanFrontier]
    );
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn completed_workflow_and_reports_survive_cache_deletion() {
    let f = Fixture::new(true, false);
    let ledger = f.create(LifetimeLimits::default());
    let first = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    let report = first.report.unwrap();
    let before = ledger.inspect().unwrap();
    let cache = f.temp.path().join(".wiki/cache");
    if cache.exists() {
        std::fs::remove_dir_all(cache).unwrap();
    }
    let after = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert_eq!(
        serde_json::to_value(after.report.unwrap()).unwrap(),
        serde_json::to_value(report).unwrap()
    );
    assert_eq!(ledger.inspect().unwrap().budget, before.budget);
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn explicit_model_rebind_preserves_genesis_lifetime_caps_and_original_tasks() {
    let mut f = Fixture::new(true, false);
    let ledger = f.create(LifetimeLimits::default());
    let before = ledger.inspect().unwrap();
    ledger.start().unwrap();
    ledger
        .pause(StopReason::User("Caller paused before remote work".into()))
        .unwrap();
    let path = f.temp.path().join("providers.toml");
    let changed = std::fs::read_to_string(&path)
        .unwrap()
        .replace("revision=\"r1\"", "revision=\"r2\"");
    provider::private(&path, changed);
    f.generation = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&f.fs, f.app.vault_id(), "primary", Capability::Generate)
        .unwrap();
    runner::resume(&f.app, &ledger, &f.runtime()).unwrap();
    let after = ledger.inspect().unwrap();
    assert_eq!(after.spec_hash, before.spec_hash);
    assert_eq!(after.spec, before.spec);
    assert_eq!(after.effective_limits, before.effective_limits);
    assert_eq!(
        after.effective_deadline_utc_ms,
        before.effective_deadline_utc_ms
    );
    assert_eq!(after.research.as_ref().unwrap().binding.number, 1);
    for (key, original) in before.tasks {
        assert_eq!(after.tasks[&key].spec, original.spec);
    }
    assert_eq!(after.budget.dispatched_requests, 5);
}

#[test]
fn source_changed_during_paid_response_is_withheld_in_partial_report() {
    let f = Fixture::new(true, false);
    let original = f
        .app
        .source_add(CaptureRequest {
            title: "Alpha preliminary finding".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "initial.txt".into(),
            original: b"Alpha preliminary finding before the change.".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let source = original.allocated_ids["source"].clone();
    let fs = f.fs.clone();
    *f.model.hook.lock().unwrap() = Some((
        TaskStage::PlanFrontier,
        Box::new(move || {
            let app = OfflineApp::new(
                fs,
                OperationOptions {
                    lock_timeout_ms: 5000,
                    ..Default::default()
                },
            )
            .unwrap();
            app.source_refresh(
                source,
                CaptureRequest {
                    title: "Alpha preliminary finding".into(),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: "initial.txt".into(),
                    original: b"Changed source bytes invalidate the old passage.".to_vec(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: None,
                },
            )
            .unwrap();
        }),
    ));
    let ledger = f.create(LifetimeLimits::default());
    let before = ledger.inspect().unwrap();
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    let report = result.report.unwrap();
    assert!(report.partial);
    assert!(report.passages.is_empty());
    assert!(report.gaps.iter().any(|gap| gap.code == "stale_passage"));
    let after = ledger.inspect().unwrap();
    assert_eq!(after.spec_hash, before.spec_hash);
    assert_eq!(after.budget.dispatched_requests, 1);
    assert!(after.attempts[0].spool.is_some());
}

fn stale_generation_receipt(stage: TaskStage) {
    let f = Fixture::new(true, false);
    let original = f
        .app
        .source_add(CaptureRequest {
            title: "Alpha preliminary finding".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "initial.txt".into(),
            original: b"Alpha preliminary finding before the change.".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let source = original.allocated_ids["source"].clone();
    let fs = f.fs.clone();
    *f.model.hook.lock().unwrap() = Some((
        stage,
        Box::new(move || {
            OfflineApp::new(fs, OperationOptions::default())
                .unwrap()
                .source_refresh(
                    source,
                    CaptureRequest {
                        title: "Alpha preliminary finding".into(),
                        origin_kind: SourceOrigin::LocalFile,
                        origin: "initial.txt".into(),
                        original: b"Changed source bytes invalidate the old passage.".to_vec(),
                        extraction: ExtractionInput::Utf8Preserve,
                        media_type: None,
                    },
                )
                .unwrap();
        }),
    ));
    let mut scope = f.scope();
    scope.explicit_urls.clear();
    let ledger = f.create_scope(scope, LifetimeLimits::default());
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    let report = result.report.unwrap();
    assert!(report.partial);
    assert!(report.passages.is_empty());
    let after = ledger.inspect().unwrap();
    let paid = after
        .attempts
        .iter()
        .find(|attempt| after.tasks[&attempt.attempt.task_key].spec.stage == stage)
        .unwrap();
    assert_eq!(paid.phase, AttemptPhase::Settled);
    assert!(paid.spool.is_some());
    assert!(paid.outputs.is_empty());
    let reference = paid.receipt.as_ref().unwrap();
    let bytes = std::fs::read(f.temp.path().join(reference.path.as_str())).unwrap();
    assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
    let text = std::str::from_utf8(&bytes).unwrap();
    let fenced = text
        .split_once("```lwiki.run-event.v1\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    let receipt: UsageReceipt =
        serde_json::from_value(serde_json::from_str::<Value>(fenced).unwrap()["receipt"].clone())
            .unwrap();
    assert_eq!(receipt.output_disposition, OutputDisposition::Unknown);
    assert_ne!(after.tasks[&paid.attempt.task_key].state, TaskState::Failed);
}

#[test]
fn gap_response_after_source_edit_is_unknown_with_protected_spool() {
    stale_generation_receipt(TaskStage::AssessGaps);
}

#[test]
fn synthesis_response_after_source_edit_is_unknown_with_protected_spool() {
    stale_generation_receipt(TaskStage::Synthesize);
}

#[test]
fn three_sources_use_later_concurrency_waves_without_mirrored_support_inflation() {
    let f = Fixture::new(true, false);
    let mut scope = f.scope();
    scope.explicit_urls = (0..3)
        .map(|n| format!("https://example.org/source-{n}"))
        .collect();
    let ledger = f.create_scope(
        scope,
        LifetimeLimits {
            concurrency: 2,
            ..Default::default()
        },
    );
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert!(!result.report.unwrap().partial);
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Completed);
    assert_eq!(inspection.budget.dispatched_requests, 9);
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 3);
    let state = inspection.research.unwrap();
    assert_eq!(state.origins.len(), 3);
    assert_eq!(state.support_groups.len(), 1);
}

#[test]
fn later_capture_wave_reaches_actual_lifetime_cap_without_synthesis() {
    let f = Fixture::new(true, false);
    let mut scope = f.scope();
    scope.explicit_urls = (0..3)
        .map(|n| format!("https://example.org/capped-source-{n}"))
        .collect();
    let ledger = f.create_scope(
        scope,
        LifetimeLimits {
            requests: 4,
            concurrency: 2,
            ..Default::default()
        },
    );
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert_eq!(result.stop_code, Some(ErrorCode::BudgetExceeded));
    assert!(result.report.unwrap().partial);
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.budget.dispatched_requests, 4);
    assert_eq!(inspection.research.unwrap().origins.len(), 3);
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        *f.model.stages.lock().unwrap(),
        vec![TaskStage::PlanFrontier]
    );
    assert!(
        inspection
            .attempts
            .iter()
            .all(|attempt| attempt.phase == AttemptPhase::Settled)
    );
}

#[test]
fn unavailable_capture_keeps_its_origin_and_explicit_gap_with_an_independent_source() {
    let f = Fixture::new(true, false);
    let failed_url = "https://example.org/unavailable";
    *f.connector.fail_url.lock().unwrap() = Some(failed_url.into());
    let mut scope = f.scope();
    scope.explicit_urls = vec![failed_url.into(), "https://example.org/available".into()];
    let ledger = f.create_scope(scope, LifetimeLimits::default());
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    let report = result.report.unwrap();
    assert!(!report.partial);
    assert!(!report.passages.is_empty());
    assert!(report.synthesis.is_some());
    assert!(
        report
            .gaps
            .iter()
            .any(|gap| gap.code == "unsupported_source")
    );
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Completed);
    assert_eq!(inspection.research.as_ref().unwrap().origins.len(), 2);
    let captures = inspection
        .tasks
        .values()
        .filter(|task| task.spec.stage == TaskStage::Capture)
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 2);
    assert_eq!(
        captures
            .iter()
            .filter(|task| task.state == TaskState::Completed)
            .count(),
        2
    );
    let unavailable = captures
        .iter()
        .find(|task| {
            let bytes = std::fs::read(f.temp.path().join(task.spec.input.path.as_str())).unwrap();
            let input: RemoteInput = serde_json::from_slice(&bytes).unwrap();
            matches!(input.operation, RemoteOperation::Fetch { url, .. } if url == failed_url)
        })
        .unwrap();
    let unavailable_source = unavailable
        .outputs
        .iter()
        .find(|output| output.record.expected_kind == RecordKind::Source)
        .unwrap();
    assert!(
        report
            .passages
            .iter()
            .all(|passage| match &passage.citation {
                CitationRef::Source(citation) =>
                    citation.source_id != unavailable_source.record.record_id,
                CitationRef::Assertion(_) => false,
            })
    );
    assert!(
        inspection
            .attempts
            .iter()
            .all(|attempt| attempt.phase == AttemptPhase::Settled)
    );
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn proven_not_sent_capture_failure_preserves_origin_and_independent_source_work() {
    let f = Fixture::new(true, false);
    let failed_url = "https://example.org/not-sent";
    *f.connector.reject_url.lock().unwrap() = Some(failed_url.into());
    let mut scope = f.scope();
    scope.explicit_urls = vec![failed_url.into(), "https://example.org/available".into()];
    let ledger = f.create_scope(scope, LifetimeLimits::default());
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    let report = result.report.unwrap();
    assert!(report.partial);
    assert!(!report.passages.is_empty());
    assert!(report.synthesis.is_some());
    assert!(report.gaps.iter().any(|gap| gap.code == "failed_stage"));
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Paused);
    assert_eq!(inspection.research.as_ref().unwrap().origins.len(), 2);
    let failed = inspection
        .tasks
        .values()
        .find(|task| task.spec.stage == TaskStage::Capture && task.state == TaskState::Failed)
        .unwrap();
    let attempt = inspection
        .attempts
        .iter()
        .find(|attempt| attempt.attempt.task_key == failed.spec.key)
        .unwrap();
    assert_eq!(attempt.phase, AttemptPhase::Settled);
    assert_eq!(attempt.billing, BillingDisposition::ReleasedNotSent);
    assert_eq!(attempt.remote_exposure, RemoteExposure::TerminalConfirmed);
    assert_eq!(inspection.budget.dispatched_requests, 5);
    assert!(inspection.tasks.values().any(|task| task.spec.stage == TaskStage::Capture && task.state == TaskState::Completed));
}

fn resume_pending_synthesis(scope_rounds: u32) {
    let f = Fixture::new(true, false);
    let mut scope = f.scope();
    scope.limits.rounds = scope_rounds;
    let ledger = f.create_scope(
        scope,
        LifetimeLimits {
            requests: 4,
            ..Default::default()
        },
    );
    let first = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert_eq!(first.stop_code, Some(ErrorCode::BudgetExceeded));
    let before = ledger.inspect().unwrap();
    assert_eq!(before.budget.dispatched_requests, 4);
    assert_eq!(before.research.as_ref().unwrap().rounds.len(), 1);
    let result = runner::resume_with_amendment(
        &f.app,
        &ledger,
        &f.runtime(),
        Some(ResearchResumeAmendment {
            reason: "Caller authorized one final synthesis request".into(),
            limits: LifetimeLimits {
                requests: 5,
                ..before.effective_limits.clone()
            },
            deadline_utc_ms: before.effective_deadline_utc_ms,
        }),
    )
    .unwrap();
    assert_eq!(result.stop_code, None);
    assert!(!result.report.unwrap().partial);
    let after = ledger.inspect().unwrap();
    assert_eq!(after.spec_hash, before.spec_hash);
    assert_eq!(after.spec.limits.requests, 4);
    assert_eq!(after.effective_limits.requests, 5);
    assert_eq!(after.budget.dispatched_requests, 5);
    assert_eq!(after.research.unwrap().rounds_started, 1);
}

#[test]
fn final_assessed_round_resumes_pending_synthesis_without_new_round_or_budget_reset() {
    resume_pending_synthesis(1);
}

#[test]
fn stopped_assessment_resumes_synthesis_at_the_same_round_when_rounds_remain() {
    resume_pending_synthesis(3);
}

#[test]
fn offline_limit_amendment_resume_preserves_the_full_tree_and_counters() {
    let f = Fixture::new(true, false);
    let ledger = f.create(LifetimeLimits::default());
    ledger.start().unwrap();
    ledger
        .pause(StopReason::User("Caller paused research".into()))
        .unwrap();
    let before = ledger.inspect().unwrap();
    let tree = provider::tree(f.temp.path());
    let mut runtime = f.runtime();
    runtime.job_options.policy.offline = true;
    let result = runner::resume_with_amendment(
        &f.app,
        &ledger,
        &runtime,
        Some(ResearchResumeAmendment {
            limits: LifetimeLimits {
                requests: before.effective_limits.requests + 1,
                ..before.effective_limits.clone()
            },
            deadline_utc_ms: before.effective_deadline_utc_ms,
            reason: "Caller previewed an additional request".into(),
        }),
    )
    .unwrap();
    assert!(!result.network_used);
    assert_eq!(provider::tree(f.temp.path()), tree);
    let after = ledger.inspect().unwrap();
    assert_eq!(after.effective_limits, before.effective_limits);
    assert_eq!(after.budget, before.budget);
    assert_eq!(f.inputs.0.load(Ordering::SeqCst), 0);
}

struct ReportAdmissionCrash {
    journal: std::path::PathBuf,
    fired: AtomicBool,
}
impl LedgerFault for ReportAdmissionCrash {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterJournalSync
            && !self.fired.load(Ordering::SeqCst)
            && std::fs::read(&self.journal)
                .unwrap_or_default()
                .windows(b"stage_changes".len())
                .any(|window| window == b"stage_changes")
        {
            self.fired.store(true, Ordering::SeqCst);
            return Err(WikiError::new(
                ErrorCode::Internal,
                "Fixture crash after durable report admission",
            ));
        }
        Ok(())
    }
}

#[test]
fn admitted_pending_report_recovers_after_clock_advance_without_new_paid_calls() {
    let mut f = Fixture::new(true, false);
    f.options.fault = Some(Arc::new(ReportAdmissionCrash {
        journal: f
            .temp
            .path()
            .join(".wiki/state/jobs/run_workflow/journal.bin"),
        fired: AtomicBool::new(false),
    }));
    let ledger = f.create(LifetimeLimits::default());
    let error = runner::run(&f.app, &ledger, &f.runtime()).unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    let before = ledger.inspect().unwrap();
    assert_eq!(before.budget.dispatched_requests, 5);
    let pending = before
        .tasks
        .values()
        .find(|task| task.spec.stage == TaskStage::StageChanges)
        .unwrap();
    assert_eq!(pending.state, TaskState::Pending);
    let descriptor = std::fs::read(f.temp.path().join(pending.spec.input.path.as_str())).unwrap();
    let report: research::ResearchReport = serde_json::from_slice(&descriptor).unwrap();
    let (expected, _) = research::stages::output_write(
        &before.spec.vault_id,
        &before.spec.run_id,
        &pending.spec,
        None,
        serde_json::to_value(&report).unwrap(),
        before.spec.created_at_utc_ms,
    )
    .unwrap();
    f.clock.utc.fetch_add(5000, Ordering::SeqCst);
    let result = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert!(!result.report.unwrap().partial);
    let after = ledger.inspect().unwrap();
    assert_eq!(after.state, RunState::Completed);
    assert_eq!(after.budget.dispatched_requests, 5);
    assert_eq!(after.tasks[&pending.spec.key].outputs, vec![expected]);
    assert_eq!(
        std::fs::read(f.temp.path().join(pending.spec.input.path.as_str())).unwrap(),
        descriptor
    );
}

struct CancelSecondPage {
    cancel: CancellationToken,
    count: AtomicUsize,
}
impl lwiki::vault::DurableIo for CancelSecondPage {
    fn create_stage(&self, path: &std::path::Path) -> std::io::Result<std::fs::File> {
        lwiki::vault::DurableIo::create_stage(&lwiki::vault::NativeIo, path)
    }
    fn create_private_stage(&self, path: &std::path::Path) -> std::io::Result<std::fs::File> {
        lwiki::vault::DurableIo::create_private_stage(&lwiki::vault::NativeIo, path)
    }
    fn create_private_directory(&self, path: &std::path::Path) -> std::io::Result<()> {
        lwiki::vault::DurableIo::create_private_directory(&lwiki::vault::NativeIo, path)
    }
    fn open_append(&self, path: &std::path::Path) -> std::io::Result<std::fs::File> {
        lwiki::vault::DurableIo::open_append(&lwiki::vault::NativeIo, path)
    }
    fn truncate_file(&self, file: &std::fs::File, size: u64) -> std::io::Result<()> {
        lwiki::vault::DurableIo::truncate_file(&lwiki::vault::NativeIo, file, size)
    }
    fn write_stage(&self, file: &mut std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
        lwiki::vault::DurableIo::write_stage(&lwiki::vault::NativeIo, file, bytes)
    }
    fn sync_file(&self, file: &std::fs::File) -> std::io::Result<()> {
        lwiki::vault::DurableIo::sync_file(&lwiki::vault::NativeIo, file)
    }
    fn replace(&self, from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        if to.file_name().is_some_and(|name| name == "change.md")
            && std::fs::read(from)?
                .windows(b"research_proposal".len())
                .any(|window| window == b"research_proposal")
            && self.count.fetch_add(1, Ordering::SeqCst) + 1 == 2
        {
            self.cancel.cancel();
        }
        lwiki::vault::DurableIo::replace(&lwiki::vault::NativeIo, from, to)
    }
    fn remove(&self, path: &std::path::Path) -> std::io::Result<()> {
        lwiki::vault::DurableIo::remove(&lwiki::vault::NativeIo, path)
    }
    fn create_directory(&self, path: &std::path::Path) -> std::io::Result<()> {
        lwiki::vault::DurableIo::create_directory(&lwiki::vault::NativeIo, path)
    }
    fn sync_directory(
        &self,
        path: &std::path::Path,
    ) -> std::io::Result<lwiki::vault::DirectorySync> {
        lwiki::vault::DurableIo::sync_directory(&lwiki::vault::NativeIo, path)
    }
}

#[test]
fn own_first_page_update_resumes_second_without_rebinding_or_new_paid_work() {
    let f = Fixture::new(true, false);
    f.app
        .source_add(CaptureRequest {
            title: "Alpha preliminary finding".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "initial.txt".into(),
            original: b"Alpha preliminary finding remains unassessed.".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    std::fs::create_dir_all(f.temp.path().join("pages")).unwrap();
    for index in 1..=2 {
        std::fs::write(f.temp.path().join(format!("pages/alpha{index}.md")), format!("---\nwiki_schema: \"1\"\nwiki_id: page_alpha{index}\nwiki_kind: page\nwiki_status: draft\ntitle: Alpha preliminary finding {index}\n---\nAlpha preliminary finding original user page.\n")).unwrap();
    }
    f.model.update_pages.store(true, Ordering::SeqCst);
    let mut scope = f.scope();
    scope.explicit_urls.clear();
    scope.apply = true;
    let ledger = f.create_scope(scope, LifetimeLimits::default());
    let gated = OfflineApp::new(
        VaultFs::with_io(
            f.fs.root().clone(),
            Arc::new(CancelSecondPage {
                cancel: f.options.cancel.clone(),
                count: AtomicUsize::new(0),
            }),
        ),
        OperationOptions::default(),
    )
    .unwrap();
    let first = runner::run(&gated, &ledger, &f.runtime()).unwrap();
    assert_eq!(first.stop_code, Some(ErrorCode::Cancelled));
    assert!(first.report.unwrap().partial);
    let before = ledger.inspect().unwrap();
    assert_eq!(before.state, RunState::Paused);
    let synthesis = before
        .tasks
        .values()
        .find(|task| task.spec.stage == TaskStage::Synthesize)
        .unwrap();
    assert_eq!(synthesis.state, TaskState::Completed);
    let engine = lwiki::changes::ChangeEngine::new(f.fs.clone()).unwrap();
    let page_changes = std::fs::read_dir(f.temp.path().join("changes"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let change_id = RecordId::new(entry.file_name().to_str().unwrap()).unwrap();
            engine.inspect(&change_id).unwrap()
        })
        .filter(|change| {
            change
                .manifest
                .allocated_ids
                .contains_key("research_proposal")
        })
        .collect::<Vec<_>>();
    assert_eq!(page_changes.len(), 2);
    assert_eq!(
        page_changes
            .iter()
            .filter(|change| change.status == lwiki::changes::ChangeStatus::Committed)
            .count(),
        1
    );
    let mut options = f.options.clone();
    options.cancel = CancellationToken::default();
    let reopened = JobLedger::new(
        f.fs.clone(),
        id("vault_test"),
        id("run_workflow"),
        options.clone(),
    )
    .unwrap();
    let mut runtime = f.runtime();
    runtime.job_options = options;
    let result = runner::resume(&f.app, &reopened, &runtime).unwrap();
    assert!(!result.report.unwrap().partial);
    let after = reopened.inspect().unwrap();
    assert_eq!(after.state, RunState::Completed);
    assert_eq!(after.spec_hash, before.spec_hash);
    assert_eq!(
        after.research.as_ref().unwrap().binding.number,
        before.research.as_ref().unwrap().binding.number
    );
    assert_eq!(after.tasks[&synthesis.spec.key], *synthesis);
    assert_eq!(
        after.budget.dispatched_requests,
        before.budget.dispatched_requests
    );
    for change in page_changes {
        assert_eq!(
            engine.inspect(&change.prepared.change_id).unwrap().status,
            lwiki::changes::ChangeStatus::Committed
        );
    }
}

struct CancelAfterSynthesis {
    journal: std::path::PathBuf,
    cancel: CancellationToken,
}
impl LedgerFault for CancelAfterSynthesis {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        let marker = b"\"stage\":\"synthesize\"";
        if point == LedgerCheckpoint::AfterSettlement
            && std::fs::read(&self.journal)
                .unwrap_or_default()
                .windows(marker.len())
                .any(|window| window == marker)
        {
            self.cancel.cancel();
        }
        Ok(())
    }
}

#[test]
fn completed_synthesis_cancelled_before_local_reporting_resumes_without_paid_work() {
    let mut f = Fixture::new(true, false);
    f.options.fault = Some(Arc::new(CancelAfterSynthesis {
        journal: f
            .temp
            .path()
            .join(".wiki/state/jobs/run_workflow/journal.bin"),
        cancel: f.options.cancel.clone(),
    }));
    let ledger = f.create(LifetimeLimits::default());
    let first = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert_eq!(first.stop_code, Some(ErrorCode::Cancelled));
    assert!(first.report.unwrap().partial);
    let before = ledger.inspect().unwrap();
    assert_eq!(before.budget.dispatched_requests, 5);
    let synthesis = before
        .tasks
        .values()
        .find(|task| task.spec.stage == TaskStage::Synthesize)
        .unwrap();
    assert_eq!(synthesis.state, TaskState::Completed);
    let mut options = f.options.clone();
    options.cancel = CancellationToken::default();
    options.fault = None;
    let reopened = JobLedger::new(
        f.fs.clone(),
        id("vault_test"),
        id("run_workflow"),
        options.clone(),
    )
    .unwrap();
    let mut runtime = f.runtime();
    runtime.job_options = options;
    let result = runner::resume(&f.app, &reopened, &runtime).unwrap();
    assert!(!result.report.unwrap().partial);
    let after = reopened.inspect().unwrap();
    assert_eq!(after.state, RunState::Completed);
    assert_eq!(
        after.research.as_ref().unwrap().binding.number,
        before.research.as_ref().unwrap().binding.number
    );
    assert_eq!(after.tasks[&synthesis.spec.key], *synthesis);
    assert_eq!(after.budget.dispatched_requests, 5);
}

fn captured_source_refresh_resumes(matches_question: bool) {
    let f = Fixture::new(true, false);
    let initial_requests = if matches_question { 2 } else { 3 };
    let scope = f.scope();
    let ledger = f.create_scope(
        scope.clone(),
        LifetimeLimits {
            requests: initial_requests,
            ..Default::default()
        },
    );
    let first = runner::run(&f.app, &ledger, &f.runtime()).unwrap();
    assert_eq!(first.stop_code, Some(ErrorCode::BudgetExceeded));
    let original_changes = first.report.unwrap().proposed_changes;
    let before = ledger.inspect().unwrap();
    assert_eq!(before.budget.dispatched_requests, initial_requests);
    let original_extract = before
        .tasks
        .values()
        .find(|task| task.spec.stage == TaskStage::Extract)
        .unwrap();
    assert!(
        before
            .research
            .as_ref()
            .unwrap()
            .active_tasks
            .contains(&original_extract.spec.key)
    );
    assert!(!original_extract.spec.source_bindings.is_empty());
    if !matches_question {
        assert_eq!(original_extract.state, TaskState::Completed);
    }
    let extract_calls = f
        .model
        .stages
        .lock()
        .unwrap()
        .iter()
        .filter(|stage| **stage == TaskStage::Extract)
        .count();
    let captured = before
        .tasks
        .values()
        .find(|task| task.spec.stage == TaskStage::Capture && task.state == TaskState::Completed)
        .unwrap();
    let source = captured
        .outputs
        .iter()
        .find(|output| output.record.expected_kind == RecordKind::Source)
        .unwrap()
        .record
        .record_id
        .clone();
    let old_revision = captured
        .outputs
        .iter()
        .find(|output| output.record.expected_kind == RecordKind::Revision)
        .unwrap()
        .record
        .record_id
        .clone();
    let refreshed = f
        .app
        .source_refresh(
            source,
            CaptureRequest {
                title: if matches_question {
                    "Refreshed Alpha preliminary finding"
                } else {
                    "Unrelated topic"
                }
                .into(),
                origin_kind: SourceOrigin::Url,
                origin: "https://example.org/research-source".into(),
                original: if matches_question {
                    b"Alpha refreshed preliminary finding supplied by the caller.".to_vec()
                } else {
                    b"Completely unrelated material about quarks.".to_vec()
                },
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/plain; charset=utf-8".into()),
            },
        )
        .unwrap();
    let new_revision = &refreshed.allocated_ids["revision"];
    if !matches_question {
        let fresh = inspection::inspect(&f.fs, f.app.vault_id(), &scope).unwrap();
        assert_eq!(
            fresh.records,
            before.research.as_ref().unwrap().binding.input_records
        );
        assert_eq!(
            fresh.dependencies,
            before.research.as_ref().unwrap().binding.read_preconditions
        );
        assert!(fresh.passages.is_empty());
    }
    let result = runner::resume_with_amendment(
        &f.app,
        &ledger,
        &f.runtime(),
        Some(ResearchResumeAmendment {
            limits: LifetimeLimits {
                requests: 5,
                ..before.effective_limits.clone()
            },
            deadline_utc_ms: before.effective_deadline_utc_ms,
            reason: "Caller authorized current-source replan and remaining generation".into(),
        }),
    )
    .unwrap();
    let report = result.report.unwrap();
    assert!(report.gaps.iter().any(
        |gap| gap.code == "stale_passage" && gap.task_key.as_ref() == Some(&captured.spec.key)
    ));
    if matches_question {
        assert!(report.passages.iter().any(|passage| matches!(&passage.citation, CitationRef::Source(citation) if &citation.source_revision == new_revision)));
    } else {
        assert!(report.passages.is_empty());
    }
    assert!(report.passages.iter().all(|passage| !matches!(&passage.citation, CitationRef::Source(citation) if citation.source_revision == old_revision)));
    let after = ledger.inspect().unwrap();
    if matches_question {
        assert_eq!(after.state, RunState::Completed);
        assert!(!report.partial);
    } else {
        // The completed old extraction proposal remains reviewable with its
        // original source proof, so the fresh synthesis reports its conflict.
        assert_eq!(after.state, RunState::Paused);
        assert!(report.partial);
        assert!(report.synthesis.is_some());
        assert!(!original_changes.is_empty());
        assert!(
            original_changes
                .iter()
                .all(|change| report.proposed_changes.contains(change))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("conflicts with current bytes"))
        );
    }
    assert_eq!(after.spec_hash, before.spec_hash);
    for original in &before.attempts {
        assert_eq!(
            after
                .attempts
                .iter()
                .find(|attempt| attempt.attempt == original.attempt),
            Some(original),
        );
    }
    assert!(
        before
            .budget
            .unknown_attempts
            .iter()
            .all(|attempt| after.budget.unknown_attempts.contains(attempt))
    );
    assert_eq!(after.spec.limits.requests, initial_requests);
    assert_eq!(after.effective_limits.requests, 5);
    assert_eq!(
        after.effective_deadline_utc_ms,
        before.effective_deadline_utc_ms
    );
    assert_eq!(after.budget.dispatched_requests, 5);
    assert_eq!(after.research.as_ref().unwrap().binding.number, 1);
    assert_eq!(
        after.research.as_ref().unwrap().origins,
        before.research.as_ref().unwrap().origins
    );
    assert_eq!(after.tasks[&captured.spec.key].spec, captured.spec);
    assert_eq!(
        after.tasks[&original_extract.spec.key].spec,
        original_extract.spec
    );
    assert!(
        !after
            .research
            .as_ref()
            .unwrap()
            .active_tasks
            .contains(&original_extract.spec.key)
    );
    assert_eq!(
        after
            .attempts
            .iter()
            .filter(|attempt| attempt.attempt.task_key == captured.spec.key)
            .count(),
        1
    );
    assert_eq!(f.connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.model
            .stages
            .lock()
            .unwrap()
            .iter()
            .filter(|stage| **stage == TaskStage::Extract)
            .count(),
        extract_calls
    );
}

#[test]
fn captured_source_refresh_rebinds_current_proofs_without_refetch_or_stale_extraction() {
    captured_source_refresh_resumes(true);
}

#[test]
fn nonmatching_capture_refresh_rebinds_stale_extract_with_equal_lexical_inputs() {
    captured_source_refresh_resumes(false);
}

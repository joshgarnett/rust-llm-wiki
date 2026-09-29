//! Ordered research dispatch uses disposable vaults and injected transports only.
use lwiki as library;
#[allow(dead_code)]
#[path = "fixtures/p16b/common.rs"]
mod common;
use common::{Clock, Inputs, Runner, ZeroJitter, hash, id, private, quote};
use lwiki::{
    config::providers::{ProviderConfig, TrustedService},
    domain::*,
    graph::packet::canonical_json,
    jobs::*,
    providers::{
        credentials::{CredentialBroker, CredentialOptions},
        dispatcher::{Dispatcher, ResearchDispatchOutcome, ResearchDispatchWork},
        public_fetch::PublicFetchOptions,
        types::*,
        wire::task_fingerprints,
    },
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    generation: TrustedService,
    search: TrustedService,
    job: JobLedger,
    options: JobOptions,
    clock: Arc<Clock>,
    broker: Arc<CredentialBroker>,
    tasks: Vec<TaskSpec>,
}
impl Fixture {
    fn new(roles: &[Capability], requests: u64, concurrency: u32) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("WIKI.md"), b"---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Research dispatch\n---\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        let config = temp.path().join("providers.toml");
        private(
            &config,
            format!(
                "version=1\n[profiles.primary]\ngeneration=\"generation\"\nsearch=\"search\"\n[services.generation]\nadapter=\"chat-completions-v1\"\nurl=\"https://gateway.example/chat\"\nmodel=\"test-model\"\nrevision=\"r1\"\n[services.generation.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\n[services.search]\nadapter=\"brave-web-v1\"\nurl=\"https://api.search.brave.com/res/v1/web/search\"\n[services.search.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\nheader=\"X-Subscription-Token\"\nprefix=\"\"\n[vault_bindings.test]\nroot={}\nwiki_id=\"vault_test\"\nallowed_profiles=[\"primary\"]\n",
                quote(root.to_str().unwrap())
            ),
        );
        let provider = ProviderConfig::load(&config).unwrap();
        let generation = provider
            .authorize(&fs, &id("vault_test"), "primary", Capability::Generate)
            .unwrap();
        let search = provider
            .authorize(&fs, &id("vault_test"), "primary", Capability::Search)
            .unwrap();
        let clock = Arc::new(Clock::new());
        let now = clock.read().unwrap().utc_ms;
        let options = JobOptions {
            clock: clock.clone(),
            fault: None,
            cancel: CancellationToken::default(),
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        };
        let scope = b"{\"question\":\"bounded\"}";
        private(&root.join("scope.json"), scope);
        let services = [&generation, &search]
            .into_iter()
            .map(|service| {
                let s = service.summary();
                ServiceBindingV1 {
                    profile_id: s.profile_id,
                    capability: s.capability,
                    profile_fingerprint: s.profile_fingerprint,
                    endpoint_fingerprint: s.endpoint_fingerprint,
                }
            })
            .collect();
        let summary = generation.summary();
        let mut spec = RunSpec {
            version: 1,
            run_id: id("run_research_dispatch"),
            vault_id: id("vault_test"),
            title: "Batch".into(),
            created_at_utc_ms: now,
            deadline_utc_ms: now + 900_000,
            scope: RunScope {
                research: Some(ResearchGenesisV1 {
                    version: 1,
                    scope: BoundedPayloadRef {
                        path: VaultRelativePath::new("scope.json").unwrap(),
                        hash: hash(scope),
                        byte_len: scope.len() as u64,
                    },
                    limits: ResearchAdmissionLimits {
                        rounds: 3,
                        sources: 5,
                    },
                    initial_binding: BindingEpochV1 {
                        version: 1,
                        number: 0,
                        config_fingerprint: summary.config_fingerprint.clone(),
                        source_snapshot: None,
                        input_records: vec![],
                        read_preconditions: vec![],
                        services,
                    },
                }),
                operation: "research".into(),
                question: Some("bounded".into()),
                exclusions: vec![],
                source_snapshot: None,
                input_records: vec![],
                read_preconditions: vec![],
                // The legacy single-name map cannot represent the Search proof.
                profile_fingerprints: BTreeMap::from([(
                    "primary".into(),
                    summary.profile_fingerprint,
                )]),
                scope_payload_hash: Some(hash(scope)),
            },
            config_fingerprint: summary.config_fingerprint,
            input_fingerprint: hash([]),
            limits: LifetimeLimits {
                requests,
                concurrency,
                ..Default::default()
            },
            tasks: vec![],
            prior_accounting: PriorAccounting::None,
        };
        spec.input_fingerprint = lwiki::jobs::tasks::input_fingerprint(&spec).unwrap();
        let job = JobLedger::new(
            fs.clone(),
            spec.vault_id.clone(),
            spec.run_id.clone(),
            options.clone(),
        )
        .unwrap();
        let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(2)).unwrap();
        job.create(&writer, spec).unwrap();
        drop(writer);
        job.start().unwrap();
        std::fs::create_dir_all(root.join("runs/run_research_dispatch/inputs")).unwrap();
        let mut tasks = vec![];
        for (index, role) in roles.iter().enumerate() {
            let service = if *role == Capability::Search {
                &search
            } else {
                &generation
            };
            let operation = if *role == Capability::Search {
                RemoteOperation::Search {
                    query: format!("bounded {index}"),
                    count: 2,
                    page: 0,
                }
            } else {
                RemoteOperation::Generate {
                    instructions: "Return JSON".into(),
                    data: format!("bounded {index}"),
                    output_schema: json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
                    max_output_tokens: 16,
                }
            };
            let input = RemoteInput {
                version: 1,
                operation,
            };
            let bytes = canonical_json(&input).unwrap();
            let path =
                VaultRelativePath::new(format!("runs/run_research_dispatch/inputs/{index}.json"))
                    .unwrap();
            private(&fs.root().resolve(&path).unwrap(), &bytes);
            let fp = task_fingerprints(service, &input).unwrap();
            let mut task = TaskSpec {
                key: hash([]),
                stage: if *role == Capability::Search {
                    TaskStage::Discover
                } else {
                    TaskStage::AssessGaps
                },
                capability: Some(*role),
                priority: if index == 0 { 9 } else { 1 },
                dependencies: vec![],
                input_hash: fp.input,
                prompt_hash: fp.prompt,
                schema_hash: fp.schema,
                model_hash: Some(fp.model),
                settings_hash: fp.settings,
                source_bindings: vec![],
                input: BoundedPayloadRef {
                    path,
                    hash: hash(&bytes),
                    byte_len: bytes.len() as u64,
                },
            };
            task.key = lwiki::jobs::tasks::task_key(&task).unwrap();
            tasks.push(task);
        }
        job.admit_research_frontier(EventPayload::ResearchFrontierAdmitted {
            version: 1,
            epoch: 0,
            prior_revision: 0,
            admission_id: hash("batch-frontier"),
            round: 1,
            origins: vec![],
            tasks: tasks.clone(),
            task_origins: vec![],
            parent_outputs: vec![],
        })
        .unwrap();
        let broker = Arc::new(CredentialBroker::new(CredentialOptions {
            clock: clock.clone(),
            inputs: Arc::new(Inputs(AtomicUsize::new(0))),
            runner: Arc::new(Runner {
                calls: AtomicUsize::new(0),
                hook: None,
            }),
        }));
        Self {
            _temp: temp,
            fs,
            generation,
            search,
            job,
            options,
            clock,
            broker,
            tasks,
        }
    }
    fn work(&self) -> Vec<ResearchDispatchWork<'_>> {
        self.tasks
            .iter()
            .rev()
            .map(|task| ResearchDispatchWork {
                task_key: task.key.clone(),
                service: Some(if task.capability == Some(Capability::Search) {
                    &self.search
                } else {
                    &self.generation
                }),
                purpose: DispatchPurpose::Task,
            })
            .collect()
    }
    fn dispatcher(&self, mock: Arc<Mock>) -> Dispatcher {
        Dispatcher::new(
            self.fs.clone(),
            DispatchOptions {
                broker: self.broker.clone(),
                transport: mock,
                jitter: Arc::new(ZeroJitter),
            },
        )
    }
}
struct Mock {
    ledger: JobLedger,
    expected_reservations: usize,
    calls: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    completed: Mutex<Vec<Blake3Hash>>,
    retry: bool,
    fail_search: bool,
    later_completed: tokio::sync::Notify,
}
impl Mock {
    fn new(f: &Fixture, expected_reservations: usize, retry: bool) -> Arc<Self> {
        Arc::new(Self {
            ledger: JobLedger::new(
                f.fs.clone(),
                id("vault_test"),
                id("run_research_dispatch"),
                f.options.clone(),
            )
            .unwrap(),
            expected_reservations,
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            completed: Mutex::new(vec![]),
            retry,
            fail_search: false,
            later_completed: tokio::sync::Notify::new(),
        })
    }
}
impl Transport for Mock {
    fn execute<'a>(
        &'a self,
        request: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        let summary = request.summary();
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let i = self.ledger.inspect().unwrap();
            assert_eq!(
                i.attempts.len(),
                self.expected_reservations + if self.retry { call } else { 0 },
                "all reservations precede transport entry"
            );
            let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(count, Ordering::SeqCst);
            if self.expected_reservations == 2 && !self.retry && call == 0 {
                tokio::time::timeout(Duration::from_secs(10), self.later_completed.notified())
                    .await
                    .expect("second reserved request must run concurrently");
            } else {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.completed
                .lock()
                .unwrap()
                .push(summary.attempt.task_key.clone());
            if call > 0 {
                self.later_completed.notify_one();
            }
            if self.fail_search && summary.role == ServiceRole::Search {
                return Err(TransportFailure::new(TransportFailureCode::Timeout));
            }
            if self.retry && call == 0 {
                return Ok(TransportReply::new(
                    429,
                    vec![("Retry-After".into(), "1".into())],
                    vec![],
                )
                .unwrap());
            }
            let value = if summary.role == ServiceRole::Search {
                json!({"web":{"results":[]}})
            } else {
                json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"{\"ok\":true}"}}],"usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18,"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":2}}})
            };
            Ok(TransportReply::new(200, vec![], serde_json::to_vec(&value).unwrap()).unwrap())
        })
    }
}
fn ordered(f: &Fixture) -> Vec<Blake3Hash> {
    let mut tasks = f.tasks.iter().collect::<Vec<_>>();
    tasks.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.key.cmp(&b.key)));
    tasks.into_iter().map(|t| t.key.clone()).collect()
}
#[test]
fn final_request_uses_stable_priority_and_key_before_network() {
    let f = Fixture::new(&[Capability::Generate; 3], 1, 2);
    let mock = Mock::new(&f, 1, false);
    let d = f.dispatcher(mock.clone());
    let batch = d
        .execute_ready_batch(&f.job, &f.work(), &PublicFetchOptions::default())
        .unwrap();
    assert!(batch.stop.is_some());
    assert_eq!(batch.outcomes.len(), 1);
    assert_eq!(batch.outcomes[0].task_key, ordered(&f)[0]);
    assert!(matches!(
        &batch.outcomes[0].outcome,
        ResearchDispatchOutcome::Remote(Ok(_))
    ));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert!(d.network_used());
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
}
#[test]
fn dual_role_profile_runs_concurrently_and_returns_reservation_order() {
    let f = Fixture::new(&[Capability::Generate, Capability::Search], 2, 2);
    let mock = Mock::new(&f, 2, false);
    let d = f.dispatcher(mock.clone());
    let batch = d
        .execute_ready_batch(&f.job, &f.work(), &PublicFetchOptions::default())
        .unwrap();
    assert!(batch.stop.is_none());
    assert_eq!(
        batch
            .outcomes
            .iter()
            .map(|r| r.task_key.clone())
            .collect::<Vec<_>>(),
        ordered(&f)
    );
    assert!(
        batch
            .outcomes
            .iter()
            .all(|r| matches!(r.outcome, ResearchDispatchOutcome::Remote(Ok(_))))
    );
    assert_eq!(mock.peak.load(Ordering::SeqCst), 2);
    assert_ne!(*mock.completed.lock().unwrap(), ordered(&f));
    let i = f.job.inspect().unwrap();
    assert!(
        i.attempts
            .iter()
            .all(|a| a.bound.profile_fingerprint.is_some() && a.bound.codec.is_some())
    );
    assert_ne!(
        i.attempts[0].bound.profile_fingerprint,
        i.attempts[1].bound.profile_fingerprint
    );
}
#[test]
fn wrong_role_fails_before_any_reservation_or_network() {
    let f = Fixture::new(&[Capability::Generate, Capability::Search], 2, 2);
    let mock = Mock::new(&f, 0, false);
    let d = f.dispatcher(mock.clone());
    let mut work = f.work();
    for item in &mut work {
        item.service = Some(&f.generation);
    }
    assert!(
        d.execute_ready_batch(&f.job, &work, &PublicFetchOptions::default())
            .is_err()
    );
    assert!(f.job.inspect().unwrap().attempts.is_empty());
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    assert!(!d.network_used());
}
#[test]
fn retry_after_is_durable_and_joins_a_later_wave() {
    let f = Fixture::new(&[Capability::Generate], 3, 1);
    let mock = Mock::new(&f, 1, true);
    let d = f.dispatcher(mock.clone());
    let before = f.clock.read().unwrap().utc_ms;
    let first = d
        .execute_ready_batch(&f.job, &f.work(), &PublicFetchOptions::default())
        .unwrap();
    let ResearchDispatchOutcome::Remote(Err(error)) = &first.outcomes[0].outcome else {
        panic!("expected retryable rejection")
    };
    assert!(matches!(
        error.retry,
        RetryDecision::After { delay_ms: 1000, .. }
    ));
    assert_eq!(
        mock.calls.load(Ordering::SeqCst),
        1,
        "no worker-local retry loop"
    );
    let inspected = f.job.inspect().unwrap();
    let not_before = inspected.research.as_ref().unwrap().retry_not_before[&f.tasks[0].key];
    assert!(not_before >= before + 1000);
    assert!(inspected.attempts[0].receipt.is_some());
    assert!(
        d.execute_ready_batch(&f.job, &f.work(), &PublicFetchOptions::default())
            .is_err()
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    f.clock.utc.fetch_add(1100, Ordering::SeqCst);
    f.clock.mono.fetch_add(1100, Ordering::SeqCst);
    let second = d
        .execute_ready_batch(&f.job, &f.work(), &PublicFetchOptions::default())
        .unwrap();
    assert!(matches!(
        &second.outcomes[0].outcome,
        ResearchDispatchOutcome::Remote(Ok(_))
    ));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 2);
}
#[test]
fn competing_dispatchers_cannot_race_the_final_request_slot() {
    let f = Fixture::new(&[Capability::Generate; 2], 1, 2);
    let mock = Mock::new(&f, 1, false);
    let d = f.dispatcher(mock.clone());
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let other = JobLedger::new(
            f.fs.clone(),
            id("vault_test"),
            id("run_research_dispatch"),
            f.options.clone(),
        )
        .unwrap();
        let dispatch = &d;
        let fixture = &f;
        let barrier_ref = &barrier;
        let handle = scope.spawn(move || {
            barrier_ref.wait();
            dispatch.execute_ready_batch(&other, &fixture.work(), &PublicFetchOptions::default())
        });
        barrier.wait();
        let _ = d.execute_ready_batch(&f.job, &f.work(), &PublicFetchOptions::default());
        let _ = handle.join().unwrap();
    });
    let i = f.job.inspect().unwrap();
    assert_eq!(i.budget.dispatched_requests, 1);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(i.attempts[0].attempt.task_key, ordered(&f)[0]);
}

#[test]
fn sibling_failure_preserves_every_paid_result_and_hold() {
    let f = Fixture::new(&[Capability::Generate, Capability::Search], 2, 2);
    let mut mock = Mock::new(&f, 2, false);
    Arc::get_mut(&mut mock).unwrap().fail_search = true;
    let d = f.dispatcher(mock.clone());
    let batch = d
        .execute_ready_batch(&f.job, &f.work(), &PublicFetchOptions::default())
        .unwrap();
    assert_eq!(batch.outcomes.len(), 2);
    let mut paid_success = 0;
    let mut unknown = 0;
    for result in &batch.outcomes {
        match &result.outcome {
            ResearchDispatchOutcome::Remote(Ok(out)) => {
                paid_success += 1;
                assert_eq!(out.attempt.task_key, result.task_key);
                assert_eq!(out.spool.attempt, out.attempt);
            }
            ResearchDispatchOutcome::Remote(Err(out)) => {
                unknown += 1;
                assert_eq!(out.disposition, DispatchDisposition::OutcomeUnknown);
                assert_eq!(out.attempt.as_ref().unwrap().task_key, result.task_key);
            }
            _ => panic!("unexpected public outcome"),
        }
    }
    assert_eq!((paid_success, unknown), (1, 1));
    let i = f.job.inspect().unwrap();
    assert_eq!(i.budget.dispatched_requests, 2);
    assert_eq!(i.attempts.len(), 2);
    assert_eq!(i.attempts.iter().filter(|a| a.spool.is_some()).count(), 1);
    assert!(
        i.attempts
            .iter()
            .any(|a| a.billing == BillingDisposition::UnknownReserved)
    );
}

struct PublicMock {
    ledger: JobLedger,
    preflight_failure: bool,
    entered: AtomicUsize,
}
impl lwiki::providers::public_fetch::PublicConnector for PublicMock {
    fn connect<'a>(
        &'a self,
        request: lwiki::providers::public_fetch::PinnedRequest,
    ) -> Result<lwiki::providers::public_fetch::ConnectionFuture<'a>> {
        if self.preflight_failure {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "fixture unavailable",
            ));
        }
        Ok(Box::pin(async move {
            assert_eq!(self.ledger.inspect().unwrap().attempts.len(), 1);
            self.entered.fetch_add(1, Ordering::SeqCst);
            request.observe_body_bytes(7);
            Ok(lwiki::providers::public_fetch::PublicReply::new(
                200,
                vec![("content-type".into(), "text/plain".into())],
                b"bounded".to_vec(),
            ))
        }))
    }
}
#[test]
fn public_fetch_reports_actual_entry_and_uses_effective_research_binding() {
    use lwiki::providers::public_fetch::{
        FetchLimits, PUBLIC_PROFILE, input_fingerprint, settings_fingerprint,
    };
    for preflight_failure in [true, false] {
        let f = Fixture::new(&[], 1, 1);
        let url = "https://1.1.1.1/bounded";
        let input = RemoteInput {
            version: 1,
            operation: RemoteOperation::Fetch {
                url: url.into(),
                limits: FetchLimits::default(),
            },
        };
        let bytes = canonical_json(&input).unwrap();
        let path = VaultRelativePath::new("runs/run_research_dispatch/inputs/fetch.json").unwrap();
        private(&f.fs.root().resolve(&path).unwrap(), &bytes);
        let mut task = TaskSpec {
            key: hash([]),
            stage: TaskStage::Capture,
            capability: Some(Capability::Fetch),
            priority: 0,
            dependencies: vec![],
            input_hash: input_fingerprint(&input).unwrap(),
            prompt_hash: None,
            schema_hash: None,
            model_hash: None,
            settings_hash: settings_fingerprint(),
            source_bindings: vec![],
            input: BoundedPayloadRef {
                path,
                hash: hash(&bytes),
                byte_len: bytes.len() as u64,
            },
        };
        task.key = lwiki::jobs::tasks::task_key(&task).unwrap();
        f.job
            .admit_research_frontier(EventPayload::ResearchFrontierAdmitted {
                version: 1,
                epoch: 0,
                prior_revision: 1,
                admission_id: hash("fetch-frontier"),
                round: 1,
                origins: vec![ResearchOriginV1 {
                    key: hash(url),
                    url: url.into(),
                    round: 1,
                }],
                tasks: vec![task.clone()],
                task_origins: vec![ResearchTaskOriginV1 {
                    task_key: task.key.clone(),
                    origin_key: hash(url),
                    parent_capture: None,
                }],
                parent_outputs: vec![],
            })
            .unwrap();
        // Research permits Public Fetch independently of the legacy name map.
        assert!(
            !f.job
                .inspect()
                .unwrap()
                .spec
                .scope
                .profile_fingerprints
                .contains_key(PUBLIC_PROFILE)
        );
        let mock = Arc::new(PublicMock {
            ledger: JobLedger::new(
                f.fs.clone(),
                id("vault_test"),
                id("run_research_dispatch"),
                f.options.clone(),
            )
            .unwrap(),
            preflight_failure,
            entered: AtomicUsize::new(0),
        });
        let d = f.dispatcher(Mock::new(&f, 0, false));
        let options = PublicFetchOptions {
            connector: mock.clone(),
            ..Default::default()
        };
        let out = d.execute_public(&f.job, &task.key, &options);
        assert_eq!(out.is_ok(), !preflight_failure);
        assert_eq!(d.network_used(), !preflight_failure);
        assert_eq!(
            mock.entered.load(Ordering::SeqCst),
            usize::from(!preflight_failure)
        );
        let i = f.job.inspect().unwrap();
        let bound = &i.attempts[0].bound;
        assert_eq!(
            bound.config_fingerprint,
            i.research.as_ref().unwrap().binding.config_fingerprint
        );
        assert_eq!(
            bound.profile_fingerprint.as_ref(),
            Some(&settings_fingerprint())
        );
        assert!(bound.codec.is_none());
        if preflight_failure {
            assert_eq!(out.err().unwrap().disposition, DispatchDisposition::NotSent);
            assert_eq!(i.budget.dispatched_requests, 0);
        }
    }
}

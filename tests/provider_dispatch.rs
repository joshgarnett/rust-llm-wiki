use lwiki as library;
#[path = "fixtures/p16b/common.rs"]
mod common;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod wire_common;
use common::*;
use lwiki::{
    domain::ErrorCode,
    jobs::diagnostics::DiagnosticKind,
    jobs::*,
    providers::{dispatcher::Dispatcher, types::*},
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct NeverTransport(AtomicUsize);
impl Transport for NeverTransport {
    fn execute<'a>(
        &'a self,
        _: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(TransportFailure::new(TransportFailureCode::Unavailable)) })
    }
}
#[test]
fn offline_dry_run_before_secret_resolution_zero_dns_http_helpers() {
    for policy in [
        ExecutionPolicy {
            offline: true,
            ..ExecutionPolicy::default()
        },
        ExecutionPolicy {
            dry_run: true,
            ..ExecutionPolicy::default()
        },
    ] {
        // Bootstrap explicitly online, then reopen with the prohibited execution policy.
        let c = case(
            "https://gateway.example/v1/embeddings",
            true,
            ExecutionPolicy::default(),
            |_| {},
            None,
        );
        let job = JobLedger::new(
            c.fs.clone(),
            c.spec.vault_id.clone(),
            c.spec.run_id.clone(),
            JobOptions {
                clock: c.clock.clone(),
                fault: None,
                cancel: CancellationToken::default(),
                policy,
                lock_timeout_ms: 5000,
            },
        )
        .unwrap();
        let transport = Arc::new(NeverTransport(AtomicUsize::new(0)));
        let d = Dispatcher::new(
            c.fs.clone(),
            DispatchOptions {
                broker: c.broker.clone(),
                transport: transport.clone(),
                jitter: Arc::new(ZeroJitter),
            },
        );
        let before = tree(c.temp.path());
        let f = d
            .execute(
                &job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert_eq!(f.error.code, ErrorCode::OfflineUnavailable);
        assert_eq!(transport.0.load(Ordering::SeqCst), 0);
        assert_eq!(c.runner.calls.load(Ordering::SeqCst), 0);
        assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
        assert_eq!(tree(c.temp.path()), before);
    }
}
#[test]
fn noncanonical_retained_descriptor_refuses_before_auth_or_reservation() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    let bytes = std::fs::read(c.fs.root().path().join("inputs/task.json")).unwrap();
    let input: RemoteInput = serde_json::from_slice(&bytes).unwrap();
    assert_ne!(bytes, lwiki::graph::packet::canonical_json(&input).unwrap());
    let transport = Arc::new(NeverTransport(AtomicUsize::new(0)));
    let d = Dispatcher::new(
        c.fs.clone(),
        DispatchOptions {
            broker: c.broker.clone(),
            transport: transport.clone(),
            jitter: Arc::new(ZeroJitter),
        },
    );
    let before = tree(c.temp.path());
    let f = d
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(f.error.code, ErrorCode::RecordInvalid);
    assert_eq!(transport.0.load(Ordering::SeqCst), 0);
    assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
    assert_eq!(c.runner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(tree(c.temp.path()), before);
    assert!(c.job.inspect().unwrap().attempts.is_empty());
}
#[test]
fn malformed_retained_input_never_uses_caller_hash_or_transport() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    std::fs::write(
        c.fs.root().path().join("inputs/task.json"),
        b"forged payload",
    )
    .unwrap();
    let d = Dispatcher::native(c.fs.clone(), c.broker.clone());
    let f = d
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(f.error.code, ErrorCode::FreshnessConflict);
    assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
    assert!(c.job.inspect().unwrap().attempts.is_empty());
}
#[test]
fn bounded_mock_reply_constructor_and_errors_do_not_echo_body() {
    assert!(TransportReply::new(200, vec![("Bad Name".into(), "secret".into())], vec![]).is_err());
    assert!(TransportReply::new(200, vec![], vec![1; 8 * 1024 * 1024 + 1]).is_err());
    assert!(TransportReply::new(200, vec![("X-Test".into(), "x".repeat(4097))], vec![]).is_err());
}

struct HttpBadRequest(AtomicUsize);
impl Transport for HttpBadRequest {
    fn execute<'a>(
        &'a self,
        _: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(TransportReply::new(
                400,
                vec![],
                b"private gateway error with echoed credential".to_vec(),
            )
            .unwrap())
        })
    }
}

#[test]
fn http_error_body_requires_opt_in_and_explicit_raw_inspection() {
    for retain in [false, true] {
        let c = wire_common::case(
            ServiceRole::Generate,
            RemoteInput {
                version: 1,
                operation: RemoteOperation::Generate {
                    instructions: "Return JSON".into(),
                    data: "bounded fixture".into(),
                    output_schema: json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
                    max_output_tokens: 16,
                },
            },
            "test-model",
            "https://gateway.example/chat",
            "",
            |_| {},
        );
        let transport = Arc::new(HttpBadRequest(AtomicUsize::new(0)));
        let dispatcher = Dispatcher::new(
            c.fs.clone(),
            DispatchOptions {
                broker: c.broker.clone(),
                transport: transport.clone(),
                jitter: Arc::new(ZeroJitter),
            },
        )
        .with_http_error_diagnostics(retain);
        let failure = dispatcher
            .execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);
        assert_eq!(failure.disposition, DispatchDisposition::Rejected);
        assert!(!format!("{:?}", failure.error).contains("private gateway"));
        let inspection = c.job.inspect().unwrap();
        let attempt = &inspection.attempts[0].attempt;
        let safe = c
            .job
            .inspect_diagnostic(attempt, DiagnosticKind::HttpError, false)
            .unwrap();
        assert_eq!(safe.is_some(), retain);
        if let Some(safe) = safe {
            assert_eq!(safe.reference.reason, "http_400");
            assert_eq!(
                safe.reference.observed_bytes,
                b"private gateway error with echoed credential".len() as u64
            );
            assert!(safe.body_utf8.is_none());
            let raw = c
                .job
                .inspect_diagnostic(attempt, DiagnosticKind::HttpError, true)
                .unwrap()
                .unwrap();
            assert_eq!(
                raw.body_utf8.as_deref(),
                Some("private gateway error with echoed credential")
            );
            assert!(
                c.job
                    .prune_diagnostic(attempt, DiagnosticKind::HttpError)
                    .is_err()
            );
        }
    }
}

struct RetainedReply {
    calls: AtomicUsize,
    body: serde_json::Value,
    root: std::path::PathBuf,
}
impl Transport for RetainedReply {
    fn execute<'a>(
        &'a self,
        _: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        assert_eq!(self.calls.fetch_add(1, Ordering::SeqCst), 0);
        // The original codec must already be durable when transport is entered.
        assert_eq!(
            std::fs::read_dir(self.root.join(".wiki/state/provider-codecs"))
                .unwrap()
                .count(),
            1
        );
        Box::pin(async {
            Ok(TransportReply::new(200, vec![], serde_json::to_vec(&self.body).unwrap()).unwrap())
        })
    }
}

#[test]
fn production_codecs_reopen_embeddings_api_and_probes_without_current_config_or_auth() {
    use lwiki::domain::VaultRelativePath;
    use serde_json::json;
    for role in [ServiceRole::Embed, ServiceRole::Generate] {
        for probe in [false, true] {
            let (input, body, url) = match role {
                ServiceRole::Embed => (
                    RemoteInput {
                        version: 1,
                        operation: RemoteOperation::Embed {
                            inputs: vec![EmbeddingInput {
                                input_hash: hash("content"),
                                utf8: "content".into(),
                            }],
                            expected_dimensions: Some(2),
                            representation_fingerprint: hash("render.v1"),
                        },
                    },
                    json!({"model":"test-model","data":[{"index":0,"embedding":[1,2]}],
                        "usage":{"prompt_tokens":5,"total_tokens":5}}),
                    "https://gateway.example/embeddings",
                ),
                ServiceRole::Generate => (
                    RemoteInput {
                        version: 1,
                        operation: RemoteOperation::Generate {
                            instructions: "Return JSON".into(),
                            data: "content".into(),
                            output_schema: json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
                            max_output_tokens: 16,
                        },
                    },
                    json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop",
                        "message":{"role":"assistant","content":"{\"ok\":true}"}}],
                        "usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18}}),
                    "https://gateway.example/chat",
                ),
            };
            let c = wire_common::case(role, input, "test-model", url, "", |spec| {
                let task = &mut spec.tasks[0];
                task.input.path = VaultRelativePath::new(if probe {
                    format!("runs/{}/inputs/probe.json", spec.run_id)
                } else if role == ServiceRole::Embed {
                    format!(
                        ".wiki/state/embedding-inputs/{}.json",
                        task.input.hash.hex()
                    )
                } else {
                    format!(
                        ".wiki/cache/generation/inputs/{}.json",
                        task.input.hash.hex()
                    )
                })
                .unwrap();
                task.stage = if probe {
                    TaskStage::Probe
                } else if role == ServiceRole::Embed {
                    TaskStage::Embed
                } else {
                    TaskStage::Extract
                };
                if probe {
                    task.capability = Some(Capability::Probe);
                }
            });
            let transport = Arc::new(RetainedReply {
                calls: AtomicUsize::new(0),
                body,
                root: c.fs.root().path().to_owned(),
            });
            let dispatcher = Dispatcher::new(
                c.fs.clone(),
                DispatchOptions {
                    broker: c.broker.clone(),
                    transport: transport.clone(),
                    jitter: Arc::new(ZeroJitter),
                },
            );
            let purpose = if probe {
                DispatchPurpose::Probe { role }
            } else {
                DispatchPurpose::Task
            };
            let key = &c.spec.tasks[0].key;
            let out = dispatcher
                .execute(&c.job, &c.trusted, key, purpose)
                .unwrap_or_else(|f| panic!("{}: {}", f.error.code, f.error.message));
            let inspection = c.job.inspect().unwrap();
            let codec = inspection.attempts[0].bound.codec.as_ref().unwrap();
            let codec_path = c.fs.root().resolve(&codec.path).unwrap();
            let bytes = std::fs::read(&codec_path).unwrap();
            assert_eq!(hash(&bytes), codec.hash);
            assert_eq!(bytes.len() as u64, codec.byte_len);
            let codec_tree = tree(&c.fs.root().path().join(".wiki/state/provider-codecs"));
            wire_common::private(&c.config, b"configuration deliberately unavailable");
            assert!(lwiki::config::providers::ProviderConfig::load(&c.config).is_err());
            let reopened = JobLedger::new(
                c.fs.clone(),
                c.spec.vault_id.clone(),
                c.spec.run_id.clone(),
                JobOptions {
                    clock: c.clock.clone(),
                    fault: None,
                    cancel: CancellationToken::default(),
                    policy: ExecutionPolicy {
                        offline: true,
                        ..ExecutionPolicy::default()
                    },
                    lock_timeout_ms: 5000,
                },
            )
            .unwrap();
            let auth_calls = c.inputs.0.load(Ordering::SeqCst);
            let decoded = dispatcher
                .decode_retained(&reopened, &c.trusted, key, purpose, &out.attempt)
                .unwrap();
            assert_eq!(
                serde_json::to_value(decoded).unwrap(),
                serde_json::to_value(&out.output).unwrap()
            );
            assert_eq!(
                tree(&c.fs.root().path().join(".wiki/state/provider-codecs")),
                codec_tree
            );
            let after = reopened.inspect().unwrap();
            assert_eq!(after.budget, inspection.budget);
            assert_eq!(after.attempts.len(), 1);
            // A missing cached descriptor or altered codec fails locally, preserving the paid response.
            let descriptor = c.fs.root().resolve(&c.spec.tasks[0].input.path).unwrap();
            let descriptor_bytes = std::fs::read(&descriptor).unwrap();
            std::fs::remove_file(&descriptor).unwrap();
            assert_eq!(
                dispatcher
                    .decode_retained(&reopened, &c.trusted, key, purpose, &out.attempt)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::RecoveryRequired
            );
            wire_common::private(&descriptor, descriptor_bytes);
            wire_common::private(&codec_path, b"altered codec");
            assert_eq!(
                dispatcher
                    .decode_retained(&reopened, &c.trusted, key, purpose, &out.attempt)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::RecoveryRequired
            );
            let online = JobLedger::new(
                c.fs.clone(),
                c.spec.vault_id.clone(),
                c.spec.run_id.clone(),
                JobOptions {
                    clock: c.clock.clone(),
                    fault: None,
                    cancel: CancellationToken::default(),
                    policy: ExecutionPolicy::default(),
                    lock_timeout_ms: 5000,
                },
            )
            .unwrap();
            assert_eq!(
                dispatcher
                    .execute(&online, &c.trusted, key, purpose)
                    .err()
                    .unwrap()
                    .error
                    .code,
                ErrorCode::RecoveryRequired
            );
            assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
            assert_eq!(c.inputs.0.load(Ordering::SeqCst), auth_calls);
            assert_eq!(c.runner.calls.load(Ordering::SeqCst), 0);
        }
    }
}

struct MalformedGeneration(AtomicUsize);
impl Transport for MalformedGeneration {
    fn execute<'a>(
        &'a self,
        _: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        assert_eq!(
            self.0.fetch_add(1, Ordering::SeqCst),
            0,
            "unexpected paid resend"
        );
        Box::pin(async {
            Ok(TransportReply::new(
                200,
                vec![],
                serde_json::to_vec(&serde_json::json!({
                    "model":"test-model",
                    "choices":[{"index":0,"finish_reason":"stop","message":{
                        "role":"assistant","content":"private-invalid-model-output"
                    }}],
                    "usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18,
                        "prompt_tokens_details":{"cached_tokens":3},
                        "completion_tokens_details":{"reasoning_tokens":2}}
                }))
                .unwrap(),
            )
            .unwrap())
        })
    }
}

#[test]
fn malformed_paid_generation_keeps_original_failure_and_one_receipt_after_reopen() {
    use std::{
        collections::BTreeMap,
        num::NonZeroU64,
        time::{SystemTime, UNIX_EPOCH},
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let classes = [
        BillableClass::Input,
        BillableClass::CachedInput,
        BillableClass::Output,
        BillableClass::Reasoning,
    ];
    let mut card = RateCard {
        id: "receipt-regression".into(),
        version: 1,
        fingerprint: hash([]),
        currency: Currency::new("USD").unwrap(),
        validity: PriceValidity::DispatchLocked {
            valid_from_utc_ms: now - 10_000,
            valid_until_utc_ms: now + 3_600_000,
        },
        request_fee_nanounits: 7,
        rates: classes
            .into_iter()
            .map(|class| {
                (
                    class,
                    Rate {
                        price_nanounits: 1,
                        per_units: NonZeroU64::new(1).unwrap(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
    };
    card.fingerprint = lwiki::jobs::budgets::rate_card_fingerprint(&card).unwrap();
    let rates = format!(
        "rate_card={{id=\"receipt-regression\",version=1,fingerprint={},currency=\"USD\",validity={{pricing=\"dispatch_locked\",valid_from_utc_ms={},valid_until_utc_ms={}}},request_fee_nanounits=7,rates={{input={{price_nanounits=1,per_units=1}},cached_input={{price_nanounits=1,per_units=1}},output={{price_nanounits=1,per_units=1}},reasoning={{price_nanounits=1,per_units=1}}}}}}\n",
        quote(card.fingerprint.as_str()),
        now - 10_000,
        now + 3_600_000,
    );
    let c = wire_common::case(
        ServiceRole::Generate,
        RemoteInput {
            version: 1,
            operation: RemoteOperation::Generate {
                instructions: "Return JSON".into(),
                data: "bounded fixture".into(),
                output_schema: serde_json::json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
                max_output_tokens: 16,
            },
        },
        "test-model",
        "https://gateway.example/chat",
        &rates,
        |_| {},
    );
    let transport = Arc::new(MalformedGeneration(AtomicUsize::new(0)));
    let dispatcher = Dispatcher::new(
        c.fs.clone(),
        DispatchOptions {
            broker: c.broker.clone(),
            transport: transport.clone(),
            jitter: Arc::new(ZeroJitter),
        },
    );
    let failure = dispatcher
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(failure.disposition, DispatchDisposition::Rejected);
    assert_eq!(failure.retry, RetryDecision::Never);
    assert_eq!(failure.error.code, ErrorCode::ProviderResponse);
    let original = failure.error.clone();
    assert!(!format!("{original:?}").contains("private-invalid-model-output"));
    let plan = failure.materialization.unwrap();
    let retry_plan = MaterializationPlan {
        attempt: plan.attempt.clone(),
        receipt: plan.receipt.clone(),
        draft: plan.draft.clone(),
    };
    assert_eq!(plan.receipt.output_disposition, OutputDisposition::Rejected);
    assert_eq!(
        plan.receipt.computed_cost,
        KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 25))
    );
    let attempt = plan.attempt.clone();
    let before = c.job.inspect().unwrap();
    assert_eq!(before.attempts[0].phase, AttemptPhase::Settled);
    let receipt = before.attempts[0].receipt.clone().unwrap();
    let receipt_bytes = std::fs::read(c.fs.root().resolve(&receipt.path).unwrap()).unwrap();
    let changes = tree(&c.fs.root().path().join("changes"));
    // Exercise the old failure-handler pattern with the exact plan execute has
    // already committed. It must not replace the original error with Conflict.
    lwiki::jobs::settle_receipt(&c.fs, &c.job, plan).unwrap();
    let reopened = JobLedger::new(
        c.fs.clone(),
        c.spec.vault_id.clone(),
        c.spec.run_id.clone(),
        JobOptions {
            clock: c.clock.clone(),
            fault: None,
            cancel: CancellationToken::default(),
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        },
    )
    .unwrap();
    lwiki::jobs::settle_receipt(&c.fs, &reopened, retry_plan).unwrap();
    let recovered_failure = dispatcher
        .recover_response(&reopened, &c.trusted, &c.spec.tasks[0].key, &attempt)
        .err()
        .unwrap();
    assert_eq!(recovered_failure.error.code, original.code);
    assert_eq!(recovered_failure.error.message, original.message);
    assert_eq!(
        recovered_failure.error.details["reason"],
        original.details["reason"]
    );
    assert!(recovered_failure.materialization.is_none());
    assert_eq!(transport.0.load(Ordering::SeqCst), 1);
    let after = reopened.inspect().unwrap();
    assert_eq!(after.attempts.len(), 1);
    assert_eq!(after.attempts[0].receipt.as_ref(), Some(&receipt));
    assert_eq!(after.budget, before.budget);
    assert_eq!(after.budget.dispatched_requests, 1);
    assert_eq!(after.budget.settled.requests, 1);
    assert_eq!(after.budget.known_costs[&Currency::new("USD").unwrap()], 25);
    assert_eq!(
        std::fs::read(c.fs.root().resolve(&receipt.path).unwrap()).unwrap(),
        receipt_bytes
    );
    assert_eq!(tree(&c.fs.root().path().join("changes")), changes);
    assert_eq!(
        std::fs::read_dir(
            c.fs.root()
                .path()
                .join(format!("runs/{}/events", c.spec.run_id))
        )
        .unwrap()
        .filter(|entry| entry
            .as_ref()
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("run_event_receipt_"))
        .count(),
        1
    );
}

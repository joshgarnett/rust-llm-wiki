use lwiki as library;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod common;
use common::*;
use lwiki::{
    domain::*,
    jobs::*,
    providers::{dispatcher::Dispatcher, types::*},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

struct Mock {
    calls: AtomicUsize,
    body: Mutex<Option<Value>>,
}
impl Mock {
    fn new(body: Value) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            body: Mutex::new(Some(body)),
        })
    }
}
impl Transport for Mock {
    fn execute<'a>(
        &'a self,
        request: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(request.summary().request_bytes > 0);
        let body = self
            .body
            .lock()
            .unwrap()
            .take()
            .expect("unexpected repair or retry");
        Box::pin(async move {
            Ok(TransportReply::new(200, vec![], serde_json::to_vec(&body).unwrap()).unwrap())
        })
    }
}
fn dispatcher(c: &Case, m: Arc<Mock>) -> Dispatcher {
    Dispatcher::new(
        c.fs.clone(),
        DispatchOptions {
            broker: c.broker.clone(),
            transport: m,
            jitter: Arc::new(ZeroJitter),
        },
    )
}
fn embeddings() -> RemoteInput {
    RemoteInput {
        version: 1,
        operation: RemoteOperation::Embed {
            inputs: vec![
                EmbeddingInput {
                    input_hash: hash("é"),
                    utf8: "é".into(),
                },
                EmbeddingInput {
                    input_hash: hash("é"),
                    utf8: "é".into(),
                },
            ],
            expected_dimensions: Some(2),
            representation_fingerprint: hash("render.v1"),
        },
    }
}
fn generation() -> RemoteInput {
    RemoteInput {
        version: 1,
        operation: RemoteOperation::Generate {
            instructions: "Return JSON".into(),
            data: "data".into(),
            output_schema: json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
            max_output_tokens: 16,
        },
    }
}
fn chat() -> Value {
    json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"{\"ok\":true}"}}],"usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18,"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":2}}})
}
fn rates(classes: &[BillableClass]) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut card = RateCard {
        id: "fixture".into(),
        version: 1,
        fingerprint: hash([]),
        currency: Currency::new("USD").unwrap(),
        validity: PriceValidity::DispatchLocked {
            valid_from_utc_ms: now - 10000,
            valid_until_utc_ms: now + 3_600_000,
        },
        request_fee_nanounits: 11,
        rates: classes
            .iter()
            .map(|c| {
                (
                    *c,
                    Rate {
                        price_nanounits: 5,
                        per_units: NonZeroU64::new(3).unwrap(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
    };
    card.fingerprint = lwiki::jobs::budgets::rate_card_fingerprint(&card).unwrap();
    let rates = classes
        .iter()
        .map(|class| {
            format!(
                "{}={{price_nanounits=5,per_units=3}}",
                serde_json::to_value(class).unwrap().as_str().unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "rate_card={{id=\"fixture\",version=1,fingerprint={},currency=\"USD\",validity={{pricing=\"dispatch_locked\",valid_from_utc_ms={},valid_until_utc_ms={}}},request_fee_nanounits=11,rates={{{rates}}}}}\n",
        quote(card.fingerprint.as_str()),
        now - 10000,
        now + 3_600_000
    )
}
fn metadata(c: &Case, a: &AttemptInspection) -> ResponseMetadata {
    let path =
        c.fs.root()
            .resolve(&a.spool.as_ref().unwrap().metadata.path)
            .unwrap();
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn commit_success(c: &Case, out: &DispatchOutcome) {
    let operation = out.materialization.draft.operations.last().unwrap();
    let receipt = DurableOutputRef {
        record: RecordRef {
            vault_id: c.spec.vault_id.clone(),
            record_id: out.materialization.receipt.receipt_id.clone(),
            expected_kind: RecordKind::RunEvent,
        },
        path: operation.target.clone(),
        hash: hash(operation.proposed.as_ref().unwrap()),
    };
    let writer =
        library::vault::WriterPermit::acquire(c.fs.root(), std::time::Duration::from_secs(1))
            .unwrap();
    let engine = library::changes::ChangeEngine::new(c.fs.clone()).unwrap();
    let catalog = library::catalog::Catalog::new(c.fs.clone(), c.spec.vault_id.clone());
    let prepared = engine
        .prepare(&writer, out.materialization.draft.clone())
        .unwrap()
        .prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &library::catalog::CatalogGraphValidator,
            &catalog,
        )
        .unwrap();
    drop(writer);
    c.job
        .outputs_committed(&out.attempt, &prepared, receipt, vec![], vec![])
        .unwrap();
}

#[test]
fn production_embedding_dispatch_validates_repeated_positions_and_accounts_probe_role() {
    for probe in [false, true] {
        let c = case(
            ServiceRole::Embed,
            embeddings(),
            "test-model",
            "https://gateway.example/embeddings",
            "",
            |s| {
                if probe {
                    s.tasks[0].capability = Some(Capability::Probe)
                }
            },
        );
        let m = Mock::new(
            json!({"model":"test-model","data":[{"index":1,"embedding":[3,4]},{"index":0,"embedding":[1,2]}],"usage":{"prompt_tokens":5,"total_tokens":5}}),
        );
        let out = dispatcher(&c, m.clone())
            .execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                if probe {
                    DispatchPurpose::Probe {
                        role: ServiceRole::Embed,
                    }
                } else {
                    DispatchPurpose::Task
                },
            )
            .unwrap_or_else(|f| panic!("{}", f.error.code));
        commit_success(&c, &out);
        if probe {
            assert!(matches!(
                out.output,
                ValidatedOutput::Probe {
                    role: ServiceRole::Embed
                }
            ));
        } else {
            let ValidatedOutput::Embeddings { vectors, .. } = out.output else {
                panic!()
            };
            assert_eq!(vectors, vec![vec![1., 2.], vec![3., 4.]]);
        }
        let inspect = c.job.inspect().unwrap();
        assert_eq!(inspect.budget.dispatched_requests, 1);
        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
        assert!(inspect.attempts[0].receipt.is_some());
        assert!(inspect.attempts[0].cache_outputs.is_empty());
        let KnownOrUnknown::Known(u) = metadata(&c, &inspect.attempts[0]).usage else {
            panic!()
        };
        assert_eq!(
            u.billable_units[&BillableClass::Input],
            KnownOrUnknown::Known(5)
        );
    }
}

#[test]
fn production_generation_failures_keep_actual_paid_receipts_without_repair() {
    let rate = rates(&[
        BillableClass::Input,
        BillableClass::CachedInput,
        BillableClass::Output,
        BillableClass::Reasoning,
    ]);
    for failure in 0..5 {
        let c = case(
            ServiceRole::Generate,
            generation(),
            "test-model",
            "https://gateway.example/chat",
            &rate,
            |_| {},
        );
        let mut v = chat();
        match failure {
            0 => v["choices"][0]["finish_reason"] = "length".into(),
            1 => v["choices"][0]["message"]["refusal"] = "refused".into(),
            2 => v["choices"][0]["message"]["tool_calls"] = json!([{}]),
            3 => v["choices"][0]["message"]["content"] = "prose {\"ok\":true}".into(),
            _ => v["choices"][0]["message"]["content"] = "{\"ok\":\"bad\"}".into(),
        }
        let m = Mock::new(v);
        let f = dispatcher(&c, m.clone())
            .execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert!(f.spool.is_some());
        assert!(f.materialization.is_some());
        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
        let inspect = c.job.inspect().unwrap();
        assert_eq!(inspect.attempts[0].phase, AttemptPhase::Settled);
        assert!(inspect.attempts[0].receipt.is_some());
        assert_eq!(
            inspect.budget.known_costs[&Currency::new("USD").unwrap()],
            42
        );
        assert!(inspect.budget.guarantee_intact);
        assert_eq!(
            metadata(&c, &inspect.attempts[0]).computed_cost,
            KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 42))
        );
    }
}

#[test]
fn production_missing_usage_remains_unknown_and_curated_model_breach_stops_guarantee() {
    for mismatch in [false, true] {
        let c = case(
            ServiceRole::Embed,
            embeddings(),
            if mismatch {
                "text-embedding-3-small"
            } else {
                "test-model"
            },
            if mismatch {
                "https://api.openai.com/v1/embeddings"
            } else {
                "https://gateway.example/embeddings"
            },
            &rates(&[BillableClass::Input]),
            |_| {},
        );
        let mut v = json!({"model":"test-model","data":[{"index":0,"embedding":[1,2]},{"index":1,"embedding":[3,4]}]});
        if mismatch {
            v["usage"] = json!({"prompt_tokens":5,"total_tokens":5});
        }
        let m = Mock::new(v);
        let result = dispatcher(&c, m.clone()).execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        );
        if let Ok(out) = &result {
            commit_success(&c, out);
        }
        assert_eq!(result.is_err(), mismatch);
        let inspect = c.job.inspect().unwrap();
        assert_eq!(inspect.budget.guarantee_intact, !mismatch);
        assert_eq!(
            inspect.attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
        assert!(inspect.attempts[0].receipt.is_some());
        let md = metadata(&c, &inspect.attempts[0]);
        assert_eq!(md.computed_cost, KnownOrUnknown::Unknown);
        if mismatch {
            assert_eq!(md.failure_code.as_deref(), Some("wire_contract_violation"));
            let KnownOrUnknown::Known(u) = md.usage else {
                panic!()
            };
            assert_eq!(
                u.billable_units[&BillableClass::Input],
                KnownOrUnknown::Known(5)
            );
        }
    }
}

#[test]
fn production_wire_mutations_and_unprovable_hard_money_refuse_before_secret_or_transport() {
    for variant in 0..6 {
        let c = case(
            ServiceRole::Generate,
            generation(),
            "test-model",
            "https://gateway.example/chat",
            "",
            |s| match variant {
                0 => s.tasks[0].prompt_hash = Some(hash("wrong")),
                1 => s.tasks[0].schema_hash = Some(hash("wrong")),
                2 => s.tasks[0].settings_hash = hash("wrong"),
                3 => s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 100)),
                5 => s.tasks[0]
                    .source_bindings
                    .push(lwiki::changes::ReadDependency {
                        path: VaultRelativePath::new("source.md").unwrap(),
                        expected: lwiki::vault::ExpectedState::Absent,
                    }),
                _ => {}
            },
        );
        if variant == 4 {
            std::fs::write(
                c.fs.root().path().join("inputs/task.json"),
                b"changed input",
            )
            .unwrap();
        }
        if variant == 5 {
            std::fs::write(c.fs.root().path().join("source.md"), b"changed source").unwrap();
        }
        let m = Mock::new(chat());
        assert!(
            dispatcher(&c, m.clone())
                .execute(
                    &c.job,
                    &c.trusted,
                    &c.spec.tasks[0].key,
                    DispatchPurpose::Task
                )
                .is_err()
        );
        assert_eq!(m.calls.load(Ordering::SeqCst), 0);
        assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
        assert!(c.job.inspect().unwrap().attempts.is_empty());
    }
}

#[test]
fn production_noncanonical_descriptor_and_invalid_schema_options_refuse_before_authority() {
    for variant in 0..4 {
        let mut input = generation();
        let mut extra = "";
        if let RemoteOperation::Generate {
            output_schema,
            max_output_tokens,
            ..
        } = &mut input.operation
        {
            match variant {
                1 => *output_schema = json!({"$ref":"https://example.invalid/schema"}),
                2 => *max_output_tokens = 5000,
                3 => {
                    *output_schema = json!({"type":"object","properties":{"x":{"type":"string","minLength":2}},"required":["x"],"additionalProperties":false});
                    extra = "response_mode=\"json-schema\"\n"
                }
                _ => {}
            }
        }
        let c = case_with_encoding(
            ServiceRole::Generate,
            input,
            "test-model",
            "https://gateway.example/chat",
            extra,
            variant != 0,
            |_| {},
        );
        let m = Mock::new(chat());
        assert!(
            dispatcher(&c, m.clone())
                .execute(
                    &c.job,
                    &c.trusted,
                    &c.spec.tasks[0].key,
                    DispatchPurpose::Task
                )
                .is_err()
        );
        assert_eq!(m.calls.load(Ordering::SeqCst), 0);
        assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
        assert!(c.job.inspect().unwrap().attempts.is_empty());
    }
}

#[test]
fn production_official_total_overruns_without_partitions_survive_reopen_as_breaches() {
    for variant in 0..4 {
        let c = case(
            ServiceRole::Generate,
            generation(),
            "gpt-4.1-2025-04-14",
            "https://api.openai.com/v1/chat/completions",
            "",
            |_| {},
        );
        let mut v = chat();
        v["model"] = "gpt-4.1-2025-04-14".into();
        v["usage"] = match variant {
            0 => json!({"prompt_tokens":1_047_577,"completion_tokens":8,"total_tokens":1_047_585}),
            1 => json!({"prompt_tokens":10,"completion_tokens":17,"total_tokens":27}),
            2 => json!({"total_tokens":1_047_593}),
            _ => json!({"completion_tokens_details":{"reasoning_tokens":1}}),
        };
        let m = Mock::new(v);
        let f = dispatcher(&c, m.clone())
            .execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert!(f.spool.is_some());
        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
        let i = c.job.inspect().unwrap();
        assert!(!i.budget.guarantee_intact);
        assert!(i.attempts[0].receipt.is_some());
        let md = metadata(&c, &i.attempts[0]);
        assert_eq!(md.failure_code.as_deref(), Some("wire_contract_violation"));
        assert_eq!(md.computed_cost, KnownOrUnknown::Unknown);
        let KnownOrUnknown::Known(u) = md.usage else {
            panic!()
        };
        assert!(
            u.billable_units
                .values()
                .all(|v| *v == KnownOrUnknown::Unknown)
        );
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
        assert!(!reopened.inspect().unwrap().budget.guarantee_intact);
    }
}

#[test]
fn production_official_embedding_total_only_breach_and_absent_usage_control() {
    for breach in [false, true] {
        let c = case(
            ServiceRole::Embed,
            embeddings(),
            "text-embedding-3-small",
            "https://api.openai.com/v1/embeddings",
            "",
            |_| {},
        );
        let mut v = json!({"model":"text-embedding-3-small","data":[{"index":0,"embedding":[1,2]},{"index":1,"embedding":[3,4]}]});
        if breach {
            v["usage"] = json!({"total_tokens":16385});
        }
        let m = Mock::new(v);
        let result = dispatcher(&c, m.clone()).execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        );
        if let Ok(out) = &result {
            commit_success(&c, out);
        }
        assert_eq!(result.is_err(), breach);
        let i = c.job.inspect().unwrap();
        assert_eq!(i.budget.guarantee_intact, !breach);
        assert!(i.attempts[0].receipt.is_some());
        let md = metadata(&c, &i.attempts[0]);
        assert_eq!(md.computed_cost, KnownOrUnknown::Unknown);
        let KnownOrUnknown::Known(u) = md.usage else {
            panic!()
        };
        assert_eq!(
            u.billable_units[&BillableClass::Input],
            KnownOrUnknown::Unknown
        );
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
        assert_eq!(reopened.inspect().unwrap().budget.guarantee_intact, !breach);
    }
}

#[test]
fn production_official_complete_prices_admit_hard_money_and_charge_partitioned_usage_once() {
    let rate = rates(&[
        BillableClass::Input,
        BillableClass::CachedInput,
        BillableClass::Output,
        BillableClass::Reasoning,
    ]);
    for invalid in [false, true] {
        let c = case(
            ServiceRole::Generate,
            generation(),
            "gpt-4.1-2025-04-14",
            "https://api.openai.com/v1/chat/completions",
            &rate,
            |s| s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 4_000_000)),
        );
        let mut v = chat();
        v["model"] = "gpt-4.1-2025-04-14".into();
        v["usage"]["completion_tokens_details"]["reasoning_tokens"] = 0.into();
        if invalid {
            v["choices"][0]["message"]["content"] = "{\"ok\":1}".into();
        }
        let m = Mock::new(v);
        let result = dispatcher(&c, m.clone()).execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        );
        if invalid {
            assert!(result.is_err());
        } else {
            let out = result.unwrap_or_else(|f| panic!("{} {}", f.error.code, f.error.message));
            commit_success(&c, &out);
        }
        let i = c.job.inspect().unwrap();
        assert!(i.budget.guarantee_intact);
        assert_eq!(i.budget.known_costs[&Currency::new("USD").unwrap()], 42);
        assert_eq!(
            metadata(&c, &i.attempts[0]).computed_cost,
            KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 42))
        );
        assert_eq!(
            i.attempts[0].phase,
            if invalid {
                AttemptPhase::Settled
            } else {
                AttemptPhase::OutputCommitted
            }
        );
        if invalid {
            assert!(i.budget.unknown_attempts.is_empty());
        }
        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn production_official_missing_partitions_keeps_the_complete_money_reservation() {
    for no_usage in [false, true] {
        let rate = rates(&[
            BillableClass::Input,
            BillableClass::CachedInput,
            BillableClass::Output,
            BillableClass::Reasoning,
        ]);
        let c = case(
            ServiceRole::Generate,
            generation(),
            "gpt-4.1-2025-04-14",
            "https://api.openai.com/v1/chat/completions",
            &rate,
            |s| s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 4_000_000)),
        );
        let mut v = chat();
        v["model"] = "gpt-4.1-2025-04-14".into();
        if no_usage {
            v.as_object_mut().unwrap().remove("usage");
        } else {
            v["usage"] = json!({"prompt_tokens":10,"completion_tokens":8,"total_tokens":18});
        }
        let m = Mock::new(v);
        let out = dispatcher(&c, m)
            .execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .unwrap_or_else(|f| panic!("{}", f.error.code));
        commit_success(&c, &out);
        let i = c.job.inspect().unwrap();
        assert!(i.budget.guarantee_intact);
        assert_eq!(i.attempts[0].billing, BillingDisposition::UnknownReserved);
        assert_eq!(i.budget.outstanding.cost, i.attempts[0].allowance.cost);
        assert!(
            i.budget
                .outstanding
                .cost
                .as_ref()
                .is_some_and(|m| m.nanounits() > 3_000_000)
        );
        assert!(i.budget.known_costs.is_empty());
    }
}

#[test]
fn production_generation_probe_accounts_generation_usage_and_validates_before_reducing_output() {
    let rate = rates(&[
        BillableClass::Input,
        BillableClass::CachedInput,
        BillableClass::Output,
        BillableClass::Reasoning,
    ]);
    for invalid in [false, true] {
        let c = case(
            ServiceRole::Generate,
            generation(),
            "test-model",
            "https://gateway.example/chat",
            &rate,
            |s| s.tasks[0].capability = Some(Capability::Probe),
        );
        let mut v = chat();
        if invalid {
            v["choices"][0]["message"]["content"] = "{\"ok\":1}".into();
        }
        let m = Mock::new(v);
        let result = dispatcher(&c, m.clone()).execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Probe {
                role: ServiceRole::Generate,
            },
        );
        if let Ok(out) = &result {
            commit_success(&c, out);
        }
        if invalid {
            assert!(result.is_err())
        } else {
            assert!(matches!(
                result.unwrap_or_else(|f| panic!("{}", f.error.code)).output,
                ValidatedOutput::Probe {
                    role: ServiceRole::Generate
                }
            ));
        }
        let i = c.job.inspect().unwrap();
        assert_eq!(i.attempts[0].bound.capability, Capability::Probe);
        assert!(
            i.attempts[0]
                .bound
                .applicable_classes
                .contains(&BillableClass::Reasoning)
        );
        assert_eq!(i.budget.known_costs[&Currency::new("USD").unwrap()], 42);
        assert!(i.attempts[0].cache_outputs.is_empty());
        assert!(i.attempts[0].receipt.is_some());
        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
    }
}

use lwiki as library;
#[path = "fixtures/p18/common.rs"]
mod common;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod provider;
use common::*;
use lwiki::{
    app::remote::RemoteRuntime,
    app::*,
    changes::*,
    domain::*,
    graph::{generation_cache, *},
    jobs::diagnostics::DiagnosticKind,
    jobs::*,
    retrieval::QueryPlan,
    sources::SourceView,
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn api_and_agent_same_packet_import_semantics() {
    let f = Fixture::new();
    let raw = format!(" \n{}\n ", serde_json::to_string(&f.response()).unwrap());
    let mock = Mock::content(raw.clone(), "stop");
    let out = f
        .app
        .graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(mock.clone()),
            f.options.clone(),
        )
        .unwrap();
    assert_eq!(out.packet, f.packet);
    let imported = out.import.as_ref().unwrap();
    assert!(imported.prepared.is_some());
    assert_eq!(imported.coverage.pending_mentions, 4);
    assert_eq!(imported.coverage.materialized_assertions, 0);
    assert!(
        !f.fs
            .root()
            .path()
            .join(imported.extraction.path.as_str())
            .exists()
    );
    let agent = f.app.graph_import(raw.as_bytes(), false).unwrap();
    assert_eq!(
        agent["prepared"],
        serde_json::to_value(&imported.prepared).unwrap()
    );
    f.apply(imported.prepared.as_ref().unwrap());
    let again = f
        .app
        .graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(mock.clone()),
            f.options.clone(),
        )
        .unwrap();
    assert!(again.reused);
    assert_eq!(again.import.unwrap().allocations, imported.allocations);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let i = f.ledger().inspect().unwrap();
    assert_eq!(i.attempts[0].phase, AttemptPhase::Settled);
    assert!(i.attempts[0].spool.is_none());
    assert_eq!(i.tasks.values().next().unwrap().state, TaskState::Completed);
}

#[test]
fn uncertain_generation_retry_returns_useful_import_and_retains_prior_hold() {
    let mut f = Fixture::new();
    f.request.limits.concurrency = 2;
    f.request.limits.attempts_per_task = 2;
    let lost = Arc::new(Mock {
        calls: AtomicUsize::new(0),
        body: Mutex::new(None),
        cancel: None,
        fail: true,
    });
    assert!(
        f.app
            .graph_extract_api(
                &f.request,
                &f.service,
                &f.dispatcher(lost.clone()),
                f.options.clone()
            )
            .is_err()
    );
    assert_eq!(lost.calls.load(Ordering::SeqCst), 1);
    let valid = Mock::response(f.response());
    assert!(
        f.app
            .graph_extract_api(
                &f.request,
                &f.service,
                &f.dispatcher(valid.clone()),
                f.options.clone()
            )
            .is_err()
    );
    assert_eq!(valid.calls.load(Ordering::SeqCst), 0);
    let mut options = f.options.clone();
    options.policy.retry_uncertain = true;
    let retried = f
        .app
        .graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(valid.clone()),
            options.clone(),
        )
        .unwrap();
    assert_eq!(valid.calls.load(Ordering::SeqCst), 1);
    assert!(retried.import.as_ref().unwrap().prepared.is_some());
    assert!(!retried.warnings.is_empty());
    let ledger = JobLedger::new(
        f.fs.clone(),
        f.app.vault_id().clone(),
        f.request.run_id.clone(),
        options,
    )
    .unwrap();
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Paused);
    assert_eq!(inspection.attempts.len(), 2);
    assert_eq!(inspection.budget.dispatched_requests, 2);
    assert_eq!(inspection.attempts[0].phase, AttemptPhase::DispatchIntent);
    assert_eq!(
        inspection.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    assert_eq!(inspection.budget.remote_inflight, 1);
    let reused = f
        .app
        .graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(valid.clone()),
            f.options.clone(),
        )
        .unwrap();
    assert!(reused.reused);
    assert_eq!(valid.calls.load(Ordering::SeqCst), 1);
    assert_eq!(ledger.inspect().unwrap().budget.remote_inflight, 1);
}

#[test]
fn generation_service_cap_fails_before_packet_job_or_transport() {
    let mut f = Fixture::new();
    let before = provider::tree(f.temp.path());
    f.request.max_output_tokens = 4097;
    let mock = Mock::response(f.response());
    let error = f
        .app
        .graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(mock.clone()),
            f.options.clone(),
        )
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::Usage);
    assert_eq!(error.details["reason"], "generation_output_limit");
    assert_eq!(error.details["requested"], 4097);
    assert_eq!(error.details["effective_service_cap"], 4096);
    assert_eq!(
        error.details["configuration_key"],
        "services.service.max_output_tokens"
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider::tree(f.temp.path()), before);
}

#[test]
fn semantic_diagnostic_does_not_echo_model_controlled_identifiers() {
    let f = Fixture::new();
    let raw = serde_json::to_string(&f.response())
        .unwrap()
        .replace("\"m3\"", "\"PRIVATE_SENTINEL_SECRET\"");
    let mut response: serde_json::Value = serde_json::from_str(&raw).unwrap();
    response["mentions"][2]["quote"] = json!("Tool");
    response["mentions"][2]
        .as_object_mut()
        .unwrap()
        .remove("span");
    let mock = Mock::response(response);
    let error = f
        .app
        .graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(mock),
            f.options.clone(),
        )
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::ExtractionInvalid);
    assert!(!format!("{error:?}").contains("PRIVATE_SENTINEL_SECRET"));
    assert_eq!(
        error.details["retained_details"]["item_id_hash"],
        Blake3Hash::digest(b"PRIVATE_SENTINEL_SECRET").to_string()
    );
}

#[test]
fn paid_quote_rejection_retains_private_output_for_explicit_host_repair() {
    let f = Fixture::new();
    let mut repeated = f.response();
    repeated["mentions"][2]
        .as_object_mut()
        .unwrap()
        .remove("span");
    let raw = serde_json::to_string(&repeated).unwrap();
    let mock = Mock::content(raw.clone(), "stop");
    let error = match f.app.graph_extract_api(
        &f.request,
        &f.service,
        &f.dispatcher(mock.clone()),
        f.options.clone(),
    ) {
        Ok(_) => panic!("ambiguous quote was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::ExtractionInvalid);
    let detail = &error.details["retained_details"];
    assert_eq!(detail["reason"], "quotation_ambiguous");
    assert_eq!(detail["item_kind"], "mention");
    assert_eq!(
        detail["item_id_hash"],
        Blake3Hash::digest(b"m3").to_string()
    );
    assert_eq!(detail["window_id"], "w1");
    assert_eq!(detail["match_count"], 2);
    assert_eq!(detail["candidate_spans"].as_array().unwrap().len(), 2);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let ledger = f.ledger();
    let inspection = ledger.inspect().unwrap();
    let attempt = &inspection.attempts[0];
    assert_eq!(attempt.phase, AttemptPhase::Settled);
    assert!(attempt.spool.is_none());
    assert!(attempt.outputs.is_empty());
    let safe = ledger
        .inspect_diagnostic(&attempt.attempt, DiagnosticKind::SemanticRejection, false)
        .unwrap()
        .unwrap();
    assert_eq!(safe.reference.reason, "quotation_ambiguous");
    assert!(safe.body_utf8.is_none());
    let exposed = ledger
        .inspect_diagnostic(&attempt.attempt, DiagnosticKind::SemanticRejection, true)
        .unwrap()
        .unwrap();
    assert_eq!(exposed.body_utf8.as_deref(), Some(raw.as_str()));
    assert!(
        ledger
            .prune_diagnostic(&attempt.attempt, DiagnosticKind::SemanticRejection)
            .is_err()
    );
    let corrected = serde_json::to_vec(&f.response()).unwrap();
    assert!(f.app.graph_import(&corrected, false).unwrap()["prepared"].is_object());
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn quote_diagnostics_distinguish_absent_and_wrong_absolute_span() {
    for (quote, span, expected) in [
        ("Not in the source", None, "quotation_absent"),
        ("Tool", Some((0, 4)), "quotation_span_invalid"),
    ] {
        let f = Fixture::new();
        let mut response = f.response();
        response["mentions"][2]["quote"] = json!(quote);
        match span {
            Some((start, end)) => {
                response["mentions"][2]["span"] = json!({"start":start,"end":end})
            }
            None => {
                response["mentions"][2]
                    .as_object_mut()
                    .unwrap()
                    .remove("span");
            }
        }
        let mock = Mock::response(response);
        let error = match f.app.graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(mock),
            f.options.clone(),
        ) {
            Ok(_) => panic!("invalid quote was accepted"),
            Err(error) => error,
        };
        let detail = &error.details["retained_details"];
        assert_eq!(detail["reason"], expected);
        assert_eq!(
            detail["item_id_hash"],
            Blake3Hash::digest(b"m3").to_string()
        );
        assert_eq!(detail["match_count"], if quote == "Tool" { 2 } else { 0 });
    }
}

#[test]
fn explicit_extraction_schema_probe_uses_real_contract_fixture() {
    let f = Fixture::new();
    let response = json!({
        "schema": EXTRACTION_SCHEMA,
        "packet_id": "packet_probe",
        "packet_fingerprint": Blake3Hash::digest(b"lwiki.extraction-probe.v1"),
        "mentions": [],
        "assertions": [],
        "unresolved": [],
    });
    let mock = Mock::response(response);
    let dispatcher = f.dispatcher(mock.clone());
    let runtime = RemoteRuntime {
        service: f.service,
        dispatcher,
        job_options: f.options,
        limits: f.request.limits,
        created_at_utc_ms: f.request.created_at_utc_ms,
        deadline_utc_ms: f.request.deadline_utc_ms,
        requested_limits: None,
    };
    let outcome = f.app.probe_extraction_schema(&runtime).unwrap();
    assert!(outcome.validated);
    assert_eq!(outcome.output_contract.as_deref(), Some(EXTRACTION_SCHEMA));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let path = f
        .temp
        .path()
        .join(format!("runs/{}/inputs/probe.json", outcome.run_id));
    let retained: lwiki::providers::types::RemoteInput =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let lwiki::providers::types::RemoteOperation::Generate { output_schema, .. } =
        retained.operation
    else {
        panic!("schema probe sent a non-generation task");
    };
    let actual_schema: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/extraction-v1.json")).unwrap();
    assert_eq!(output_schema, actual_schema);
}

fn add_malformed_duplicate_source(f: &Fixture) {
    std::fs::write(
        f.temp.path().join("broken-duplicate.md"),
        format!(
            "---\nwiki_schema: \"1\"\nwiki_id: \"{}\"\nwiki_kind: source\ntitle: [broken\n---\nSafe malformed text\n",
            f.request.export.source_id
        ),
    )
    .unwrap();
}

#[test]
fn malformed_duplicate_blocks_reused_packet_and_paid_dispatch() {
    let f = Fixture::new();
    let exported = f.app.graph_extract_agent(&f.request.export).unwrap();
    assert!(exported.ready_to_import);
    add_malformed_duplicate_source(&f);
    let err = f.app.graph_extract_agent(&f.request.export).unwrap_err();
    assert_eq!(err.code, ErrorCode::ReferenceAmbiguous);
    let mock = Mock::response(f.response());
    let err = match f.app.graph_extract_api(
        &f.request,
        &f.service,
        &f.dispatcher(mock.clone()),
        f.options.clone(),
    ) {
        Ok(_) => panic!("ambiguous source reached API extraction"),
        Err(error) => error,
    };
    assert_eq!(err.code, ErrorCode::ReferenceAmbiguous);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn retained_generation_output_with_invalidated_source_projects_without_panic() {
    let f = Fixture::new();
    let mock = Mock::response(f.response());
    f.app
        .graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(mock.clone()),
            f.options.clone(),
        )
        .unwrap();
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    add_malformed_duplicate_source(&f);
    let projected = lwiki::catalog::scan::scan(&f.fs, &id("vault_test")).unwrap();
    assert!(!projected.records.contains_key(&f.request.export.source_id));
    assert!(projected.diagnostics.iter().any(|diagnostic| {
        diagnostic.details["reason"] == "generation_output_source_unresolved"
    }));
    assert!(projected.documents.iter().any(|document| {
        document.path.as_str() == "broken-duplicate.md"
            && document.record_id.is_none()
            && document.raw_text.contains("Safe malformed text")
    }));

    let checked = f.app.check().unwrap();
    assert!(checked.error_count > 0);
    assert!(checked.diagnostics.iter().any(|diagnostic| {
        diagnostic.details["reason"] == "generation_output_source_unresolved"
    }));
    f.app.index_sync(true).unwrap();
    let hits = f
        .app
        .semantic_search(
            "Safe malformed text",
            &QueryPlan::default(),
            None,
            false,
            true,
            None,
        )
        .unwrap();
    assert!(!hits.network_used);
    assert!(hits.hits.iter().any(|hit| {
        hit.locator.path.as_str() == "broken-duplicate.md"
            && hit.locator.record.is_none()
            && hit.excerpt.citation.is_none()
    }));
    assert!(hits.hits.iter().all(|hit| {
        hit.excerpt.citation.is_none()
            && hit
                .secondary_excerpts
                .iter()
                .all(|excerpt| excerpt.citation.is_none())
    }));
    let source_hits = f
        .app
        .semantic_search("Ada", &QueryPlan::default(), None, false, true, None)
        .unwrap();
    assert!(!source_hits.network_used);
    assert!(source_hits.hits.iter().all(|hit| {
        hit.excerpt.citation.is_none()
            && hit
                .secondary_excerpts
                .iter()
                .all(|excerpt| excerpt.citation.is_none())
    }));
    let graph = f
        .app
        .semantic_graph("Ada", &GraphPlan::default(), None, false, true)
        .unwrap();
    assert!(!graph.network_used);
    assert!(graph.assertions.is_empty());
    assert!(
        graph
            .seeds
            .iter()
            .all(|seed| { seed.record_ref.record_id != f.request.export.source_id })
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cached_resume_preserves_reject_accept_and_resolution() {
    let f = Fixture::new();
    let mock = Mock::response(f.response());
    let dispatch = f.dispatcher(mock.clone());
    let out = f
        .app
        .graph_extract_api(&f.request, &f.service, &dispatch, f.options.clone())
        .unwrap();
    let imp = out.import.unwrap();
    f.apply(imp.prepared.as_ref().unwrap());
    let extraction_id = imp.extraction.record.as_ref().unwrap().record_id.clone();
    let mappings = vec![
        json!({"mention_id":"m1","operation":"CreateEntity","title":"Ada","entity_type":"person","reason":"Fixture identity"}),
        json!({"mention_id":"m2","operation":"CreateEntity","title":"Acme","entity_type":"organization","reason":"Fixture identity"}),
        json!({"mention_id":"m3","operation":"CreateEntity","title":"Tool","entity_type":"component","reason":"Fixture identity"}),
        json!({"mention_id":"m4","operation":"RejectMention","reason":"Explicit fixture rejection"}),
    ];
    let resolution = json!({"schema":RESOLUTION_SCHEMA,"extraction_id":extraction_id,"expected_hash":f.record(&extraction_id).source_hash,"mappings":mappings});
    let staged = f
        .app
        .graph_resolve(&serde_json::to_vec(&resolution).unwrap())
        .unwrap();
    let prepared: PreparedChange = serde_json::from_value(staged["prepared"].clone()).unwrap();
    f.apply(&prepared);
    let mut decisions = vec![];
    for (local, decision) in [("a1", "accept"), ("a2", "reject")] {
        let aid = &imp.allocations.assertions[&PacketLocalId::new(local).unwrap()];
        let eid = &imp.allocations.evidence[&PacketLocalId::new(local).unwrap()][0];
        decisions.push(json!({"assertion_id":aid,"expected_hash":f.record(aid).source_hash,"decision":decision,"reason":"Explicit full fixture review","evidence_checks":[{"evidence_id":eid,"expected_hash":f.record(eid).source_hash,"assessment":"supports"}]}));
    }
    let reviewed = f
        .app
        .graph_review(
            &serde_json::to_vec(&json!({"schema":GRAPH_REVIEW_SCHEMA,"decisions":decisions}))
                .unwrap(),
        )
        .unwrap();
    let prepared: PreparedChange = serde_json::from_value(reviewed["prepared"].clone()).unwrap();
    f.apply(&prepared);
    let before = f.record(&extraction_id).raw;
    std::fs::remove_dir_all(f.temp.path().join(".wiki/cache/generation")).unwrap();
    let offline = OfflineApp::new(
        f.fs.clone(),
        OperationOptions {
            offline: true,
            lock_timeout_ms: 5000,
            ..Default::default()
        },
    )
    .unwrap();
    let out = offline
        .graph_extract_api(&f.request, &f.service, &dispatch, f.options.clone())
        .unwrap();
    assert!(out.reused);
    assert_eq!(
        out.import.unwrap().extraction.record.unwrap().record_id,
        extraction_id
    );
    assert_eq!(f.record(&extraction_id).raw, before);
    for (local, status) in [("a1", "accepted"), ("a2", "rejected")] {
        assert_eq!(
            f.record(&imp.allocations.assertions[&PacketLocalId::new(local).unwrap()])
                .canonical
                .unwrap()
                .string("wiki_status"),
            Some(status)
        );
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn malformed_response_charged_no_partial_activation_or_repair() {
    for mode in ["prose", "truncated", "refusal", "bad_quote"] {
        let f = Fixture::new();
        let mut response = f.response();
        if mode == "bad_quote" {
            response["mentions"][0]["quote"] = "Invented".into();
        }
        let mut m = Mock::content(
            if mode == "prose" {
                format!("Here is JSON {}", response)
            } else {
                serde_json::to_string(&response).unwrap()
            },
            if mode == "truncated" {
                "length"
            } else {
                "stop"
            },
        );
        if mode == "refusal" {
            Arc::get_mut(&mut m)
                .unwrap()
                .body
                .get_mut()
                .unwrap()
                .as_mut()
                .unwrap()["choices"][0]["message"]["refusal"] = "Refused".into();
        }
        let result = f.app.graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(m.clone()),
            f.options.clone(),
        );
        assert!(result.is_err(), "{mode}");
        let i = f.ledger().inspect().unwrap();
        assert_eq!(i.budget.dispatched_requests, 1);
        assert_eq!(i.attempts[0].phase, AttemptPhase::Settled);
        assert!(i.attempts[0].outputs.is_empty());
        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
        for p in f.fs.root().scan_markdown().unwrap() {
            let n =
                lwiki::records::parse_note(&std::fs::read(f.temp.path().join(p.as_str())).unwrap());
            assert!(!n.canonical.is_some_and(|r| matches!(
                r.kind(),
                RecordKind::Extraction
                    | RecordKind::Assertion
                    | RecordKind::Evidence
                    | RecordKind::Entity
            )));
        }
    }
}

#[test]
fn changed_source_prompt_model_creates_new_task() {
    let f = Fixture::new();
    let exported = f.app.graph_extract_agent(&f.request.export).unwrap();
    let view = SourceView::from_fs_bounded(&f.fs, 64 * 1024 * 1024, 4096).unwrap();
    let p = lwiki::graph::packet::load_packet(&view, &exported.packet.packet_id).unwrap();
    let key = generation_cache::plan_task(&p, &f.service, 1024)
        .unwrap()
        .task
        .key;
    assert_ne!(
        key,
        generation_cache::plan_task(&p, &f.service, 512)
            .unwrap()
            .task
            .key
    );
    let mut context = f.request.export.clone();
    context.limits.max_mentions = 3;
    let exported = f.app.graph_extract_agent(&context).unwrap();
    let view = SourceView::from_fs_bounded(&f.fs, 64 * 1024 * 1024, 4096).unwrap();
    let p = lwiki::graph::packet::load_packet(&view, &exported.packet.packet_id).unwrap();
    assert_ne!(
        key,
        generation_cache::plan_task(&p, &f.service, 1024)
            .unwrap()
            .task
            .key
    );
}

#[test]
fn cancel_after_dispatch_retains_completed_output_unknown_attempt() {
    let f = Fixture::new();
    let mut m = Mock::response(f.response());
    Arc::get_mut(&mut m).unwrap().cancel = Some(f.options.cancel.clone());
    let result = f.app.graph_extract_api(
        &f.request,
        &f.service,
        &f.dispatcher(m.clone()),
        f.options.clone(),
    );
    assert!(result.is_ok());
    assert_eq!(
        f.ledger().inspect().unwrap().attempts[0].phase,
        AttemptPhase::Settled
    );
    let f = Fixture::new();
    let m = Arc::new(Mock {
        calls: AtomicUsize::new(0),
        body: Mutex::new(None),
        cancel: Some(f.options.cancel.clone()),
        fail: true,
    });
    assert!(
        f.app
            .graph_extract_api(&f.request, &f.service, &f.dispatcher(m), f.options.clone())
            .is_err()
    );
    let i = f.ledger().inspect().unwrap();
    assert_eq!(i.budget.dispatched_requests, 1);
    assert_eq!(i.attempts[0].billing, BillingDisposition::UnknownReserved);
    assert!(i.attempts[0].outputs.is_empty());
}

#[test]
fn offline_miss_and_dry_run_make_no_packet_or_job_writes() {
    let f = Fixture::new();
    let mock = Mock::response(f.response());
    let dispatcher = f.dispatcher(mock.clone());
    for (dry_run, offline) in [(true, false), (false, true)] {
        let app = OfflineApp::new(
            f.fs.clone(),
            OperationOptions {
                dry_run,
                offline,
                ..Default::default()
            },
        )
        .unwrap();
        let out = app.graph_extract_api(&f.request, &f.service, &dispatcher, f.options.clone());
        if dry_run {
            assert!(out.unwrap().dry_run)
        } else {
            assert_eq!(out.err().unwrap().code, ErrorCode::OfflineUnavailable)
        }
        assert!(!f.temp.path().join("runs/run_api").exists());
        assert!(!f.temp.path().join("knowledge/extractions/packets").exists());
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
}

struct StopAfterSpool(std::sync::atomic::AtomicBool);
impl LedgerFault for StopAfterSpool {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterSpoolMetadataSync && !self.0.swap(true, Ordering::SeqCst)
        {
            Err(WikiError::new(
                ErrorCode::Internal,
                "injected durable response boundary",
            ))
        } else {
            Ok(())
        }
    }
}
fn retained(f: &Fixture, mock: Arc<Mock>) -> AttemptInspection {
    let mut options = f.options.clone();
    options.fault = Some(Arc::new(StopAfterSpool(
        std::sync::atomic::AtomicBool::new(false),
    )));
    assert!(
        f.app
            .graph_extract_api(&f.request, &f.service, &f.dispatcher(mock), options)
            .is_err()
    );
    let i = f.ledger().replay().unwrap().inspection;
    assert_eq!(i.attempts[0].phase, AttemptPhase::Received);
    i.attempts[0].clone()
}
#[test]
fn retained_response_resumes_without_transport_and_rejects_tampered_body() {
    for tamper in [false, true] {
        let f = Fixture::new();
        let mock = Mock::response(f.response());
        let a = retained(&f, mock.clone());
        if tamper {
            std::fs::write(
                f.temp
                    .path()
                    .join(a.spool.as_ref().unwrap().response.path.as_str()),
                b"tampered",
            )
            .unwrap();
        }
        let out = f.app.graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(mock.clone()),
            f.options.clone(),
        );
        if tamper {
            assert!(out.is_err());
            assert!(f.ledger().inspect().unwrap().attempts[0].outputs.is_empty());
        } else {
            assert!(out.unwrap().reused);
            assert_eq!(
                f.ledger().inspect().unwrap().attempts[0].phase,
                AttemptPhase::Settled
            );
        }
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn retained_decode_survives_source_change_but_materialization_requires_freshness() {
    let f = Fixture::new();
    let mock = Mock::response(f.response());
    let a = retained(&f, mock.clone());
    let source = f.record(&f.request.export.source_id);
    let path =
        f.fs.root()
            .scan_markdown()
            .unwrap()
            .into_iter()
            .find(|p| std::fs::read(f.temp.path().join(p.as_str())).unwrap() == source.raw)
            .unwrap();
    let changed = lwiki::records::edit_note(
        &source,
        &std::collections::BTreeMap::from([("title".into(), json!("Changed source title"))]),
        None,
        &source.source_hash,
    )
    .unwrap();
    std::fs::write(f.temp.path().join(path.as_str()), changed).unwrap();
    let dispatch = f.dispatcher(mock.clone());
    assert!(
        dispatch
            .decode_retained(
                &f.ledger(),
                &f.service,
                &a.attempt.task_key,
                lwiki::providers::types::DispatchPurpose::Task,
                &a.attempt
            )
            .is_ok()
    );
    assert!(
        f.app
            .graph_extract_api(&f.request, &f.service, &dispatch, f.options.clone())
            .is_err()
    );
    assert!(f.ledger().inspect().unwrap().attempts[0].outputs.is_empty());
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn retained_response_uses_original_model_and_rejects_saved_contract_tampering() {
    let f = Fixture::new();
    let mock = Mock::response(f.response());
    let a = retained(&f, mock.clone());
    let path = f.temp.path().join("providers.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("test-model", "changed-model");
    std::fs::write(&path, text).unwrap();
    let service = lwiki::config::providers::ProviderConfig::load(&path)
        .unwrap()
        .authorize(&f.fs, &id("vault_test"), "primary", Capability::Generate)
        .unwrap();
    let dispatch = f.dispatcher(mock.clone());
    let ledger = f.ledger();
    let before = ledger.inspect().unwrap();
    let decoded = dispatch
        .decode_retained(
            &ledger,
            &service,
            &a.attempt.task_key,
            lwiki::providers::types::DispatchPurpose::Task,
            &a.attempt,
        )
        .unwrap();
    let lwiki::providers::types::ValidatedOutput::Generation {
        value,
        returned_model,
        ..
    } = decoded
    else {
        panic!("retained generation required")
    };
    assert_eq!(value, f.response());
    assert_eq!(returned_model.as_deref(), Some("test-model"));
    assert_eq!(
        before.attempts[0].bound.requested_model.as_deref(),
        Some("test-model")
    );

    // Current configuration cannot rewrite the authenticated historical model.
    // Altering that model in the saved codec must remain a recovery failure,
    // not a rejected paid output or permission to dispatch again.
    let codec = f
        .temp
        .path()
        .join(a.bound.codec.as_ref().unwrap().path.as_str());
    let original = std::fs::read(&codec).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
    changed["original_bound"]["requested_model"] = json!("changed-model");
    std::fs::write(
        &codec,
        lwiki::graph::packet::canonical_json(&changed).unwrap(),
    )
    .unwrap();
    let error = dispatch
        .recover_response(&ledger, &service, &a.attempt.task_key, &a.attempt)
        .err()
        .expect("tampered historical model must be refused");
    assert_eq!(
        error.disposition,
        lwiki::providers::types::DispatchDisposition::OutcomeUnknown
    );
    assert!(error.materialization.is_none());
    let after = ledger.inspect().unwrap();
    assert_eq!(after.budget, before.budget);
    assert_eq!(after.attempts.len(), 1);
    assert_eq!(after.attempts[0].phase, AttemptPhase::Received);
    assert!(after.attempts[0].receipt.is_none());
    assert!(after.attempts[0].outputs.is_empty());

    std::fs::write(&codec, original).unwrap();
    let recovered =
        match dispatch.recover_response(&ledger, &service, &a.attempt.task_key, &a.attempt) {
            Ok(outcome) => outcome,
            Err(error) => panic!("restored historical codec must decode: {}", error.error),
        };
    assert_eq!(recovered.attempt, a.attempt);
    assert!(matches!(
        recovered.output,
        lwiki::providers::types::ValidatedOutput::Generation { returned_model: Some(model), .. }
            if model == "test-model"
    ));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let unchanged = ledger.inspect().unwrap();
    assert_eq!(unchanged.budget, before.budget);
    assert!(unchanged.attempts[0].outputs.is_empty());
    assert!(unchanged.attempts[0].receipt.is_none());
}

#[test]
fn default_run_identity_is_pure_stable_and_binds_source_model_revision_context() {
    let f = Fixture::new();
    let first = f
        .app
        .default_api_extraction_run_id(&f.request.export, &f.service, 1024)
        .unwrap();
    let second = f
        .app
        .default_api_extraction_run_id(&f.request.export, &f.service, 1024)
        .unwrap();
    assert_eq!(first, second);
    assert!(!f.temp.path().join("runs").exists());
    assert!(!f.temp.path().join("knowledge/extractions/packets").exists());
    let exported = f.app.graph_extract_agent(&f.request.export).unwrap();
    assert_eq!(
        first,
        f.app
            .default_api_extraction_run_id(&f.request.export, &f.service, 1024)
            .unwrap()
    );
    assert_ne!(
        first,
        f.app
            .default_api_extraction_run_id(&f.request.export, &f.service, 512)
            .unwrap()
    );
    let view = SourceView::from_fs_bounded(&f.fs, 64 * 1024 * 1024, 4096).unwrap();
    let packet = lwiki::graph::packet::load_packet(&view, &exported.packet.packet_id).unwrap();
    let task = generation_cache::plan_task(&packet, &f.service, 1024)
        .unwrap()
        .task;
    let config = f.temp.path().join("providers.toml");
    let text = std::fs::read_to_string(&config)
        .unwrap()
        .replace("test-model", "changed-model")
        .replace("revision=\"r1\"", "revision=\"r2\"");
    std::fs::write(&config, text).unwrap();
    let changed = lwiki::config::providers::ProviderConfig::load(&config)
        .unwrap()
        .authorize(&f.fs, &id("vault_test"), "primary", Capability::Generate)
        .unwrap();
    assert_ne!(
        first,
        f.app
            .default_api_extraction_run_id(&f.request.export, &changed, 1024)
            .unwrap()
    );
    let changed_task = generation_cache::plan_task(&packet, &changed, 1024)
        .unwrap()
        .task;
    assert_ne!(task.key, changed_task.key);
    assert_ne!(task.model_hash, changed_task.model_hash);
    let refresh = lwiki::sources::SourceStore::new(f.fs.clone())
        .plan_refresh(
            &f.request.export.source_id,
            lwiki::sources::CaptureRequest {
                title: "Synthetic API source".into(),
                origin_kind: lwiki::sources::SourceOrigin::LocalFile,
                origin: "fixture.md".into(),
                original: b"Ada works for Acme. Updated revision.".to_vec(),
                extraction: lwiki::sources::ExtractionInput::Utf8Preserve,
                media_type: None,
            },
        )
        .unwrap();
    let writer =
        lwiki::vault::WriterPermit::acquire(f.fs.root(), std::time::Duration::from_secs(2))
            .unwrap();
    let prepared = f
        .engine
        .prepare(&writer, refresh.draft.unwrap())
        .unwrap()
        .prepared;
    drop(writer);
    f.apply(&prepared);
    let mut request = f.request.export.clone();
    request.revision_id = None;
    assert_ne!(
        first,
        f.app
            .default_api_extraction_run_id(&request, &changed, 1024)
            .unwrap()
    );
}

fn growth_snapshot(fixture: &Fixture) -> lwiki::storage::StorageTotals {
    let inventory = lwiki::storage::inventory(&fixture.fs, &Default::default()).unwrap();
    assert!(inventory.complete);
    inventory.totals
}

fn bounded_api_delta(
    label: &str,
    before: &lwiki::storage::StorageTotals,
    after: &lwiki::storage::StorageTotals,
    max_new_files: u64,
    max_new_bytes: u64,
) {
    assert!(
        after.files.saturating_sub(before.files) <= max_new_files,
        "{label}: {before:?} -> {after:?}"
    );
    assert!(
        after.logical_bytes.saturating_sub(before.logical_bytes) <= max_new_bytes,
        "{label}: {before:?} -> {after:?}"
    );
    eprintln!(
        "S05 {label}: files {} -> {}, bytes {} -> {}, unique {} -> {}, duplicate {} -> {}",
        before.files,
        after.files,
        before.logical_bytes,
        after.logical_bytes,
        before.unique_content_bytes,
        after.unique_content_bytes,
        before.duplicate_bytes,
        after.duplicate_bytes
    );
}

fn compact_and_check_api_authority(fixture: &Fixture) {
    let before = growth_snapshot(fixture);
    let options = lwiki::storage::StorageOptions {
        retain_undo_changes: 1,
        ..Default::default()
    };
    let plan = lwiki::storage::plan_cleanup(&fixture.fs, &options).unwrap();
    let writer =
        lwiki::vault::WriterPermit::acquire(fixture.fs.root(), std::time::Duration::from_secs(5))
            .unwrap();
    let result = lwiki::storage::cleanup(
        &fixture.fs,
        &writer,
        &lwiki::storage::StorageOptions {
            expected_plan: Some(plan.plan_hash),
            ..options
        },
    )
    .unwrap();
    drop(writer);
    let after = growth_snapshot(fixture);
    assert_eq!(result.after.files, after.files);
    assert_eq!(result.after.logical_bytes, after.logical_bytes);
    // Cleanup may add fixed migration metadata in a small fixture. It must
    // retain exact job attempts, receipts, and unknown billing reservations.
    let _ = fixture.ledger().inspect().unwrap();
    eprintln!(
        "S05 cleanup: files {} -> {}, bytes {} -> {}, unique {} -> {}, duplicate {} -> {}; deleted {} files",
        before.files,
        after.files,
        before.logical_bytes,
        after.logical_bytes,
        before.unique_content_bytes,
        after.unique_content_bytes,
        before.duplicate_bytes,
        after.duplicate_bytes,
        result.deleted_files
    );
}

#[test]
fn measured_api_success_rejection_retry_and_cleanup_growth() {
    // Bounds are per one small packet and one bounded mock response. They
    // catch accidental per-event fanout without promising a whole-vault size.
    let success = Fixture::new();
    let before = growth_snapshot(&success);
    let response = success.response();
    let mock = Mock::response(response.clone());
    let extracted = success
        .app
        .graph_extract_api(
            &success.request,
            &success.service,
            &success.dispatcher(mock.clone()),
            success.options.clone(),
        )
        .unwrap();
    let imported = extracted.import.unwrap();
    success.apply(imported.prepared.as_ref().unwrap());
    let materialized = growth_snapshot(&success);
    bounded_api_delta(
        "successful API and apply",
        &before,
        &materialized,
        50,
        512 * 1024,
    );
    let reused = success
        .app
        .graph_extract_api(
            &success.request,
            &success.service,
            &success.dispatcher(mock.clone()),
            success.options.clone(),
        )
        .unwrap();
    assert!(reused.reused);
    let host_repeat = success
        .app
        .graph_import(serde_json::to_string(&response).unwrap().as_bytes(), false)
        .unwrap();
    assert_eq!(
        host_repeat["allocations"],
        serde_json::to_value(&imported.allocations).unwrap()
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let repeated = growth_snapshot(&success);
    bounded_api_delta("API and host repeat", &materialized, &repeated, 0, 4096);
    compact_and_check_api_authority(&success);
    assert_eq!(
        success
            .ledger()
            .inspect()
            .unwrap()
            .budget
            .dispatched_requests,
        1
    );

    let rejected = Fixture::new();
    let before = growth_snapshot(&rejected);
    let mut invalid = rejected.response();
    invalid["mentions"][2]
        .as_object_mut()
        .unwrap()
        .remove("span");
    let invalid_mock = Mock::response(invalid);
    assert_eq!(
        rejected
            .app
            .graph_extract_api(
                &rejected.request,
                &rejected.service,
                &rejected.dispatcher(invalid_mock.clone()),
                rejected.options.clone(),
            )
            .err()
            .expect("ambiguous paid output must be rejected")
            .code,
        ErrorCode::ExtractionInvalid
    );
    let rejection = growth_snapshot(&rejected);
    bounded_api_delta("semantic rejection", &before, &rejection, 44, 384 * 1024);
    let rejected_attempt = &rejected.ledger().inspect().unwrap().attempts[0];
    assert_eq!(rejected_attempt.phase, AttemptPhase::Settled);
    assert!(
        rejected
            .ledger()
            .inspect_diagnostic(
                &rejected_attempt.attempt,
                DiagnosticKind::SemanticRejection,
                false
            )
            .unwrap()
            .is_some()
    );
    compact_and_check_api_authority(&rejected);

    let mut retried = Fixture::new();
    retried.request.limits.concurrency = 2;
    retried.request.limits.attempts_per_task = 2;
    let before = growth_snapshot(&retried);
    let uncertain = Arc::new(Mock {
        calls: AtomicUsize::new(0),
        body: Mutex::new(None),
        cancel: None,
        fail: true,
    });
    assert!(
        retried
            .app
            .graph_extract_api(
                &retried.request,
                &retried.service,
                &retried.dispatcher(uncertain.clone()),
                retried.options.clone(),
            )
            .is_err()
    );
    let unknown = growth_snapshot(&retried);
    bounded_api_delta("unknown attempt", &before, &unknown, 24, 256 * 1024);
    let valid_mock = Mock::response(retried.response());
    let mut retry_options = retried.options.clone();
    retry_options.policy.retry_uncertain = true;
    let useful = retried
        .app
        .graph_extract_api(
            &retried.request,
            &retried.service,
            &retried.dispatcher(valid_mock.clone()),
            retry_options,
        )
        .unwrap();
    retried.apply(useful.import.unwrap().prepared.as_ref().unwrap());
    let complete = growth_snapshot(&retried);
    bounded_api_delta("retried useful output", &unknown, &complete, 32, 256 * 1024);
    let inspection = retried.ledger().inspect().unwrap();
    assert_eq!(inspection.attempts.len(), 2);
    assert_eq!(inspection.budget.dispatched_requests, 2);
    assert_eq!(
        inspection.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    compact_and_check_api_authority(&retried);
    let after = retried.ledger().inspect().unwrap();
    assert_eq!(after.attempts.len(), 2);
    assert_eq!(
        after.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    assert!(after.budget.remote_inflight > 0);
}

#[test]
fn local_retained_recovery_failures_never_reject_paid_output_and_restore_without_send() {
    use lwiki::providers::types::DispatchDisposition;
    for fault in [
        "missing_descriptor",
        "missing_body",
        "changed_body",
        "missing_codec",
        "changed_codec",
    ] {
        let f = Fixture::new();
        let mock = Mock::response(f.response());
        let attempt = retained(&f, mock.clone());
        let ledger = f.ledger();
        let task = ledger.inspect().unwrap().tasks[&attempt.attempt.task_key]
            .spec
            .clone();
        let descriptor = f.temp.path().join(task.input.path.as_str());
        let body = f
            .temp
            .path()
            .join(attempt.spool.as_ref().unwrap().response.path.as_str());
        let codec = f
            .temp
            .path()
            .join(attempt.bound.codec.as_ref().unwrap().path.as_str());
        let path = match fault {
            "missing_descriptor" => &descriptor,
            "missing_codec" | "changed_codec" => &codec,
            _ => &body,
        };
        let before = ledger.inspect().unwrap();
        let original = std::fs::read(path).unwrap();
        if fault.starts_with("missing_") {
            std::fs::remove_file(path).unwrap();
        }
        if fault.starts_with("changed_") {
            std::fs::write(path, b"changed").unwrap();
        }
        let dispatcher = f.dispatcher(mock.clone());
        let error = dispatcher
            .recover_response(
                &ledger,
                &f.service,
                &attempt.attempt.task_key,
                &attempt.attempt,
            )
            .err()
            .unwrap();
        assert_eq!(
            error.disposition,
            DispatchDisposition::OutcomeUnknown,
            "{fault}"
        );
        assert_eq!(error.attempt.as_ref(), Some(&attempt.attempt));
        assert!(error.materialization.is_none(), "{fault}");
        let after = ledger.inspect().unwrap();
        assert_eq!(after.budget, before.budget, "{fault}");
        assert_eq!(after.attempts.len(), 1, "{fault}");
        assert_eq!(after.attempts[0].phase, AttemptPhase::Received, "{fault}");
        assert!(after.attempts[0].receipt.is_none(), "{fault}");
        assert!(after.attempts[0].outputs.is_empty(), "{fault}");
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1, "{fault}");
        std::fs::write(path, original).unwrap();
        let recovered = f
            .app
            .graph_extract_api(&f.request, &f.service, &dispatcher, f.options.clone())
            .unwrap();
        assert!(recovered.reused);
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1, "{fault}");
        assert_eq!(
            ledger.inspect().unwrap().attempts[0].phase,
            AttemptPhase::Settled
        );
    }
}

struct StopAt(LedgerCheckpoint, std::sync::atomic::AtomicBool);
impl LedgerFault for StopAt {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == self.0 && !self.1.swap(true, Ordering::SeqCst) {
            Err(WikiError::new(
                ErrorCode::Internal,
                "injected extraction accounting boundary",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn acknowledged_generation_receipt_resumes_without_rewriting_output_or_resending() {
    for point in [
        LedgerCheckpoint::AfterOutputsCommitted,
        LedgerCheckpoint::BeforeSettlement,
        LedgerCheckpoint::AfterSettlement,
    ] {
        let f = Fixture::new();
        let mock = Mock::response(f.response());
        let dispatcher = f.dispatcher(mock.clone());
        let mut options = f.options.clone();
        options.fault = Some(Arc::new(StopAt(
            point,
            std::sync::atomic::AtomicBool::new(false),
        )));
        assert!(
            f.app
                .graph_extract_api(&f.request, &f.service, &dispatcher, options)
                .is_err()
        );
        let before = f.ledger().inspect().unwrap();
        let output = before.attempts[0].outputs[0].clone();
        let output_bytes = std::fs::read(f.temp.path().join(output.path.as_str())).unwrap();
        let paid_receipt = before.attempts[0].receipt.clone().unwrap();
        let recovered = dispatcher
            .recover_response(
                &f.ledger(),
                &f.service,
                &before.attempts[0].attempt.task_key,
                &before.attempts[0].attempt,
            )
            .unwrap_or_else(|failure| panic!("{:?}", failure.error));
        assert_eq!(
            recovered.materialization.receipt.output_disposition,
            OutputDisposition::Validated
        );
        assert_eq!(
            recovered.materialization.receipt.outputs,
            vec![output.clone()]
        );
        assert!(recovered.materialization.draft.operations.is_empty());
        let resumed = f
            .app
            .graph_extract_api(&f.request, &f.service, &dispatcher, f.options.clone())
            .unwrap();
        assert!(resumed.reused);
        assert_eq!(resumed.output, Some(output.clone()));
        assert_eq!(resumed.receipt, Some(paid_receipt));
        assert_eq!(
            std::fs::read(f.temp.path().join(output.path.as_str())).unwrap(),
            output_bytes
        );
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(f.ledger().inspect().unwrap().state, RunState::Completed);
    }
}

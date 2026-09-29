use lwiki as library;
#[path = "fixtures/p18/common.rs"]
mod common;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod provider;
use common::*;
use lwiki::{
    app::*,
    changes::*,
    domain::*,
    graph::{generation_cache, *},
    jobs::*,
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
fn retained_response_wrong_model_contract_never_reuses_or_resends() {
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
    assert!(
        dispatch
            .decode_retained(
                &f.ledger(),
                &service,
                &a.attempt.task_key,
                lwiki::providers::types::DispatchPurpose::Task,
                &a.attempt
            )
            .is_err()
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert!(f.ledger().inspect().unwrap().attempts[0].outputs.is_empty());
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

#[test]
fn local_retained_recovery_failures_never_reject_paid_output_and_restore_without_send() {
    use lwiki::providers::types::DispatchDisposition;
    for fault in [
        "missing_descriptor",
        "missing_body",
        "changed_body",
        "wrong_service",
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
        let path = if fault == "missing_descriptor" {
            &descriptor
        } else {
            &body
        };
        let original = std::fs::read(path).unwrap();
        if fault.starts_with("missing_") {
            std::fs::remove_file(path).unwrap();
        }
        if fault == "changed_body" {
            std::fs::write(path, b"changed").unwrap();
        }
        let config = f.temp.path().join("providers.toml");
        let config_bytes = std::fs::read(&config).unwrap();
        let different = if fault == "wrong_service" {
            let changed = String::from_utf8(config_bytes.clone())
                .unwrap()
                .replace("test-model", "changed-model");
            std::fs::write(&config, changed).unwrap();
            Some(
                lwiki::config::providers::ProviderConfig::load(&config)
                    .unwrap()
                    .authorize(&f.fs, &id("vault_test"), "primary", Capability::Generate)
                    .unwrap(),
            )
        } else {
            None
        };
        let dispatcher = f.dispatcher(mock.clone());
        let error = dispatcher
            .recover_response(
                &ledger,
                different.as_ref().unwrap_or(&f.service),
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
        assert!(ledger.inspect().unwrap().attempts[0].receipt.is_none());
        std::fs::write(path, original).unwrap();
        std::fs::write(config, config_bytes).unwrap();
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

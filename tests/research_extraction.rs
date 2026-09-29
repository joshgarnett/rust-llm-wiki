use lwiki as library;
#[path = "fixtures/p18/common.rs"]
mod common;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod provider;
use common::*;
use lwiki::{
    app::{OfflineApp, OperationOptions},
    domain::*,
    graph::{generation_cache::GenerationTaskPlan, research_extract, *},
    jobs::*,
    sources::SourceView,
    vault::WriterPermit,
};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn containing(
    f: &Fixture,
    options: JobOptions,
) -> (JobLedger, GenerationTaskPlan, VerifiedPacket, TaskSpec) {
    let exported = f.app.graph_extract_agent(&f.request.export).unwrap();
    let view = SourceView::from_fs_bounded(&f.fs, 64 * 1024 * 1024, 4096).unwrap();
    let packet = lwiki::graph::packet::load_packet(&view, &exported.packet.packet_id).unwrap();
    let run = id("run_containing");
    let plan = research_extract::plan_task(&run, &packet, &f.service, 1024, 3, vec![]).unwrap();
    let mut local = plan.task.clone();
    local.stage = TaskStage::StageChanges;
    local.capability = None;
    local.dependencies = vec![plan.task.key.clone()];
    local.key = lwiki::jobs::tasks::task_key(&local).unwrap();
    let ledger = JobLedger::new(f.fs.clone(), id("vault_test"), run.clone(), options).unwrap();
    let writer = WriterPermit::acquire(f.fs.root(), Duration::from_secs(2)).unwrap();
    research_extract::retain_input(&f.fs, &writer, &plan).unwrap();
    let summary = f.service.summary();
    let mut spec = RunSpec {
        version: 1, run_id: run, vault_id: id("vault_test"), title: "Containing extraction test".into(),
        created_at_utc_ms: f.request.created_at_utc_ms, deadline_utc_ms: f.request.deadline_utc_ms,
        scope: serde_json::from_value(json!({"operation":"containing_extraction_test","question":null,
            "exclusions":[],"source_snapshot":null,"input_records":[],"read_preconditions":[],
            "profile_fingerprints":{summary.profile_id:summary.profile_fingerprint},"scope_payload_hash":null})).unwrap(),
        config_fingerprint: summary.config_fingerprint, input_fingerprint: Blake3Hash::digest([]),
        limits: f.request.limits.clone(), tasks: vec![plan.task.clone(), local.clone()], prior_accounting: PriorAccounting::None,
    };
    spec.input_fingerprint = lwiki::jobs::tasks::input_fingerprint(&spec).unwrap();
    ledger.create(&writer, spec).unwrap();
    drop(writer);
    ledger.start().unwrap();
    (ledger, plan, packet, local)
}
fn reopen(f: &Fixture) -> JobLedger {
    JobLedger::new(
        f.fs.clone(),
        id("vault_test"),
        id("run_containing"),
        f.options.clone(),
    )
    .unwrap()
}
fn delete_cache(f: &Fixture) {
    let path = f.temp.path().join(".wiki/cache");
    if path.exists() {
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn extraction_uses_containing_accounting_and_never_completes_run() {
    let f = Fixture::new();
    let (ledger, plan, packet, local) = containing(&f, f.options.clone());
    let genesis = ledger.inspect().unwrap().spec;
    let mock = Mock::response(f.response());
    let out = f
        .app
        .execute_existing_task(
            &ledger,
            &plan.task,
            &packet,
            &f.service,
            &f.dispatcher(mock.clone()),
            false,
        )
        .unwrap();
    let i = ledger.inspect().unwrap();
    assert_eq!(out.run_id, genesis.run_id);
    assert_eq!(i.spec, genesis);
    assert_eq!(i.state, RunState::Running);
    assert_eq!(i.tasks[&plan.task.key].state, TaskState::Completed);
    assert_eq!(i.tasks[&local.key].state, TaskState::Pending);
    assert_eq!(i.budget.dispatched_requests, 1);
    assert_eq!(i.attempts[0].phase, AttemptPhase::Settled);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert!(!f.temp.path().join("runs/run_api").exists());
    let imported = out.import.unwrap();
    assert!(imported.prepared.is_some());
    assert!(
        !f.temp
            .path()
            .join(imported.extraction.path.as_str())
            .exists()
    );
}

struct StopAt(LedgerCheckpoint, AtomicBool);
impl LedgerFault for StopAt {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == self.0 && !self.1.swap(true, Ordering::SeqCst) {
            Err(WikiError::new(
                ErrorCode::Internal,
                "injected research extraction boundary",
            ))
        } else {
            Ok(())
        }
    }
}

#[test]
fn paid_local_repair_survives_cache_loss_with_zero_resend() {
    for boundary in [
        LedgerCheckpoint::AfterSpoolMetadataSync,
        LedgerCheckpoint::AfterOutputsCommitted,
        LedgerCheckpoint::BeforeSettlement,
        LedgerCheckpoint::AfterSettlement,
        LedgerCheckpoint::BeforeSpoolRemove,
    ] {
        let f = Fixture::new();
        let mut options = f.options.clone();
        options.fault = Some(Arc::new(StopAt(boundary, AtomicBool::new(false))));
        let (ledger, plan, packet, _) = containing(&f, options);
        let mock = Mock::response(f.response());
        let dispatcher = f.dispatcher(mock.clone());
        assert!(
            f.app
                .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
                .is_err(),
            "{boundary:?}"
        );
        delete_cache(&f);
        let ledger = reopen(&f);
        let before = ledger.replay().unwrap().inspection;
        let receipt = before.attempts[0].receipt.clone();
        let saved_output = before.attempts[0].outputs.first().map(|o| {
            (
                o.clone(),
                std::fs::read(f.temp.path().join(o.path.as_str())).unwrap(),
            )
        });
        let descriptor_path = f.temp.path().join(plan.task.input.path.as_str());
        let descriptor = std::fs::read(&descriptor_path).unwrap();
        std::fs::remove_file(&descriptor_path).unwrap();
        assert!(
            f.app
                .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
                .is_err()
        );
        assert_eq!(ledger.inspect().unwrap().attempts[0].receipt, receipt);
        std::fs::write(&descriptor_path, descriptor).unwrap();
        let out = f
            .app
            .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
            .unwrap();
        assert!(out.reused);
        if let Some((output, bytes)) = saved_output {
            assert_eq!(out.output, Some(output.clone()));
            assert_eq!(
                std::fs::read(f.temp.path().join(output.path.as_str())).unwrap(),
                bytes
            );
            assert_eq!(out.receipt, receipt);
        }
        let final_state = ledger.inspect().unwrap();
        assert_eq!(final_state.state, RunState::Running);
        assert_eq!(final_state.budget.dispatched_requests, 1);
        assert_eq!(final_state.attempts.len(), 1);
        assert_eq!(final_state.attempts[0].phase, AttemptPhase::Settled);
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn admitted_descriptor_and_staged_output_survive_cache_deletion() {
    let f = Fixture::new();
    let (ledger, plan, packet, _) = containing(&f, f.options.clone());
    let descriptor = std::fs::read(f.temp.path().join(plan.task.input.path.as_str())).unwrap();
    delete_cache(&f);
    assert_eq!(
        std::fs::read(f.temp.path().join(plan.task.input.path.as_str())).unwrap(),
        descriptor
    );
    let mock = Mock::response(f.response());
    let dispatcher = f.dispatcher(mock.clone());
    let first = f
        .app
        .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
        .unwrap();
    let prepared = first.import.as_ref().unwrap().prepared.clone();
    delete_cache(&f);
    let offline = OfflineApp::new(
        f.fs.clone(),
        OperationOptions {
            offline: true,
            lock_timeout_ms: 5000,
            ..Default::default()
        },
    )
    .unwrap();
    let again = offline
        .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
        .unwrap();
    assert_eq!(first.output, again.output);
    assert_eq!(first.receipt, again.receipt);
    assert_eq!(prepared, again.import.unwrap().prepared);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    // The original prepared proposal remains a real applyable changeset.
    f.apply(prepared.as_ref().unwrap());
}

#[test]
fn changed_source_cannot_activate_retained_paid_output() {
    let f = Fixture::new();
    let mut options = f.options.clone();
    options.fault = Some(Arc::new(StopAt(
        LedgerCheckpoint::AfterSpoolMetadataSync,
        AtomicBool::new(false),
    )));
    let (ledger, plan, packet, _) = containing(&f, options);
    let mock = Mock::response(f.response());
    let dispatcher = f.dispatcher(mock.clone());
    assert!(
        f.app
            .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
            .is_err()
    );
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
        &std::collections::BTreeMap::from([("title".into(), json!("Changed source"))]),
        None,
        &source.source_hash,
    )
    .unwrap();
    std::fs::write(f.temp.path().join(path.as_str()), changed).unwrap();
    let ledger = reopen(&f);
    assert!(
        f.app
            .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
            .is_err()
    );
    let i = ledger.inspect().unwrap();
    assert!(i.attempts[0].receipt.is_none());
    assert!(i.attempts[0].outputs.is_empty());
    assert_ne!(i.tasks[&plan.task.key].state, TaskState::Completed);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn retained_extraction_preserves_resolution_and_explicit_proposal_decisions() {
    use lwiki::changes::PreparedChange;
    let f = Fixture::new();
    let (ledger, plan, packet, _) = containing(&f, f.options.clone());
    let mock = Mock::response(f.response());
    let dispatcher = f.dispatcher(mock.clone());
    let first = f
        .app
        .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
        .unwrap();
    let imported = first.import.unwrap();
    f.apply(imported.prepared.as_ref().unwrap());
    let extraction_id = imported
        .extraction
        .record
        .as_ref()
        .unwrap()
        .record_id
        .clone();
    let mappings = vec![
        json!({"mention_id":"m1","operation":"CreateEntity","title":"Ada","entity_type":"person","reason":"Explicit fixture identity"}),
        json!({"mention_id":"m2","operation":"CreateEntity","title":"Acme","entity_type":"organization","reason":"Explicit fixture identity"}),
        json!({"mention_id":"m3","operation":"CreateEntity","title":"Tool","entity_type":"component","reason":"Explicit fixture identity"}),
        json!({"mention_id":"m4","operation":"RejectMention","reason":"Explicit fixture rejection"}),
    ];
    let resolution = json!({"schema":RESOLUTION_SCHEMA,"extraction_id":extraction_id,"expected_hash":f.record(&extraction_id).source_hash,"mappings":mappings});
    let result = f
        .app
        .graph_resolve(&serde_json::to_vec(&resolution).unwrap())
        .unwrap();
    let prepared: PreparedChange = serde_json::from_value(result["prepared"].clone()).unwrap();
    f.apply(&prepared);
    let mut decisions = vec![];
    for (local, decision) in [("a1", "accept"), ("a2", "reject")] {
        let aid = &imported.allocations.assertions[&PacketLocalId::new(local).unwrap()];
        let eid = &imported.allocations.evidence[&PacketLocalId::new(local).unwrap()][0];
        decisions.push(json!({"assertion_id":aid,"expected_hash":f.record(aid).source_hash,"decision":decision,"reason":"Explicit complete fixture review","evidence_checks":[{"evidence_id":eid,"expected_hash":f.record(eid).source_hash,"assessment":"supports"}]}));
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
    delete_cache(&f);
    let again = f
        .app
        .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
        .unwrap();
    assert_eq!(again.import.unwrap().allocations, imported.allocations);
    assert_eq!(f.record(&extraction_id).raw, before);
    for (local, status) in [("a1", "accepted"), ("a2", "rejected")] {
        assert_eq!(
            f.record(&imported.allocations.assertions[&PacketLocalId::new(local).unwrap()])
                .canonical
                .unwrap()
                .string("wiki_status"),
            Some(status)
        );
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(ledger.inspect().unwrap().state, RunState::Running);
}

#[test]
fn containing_options_cannot_weaken_app_offline_or_dry_run() {
    let f = Fixture::new();
    let (ledger, plan, packet, _) = containing(&f, f.options.clone());
    let before = ledger.inspect().unwrap();
    let mock = Mock::response(f.response());
    let dispatcher = f.dispatcher(mock.clone());
    for (offline, dry_run) in [(true, false), (false, true)] {
        let app = OfflineApp::new(
            f.fs.clone(),
            OperationOptions {
                offline,
                dry_run,
                lock_timeout_ms: 5000,
                ..Default::default()
            },
        )
        .unwrap();
        let result =
            app.execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false);
        if dry_run {
            assert!(result.unwrap().dry_run);
        } else {
            assert_eq!(result.err().unwrap().code, ErrorCode::OfflineUnavailable);
        }
        assert_eq!(ledger.inspect().unwrap(), before);
    }
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
}

struct SourceChangedWhileSending {
    mock: Arc<Mock>,
    path: std::path::PathBuf,
    changed: Vec<u8>,
}
impl lwiki::providers::types::Transport for SourceChangedWhileSending {
    fn execute<'a>(
        &'a self,
        request: lwiki::providers::types::AuthenticatedRequest<'a>,
        context: lwiki::providers::types::TransportContext,
    ) -> lwiki::providers::types::TransportFuture<'a> {
        let response =
            lwiki::providers::types::Transport::execute(self.mock.as_ref(), request, context);
        Box::pin(async move {
            let result = response.await;
            std::fs::write(&self.path, &self.changed).unwrap();
            result
        })
    }
}

#[test]
fn source_change_during_dispatch_preserves_paid_response_for_local_repair() {
    use lwiki::providers::{dispatcher::Dispatcher, types::DispatchOptions};
    let f = Fixture::new();
    let (ledger, plan, packet, _) = containing(&f, f.options.clone());
    let source = f.record(&f.request.export.source_id);
    let path =
        f.fs.root()
            .scan_markdown()
            .unwrap()
            .into_iter()
            .find(|p| std::fs::read(f.temp.path().join(p.as_str())).unwrap() == source.raw)
            .unwrap();
    let absolute = f.temp.path().join(path.as_str());
    let changed = lwiki::records::edit_note(
        &source,
        &std::collections::BTreeMap::from([("title".into(), json!("Changed while sending"))]),
        None,
        &source.source_hash,
    )
    .unwrap();
    let mock = Mock::response(f.response());
    let dispatcher = Dispatcher::new(
        f.fs.clone(),
        DispatchOptions {
            broker: f.broker.clone(),
            transport: Arc::new(SourceChangedWhileSending {
                mock: mock.clone(),
                path: absolute.clone(),
                changed,
            }),
            jitter: Arc::new(provider::ZeroJitter),
        },
    );
    assert!(
        f.app
            .execute_existing_task(&ledger, &plan.task, &packet, &f.service, &dispatcher, false)
            .is_err()
    );
    let i = ledger.inspect().unwrap();
    assert_eq!(i.attempts[0].phase, AttemptPhase::Received);
    assert!(i.attempts[0].receipt.is_none());
    assert!(i.attempts[0].outputs.is_empty());
    assert!(i.attempts[0].spool.is_some());
    std::fs::write(absolute, source.raw).unwrap();
    let recovered = f
        .app
        .execute_existing_task(
            &ledger,
            &plan.task,
            &packet,
            &f.service,
            &f.dispatcher(mock.clone()),
            false,
        )
        .unwrap();
    assert!(recovered.reused);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(ledger.inspect().unwrap().budget.dispatched_requests, 1);
}

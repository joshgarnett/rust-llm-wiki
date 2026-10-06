//! Accounted generation feeds the same source-local proposal importer as agent output.
use super::{generation_cache, import, packet, wire, *};
use crate::{
    app::*,
    catalog::{Catalog, CatalogGraphValidator},
    changes::*,
    config::providers::TrustedService,
    domain::*,
    jobs::{self, DispatcherLedgerApi, *},
    providers::{dispatcher::Dispatcher, types::*},
    sources::SourceView,
    vault::WriterPermit,
};
use serde::Serialize;
use std::{collections::BTreeMap, time::Duration};

pub struct ApiExtractionRequest {
    pub export: ExportRequest,
    pub run_id: RecordId,
    pub created_at_utc_ms: i64,
    pub deadline_utc_ms: i64,
    pub limits: LifetimeLimits,
    pub max_output_tokens: u64,
    pub new_extraction: bool,
    pub requested_limits: Option<crate::app::remote::RequestedJobLimits>,
}
#[derive(Serialize)]
pub struct ApiExtractionOutcome {
    pub packet: ExtractionPacket,
    pub coverage: ExtractionCoverage,
    pub run_id: RecordId,
    pub task_key: Option<Blake3Hash>,
    pub import: Option<ImportOutcome>,
    pub output: Option<DurableOutputRef>,
    pub receipt: Option<DurableOutputRef>,
    pub reused: bool,
    pub dry_run: bool,
    pub warnings: Vec<String>,
}

pub(crate) struct ExtractionTaskContext<'a> {
    pub task: &'a TaskSpec,
    pub packet: &'a VerifiedPacket,
    pub service: &'a TrustedService,
    pub dispatcher: &'a Dispatcher,
    pub options: &'a JobOptions,
    pub new_extraction: bool,
    pub complete_run: bool,
    pub coverage: &'a ExtractionCoverage,
}

impl OfflineApp {
    /// Pure stable default identity; packet storage timestamps do not identify input.
    pub fn default_api_extraction_run_id(
        &self,
        export: &ExportRequest,
        service: &TrustedService,
        max_output_tokens: u64,
    ) -> Result<RecordId> {
        let view = SourceView::from_fs_bounded(&self.fs, packet::SOURCE_CAP, 4096)?;
        let plan = packet::build_packet(&view, export)?;
        let dependencies: Vec<_> = plan
            .dependencies
            .iter()
            .filter(|d| d.path != plan.locator.path)
            .collect();
        let summary = service.summary();
        let identity = packet::canonical_json(&(
            "lwiki.api-extraction-run.v1",
            &plan.packet,
            dependencies,
            &summary.profile_id,
            &summary.service_id,
            &summary.model,
            &summary.revision,
            &summary.endpoint_fingerprint,
            &summary.profile_fingerprint,
            &summary.config_fingerprint,
            max_output_tokens,
        ))?;
        RecordId::new(format!("run_api_{}", Blake3Hash::digest(identity).hex()))
    }
    pub fn graph_extract_api(
        &self,
        request: &ApiExtractionRequest,
        service: &TrustedService,
        dispatcher: &Dispatcher,
        options: JobOptions,
    ) -> Result<ApiExtractionOutcome> {
        service.validate_generation_output_limit(request.max_output_tokens)?;
        // Planning is local and must not inspect credentials, ledger, or paid state.
        if self.options.dry_run || options.policy.dry_run {
            let view = SourceView::from_fs_bounded(&self.fs, packet::SOURCE_CAP, 4096)?;
            let plan = packet::build_packet(&view, &request.export)?;
            return Ok(ApiExtractionOutcome {
                packet: plan.packet,
                coverage: plan.coverage,
                run_id: request.run_id.clone(),
                task_key: None,
                import: None,
                output: None,
                receipt: None,
                reused: plan.reused,
                dry_run: true,
                warnings: vec![],
            });
        }
        let offline = self.options.offline || options.policy.offline;
        let initial = SourceView::from_fs_bounded(&self.fs, packet::SOURCE_CAP, 4096)?;
        let planned = packet::build_packet(&initial, &request.export)?;
        let ledger = JobLedger::new(
            self.fs.clone(),
            self.vault_id.clone(),
            request.run_id.clone(),
            options.clone(),
        )?;
        let run_path = VaultRelativePath::new(format!("runs/{}/run.md", request.run_id))?;
        let existing =
            crate::changes::prepare::read_bounded(&self.fs, &run_path, jobs::EVENT_MAX_BYTES)?
                .is_some();
        // Offline cannot create even a packet/job before discovering a retained result.
        if offline && (!planned.reused || !existing) {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "API extraction has no retained response for offline reuse",
            ));
        }
        let packet = if offline {
            packet::load_packet(&initial, &planned.packet.packet_id)?
        } else {
            let internal = OfflineApp {
                fs: self.fs.clone(),
                vault_id: self.vault_id.clone(),
                options: OperationOptions {
                    stage_only: false,
                    ..self.options
                },
            };
            let exported = internal.graph_extract_agent(&request.export)?;
            packet::load_packet(
                &SourceView::from_fs_bounded(&self.fs, packet::SOURCE_CAP, 4096)?,
                &exported.packet.packet_id,
            )?
        };
        let task_plan = generation_cache::plan_task(&packet, service, request.max_output_tokens)?;
        let task = &task_plan.task;
        let engine = ChangeEngine::new(self.fs.clone())?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        if !existing {
            let writer = WriterPermit::acquire(
                self.fs.root(),
                Duration::from_millis(self.options.lock_timeout_ms),
            )?;
            engine.recover(&writer, &CatalogGraphValidator, &catalog)?;
            catalog.guard_current(None)?;
            generation_cache::retain_input(&self.fs, &writer, &task_plan)?;
            let summary = service.summary();
            let mut spec = RunSpec {
                version: 1,
                run_id: request.run_id.clone(),
                vault_id: self.vault_id.clone(),
                title: "Bounded API extraction".into(),
                created_at_utc_ms: request.created_at_utc_ms,
                deadline_utc_ms: request.deadline_utc_ms,
                scope: RunScope {
                    operation: "graph_extract_api".into(),
                    question: None,
                    exclusions: vec![],
                    source_snapshot: None,
                    input_records: vec![RecordRef {
                        vault_id: self.vault_id.clone(),
                        record_id: packet.packet().packet_id.clone(),
                        expected_kind: RecordKind::ExtractionPacket,
                    }],
                    read_preconditions: vec![ReadDependency {
                        path: packet.locator().path.clone(),
                        expected: crate::vault::ExpectedState::Hash(
                            packet.locator().observed_hash.clone(),
                        ),
                    }],
                    profile_fingerprints: BTreeMap::from([(
                        summary.profile_id,
                        summary.profile_fingerprint,
                    )]),
                    scope_payload_hash: Some(packet.packet().packet_fingerprint.clone()),
                },
                config_fingerprint: summary.config_fingerprint,
                input_fingerprint: Blake3Hash::digest([]),
                limits: request.limits.clone(),
                tasks: vec![task.clone()],
                prior_accounting: self.prior_accounting_for_new_run()?,
            };
            spec.input_fingerprint = jobs::tasks::input_fingerprint(&spec)?;
            ledger.create(&writer, spec)?;
        } else if !offline {
            let writer = WriterPermit::acquire(
                self.fs.root(),
                Duration::from_millis(self.options.lock_timeout_ms),
            )?;
            generation_cache::retain_input(&self.fs, &writer, &task_plan)?;
        }
        let inspection = ledger.replay()?.inspection;
        if existing {
            let deadline_ms = u64::try_from(
                request
                    .deadline_utc_ms
                    .saturating_sub(request.created_at_utc_ms),
            )
            .unwrap_or(0);
            crate::app::remote::validate_retained_arguments(
                &inspection,
                &request.limits,
                deadline_ms,
                request.requested_limits.as_ref(),
            )?;
        }
        if inspection.spec.scope.operation != "graph_extract_api"
            || inspection.tasks.len() != 1
            || inspection
                .tasks
                .get(&task.key)
                .is_none_or(|t| &t.spec != task)
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "extraction run inputs changed; use a new run ID",
            ));
        }
        self.execute_generation_task(
            &ledger,
            ExtractionTaskContext {
                task,
                packet: &packet,
                service,
                dispatcher,
                options: &options,
                new_extraction: request.new_extraction,
                complete_run: true,
                coverage: &planned.coverage,
            },
        )
    }
    pub(crate) fn execute_generation_task(
        &self,
        ledger: &JobLedger,
        context: ExtractionTaskContext<'_>,
    ) -> Result<ApiExtractionOutcome> {
        let ExtractionTaskContext {
            task,
            packet,
            service,
            dispatcher,
            options,
            new_extraction,
            complete_run,
            coverage,
        } = context;
        let offline = self.options.offline || options.policy.offline;
        let inspection = ledger.replay()?.inspection;
        if inspection
            .tasks
            .get(&task.key)
            .is_none_or(|t| &t.spec != task)
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "extraction task differs from admitted inputs",
            ));
        }
        let engine = ChangeEngine::new(self.fs.clone())?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let retained_task = &inspection.tasks[&task.key];
        // Canonical reservations may change after packet export. Revalidate before
        // either paid admission or returning retained output as reusable.
        let fresh_view = SourceView::from_fs_bounded(&self.fs, packet::SOURCE_CAP, 4096)?;
        let fresh_packet = packet::load_packet(&fresh_view, &packet.packet().packet_id)?;
        if fresh_packet.packet() != packet.packet()
            || fresh_packet.dependencies() != packet.dependencies()
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "extraction source or packet authority changed before execution",
            ));
        }
        let (output, receipt, response, reused) = if retained_task.state == TaskState::Completed {
            let output = retained_task
                .outputs
                .first()
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::RecoveryRequired,
                        "completed extraction has no output",
                    )
                })?
                .clone();
            let retained = generation_cache::load_output(&self.fs, &output, task, packet)?;
            let receipt = inspection
                .attempts
                .iter()
                .find(|a| a.attempt == retained.attempt)
                .and_then(|a| a.receipt.clone())
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::RecoveryRequired,
                        "completed generation receipt missing",
                    )
                })?;
            ledger.settle(&retained.attempt)?;
            ledger.remove_spool_after_verified_commit(&retained.attempt)?;
            if complete_run && inspection.state == RunState::Running {
                crate::app::remote::finish_provider_job(ledger)?;
            }
            self.checkpoint_generation(ledger)?;
            (output, receipt, retained.response.into_bytes(), true)
        } else {
            let pending = inspection.attempts.iter().find(|a| {
                a.attempt.task_key == task.key
                    && matches!(
                        a.phase,
                        AttemptPhase::Received | AttemptPhase::OutputCommitted
                    )
            });
            let generated = if let Some(a) = pending {
                match dispatcher.recover_response(ledger, service, &task.key, &a.attempt) {
                    Ok(output) => output,
                    Err(failure) => {
                        if let Some(plan) = &failure.materialization {
                            self.commit_generation_task(ledger, plan, complete_run)?;
                            self.checkpoint_generation(ledger)?;
                        }
                        return Err(failure.error);
                    }
                }
            } else {
                if offline {
                    return Err(WikiError::new(
                        ErrorCode::OfflineUnavailable,
                        "API extraction requires a paid request",
                    ));
                }
                if options.cancel.is_cancelled() {
                    return Err(WikiError::new(
                        ErrorCode::Cancelled,
                        "extraction cancelled before dispatch",
                    ));
                }
                if inspection
                    .attempts
                    .iter()
                    .any(|attempt| attempt.phase == AttemptPhase::DispatchIntent)
                    && !options.policy.retry_uncertain
                {
                    let mut error = WikiError::new(
                        ErrorCode::RecoveryRequired,
                        "uncertain extraction requires explicit --retry-uncertain; prior billing and concurrency holds remain",
                    );
                    error.details = serde_json::json!({"reason":"uncertain_retry_requires_opt_in","recovery_action":format!("retry graph extract with --run {} --retry-uncertain after inspecting jobs status; the prior attempt may be billed", inspection.spec.run_id)});
                    return Err(extraction_failure(
                        error,
                        &inspection.spec.run_id,
                        packet,
                        &task.key,
                        coverage,
                    ));
                }
                match inspection.state {
                    RunState::Planned if complete_run => {
                        ledger.start()?;
                    }
                    RunState::Paused | RunState::Stopped if complete_run => {
                        ledger.resume(None)?;
                    }
                    RunState::Planned
                    | RunState::Paused
                    | RunState::Stopped
                    | RunState::Failed
                    | RunState::Completed => {
                        return Err(WikiError::new(
                            ErrorCode::RecoveryRequired,
                            "run cannot dispatch new extraction",
                        ));
                    }
                    RunState::Running => {}
                }
                dispatcher
                    .execute(ledger, service, &task.key, DispatchPurpose::Task)
                    .map_err(|failure| {
                        extraction_failure(
                            failure.error,
                            &inspection.spec.run_id,
                            packet,
                            &task.key,
                            coverage,
                        )
                    })?
            };
            let ValidatedOutput::Generation { text, .. } = generated.output else {
                return Err(WikiError::invalid("generation result has wrong capability"));
            };
            let response = text.into_bytes();
            let view = SourceView::from_fs_bounded(&self.fs, packet::SOURCE_CAP, 4096)?;
            let current_packet = packet::load_packet(&view, &packet.packet().packet_id)?;
            if current_packet.packet() != packet.packet()
                || current_packet.dependencies() != packet.dependencies()
            {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "extraction source changed before publication",
                ));
            }
            if let Err(error) = wire::validate_response(packet, &view, &response) {
                if error.code != ErrorCode::ExtractionInvalid {
                    return Err(error);
                }
                let reason = error
                    .details
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("extraction_invalid");
                // This paid output remains private and has no authority as evidence.
                // Retain it before publishing the rejected receipt so a host can
                // inspect and submit corrected JSON through the ordinary importer.
                let diagnostic_error = ledger
                    .retain_diagnostic(
                        &generated.attempt,
                        jobs::diagnostics::DiagnosticKind::SemanticRejection,
                        &response,
                        response.len() as u64,
                        reason,
                    )
                    .err();
                let rejected = jobs::checkpoint::receipt_plan(
                    ledger,
                    &generated.attempt,
                    OutputDisposition::Rejected,
                    vec![],
                    vec![],
                    vec![],
                )?;
                self.commit_generation_task(ledger, &rejected, complete_run)?;
                self.checkpoint_generation(ledger)?;
                if diagnostic_error.is_some() {
                    let mut error = error;
                    error.details["diagnostic_retention"] =
                        serde_json::Value::String("failed".into());
                    return Err(extraction_failure(
                        error,
                        &inspection.spec.run_id,
                        packet,
                        &task.key,
                        coverage,
                    ));
                }
                return Err(extraction_failure(
                    error,
                    &inspection.spec.run_id,
                    packet,
                    &task.key,
                    coverage,
                ));
            }
            if generated.materialization.receipt.output_disposition == OutputDisposition::Validated
            {
                let (output, receipt) = self.reuse_acknowledged_generation_task(
                    ledger,
                    task,
                    packet,
                    &generated.attempt,
                    &generated.materialization.receipt,
                    &response,
                    complete_run,
                )?;
                (output, receipt, response, true)
            } else {
                let timestamp = generated
                    .materialization
                    .draft
                    .operations
                    .iter()
                    .filter_map(|o| o.proposed.as_ref())
                    .find_map(|bytes| {
                        crate::records::parse_note(bytes)
                            .canonical
                            .and_then(|record| record.string("wiki_occurred_at").map(str::to_owned))
                    })
                    .ok_or_else(|| {
                        WikiError::new(ErrorCode::RecoveryRequired, "received timestamp missing")
                    })?;
                let (output, write) = generation_cache::output_write(
                    &self.vault_id,
                    &generated.attempt,
                    packet,
                    &response,
                    &timestamp,
                )?;
                let plan = jobs::checkpoint::receipt_plan(
                    ledger,
                    &generated.attempt,
                    OutputDisposition::Validated,
                    vec![output.clone()],
                    vec![],
                    vec![write],
                )?;
                let receipt = self.commit_generation_task(ledger, &plan, complete_run)?;
                self.checkpoint_generation(ledger)?;
                (output, receipt, response, pending.is_some())
            }
        };
        // The sole importer performs all structural/evidence/identity validation and idempotency.
        let writer = WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(self.options.lock_timeout_ms),
        )?;
        engine.recover(&writer, &CatalogGraphValidator, &catalog)?;
        catalog.guard_current(None)?;
        let view = SourceView::from_fs_bounded(&self.fs, packet::SOURCE_CAP, 4096)?;
        let validated = wire::validate_response(packet, &view, &response)?;
        let imported = import::stage_import(
            &engine,
            &writer,
            &validated,
            if new_extraction {
                OriginPolicy::AllowNewResponse
            } else {
                OriginPolicy::ReuseOrConflict
            },
        )?;
        Ok(ApiExtractionOutcome {
            packet: packet.packet().clone(),
            coverage: imported.coverage.clone(),
            run_id: inspection.spec.run_id.clone(),
            task_key: Some(task.key.clone()),
            import: Some(imported),
            output: Some(output),
            receipt: Some(receipt),
            reused,
            dry_run: false,
            warnings: if complete_run {
                crate::app::remote::finish_provider_job(ledger)?
                    .into_iter()
                    .collect()
            } else {
                vec![]
            },
        })
    }
    #[cfg(test)]
    fn reuse_acknowledged_generation(
        &self,
        ledger: &JobLedger,
        task: &TaskSpec,
        packet: &VerifiedPacket,
        attempt: &AttemptRef,
        retained_receipt: &UsageReceipt,
        response: &[u8],
    ) -> Result<(DurableOutputRef, DurableOutputRef)> {
        self.reuse_acknowledged_generation_task(
            ledger,
            task,
            packet,
            attempt,
            retained_receipt,
            response,
            true,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn reuse_acknowledged_generation_task(
        &self,
        ledger: &JobLedger,
        task: &TaskSpec,
        packet: &VerifiedPacket,
        attempt: &AttemptRef,
        retained_receipt: &UsageReceipt,
        response: &[u8],
        complete_run: bool,
    ) -> Result<(DurableOutputRef, DurableOutputRef)> {
        if retained_receipt.outputs.len() != 1 || !retained_receipt.cache_outputs.is_empty() {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "retained extraction output shape differs",
            ));
        }
        let output = retained_receipt.outputs[0].clone();
        let retained = generation_cache::load_output(&self.fs, &output, task, packet)?;
        if retained.attempt != *attempt || retained.response.as_bytes() != response {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "retained extraction response differs",
            ));
        }
        let current = ledger.inspect()?;
        let receipt = current
            .attempts
            .iter()
            .find(|a| a.attempt == *attempt)
            .and_then(|a| a.receipt.clone())
            .ok_or_else(|| {
                WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "retained extraction receipt missing",
                )
            })?;
        ledger.settle(attempt)?;
        ledger.finish_remote_task(&task.key, vec![output.clone()], vec![], |_| Ok(false))?;
        if complete_run && ledger.inspect()?.state == RunState::Running {
            crate::app::remote::finish_provider_job(ledger)?;
        }
        ledger.remove_spool_after_verified_commit(attempt)?;
        self.checkpoint_generation(ledger)?;
        Ok((output, receipt))
    }
    fn commit_generation_task(
        &self,
        ledger: &JobLedger,
        plan: &MaterializationPlan,
        complete_run: bool,
    ) -> Result<DurableOutputRef> {
        let receipt_id = &plan.receipt.receipt_id;
        let receipt_path = VaultRelativePath::new(format!(
            "runs/{}/events/{receipt_id}.md",
            plan.attempt.run_id
        ))?;
        let receipt_bytes = plan
            .draft
            .operations
            .iter()
            .find(|o| o.target == receipt_path)
            .and_then(|o| o.proposed.clone())
            .or(crate::changes::prepare::read_bounded(
                &self.fs,
                &receipt_path,
                EVENT_MAX_BYTES,
            )?)
            .ok_or_else(|| WikiError::invalid("receipt payload missing"))?;
        let reference = DurableOutputRef {
            record: RecordRef {
                vault_id: self.vault_id.clone(),
                record_id: receipt_id.clone(),
                expected_kind: RecordKind::RunEvent,
            },
            path: receipt_path,
            hash: Blake3Hash::digest(receipt_bytes),
        };
        let writer = WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(self.options.lock_timeout_ms),
        )?;
        let engine = ChangeEngine::new(self.fs.clone())?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        engine.recover(&writer, &CatalogGraphValidator, &catalog)?;
        let prepared = engine.prepare(&writer, plan.draft.clone())?.prepared;
        engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
        drop(writer);
        ledger.outputs_committed(
            &plan.attempt,
            &prepared,
            reference.clone(),
            plan.receipt.outputs.clone(),
            vec![],
        )?;
        ledger.settle(&plan.attempt)?;
        if plan.receipt.output_disposition == OutputDisposition::Validated {
            ledger.finish_remote_task(
                &plan.attempt.task_key,
                plan.receipt.outputs.clone(),
                vec![],
                |_: &VectorCacheRef| Ok(false),
            )?;
            if complete_run {
                crate::app::remote::finish_provider_job(ledger)?;
            }
        }
        ledger.remove_spool_after_verified_commit(&plan.attempt)?;
        Ok(reference)
    }
    fn checkpoint_generation(&self, ledger: &JobLedger) -> Result<()> {
        let draft = ledger.checkpoint_plan()?;
        if draft.operations.is_empty() {
            return Ok(());
        }
        let writer = WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(self.options.lock_timeout_ms),
        )?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let prepared = if catalog.operation_state()?.is_some() {
            crate::jobs::checkpoint::publish_job_draft(&self.fs, &writer, ledger, draft)?
        } else {
            let engine = ChangeEngine::new(self.fs.clone())?;
            let prepared = engine.prepare(&writer, draft)?.prepared;
            engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
            prepared
        };
        drop(writer);
        ledger.checkpoint_committed(&prepared)?;
        Ok(())
    }
}

fn extraction_failure(
    mut error: WikiError,
    run_id: &RecordId,
    packet: &VerifiedPacket,
    task: &Blake3Hash,
    coverage: &ExtractionCoverage,
) -> WikiError {
    error.details = serde_json::json!({"run_id":run_id,"task_key":task,"packet_id":packet.packet().packet_id,"coverage":coverage,"proposals_staged":false,"retained_details":error.details});
    error
}

#[cfg(test)]
use crate::providers::wire_tests::common as provider;

#[cfg(test)]
#[allow(unused_imports)]
#[path = "../../tests/fixtures/p18/common.rs"]
mod acceptance_fixture;
#[cfg(test)]
mod recovery_tests {
    use super::*;
    use acceptance_fixture::{Fixture, Mock};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct KeepAcknowledgedSpool(AtomicBool);
    impl LedgerFault for KeepAcknowledgedSpool {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            if point == LedgerCheckpoint::BeforeSpoolRemove && !self.0.swap(true, Ordering::SeqCst)
            {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "retain acknowledged response for interleaving",
                ))
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn pending_branch_consumes_already_acknowledged_validated_plan_without_output_write() {
        let f = Fixture::new();
        let mock = Mock::response(f.response());
        let dispatcher = f.dispatcher(mock.clone());
        let mut options = f.options.clone();
        options.fault = Some(Arc::new(KeepAcknowledgedSpool(AtomicBool::new(false))));
        assert!(
            f.app
                .graph_extract_api(&f.request, &f.service, &dispatcher, options)
                .is_err()
        );
        let ledger = f.ledger();
        let inspection = ledger.inspect().unwrap();
        let attempt = &inspection.attempts[0];
        let generated = dispatcher
            .recover_response(
                &ledger,
                &f.service,
                &attempt.attempt.task_key,
                &attempt.attempt,
            )
            .unwrap_or_else(|failure| panic!("{:?}", failure.error));
        assert!(generated.materialization.draft.operations.is_empty());
        let ValidatedOutput::Generation { text, .. } = generated.output else {
            panic!("generation")
        };
        let view = SourceView::from_fs_bounded(&f.fs, packet::SOURCE_CAP, 4096).unwrap();
        let verified = packet::load_packet(&view, &f.packet.packet_id).unwrap();
        let task = &inspection.tasks[&attempt.attempt.task_key].spec;
        let output = attempt.outputs[0].clone();
        let original = std::fs::read(f.temp.path().join(output.path.as_str())).unwrap();
        let (reused, receipt) = f
            .app
            .reuse_acknowledged_generation(
                &ledger,
                task,
                &verified,
                &generated.attempt,
                &generated.materialization.receipt,
                text.as_bytes(),
            )
            .unwrap();
        assert_eq!(reused, output);
        assert_eq!(Some(receipt), attempt.receipt);
        assert_eq!(
            std::fs::read(f.temp.path().join(output.path.as_str())).unwrap(),
            original
        );
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(ledger.inspect().unwrap().state, RunState::Completed);
        assert!(ledger.inspect().unwrap().attempts[0].spool.is_none());
    }
}

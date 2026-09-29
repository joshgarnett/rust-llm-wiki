//! Explicit, accounted probes use the same sealed wire and paid-response ledger.
use super::{OfflineApp, remote::RemoteRuntime};
use crate::{
    domain::*,
    graph::packet::canonical_json,
    jobs::*,
    providers::{types::*, wire},
    vault::{ExpectedState, WriterPermit},
};
use serde::Serialize;
use std::{collections::BTreeMap, time::Duration};

#[derive(Debug, Serialize)]
pub struct ProbeOutcome {
    pub run_id: RecordId,
    pub role: ServiceRole,
    pub validated: bool,
    pub network_used: bool,
    pub inspection: LedgerInspection,
}
impl OfflineApp {
    pub fn probe_provider(
        &self,
        runtime: &RemoteRuntime,
        role: ServiceRole,
    ) -> Result<ProbeOutcome> {
        if self.options.dry_run
            || self.options.offline
            || runtime.job_options.policy.dry_run
            || runtime.job_options.policy.offline
        {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "provider probe prohibited by execution policy",
            ));
        }
        if runtime.service.summary().capability != role.capability() {
            return Err(WikiError::new(
                ErrorCode::ProfileUntrusted,
                "provider probe role differs",
            ));
        }
        let operation = match role {
            ServiceRole::Embed => {
                let utf8 = "lwiki explicit accounted probe".to_string();
                RemoteOperation::Embed {
                    inputs: vec![EmbeddingInput {
                        input_hash: Blake3Hash::digest(utf8.as_bytes()),
                        utf8,
                    }],
                    expected_dimensions: None,
                    representation_fingerprint: Blake3Hash::digest(
                        b"lwiki.probe.representation.v1",
                    ),
                }
            }
            ServiceRole::Generate => RemoteOperation::Generate {
                instructions: "Return a JSON object with ok set to true.".into(),
                data: "Explicit provider probe.".into(),
                output_schema: serde_json::json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
                max_output_tokens: probe_output_limit(runtime)?,
            },
            ServiceRole::Search => RemoteOperation::Search {
                query: "lwiki provider probe".into(),
                count: 1,
                page: 0,
            },
        };
        let input = RemoteInput {
            version: 1,
            operation,
        };
        let descriptor = canonical_json(&input)?;
        let fingerprints = wire::task_fingerprints(&runtime.service, &input)?;
        let run_id = RecordId::new(format!("run_probe_{}", uuid::Uuid::now_v7()))?;
        let path = VaultRelativePath::new(format!("runs/{run_id}/inputs/probe.json"))?;
        let mut task = TaskSpec {
            key: Blake3Hash::digest([]),
            stage: TaskStage::Probe,
            capability: Some(Capability::Probe),
            priority: 0,
            dependencies: vec![],
            input_hash: fingerprints.input,
            prompt_hash: fingerprints.prompt,
            schema_hash: fingerprints.schema,
            model_hash: Some(fingerprints.model),
            settings_hash: fingerprints.settings,
            source_bindings: vec![],
            input: BoundedPayloadRef {
                path: path.clone(),
                hash: Blake3Hash::digest(&descriptor),
                byte_len: descriptor.len() as u64,
            },
        };
        task.key = crate::jobs::tasks::task_key(&task)?;
        let summary = runtime.service.summary();
        let mut spec = RunSpec {
            version: 1,
            run_id: run_id.clone(),
            vault_id: self.vault_id.clone(),
            title: "Explicit accounted provider probe".into(),
            created_at_utc_ms: runtime.created_at_utc_ms,
            deadline_utc_ms: runtime.deadline_utc_ms,
            scope: RunScope {
                research: None,
                operation: "doctor_probe".into(),
                question: None,
                exclusions: vec![],
                source_snapshot: None,
                input_records: vec![],
                read_preconditions: vec![],
                profile_fingerprints: BTreeMap::from([(
                    summary.profile_id.clone(),
                    summary.profile_fingerprint,
                )]),
                scope_payload_hash: None,
            },
            config_fingerprint: summary.config_fingerprint,
            input_fingerprint: Blake3Hash::digest([]),
            limits: runtime.limits.clone(),
            tasks: vec![task.clone()],
            prior_accounting: self.prior_accounting_for_new_run()?,
        };
        spec.input_fingerprint = crate::jobs::tasks::input_fingerprint(&spec)?;
        let ledger = JobLedger::new(
            self.fs.clone(),
            self.vault_id.clone(),
            run_id.clone(),
            runtime.job_options.clone(),
        )?;
        let writer = WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(self.options.lock_timeout_ms),
        )?;
        self.fs.ensure_directory(
            &VaultRelativePath::new(format!("runs/{run_id}/inputs"))?,
            &writer,
        )?;
        let staged = self.fs.stage(&path, &descriptor, &writer)?;
        self.fs.replace(staged, &ExpectedState::Absent, &writer)?;
        ledger.create(&writer, spec)?;
        drop(writer);
        ledger.start()?;
        let result = (|| {
            let outcome = match runtime.dispatcher.execute(
                &ledger,
                &runtime.service,
                &task.key,
                DispatchPurpose::Probe { role },
            ) {
                Ok(outcome) => outcome,
                Err(mut failure) => {
                    if let Some(attempt) = &failure.attempt {
                        let actual = ledger
                            .inspect()?
                            .attempts
                            .into_iter()
                            .find(|a| &a.attempt == attempt);
                        if actual.as_ref().is_some_and(|a| a.receipt.is_none()) {
                            if let Some(plan) = failure.materialization.take() {
                                crate::research::acquire::settle_receipt(&self.fs, &ledger, plan)?;
                            }
                        } else if actual
                            .as_ref()
                            .is_some_and(|a| a.phase == AttemptPhase::OutputCommitted)
                        {
                            ledger.settle(attempt)?;
                        }
                    }
                    let mut error = failure.error;
                    error.details = serde_json::json!({"run_id": run_id, "cause": error.details});
                    return Err(error);
                }
            };
            if !matches!(outcome.output, ValidatedOutput::Probe { role: actual } if actual == role)
            {
                return Err(WikiError::new(
                    ErrorCode::ProviderResponse,
                    "probe response role differs",
                ));
            }
            let receipt = crate::jobs::checkpoint::receipt_plan(
                &ledger,
                &outcome.attempt,
                OutputDisposition::Validated,
                vec![],
                vec![],
                vec![],
            )?;
            crate::research::acquire::settle_receipt(&self.fs, &ledger, receipt)?;
            ledger.finish_remote_task(&task.key, vec![], vec![], |_| Ok(false))?;
            ledger.complete_run()?;
            Ok(ProbeOutcome {
                run_id,
                role,
                validated: true,
                network_used: runtime.dispatcher.network_used(),
                inspection: ledger.inspect()?,
            })
        })();
        result.map_err(|mut error| {
            error.network_used |= runtime.dispatcher.network_used();
            error
        })
    }
}

fn probe_output_limit(runtime: &RemoteRuntime) -> Result<u64> {
    let mut limit = 256u64.min(runtime.service.service().max_output_tokens.unwrap_or(4096));
    for class in [BillableClass::Output, BillableClass::Reasoning] {
        if let Some(ceiling) = runtime.limits.billable_units.get(&class) {
            limit = limit.min(*ceiling);
        }
    }
    if limit == 0 {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "generation probe output allowance is zero",
        ));
    }
    Ok(limit)
}

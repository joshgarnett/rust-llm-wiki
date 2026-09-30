//! Explicit, accounted probes use the same sealed wire and paid-response ledger.
use super::{OfflineApp, remote::RemoteRuntime};
use crate::{
    domain::*,
    graph::{
        extraction_types::EXTRACTION_SCHEMA,
        packet::{self, canonical_json},
    },
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_contract: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct ProviderJobAmendment {
    pub run_id: RecordId,
    pub applied: bool,
    pub requested_limits: LifetimeLimits,
    pub requested_deadline_utc_ms: i64,
    pub inspection: LedgerInspection,
}
fn preview_amendment(
    previous: &LedgerInspection,
    requested: &LifetimeLimits,
    deadline: i64,
) -> Result<()> {
    let old = &previous.effective_limits;
    if deadline < previous.effective_deadline_utc_ms
        || requested.requests < old.requests
        || requested.concurrency < old.concurrency
        || requested.attempts_per_task < old.attempts_per_task
    {
        return Err(WikiError::invalid(
            "amendment must preserve or raise lifetime limits and deadline",
        ));
    }
    for (prior, next) in [
        (old.request_bytes, requested.request_bytes),
        (old.response_bytes, requested.response_bytes),
        (
            old.requests_per_minute.map(u64::from),
            requested.requests_per_minute.map(u64::from),
        ),
        (old.tokens_per_minute, requested.tokens_per_minute),
    ] {
        if prior.is_none() && next.is_some()
            || prior.zip(next).is_some_and(|(prior, next)| next < prior)
        {
            return Err(WikiError::invalid(
                "amendment cannot add or lower historical ceilings",
            ));
        }
    }
    if requested
        .billable_units
        .keys()
        .any(|key| !old.billable_units.contains_key(key))
        || old.billable_units.iter().any(|(key, prior)| {
            requested
                .billable_units
                .get(key)
                .is_some_and(|next| next < prior)
        })
    {
        return Err(WikiError::invalid(
            "amendment cannot add or lower billable class ceilings",
        ));
    }
    if old.max_cost.is_none() && requested.max_cost.is_some()
        || old
            .max_cost
            .as_ref()
            .zip(requested.max_cost.as_ref())
            .is_some_and(|(prior, next)| {
                prior.currency() != next.currency() || next.nanounits() < prior.nanounits()
            })
    {
        return Err(WikiError::invalid(
            "amendment cannot add or lower cost ceiling",
        ));
    }
    Ok(())
}
impl OfflineApp {
    /// Preview or append a monotonic lifetime amendment to a retained provider job.
    /// The ledger owns the final guarded validation and cumulative accounting.
    pub fn amend_provider_job(
        &self,
        run_id: &RecordId,
        limits: LifetimeLimits,
        deadline_ms: u64,
        reason: &str,
    ) -> Result<ProviderJobAmendment> {
        if deadline_ms == 0 || deadline_ms > 86_400_000 || reason.is_empty() || reason.len() > 256 {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "invalid provider job amendment",
            ));
        }
        crate::jobs::budgets::validate_limits(&limits)?;
        let options = super::remote::native_job_options(
            ExecutionPolicy {
                offline: self.options.offline,
                dry_run: self.options.dry_run,
                retry_uncertain: false,
            },
            self.options.lock_timeout_ms,
        );
        let deadline = options
            .clock
            .read()?
            .utc_ms
            .checked_add(deadline_ms as i64)
            .ok_or_else(|| WikiError::invalid("provider job deadline overflows"))?;
        self.amend_provider_job_at(run_id, limits, deadline, reason)
    }
    pub fn amend_provider_job_overrides(
        &self,
        run_id: &RecordId,
        requested: &super::remote::RequestedJobLimits,
        reason: &str,
    ) -> Result<ProviderJobAmendment> {
        let options = super::remote::native_job_options(
            ExecutionPolicy {
                offline: self.options.offline,
                dry_run: self.options.dry_run,
                retry_uncertain: false,
            },
            self.options.lock_timeout_ms,
        );
        let now = options.clock.read()?.utc_ms;
        let ledger = JobLedger::new(
            self.fs.clone(),
            self.vault_id.clone(),
            run_id.clone(),
            options,
        )?;
        let inspection = ledger.inspect()?;
        let limits = requested.merge_into(&inspection.effective_limits)?;
        let deadline = if let Some(ms) = requested.deadline_ms {
            if ms == 0 || ms > 86_400_000 {
                return Err(WikiError::new(
                    ErrorCode::Usage,
                    "deadline-ms must be 1..=86400000",
                ));
            }
            now.checked_add(ms as i64)
                .ok_or_else(|| WikiError::invalid("provider job deadline overflows"))?
        } else {
            inspection.effective_deadline_utc_ms
        };
        self.amend_provider_job_at(run_id, limits, deadline, reason)
    }
    fn amend_provider_job_at(
        &self,
        run_id: &RecordId,
        limits: LifetimeLimits,
        deadline: i64,
        reason: &str,
    ) -> Result<ProviderJobAmendment> {
        if reason.trim().is_empty() || reason.len() > 256 {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "job amendment requires a reason of 1–256 bytes",
            ));
        }
        crate::jobs::budgets::validate_limits(&limits)?;
        let options = super::remote::native_job_options(
            ExecutionPolicy {
                offline: self.options.offline,
                dry_run: self.options.dry_run,
                retry_uncertain: false,
            },
            self.options.lock_timeout_ms,
        );
        let ledger = JobLedger::new(
            self.fs.clone(),
            self.vault_id.clone(),
            run_id.clone(),
            options,
        )?;
        let inspection = ledger.inspect()?;
        if !matches!(
            inspection.state,
            RunState::Planned | RunState::Paused | RunState::Stopped
        ) {
            return Err(WikiError::invalid(
                "only retained planned/paused/stopped jobs may be amended",
            ));
        }
        preview_amendment(&inspection, &limits, deadline)?;
        if !self.options.dry_run {
            ledger.amend_retained_limits(limits.clone(), deadline, reason.to_owned())?;
        }
        Ok(ProviderJobAmendment {
            run_id: run_id.clone(),
            applied: !self.options.dry_run,
            requested_limits: limits,
            requested_deadline_utc_ms: deadline,
            inspection: if self.options.dry_run {
                inspection
            } else {
                ledger.inspect()?
            },
        })
    }
    pub fn probe_provider(
        &self,
        runtime: &RemoteRuntime,
        role: ServiceRole,
    ) -> Result<ProbeOutcome> {
        self.probe_provider_contract(runtime, role, false)
    }
    /// Explicit paid probe using the actual extraction schema and prompt family.
    /// It validates request/schema compatibility, not semantic source grounding.
    pub fn probe_extraction_schema(&self, runtime: &RemoteRuntime) -> Result<ProbeOutcome> {
        self.probe_provider_contract(runtime, ServiceRole::Generate, true)
    }
    fn probe_provider_contract(
        &self,
        runtime: &RemoteRuntime,
        role: ServiceRole,
        extraction_schema: bool,
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
            ServiceRole::Generate if extraction_schema => RemoteOperation::Generate {
                instructions: packet::INSTRUCTIONS.into(),
                data: serde_json::json!({
                    "schema": "lwiki.extraction-probe.v1",
                    "window": {"id":"w1","span":{"start":0,"end":16},"text":"Ada works at Acme"},
                    "response_instruction": "Return a valid lwiki.extraction.v1 JSON object with the stated packet identity and empty mentions, assertions, and unresolved arrays. Do not infer facts from this synthetic fixture.",
                    "packet_id": "packet_probe",
                    "packet_fingerprint": Blake3Hash::digest(b"lwiki.extraction-probe.v1"),
                }).to_string(),
                output_schema: packet::schema()?,
                max_output_tokens: probe_output_limit(runtime)?,
            },
            ServiceRole::Generate => RemoteOperation::Generate {
                instructions: "Return a JSON object with ok set to true.".into(),
                data: "Explicit provider probe.".into(),
                output_schema: serde_json::json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
                max_output_tokens: probe_output_limit(runtime)?,
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
                operation: if extraction_schema {
                    "doctor_extraction_schema_probe"
                } else {
                    "doctor_probe"
                }
                .into(),
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
                                crate::jobs::settle_receipt(&self.fs, &ledger, plan)?;
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
            crate::jobs::settle_receipt(&self.fs, &ledger, receipt)?;
            ledger.finish_remote_task(&task.key, vec![], vec![], |_| Ok(false))?;
            ledger.complete_run()?;
            Ok(ProbeOutcome {
                run_id,
                role,
                validated: true,
                network_used: runtime.dispatcher.network_used(),
                inspection: ledger.inspect()?,
                output_contract: extraction_schema.then(|| EXTRACTION_SCHEMA.into()),
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

//! Explicit research commands; dry planning never opens provider configuration.
use crate::{
    app::{OfflineApp, remote},
    domain::*,
    jobs::*,
    research::{self, *},
};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

#[derive(Debug, Args)]
pub struct ResearchArguments {
    #[command(subcommand)]
    pub command: ResearchCommand,
}
#[derive(Debug, Subcommand)]
pub enum ResearchCommand {
    /// Preview a research scope without provider calls or a persisted run.
    Plan(ResearchPlanArguments),
    /// Run bounded research; --offline returns a preview without starting a run.
    Run(ResearchPlanArguments),
    /// Resume retained research with its existing lifetime limits.
    Resume(ResearchResumeArguments),
    /// Inspect a retained research run without provider credentials.
    Status {
        /// Retained research run ID; use the ID returned by the original run.
        run_id: RecordId,
    },
    /// Read the latest retained research report and its gaps.
    Report {
        /// Retained research run ID; use the ID returned by the original run.
        run_id: RecordId,
    },
}
impl ResearchCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Plan(_) => "research plan",
            Self::Run(_) => "research run",
            Self::Resume(_) => "research resume",
            Self::Status { .. } => "research status",
            Self::Report { .. } => "research report",
        }
    }
}
#[derive(Debug, Args)]
pub struct ResearchPlanArguments {
    /// Research question to investigate.
    pub question: String,
    /// Explicit public acquisition URL; repeat as needed.
    #[arg(long = "url")]
    pub urls: Vec<String>,
    /// Research scope exclusion; repeat as needed.
    #[arg(long = "exclude")]
    pub exclusions: Vec<String>,
    /// Trusted search-provider profile for optional source discovery.
    #[arg(long)]
    pub search_profile: Option<String>,
    /// Maximum research rounds across the run.
    #[arg(long, default_value_t = 3)]
    pub max_rounds: u32,
    /// Maximum acquired research sources.
    #[arg(long, default_value_t = 15)]
    pub max_sources: u32,
    /// Maximum generated output tokens per research stage.
    #[arg(long, default_value_t = 4096)]
    pub stage_output_tokens: u64,
    /// Apply the run's generated page proposals after their guarded preparation.
    #[arg(long)]
    pub apply: bool,
    /// Explicit new run ID; omit to allocate one automatically.
    #[arg(long)]
    pub run_id: Option<RecordId>,
    #[command(flatten)]
    pub remote: super::remote::RemoteArguments,
}
#[derive(Debug, Args)]
pub struct ResearchResumeArguments {
    /// Retained research run ID; use the ID returned by the original run.
    pub run_id: RecordId,
    /// Absolute path to trusted private provider TOML outside the wiki.
    #[arg(long)]
    pub providers_config: Option<std::path::PathBuf>,
    /// Opt into retrying uncertain work under retained accounting; prior attempts may be billed.
    #[arg(long)]
    pub retry_uncertain: bool,
    /// JSON with complete lifetime limits, an absolute UTC deadline, and reason.
    /// Omit this flag to preserve the run's effective limits and deadline.
    #[arg(long)]
    pub amend_limits: Option<std::path::PathBuf>,
}
pub struct ResearchCliOutcome {
    pub data: Value,
    pub network_used: bool,
    pub partial: bool,
    pub warnings: Vec<String>,
}
fn encoded(value: impl serde::Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| WikiError::invalid("research result encoding"))
}
fn completed_outcome(outcome: ResearchOutcome) -> Result<ResearchCliOutcome> {
    let data = encoded(&outcome)?;
    if let Some(code) = outcome.stop_code {
        let mut error = WikiError::new(
            code,
            outcome
                .report
                .as_ref()
                .map_or("Research stopped", |report| report.stop_reason.as_str()),
        );
        error.network_used = outcome.network_used;
        error.details = json!({"research_outcome": data});
        return Err(error);
    }
    Ok(ResearchCliOutcome {
        data,
        network_used: outcome.network_used,
        partial: outcome.report.as_ref().is_some_and(|report| report.partial),
        warnings: outcome.report.map_or(vec![], |report| report.warnings),
    })
}
fn local_options(app: &OfflineApp, retry_uncertain: bool) -> JobOptions {
    remote::native_job_options(
        ExecutionPolicy {
            offline: app.options().offline,
            dry_run: app.options().dry_run,
            retry_uncertain,
        },
        app.options().lock_timeout_ms,
    )
}
fn stored_scope(app: &OfflineApp, inspection: &LedgerInspection) -> Result<ResearchScope> {
    let genesis = inspection
        .spec
        .scope
        .research
        .as_ref()
        .ok_or_else(|| WikiError::invalid("run is not a research run"))?;
    let bytes =
        crate::changes::prepare::read_bounded(app.fs(), &genesis.scope.path, 256 * 1024)?
            .ok_or_else(|| WikiError::new(ErrorCode::RecoveryRequired, "research scope missing"))?;
    let scope: ResearchScope = crate::changes::prepare::strict_json(&bytes)?;
    research::plan::validate_scope(&scope)?;
    if bytes.len() as u64 != genesis.scope.byte_len
        || Blake3Hash::digest(&bytes) != genesis.scope.hash
        || crate::graph::packet::canonical_json(&scope)? != bytes
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "research scope changed",
        ));
    }
    Ok(scope)
}
fn search_service(
    app: &OfflineApp,
    scope: &ResearchScope,
    remote_args: &super::remote::RemoteArguments,
) -> Result<Option<crate::config::providers::TrustedService>> {
    scope
        .search_profile
        .as_deref()
        .map(|profile| {
            let path = remote_args
                .providers_config
                .clone()
                .map(Ok)
                .unwrap_or_else(remote::default_provider_path)?;
            crate::config::providers::ProviderConfig::load(&path)?.authorize(
                app.fs(),
                app.vault_id(),
                profile,
                Capability::Search,
            )
        })
        .transpose()
}
pub fn execute(
    command: &ResearchCommand,
    app: &OfflineApp,
    profile: Option<&str>,
) -> Result<ResearchCliOutcome> {
    let local = |data, partial| ResearchCliOutcome {
        data,
        network_used: false,
        partial,
        warnings: vec![],
    };
    match command {
        ResearchCommand::Status { run_id } => {
            let status = research::runner::status(
                app.fs(),
                app.vault_id(),
                run_id,
                local_options(app, false),
            )?;
            Ok(local(encoded(status)?, false))
        }
        ResearchCommand::Report { run_id } => {
            let ledger = JobLedger::new(
                app.fs().clone(),
                app.vault_id().clone(),
                run_id.clone(),
                local_options(app, false),
            )?;
            let inspection = ledger.inspect()?;
            let report = research::report::latest(app.fs(), &inspection)?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::RecordNotFound,
                    "research report is not published yet",
                )
            })?;
            let partial = report.partial;
            Ok(local(encoded(report)?, partial))
        }
        ResearchCommand::Plan(arguments) | ResearchCommand::Run(arguments) => {
            let limits = ResearchLimits {
                rounds: arguments.max_rounds,
                sources: arguments.max_sources,
                stage_output_tokens: arguments.stage_output_tokens,
                ..ResearchLimits::default()
            };
            let scope = ResearchScope {
                version: 1,
                question: arguments.question.clone(),
                exclusions: arguments.exclusions.clone(),
                explicit_urls: arguments.urls.clone(),
                generation_profile: profile.unwrap_or("unselected").into(),
                search_profile: arguments.search_profile.clone(),
                limits,
                apply: arguments.apply,
            };
            research::plan::validate_scope(&scope)?;
            let lifetime = arguments.remote.limits()?;
            let current = research::inspection::inspect(app.fs(), app.vault_id(), &scope)?;
            if app.options().dry_run || matches!(command, ResearchCommand::Plan(_)) {
                return Ok(local(
                    json!({"version":1,"scope":scope,"inspection":current,
                    "lifetime_limits":lifetime,"deadline_ms":arguments.remote.deadline_ms,
                    "stages":["inspect_existing","plan_frontier","discover","capture","extract","assess_gaps","synthesize","stage_changes"],
                    "remote_work":"unknown","persisted":false}),
                    false,
                ));
            }
            if app.options().offline {
                let mut outcome = local(
                    json!({"status":"offline_preview","scope":scope,"inspection":current,"offline":true,
                    "remote_work":"unavailable","persisted":false}),
                    true,
                );
                outcome
                    .warnings
                    .push("Offline preview only: no research run was started or persisted.".into());
                return Ok(outcome);
            }
            if app.options().stage_only {
                return Err(WikiError::new(
                    ErrorCode::Usage,
                    "research prepares proposals by default; use research plan for a preview",
                ));
            }
            let native = arguments
                .remote
                .runtime(app, profile, Capability::Generate)?;
            let search = search_service(app, &scope, &arguments.remote)?;
            let mut services = vec![ServiceBindingV1 {
                profile_id: native.service.summary().profile_id,
                capability: Capability::Generate,
                profile_fingerprint: native.service.summary().profile_fingerprint,
                endpoint_fingerprint: native.service.summary().endpoint_fingerprint,
            }];
            if let Some(service) = &search {
                let summary = service.summary();
                services.push(ServiceBindingV1 {
                    profile_id: summary.profile_id,
                    capability: Capability::Search,
                    profile_fingerprint: summary.profile_fingerprint,
                    endpoint_fingerprint: summary.endpoint_fingerprint,
                });
            }
            services
                .sort_by(|a, b| (a.capability, &a.profile_id).cmp(&(b.capability, &b.profile_id)));
            let binding = BindingEpochV1 {
                version: 1,
                number: 0,
                config_fingerprint: native.service.summary().config_fingerprint,
                source_snapshot: Some(current.snapshot),
                input_records: current.records,
                read_preconditions: current.dependencies,
                services,
            };
            let run_id = match &arguments.run_id {
                Some(id) => id.clone(),
                None => RecordId::new(format!("run_research_{}", uuid::Uuid::now_v7()))?,
            };
            let mut planned = research::plan::plan(
                scope,
                app.vault_id().clone(),
                run_id,
                binding,
                &native.service,
                &current.passages,
                lifetime,
                native.created_at_utc_ms,
                native.deadline_utc_ms,
            )?;
            planned.spec.prior_accounting = app.prior_accounting_for_new_run()?;
            planned.spec.input_fingerprint = crate::jobs::tasks::input_fingerprint(&planned.spec)?;
            let public = crate::providers::public_fetch::PublicFetchOptions::default();
            let runtime = ResearchRuntime {
                generation: &native.service,
                search: search.as_ref(),
                dispatcher: &native.dispatcher,
                public_fetch: &public,
                job_options: native.job_options.clone(),
            };
            let outcome = (|| {
                let ledger =
                    research::runner::create(app.fs(), &planned, native.job_options.clone())?;
                research::runner::run(app, &ledger, &runtime)
            })()
            .map_err(|mut e| {
                e.network_used |= native.dispatcher.network_used();
                e
            })?;
            completed_outcome(outcome)
        }
        ResearchCommand::Resume(arguments) => {
            let remote_args = super::remote::RemoteArguments {
                providers_config: arguments.providers_config.clone(),
                retry_uncertain: arguments.retry_uncertain,
                ..Default::default()
            };
            let amendment = arguments
                .amend_limits
                .as_ref()
                .map(|path| {
                    use std::io::Read;
                    let file = std::fs::File::open(path).map_err(|_| {
                        WikiError::new(ErrorCode::Usage, "cannot read research amendment file")
                    })?;
                    let mut bytes = Vec::new();
                    file.take(65537).read_to_end(&mut bytes).map_err(|_| {
                        WikiError::new(ErrorCode::Usage, "cannot read research amendment file")
                    })?;
                    if bytes.len() > 65536 {
                        return Err(WikiError::new(
                            ErrorCode::Usage,
                            "research amendment exceeds 64 KiB",
                        ));
                    }
                    let amendment: ResearchResumeAmendment =
                        crate::changes::prepare::strict_json(&bytes)?;
                    crate::jobs::budgets::validate_limits(&amendment.limits)?;
                    if amendment.reason.is_empty() || amendment.reason.len() > 256 {
                        return Err(WikiError::new(
                            ErrorCode::Usage,
                            "research amendment reason must contain 1–256 bytes",
                        ));
                    }
                    Ok(amendment)
                })
                .transpose()?;
            let ledger = JobLedger::new(
                app.fs().clone(),
                app.vault_id().clone(),
                arguments.run_id.clone(),
                local_options(app, arguments.retry_uncertain),
            )?;
            let inspection = ledger.inspect()?;
            let scope = stored_scope(app, &inspection)?;
            if app.options().dry_run {
                return Ok(local(
                    json!({"run_id":arguments.run_id,"inspection":inspection,
                    "scope":scope,"amendment":amendment,"remote_work":"unknown","persisted":false}),
                    false,
                ));
            }
            if profile.is_some_and(|p| p != scope.generation_profile) {
                return Err(WikiError::new(
                    ErrorCode::Usage,
                    "resume keeps the stored generation profile name; edit its trusted model configuration to rebind",
                ));
            }
            if app.options().offline {
                let report = research::report::latest(app.fs(), &inspection)?;
                return Ok(local(
                    json!({"inspection":inspection,"report":report,"amendment":amendment,"offline":true}),
                    true,
                ));
            }
            let native =
                remote_args.runtime(app, Some(&scope.generation_profile), Capability::Generate)?;
            let search = search_service(app, &scope, &remote_args)?;
            let public = crate::providers::public_fetch::PublicFetchOptions::default();
            let runtime = ResearchRuntime {
                generation: &native.service,
                search: search.as_ref(),
                dispatcher: &native.dispatcher,
                public_fetch: &public,
                job_options: native.job_options.clone(),
            };
            let outcome =
                research::runner::resume_with_amendment(app, &ledger, &runtime, amendment)
                    .map_err(|mut e| {
                        e.network_used |= native.dispatcher.network_used();
                        e
                    })?;
            completed_outcome(outcome)
        }
    }
}

//! Shared explicit provider and lifetime-budget options for remote commands.
use crate::{
    app::remote::{self, RemoteRuntime},
    domain::*,
    jobs::*,
};
use clap::Args;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, Args)]
pub struct RemoteArguments {
    /// Private TOML configuration; never discover provider files inside a vault.
    #[arg(long)]
    pub providers_config: Option<PathBuf>,
    /// Lifetime request-attempt ceiling, including retries.
    #[arg(long)]
    pub max_requests: Option<u64>,
    /// Maximum simultaneous provider requests.
    #[arg(long)]
    pub concurrency: Option<u32>,
    /// Maximum attempts for an individual provider task.
    #[arg(long)]
    pub attempts_per_task: Option<u32>,
    /// Overall remote operation deadline in milliseconds from startup.
    #[arg(long)]
    pub deadline_ms: Option<u64>,
    /// Lifetime ceiling on outgoing request bytes.
    #[arg(long)]
    pub max_request_bytes: Option<u64>,
    /// Lifetime ceiling on incoming response bytes.
    #[arg(long)]
    pub max_response_bytes: Option<u64>,
    /// Lifetime ceiling for each input and cached-input billable class.
    #[arg(long)]
    pub max_input_units: Option<u64>,
    /// Lifetime ceiling for each output and reasoning billable class.
    #[arg(long)]
    pub max_output_units: Option<u64>,
    /// Checked decimal ceiling; requires a complete provable provider bound.
    #[arg(long)]
    pub max_cost: Option<String>,
    /// Currency code used with --max-cost.
    #[arg(long, default_value = "USD")]
    pub currency: String,
    /// Maximum provider requests dispatched per minute.
    #[arg(long)]
    pub requests_per_minute: Option<u32>,
    /// Maximum accounted provider tokens per minute.
    #[arg(long)]
    pub tokens_per_minute: Option<u64>,
    /// Opt into retrying uncertain work under retained accounting; prior attempts may be billed.
    #[arg(long)]
    pub retry_uncertain: bool,
    /// Retain a bounded private HTTP error-body diagnostic for explicit inspection.
    #[arg(long)]
    pub retain_http_error_body: bool,
}
impl Default for RemoteArguments {
    fn default() -> Self {
        Self {
            providers_config: None,
            max_requests: None,
            concurrency: None,
            attempts_per_task: None,
            deadline_ms: None,
            max_request_bytes: None,
            max_response_bytes: None,
            max_input_units: None,
            max_output_units: None,
            max_cost: None,
            currency: "USD".into(),
            requests_per_minute: None,
            tokens_per_minute: None,
            retry_uncertain: false,
            retain_http_error_body: false,
        }
    }
}
impl RemoteArguments {
    pub fn limits(&self) -> Result<LifetimeLimits> {
        if self.max_requests == Some(0)
            || self.concurrency == Some(0)
            || self.attempts_per_task == Some(0)
            || self.deadline_ms == Some(0)
            || self.deadline_ms.is_some_and(|value| value > 86_400_000)
            || self.max_request_bytes == Some(0)
            || self.max_response_bytes == Some(0)
            || self.requests_per_minute == Some(0)
            || self.tokens_per_minute == Some(0)
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "invalid remote lifetime limits",
            ));
        }
        let mut billable_units = BTreeMap::new();
        if let Some(n) = self.max_input_units {
            billable_units.insert(BillableClass::Input, n);
            billable_units.insert(BillableClass::CachedInput, n);
        }
        if let Some(n) = self.max_output_units {
            billable_units.insert(BillableClass::Output, n);
            billable_units.insert(BillableClass::Reasoning, n);
        }
        Ok(LifetimeLimits {
            requests: self.max_requests.unwrap_or(60),
            concurrency: self.concurrency.unwrap_or(2),
            attempts_per_task: self.attempts_per_task.unwrap_or(3),
            request_bytes: self.max_request_bytes,
            response_bytes: self.max_response_bytes,
            billable_units,
            max_cost: self
                .max_cost
                .as_ref()
                .map(|s| Money::parse_decimal(Currency::new(self.currency.clone())?, s))
                .transpose()?,
            requests_per_minute: self.requests_per_minute,
            tokens_per_minute: self.tokens_per_minute,
        })
    }
    pub fn requested_limits(&self) -> Result<remote::RequestedJobLimits> {
        let specified = [
            ("requests", self.max_requests.is_some()),
            ("concurrency", self.concurrency.is_some()),
            ("attempts_per_task", self.attempts_per_task.is_some()),
            ("request_bytes", self.max_request_bytes.is_some()),
            ("response_bytes", self.max_response_bytes.is_some()),
            ("input_units", self.max_input_units.is_some()),
            ("output_units", self.max_output_units.is_some()),
            ("max_cost", self.max_cost.is_some()),
            ("requests_per_minute", self.requests_per_minute.is_some()),
            ("tokens_per_minute", self.tokens_per_minute.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, yes)| yes.then_some(name))
        .collect();
        Ok(remote::RequestedJobLimits {
            limits: self.limits()?,
            specified,
            deadline_ms: self.deadline_ms,
        })
    }
    pub fn runtime(
        &self,
        app: &crate::app::OfflineApp,
        profile: Option<&str>,
        capability: Capability,
    ) -> Result<RemoteRuntime> {
        let profile = profile.ok_or_else(|| {
            WikiError::new(
                ErrorCode::ConfigInvalid,
                "select a trusted provider --profile",
            )
        })?;
        let policy = ExecutionPolicy {
            offline: app.options().offline,
            dry_run: app.options().dry_run,
            retry_uncertain: self.retry_uncertain,
        };
        let mut runtime = remote::native_runtime(
            app.fs(),
            app.vault_id(),
            remote::NativeRuntimeRequest {
                config_path: self.providers_config.as_deref(),
                profile,
                capability,
                options: remote::native_job_options(policy, app.options().lock_timeout_ms),
                limits: self.limits()?,
                deadline_ms: self.deadline_ms.unwrap_or(900_000),
            },
        )?;
        runtime.requested_limits = Some(self.requested_limits()?);
        runtime.dispatcher = runtime
            .dispatcher
            .with_http_error_diagnostics(self.retain_http_error_body);
        Ok(runtime)
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ProbeRole {
    Embed,
    Generate,
}
impl ProbeRole {
    pub fn service_role(self) -> crate::providers::types::ServiceRole {
        match self {
            Self::Embed => crate::providers::types::ServiceRole::Embed,
            Self::Generate => crate::providers::types::ServiceRole::Generate,
        }
    }
}

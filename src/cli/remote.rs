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
    #[arg(long, default_value_t = 60)]
    pub max_requests: u64,
    /// Maximum simultaneous provider requests.
    #[arg(long, default_value_t = 2)]
    pub concurrency: u32,
    /// Maximum attempts for an individual provider task.
    #[arg(long, default_value_t = 3)]
    pub attempts_per_task: u32,
    /// Overall remote operation deadline in milliseconds from startup.
    #[arg(long, default_value_t = 900000)]
    pub deadline_ms: u64,
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
}
impl Default for RemoteArguments {
    fn default() -> Self {
        Self {
            providers_config: None,
            max_requests: 60,
            concurrency: 2,
            attempts_per_task: 3,
            deadline_ms: 900000,
            max_request_bytes: None,
            max_response_bytes: None,
            max_input_units: None,
            max_output_units: None,
            max_cost: None,
            currency: "USD".into(),
            requests_per_minute: None,
            tokens_per_minute: None,
            retry_uncertain: false,
        }
    }
}
impl RemoteArguments {
    pub fn limits(&self) -> Result<LifetimeLimits> {
        if self.max_requests == 0
            || self.concurrency == 0
            || self.attempts_per_task == 0
            || self.deadline_ms == 0
            || self.deadline_ms > 86_400_000
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
            requests: self.max_requests,
            concurrency: self.concurrency,
            attempts_per_task: self.attempts_per_task,
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
        remote::native_runtime(
            app.fs(),
            app.vault_id(),
            remote::NativeRuntimeRequest {
                config_path: self.providers_config.as_deref(),
                profile,
                capability,
                options: remote::native_job_options(policy, app.options().lock_timeout_ms),
                limits: self.limits()?,
                deadline_ms: self.deadline_ms,
            },
        )
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

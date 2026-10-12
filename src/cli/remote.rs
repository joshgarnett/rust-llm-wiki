//! Shared explicit provider and lifetime-budget options for remote commands.
use crate::{
    app::remote::{self, RemoteRuntime},
    domain::*,
    jobs::*,
};
use clap::Args;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, Default, Args)]
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
    #[arg(long, requires = "max_cost")]
    pub currency: Option<String>,
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
/// One invocation's resolved profile and user defaults. Portable settings cannot
/// provide a configuration path or authorize a service.
pub struct RemoteContext<'a> {
    pub profile: Option<&'a str>,
    pub wiki_id: Option<&'a RecordId>,
    pub document: &'a crate::config::LocalConfigDocument,
    pub allow_ambient_provider: bool,
}
impl RemoteArguments {
    pub fn currency_name(&self) -> &str {
        self.currency.as_deref().unwrap_or("USD")
    }
    pub fn effective(&self, context: &RemoteContext<'_>) -> Self {
        let mut effective = self.clone();
        let defaults = &context.document.remote;
        effective.max_requests = self.max_requests.or(defaults.max_requests);
        effective.concurrency = self.concurrency.or(defaults.concurrency);
        effective.attempts_per_task = self.attempts_per_task.or(defaults.attempts_per_task);
        effective.deadline_ms = self.deadline_ms.or(defaults.deadline_ms);
        effective.max_request_bytes = self.max_request_bytes.or(defaults.max_request_bytes);
        effective.max_response_bytes = self.max_response_bytes.or(defaults.max_response_bytes);
        effective.max_input_units = self.max_input_units.or(defaults.max_input_units);
        effective.max_output_units = self.max_output_units.or(defaults.max_output_units);
        effective.max_cost = self.max_cost.clone().or_else(|| defaults.max_cost.clone());
        effective.currency = self.currency.clone().or_else(|| defaults.currency.clone());
        effective.requests_per_minute = self.requests_per_minute.or(defaults.requests_per_minute);
        effective.tokens_per_minute = self.tokens_per_minute.or(defaults.tokens_per_minute);
        effective.providers_config = self.providers_config.clone().or_else(|| {
            context
                .document
                .saved_provider_config(context.wiki_id)
                .map(|(path, _)| path.to_path_buf())
        });
        effective
    }
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
                .map(|s| Money::parse_decimal(Currency::new(self.currency_name())?, s))
                .transpose()?,
            requests_per_minute: self.requests_per_minute,
            tokens_per_minute: self.tokens_per_minute,
        })
    }
    pub fn requested_limits_with_currency(
        &self,
        currency: Option<&str>,
    ) -> Result<remote::RequestedJobLimits> {
        let mut requested = self.requested_limits()?;
        if let Some(amount) = &self.max_cost {
            let currency = self
                .currency
                .as_deref()
                .or(currency)
                .unwrap_or(self.currency_name());
            requested.limits.max_cost =
                Some(Money::parse_decimal(Currency::new(currency)?, amount)?);
        }
        Ok(requested)
    }
    pub fn requested_limits(&self) -> Result<remote::RequestedJobLimits> {
        if self.currency.is_some() && self.max_cost.is_none() {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "--currency requires --max-cost",
            ));
        }
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
        context: &RemoteContext<'_>,
        capability: Capability,
    ) -> Result<RemoteRuntime> {
        let effective = self.effective(context);
        let profile = context.profile.ok_or_else(|| {
            WikiError::new(
                ErrorCode::ConfigInvalid,
                "select a trusted provider --profile",
            )
        })?;
        if !context.allow_ambient_provider && effective.providers_config.is_none() {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "--no-user-config requires an explicit --providers-config for remote work",
            ));
        }
        let policy = ExecutionPolicy {
            offline: app.options().offline,
            dry_run: app.options().dry_run,
            retry_uncertain: self.retry_uncertain,
        };
        let mut runtime = remote::native_runtime(
            app.fs(),
            app.vault_id(),
            remote::NativeRuntimeRequest {
                config_path: effective.providers_config.as_deref(),
                profile,
                capability,
                options: remote::native_job_options(policy, app.options().lock_timeout_ms),
                limits: effective.limits()?,
                deadline_ms: effective.deadline_ms.unwrap_or(DEFAULT_DEADLINE_MS),
            },
        )?;
        runtime.requested_limits =
            Some(self.requested_limits_with_currency(effective.currency.as_deref())?);
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

#[cfg(test)]
mod configuration_tests {
    use super::*;

    #[test]
    fn saved_limits_apply_to_new_jobs_without_amending_retained_work() {
        let mut document = crate::config::LocalConfigDocument::default();
        document.remote.max_requests = Some(7);
        document.remote.deadline_ms = Some(1234);
        document.remote.max_input_units = Some(50);
        let context = RemoteContext {
            profile: Some("saved"),
            wiki_id: None,
            document: &document,
            allow_ambient_provider: true,
        };
        let cli = RemoteArguments::default();
        let effective = cli.effective(&context);
        assert_eq!(effective.limits().unwrap().requests, 7);
        assert_eq!(effective.deadline_ms, Some(1234));
        let requested = cli.requested_limits().unwrap();
        assert!(requested.specified.is_empty());
        assert_eq!(requested.deadline_ms, None);
        let retained = RemoteArguments {
            max_requests: Some(3),
            ..Default::default()
        }
        .limits()
        .unwrap();
        assert_eq!(requested.merge_into(&retained).unwrap(), retained);
        let explicit = RemoteArguments {
            max_requests: Some(9),
            deadline_ms: Some(5000),
            ..Default::default()
        };
        assert_eq!(explicit.effective(&context).limits().unwrap().requests, 9);
        let requested = explicit.requested_limits().unwrap();
        assert_eq!(
            requested.specified,
            std::collections::BTreeSet::from(["requests"])
        );
        assert_eq!(requested.deadline_ms, Some(5000));
        assert_eq!(requested.merge_into(&retained).unwrap().requests, 9);
    }

    #[test]
    fn configured_currency_matches_new_and_retained_cost_requests() {
        let mut document = crate::config::LocalConfigDocument::default();
        document.remote.currency = Some("EUR".into());
        let context = RemoteContext {
            profile: None,
            wiki_id: None,
            document: &document,
            allow_ambient_provider: true,
        };
        let cli = RemoteArguments {
            max_cost: Some("1.00".into()),
            ..Default::default()
        };
        let effective = cli.effective(&context);
        let requested = cli
            .requested_limits_with_currency(effective.currency.as_deref())
            .unwrap();
        assert_eq!(
            requested.limits.max_cost,
            effective.limits().unwrap().max_cost
        );
        assert_eq!(
            requested.specified,
            std::collections::BTreeSet::from(["max_cost"])
        );
        let amended = cli.requested_limits_with_currency(Some("CAD")).unwrap();
        assert_eq!(amended.limits.max_cost.unwrap().currency().as_str(), "CAD");
        let explicit = RemoteArguments {
            max_cost: Some("1.00".into()),
            currency: Some("USD".into()),
            ..Default::default()
        };
        assert_eq!(
            explicit
                .requested_limits_with_currency(Some("CAD"))
                .unwrap()
                .limits
                .max_cost
                .unwrap()
                .currency()
                .as_str(),
            "USD"
        );
        assert!(
            RemoteArguments {
                currency: Some("EUR".into()),
                ..Default::default()
            }
            .requested_limits()
            .is_err()
        );
    }

    #[test]
    fn explicit_provider_and_currency_override_saved_values() {
        let mut document = crate::config::LocalConfigDocument {
            providers_config: Some(PathBuf::from("/saved/providers.toml")),
            ..Default::default()
        };
        document.remote.currency = Some("EUR".into());
        document.remote.max_cost = Some("1.00".into());
        let context = RemoteContext {
            profile: None,
            wiki_id: None,
            document: &document,
            allow_ambient_provider: true,
        };
        let cli = RemoteArguments::default();
        let effective = cli.effective(&context);
        assert_eq!(effective.providers_config, document.providers_config);
        assert_eq!(effective.currency.as_deref(), Some("EUR"));
        assert!(cli.requested_limits().unwrap().specified.is_empty());
        let cli = RemoteArguments {
            providers_config: Some(PathBuf::from("/chosen/providers.toml")),
            currency: Some("USD".into()),
            max_cost: Some("2.00".into()),
            ..Default::default()
        };
        let effective = cli.effective(&context);
        assert_eq!(effective.providers_config, cli.providers_config);
        assert_eq!(
            effective.limits().unwrap().max_cost,
            cli.limits().unwrap().max_cost
        );
    }
}

#[cfg(test)]
mod provider_scope_tests {
    use super::*;

    #[test]
    fn provider_resolution_uses_cli_then_authenticated_wiki_then_global() {
        let wiki_id = RecordId::new("Vault.B").unwrap();
        let mut document = crate::config::LocalConfigDocument {
            providers_config: Some(PathBuf::from("/global/providers.toml")),
            ..Default::default()
        };
        document.vaults.insert(
            wiki_id.clone(),
            crate::config::LocalPreferences {
                providers_config: Some(PathBuf::from("/wiki-b/providers.toml")),
                ..Default::default()
            },
        );
        let selected = RemoteContext {
            profile: None,
            wiki_id: Some(&wiki_id),
            document: &document,
            allow_ambient_provider: false,
        };
        assert_eq!(
            RemoteArguments::default()
                .effective(&selected)
                .providers_config,
            Some(PathBuf::from("/wiki-b/providers.toml"))
        );
        let explicit = RemoteArguments {
            providers_config: Some(PathBuf::from("/cli/providers.toml")),
            ..Default::default()
        };
        assert_eq!(
            explicit.effective(&selected).providers_config,
            explicit.providers_config
        );
        let unselected = RemoteContext {
            wiki_id: None,
            ..selected
        };
        assert_eq!(
            RemoteArguments::default()
                .effective(&unselected)
                .providers_config,
            document.providers_config
        );
        let unknown = RecordId::new("Vault.Unknown").unwrap();
        let unknown = RemoteContext {
            wiki_id: Some(&unknown),
            ..unselected
        };
        assert_eq!(
            RemoteArguments::default()
                .effective(&unknown)
                .providers_config,
            document.providers_config
        );
    }
}

//! Explicit remote application inputs; ordinary local commands never construct these.
use crate::{
    config::providers::{ProviderConfig, TrustedService},
    domain::*,
    jobs::*,
    providers::{
        credentials::{
            CredentialBroker, CredentialOptions, NativeCredentialClock, NativeHelperRunner,
            NativeSecretInputs,
        },
        dispatcher::Dispatcher,
    },
    vault::VaultFs,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub use crate::providers::credentials::NativeCredentialClock as NativeJobClock;

pub struct RemoteRuntime {
    pub service: TrustedService,
    pub dispatcher: Dispatcher,
    pub job_options: JobOptions,
    pub limits: LifetimeLimits,
    pub created_at_utc_ms: i64,
    pub deadline_utc_ms: i64,
    pub requested_limits: Option<RequestedJobLimits>,
}

/// Only this documented platform configuration path is consulted. Vault content
/// cannot supply a path, endpoint, helper, or credential. No key is read here.
pub fn default_provider_path() -> Result<PathBuf> {
    let missing = || {
        WikiError::new(
            ErrorCode::ConfigInvalid,
            "private provider configuration location unavailable; use --providers-config",
        )
    };
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .ok_or_else(missing)?
        .join("Library/Application Support");
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .ok_or_else(missing)?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = if let Some(path) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(missing());
        }
        path
    } else {
        std::env::var_os("HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .ok_or_else(missing)?
            .join(".config")
    };
    #[cfg(not(any(unix, windows)))]
    return Err(missing());
    #[cfg(any(unix, windows))]
    {
        if !base.is_absolute() {
            return Err(missing());
        }
        Ok(base.join("lwiki/providers.toml"))
    }
}

pub struct NativeRuntimeRequest<'a> {
    pub config_path: Option<&'a Path>,
    pub profile: &'a str,
    pub capability: Capability,
    pub options: JobOptions,
    pub limits: LifetimeLimits,
    pub deadline_ms: u64,
}

pub fn native_runtime(
    fs: &VaultFs,
    vault_id: &RecordId,
    request: NativeRuntimeRequest<'_>,
) -> Result<RemoteRuntime> {
    let NativeRuntimeRequest {
        config_path,
        profile,
        capability,
        options,
        limits,
        deadline_ms,
    } = request;
    if deadline_ms == 0 || deadline_ms > 86_400_000 {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "deadline-ms must be 1..=86400000",
        ));
    }
    let path = config_path
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(default_provider_path)?;
    let service = ProviderConfig::load(&path)?.authorize(fs, vault_id, profile, capability)?;
    let now = options.clock.read()?;
    let deadline = now
        .utc_ms
        .checked_add(
            i64::try_from(deadline_ms)
                .map_err(|_| WikiError::new(ErrorCode::Usage, "deadline overflows"))?,
        )
        .ok_or_else(|| WikiError::new(ErrorCode::BudgetExceeded, "deadline overflows"))?;
    let broker = Arc::new(CredentialBroker::new(CredentialOptions {
        clock: options.clock.clone(),
        inputs: Arc::new(NativeSecretInputs),
        runner: Arc::new(NativeHelperRunner),
    }));
    Ok(RemoteRuntime {
        service,
        dispatcher: Dispatcher::native(fs.clone(), broker),
        job_options: options,
        limits,
        created_at_utc_ms: now.utc_ms,
        deadline_utc_ms: deadline,
        requested_limits: None,
    })
}

pub fn native_job_options(policy: ExecutionPolicy, lock_timeout_ms: u64) -> JobOptions {
    JobOptions {
        clock: Arc::new(NativeCredentialClock::default()),
        fault: None,
        cancel: CancellationToken::native_cli(),
        policy,
        lock_timeout_ms,
    }
}

/// Explicit CLI overrides are checked before retained work can dispatch.
#[derive(Debug, Clone)]
pub struct RequestedJobLimits {
    pub limits: LifetimeLimits,
    pub specified: std::collections::BTreeSet<&'static str>,
    pub deadline_ms: Option<u64>,
}
pub(crate) fn validate_retained_arguments(
    inspection: &LedgerInspection,
    limits: &LifetimeLimits,
    deadline_ms: u64,
    requested: Option<&RequestedJobLimits>,
) -> Result<()> {
    let old = &inspection.effective_limits;
    let mismatch = if let Some(r) = requested {
        r.specified.iter().any(|field| match *field {
            "requests" => r.limits.requests != old.requests,
            "concurrency" => r.limits.concurrency != old.concurrency,
            "attempts_per_task" => r.limits.attempts_per_task != old.attempts_per_task,
            "request_bytes" => r.limits.request_bytes != old.request_bytes,
            "response_bytes" => r.limits.response_bytes != old.response_bytes,
            "input_units" => [BillableClass::Input, BillableClass::CachedInput]
                .iter()
                .any(|k| r.limits.billable_units.get(k) != old.billable_units.get(k)),
            "output_units" => [BillableClass::Output, BillableClass::Reasoning]
                .iter()
                .any(|k| r.limits.billable_units.get(k) != old.billable_units.get(k)),
            "max_cost" => r.limits.max_cost != old.max_cost,
            "requests_per_minute" => r.limits.requests_per_minute != old.requests_per_minute,
            "tokens_per_minute" => r.limits.tokens_per_minute != old.tokens_per_minute,
            _ => true,
        })
    } else {
        limits != old
    };
    let requested_deadline = requested.map_or(Some(deadline_ms), |r| r.deadline_ms);
    let original = inspection
        .spec
        .deadline_utc_ms
        .checked_sub(inspection.spec.created_at_utc_ms)
        .and_then(|n| u64::try_from(n).ok());
    if mismatch || requested_deadline.is_some_and(|n| original != Some(n)) {
        let mut error = WikiError::new(
            ErrorCode::Usage,
            "retained jobs keep cumulative limits and the original deadline; use jobs amend --run ID --reason REASON to change them explicitly",
        );
        error.details = serde_json::json!({"reason":"retained_limits_require_amendment","run_id":inspection.spec.run_id,"effective_limits":old,"effective_deadline_utc_ms":inspection.effective_deadline_utc_ms,"next_action":format!("lwiki jobs amend --run {} --reason REASON",inspection.spec.run_id)});
        return Err(error);
    }
    Ok(())
}

impl RequestedJobLimits {
    pub fn merge_into(&self, retained: &LifetimeLimits) -> Result<LifetimeLimits> {
        let mut result = retained.clone();
        for field in &self.specified {
            match *field {
                "requests" => result.requests = self.limits.requests,
                "concurrency" => result.concurrency = self.limits.concurrency,
                "attempts_per_task" => result.attempts_per_task = self.limits.attempts_per_task,
                "request_bytes" => result.request_bytes = self.limits.request_bytes,
                "response_bytes" => result.response_bytes = self.limits.response_bytes,
                "max_cost" => result.max_cost = self.limits.max_cost.clone(),
                "requests_per_minute" => {
                    result.requests_per_minute = self.limits.requests_per_minute
                }
                "tokens_per_minute" => result.tokens_per_minute = self.limits.tokens_per_minute,
                "input_units" | "output_units" => {
                    let classes = if *field == "input_units" {
                        [BillableClass::Input, BillableClass::CachedInput]
                    } else {
                        [BillableClass::Output, BillableClass::Reasoning]
                    };
                    for class in classes {
                        if let Some(value) = self.limits.billable_units.get(&class) {
                            result.billable_units.insert(class, *value);
                        }
                    }
                }
                _ => {
                    return Err(WikiError::new(
                        ErrorCode::Usage,
                        "unknown job amendment limit",
                    ));
                }
            }
        }
        Ok(result)
    }
}

/// Useful committed outputs can coexist with a still unknown earlier attempt.
/// Keep its original billing/concurrency reservation and pause the job for inspection.
pub(crate) fn finish_provider_job(ledger: &JobLedger) -> Result<Option<String>> {
    let inspection = ledger.inspect()?;
    if inspection
        .tasks
        .values()
        .any(|task| !matches!(task.state, TaskState::Completed | TaskState::Failed))
    {
        return Ok(None);
    }
    if inspection
        .attempts
        .iter()
        .any(|attempt| attempt.phase != AttemptPhase::Settled)
    {
        if inspection.state == RunState::Running {
            ledger.pause(StopReason::OutcomeUnknown)?;
        }
        return Ok(Some(format!(
            "Job {} has finished tasks but retains an unsettled earlier attempt and its billing/concurrency holds. Inspect with jobs status --run {}.",
            inspection.spec.run_id, inspection.spec.run_id
        )));
    }
    let failed = inspection
        .tasks
        .values()
        .any(|task| task.state == TaskState::Failed);
    if matches!(inspection.state, RunState::Paused | RunState::Stopped) {
        ledger.resume(None)?;
    }
    if !matches!(inspection.state, RunState::Completed | RunState::Failed) {
        ledger.complete_run()?;
    }
    Ok(failed.then(|| format!(
        "Job {} finished with rejected embedding work; unchanged tasks kept their original Run and accounting. Inspect with jobs status --run {}.",
        inspection.spec.run_id, inspection.spec.run_id
    )))
}

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
    })
}

pub fn native_job_options(policy: ExecutionPolicy, lock_timeout_ms: u64) -> JobOptions {
    JobOptions {
        clock: Arc::new(NativeCredentialClock::default()),
        fault: None,
        cancel: CancellationToken::default(),
        policy,
        lock_timeout_ms,
    }
}

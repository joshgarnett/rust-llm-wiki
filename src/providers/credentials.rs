//! Bounded secret resolution. Only the private dispatcher receives credentials.
use crate::{
    config::providers::{
        self, AuthConfig, HelperOutputMode, SecretReference, StaticSource, TrustedService,
    },
    domain::*,
    jobs::{CancellationToken, ClockReading, ExecutionPolicy, JobClock},
    vault::VaultFs,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn auth_error(message: &'static str) -> WikiError {
    WikiError::new(ErrorCode::ProviderAuth, message)
}
fn policy(
    context: &CredentialContext,
    clock: &dyn JobClock,
    before: Option<ClockReading>,
) -> Result<ClockReading> {
    if context.policy.dry_run || context.policy.offline {
        return Err(WikiError::new(
            ErrorCode::OfflineUnavailable,
            "offline/dry-run policy prohibits credential resolution",
        ));
    }
    if context.cancel.is_cancelled() {
        return Err(WikiError::new(
            ErrorCode::Cancelled,
            "credential resolution cancelled",
        ));
    }
    let now = clock
        .read()
        .map_err(|_| auth_error("credential clock unavailable"))?;
    if now.utc_ms < 0 || now.utc_ms > 253_402_300_799_999 || now.utc_ms >= context.deadline_utc_ms {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "credential resolution deadline expired",
        ));
    }
    if before.is_some_and(|b| now.utc_ms < b.utc_ms || now.monotonic_ms < b.monotonic_ms) {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "credential clock regressed",
        ));
    }
    Ok(now)
}
/// No Debug/serialization or public content getter. Construction is bounded data,
/// never an authenticated-dispatch grant.
pub struct SecretBytes(Vec<u8>);
impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() > providers::MAX_SECRET_BYTES {
            return Err(auth_error("credential input exceeds byte ceiling"));
        }
        Ok(Self(bytes))
    }
    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.fill(0);
        std::hint::black_box(&mut self.0);
    }
}
pub trait SecretInputs: Send + Sync {
    fn environment(&self, name: &str, max_bytes: usize) -> Result<Option<SecretBytes>>;
    fn file(&self, absolute: &Path, max_bytes: usize) -> Result<SecretBytes>;
}
pub struct NativeSecretInputs;
impl SecretInputs for NativeSecretInputs {
    fn environment(&self, name: &str, max_bytes: usize) -> Result<Option<SecretBytes>> {
        #[cfg(test)]
        crate::catalog::query_diagnostics::access("credential_input");
        let Some(value) = std::env::var_os(name) else {
            return Ok(None);
        };
        let value = value
            .into_string()
            .map_err(|_| auth_error("credential encoding invalid"))?;
        if value.len() > max_bytes.min(providers::MAX_SECRET_BYTES) {
            return Err(auth_error("credential input exceeds byte ceiling"));
        }
        SecretBytes::new(value.into_bytes()).map(Some)
    }
    fn file(&self, path: &Path, max_bytes: usize) -> Result<SecretBytes> {
        #[cfg(test)]
        crate::catalog::query_diagnostics::access("credential_input");
        let mut bytes =
            providers::checked_file(path, max_bytes.min(providers::MAX_SECRET_BYTES), true)
                .map_err(|_| auth_error("credential file unavailable or invalid"))?;
        strip_newline(&mut bytes);
        SecretBytes::new(bytes)
    }
}
pub struct HelperInvocation {
    argv: Vec<String>,
    cwd: PathBuf,
}
impl HelperInvocation {
    pub fn new(argv: Vec<String>, cwd: PathBuf) -> Result<Self> {
        if argv.is_empty()
            || argv.len() > providers::MAX_HELPER_ARGS
            || argv.iter().any(|a| {
                a.is_empty()
                    || a.len() > providers::MAX_HELPER_ARG_BYTES
                    || a.chars().any(char::is_control)
            })
            || argv.iter().map(String::len).sum::<usize>() > providers::MAX_HELPER_ARG_TOTAL
            || !Path::new(&argv[0]).is_absolute()
            || !cwd.is_absolute()
            || cwd.to_str().is_none()
            || argv[0].to_ascii_lowercase().ends_with(".bat")
            || argv[0].to_ascii_lowercase().ends_with(".cmd")
        {
            return Err(auth_error("invalid bounded helper invocation"));
        }
        Ok(Self { argv, cwd })
    }
    pub fn argv(&self) -> &[String] {
        &self.argv
    }
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}
pub struct HelperLimits {
    pub timeout_ms: u64,
    pub max_stdout_bytes: usize,
    pub deadline_utc_ms: i64,
}
pub struct HelperOutput {
    stdout: SecretBytes,
}
impl HelperOutput {
    pub fn new(stdout: Vec<u8>) -> Result<Self> {
        Ok(Self {
            stdout: SecretBytes::new(stdout)?,
        })
    }
    #[cfg(test)]
    pub(super) fn stdout(&self) -> &[u8] {
        self.stdout.as_bytes()
    }
}
pub trait HelperRunner: Send + Sync {
    fn run(
        &self,
        invocation: &HelperInvocation,
        limits: &HelperLimits,
        clock: &dyn JobClock,
        cancel: &CancellationToken,
    ) -> Result<HelperOutput>;
}
pub struct NativeHelperRunner;
pub struct NativeCredentialClock {
    start: Instant,
}
impl Default for NativeCredentialClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}
impl JobClock for NativeCredentialClock {
    fn read(&self) -> Result<ClockReading> {
        let utc_ms = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| auth_error("credential clock invalid"))?
                .as_millis(),
        )
        .map_err(|_| auth_error("credential clock invalid"))?;
        let monotonic_ms = u64::try_from(self.start.elapsed().as_millis())
            .map_err(|_| auth_error("credential clock invalid"))?;
        Ok(ClockReading {
            utc_ms,
            monotonic_ms,
        })
    }
}
pub struct CredentialOptions {
    pub clock: Arc<dyn JobClock>,
    pub inputs: Arc<dyn SecretInputs>,
    pub runner: Arc<dyn HelperRunner>,
}
pub struct CredentialContext {
    pub policy: ExecutionPolicy,
    pub cancel: CancellationToken,
    pub deadline_utc_ms: i64,
}
struct CacheEntry {
    headers: Arc<Vec<(String, SecretBytes)>>,
    epoch: u64,
    acquired: ClockReading,
    valid_until_utc_ms: i64,
    valid_for_ms: u64,
}
pub(crate) struct CredentialLease {
    headers: Arc<Vec<(String, SecretBytes)>>,
    epoch: u64,
    auth_key: Blake3Hash,
    acquired: ClockReading,
    valid_for_ms: u64,
    valid_until_utc_ms: i64,
    clock: Arc<dyn JobClock>,
}
impl CredentialLease {
    /// Uses the acquisition clock's origin, never a caller's substituted clock.
    pub(super) fn check_validity(&self) -> Result<()> {
        let now = self
            .clock
            .read()
            .map_err(|_| auth_error("credential clock unavailable"))?;
        if now.utc_ms < self.acquired.utc_ms || now.monotonic_ms < self.acquired.monotonic_ms {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "credential clock regressed",
            ));
        }
        if now.utc_ms >= self.valid_until_utc_ms
            || now.monotonic_ms - self.acquired.monotonic_ms >= self.valid_for_ms
        {
            return Err(auth_error("credential lease expired before dispatch"));
        }
        Ok(())
    }
    pub(super) fn headers(&self) -> &[(String, SecretBytes)] {
        &self.headers
    }
    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }
    #[cfg(test)]
    pub(super) fn valid_until_utc_ms(&self) -> i64 {
        self.valid_until_utc_ms
    }
}
pub struct CredentialBroker {
    options: CredentialOptions,
    cache: Mutex<BTreeMap<Blake3Hash, Arc<Mutex<Option<CacheEntry>>>>>,
    epoch: Mutex<u64>,
}
impl CredentialBroker {
    pub fn new(options: CredentialOptions) -> Self {
        Self {
            options,
            cache: Mutex::new(BTreeMap::new()),
            epoch: Mutex::new(0),
        }
    }
    #[cfg(test)]
    pub(super) fn cache_stats(&self) -> (usize, usize) {
        let map = self.cache.lock().unwrap();
        (
            map.len(),
            map.values()
                .filter(|entry| entry.lock().unwrap().is_some())
                .count(),
        )
    }
    pub(crate) fn resolve(
        &self,
        service: &TrustedService,
        fs: &VaultFs,
        context: &CredentialContext,
    ) -> Result<CredentialLease> {
        self.resolve_inner(service, fs, context, None)
    }
    pub(crate) fn refresh_after_401(
        &self,
        service: &TrustedService,
        fs: &VaultFs,
        observed_epoch: u64,
        context: &CredentialContext,
    ) -> Result<CredentialLease> {
        // Policy/trust checks precede even inspecting the cache or auth mode.
        policy(context, self.options.clock.as_ref(), None)?;
        service.recheck(fs)?;
        if !matches!(service.service().auth, AuthConfig::Command { .. }) {
            return Err(auth_error(
                "static authentication cannot refresh after unauthorized response",
            ));
        }
        self.resolve_inner(service, fs, context, Some(observed_epoch))
    }
    /// The dispatcher rechecks this association and both clocks after durable
    /// send authorization, before passing any authenticated headers to transport.
    pub(crate) fn validate_lease(
        &self,
        service: &TrustedService,
        fs: &VaultFs,
        lease: &CredentialLease,
        context: &CredentialContext,
    ) -> Result<()> {
        let first = policy(context, self.options.clock.as_ref(), Some(lease.acquired))?;
        if &lease.auth_key != service.auth_key() {
            return Err(auth_error("credential lease belongs to another service"));
        }
        service.recheck(fs)?;
        let now = policy(context, self.options.clock.as_ref(), Some(first))?;
        if now.utc_ms >= lease.valid_until_utc_ms
            || now.monotonic_ms - lease.acquired.monotonic_ms >= lease.valid_for_ms
        {
            return Err(auth_error("credential lease expired before dispatch"));
        }
        Ok(())
    }
    fn resolve_inner(
        &self,
        service: &TrustedService,
        fs: &VaultFs,
        context: &CredentialContext,
        refresh: Option<u64>,
    ) -> Result<CredentialLease> {
        let first = policy(context, self.options.clock.as_ref(), None)?;
        service.recheck(fs)?;
        match &service.service().auth {
            AuthConfig::Static {
                source,
                header,
                prefix,
            } => {
                let token = self.source(source)?;
                let mut headers = vec![(header.clone(), header_value(prefix, &token)?)];
                self.extra_headers(service, &mut headers)?;
                service.recheck(fs)?;
                let completed = policy(context, self.options.clock.as_ref(), Some(first))?;
                Ok(CredentialLease {
                    clock: self.options.clock.clone(),
                    headers: Arc::new(headers),
                    epoch: 0,
                    auth_key: service.auth_key().clone(),
                    acquired: completed,
                    valid_for_ms: u64::try_from(context.deadline_utc_ms - completed.utc_ms)
                        .map_err(|_| auth_error("credential expiry invalid"))?,
                    valid_until_utc_ms: context.deadline_utc_ms,
                })
            }
            AuthConfig::Command {
                argv,
                output,
                timeout_seconds,
                ttl_seconds,
                refresh_skew_seconds,
                header,
                prefix,
            } => {
                let slot = {
                    let mut map = self
                        .cache
                        .lock()
                        .map_err(|_| auth_error("credential cache unavailable"))?;
                    if map.len() >= 128 && !map.contains_key(service.auth_key()) {
                        return Err(auth_error("credential cache capacity exceeded"));
                    }
                    map.entry(service.auth_key().clone())
                        .or_insert_with(|| Arc::new(Mutex::new(None)))
                        .clone()
                };
                let mut cached = self.wait_slot(&slot, context, first)?;
                let now = policy(context, self.options.clock.as_ref(), Some(first))?;
                service.recheck(fs)?;
                if let Some(entry) = cached.as_ref() {
                    if now.utc_ms < entry.acquired.utc_ms
                        || now.monotonic_ms < entry.acquired.monotonic_ms
                    {
                        return Err(WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "cached credential clock regressed",
                        ));
                    }
                    if now.utc_ms < entry.valid_until_utc_ms
                        && now.monotonic_ms - entry.acquired.monotonic_ms < entry.valid_for_ms
                        && refresh.is_none_or(|epoch| epoch != entry.epoch)
                    {
                        let mut headers = vec![(
                            entry.headers[0].0.clone(),
                            SecretBytes::new(entry.headers[0].1.0.clone())?,
                        )];
                        self.extra_headers(service, &mut headers)?;
                        service.recheck(fs)?;
                        let final_clock = policy(context, self.options.clock.as_ref(), Some(now))?;
                        if final_clock.utc_ms >= entry.valid_until_utc_ms
                            || final_clock.monotonic_ms - entry.acquired.monotonic_ms
                                >= entry.valid_for_ms
                        {
                            return Err(auth_error("cached credential expired during resolution"));
                        }
                        return Ok(CredentialLease {
                            clock: self.options.clock.clone(),
                            headers: Arc::new(headers),
                            epoch: entry.epoch,
                            auth_key: service.auth_key().clone(),
                            acquired: entry.acquired,
                            valid_for_ms: entry.valid_for_ms,
                            valid_until_utc_ms: entry.valid_until_utc_ms,
                        });
                    }
                }
                let invocation = HelperInvocation::new(argv.clone(), service.cwd().to_path_buf())?;
                let remaining = u64::try_from(context.deadline_utc_ms - now.utc_ms)
                    .map_err(|_| auth_error("credential deadline invalid"))?;
                let limits = HelperLimits {
                    timeout_ms: (u64::from(*timeout_seconds) * 1000).min(remaining),
                    max_stdout_bytes: providers::MAX_SECRET_BYTES,
                    deadline_utc_ms: context.deadline_utc_ms,
                };
                let result = self
                    .options
                    .runner
                    .run(
                        &invocation,
                        &limits,
                        self.options.clock.as_ref(),
                        &context.cancel,
                    )
                    .map_err(redact_runner_error)?;
                let completed = policy(context, self.options.clock.as_ref(), Some(now))?;
                service.recheck(fs)?;
                let (token, expiry) = parse_output(result, *output)?;
                let skew = i64::from(*refresh_skew_seconds) * 1000;
                let ttl_end = completed
                    .utc_ms
                    .checked_add(i64::from(*ttl_seconds) * 1000)
                    .ok_or_else(|| auth_error("credential expiry invalid"))?;
                let valid_until = if let Some(expiry) = expiry {
                    let adjusted = expiry
                        .checked_sub(skew)
                        .ok_or_else(|| auth_error("credential expiry invalid"))?;
                    if adjusted <= completed.utc_ms {
                        return Err(auth_error("credential expiry is inside refresh skew"));
                    }
                    adjusted.min(ttl_end)
                } else {
                    ttl_end
                };
                let primary = vec![(header.clone(), header_value(prefix, &token)?)];
                let mut headers = vec![(
                    primary[0].0.clone(),
                    SecretBytes::new(primary[0].1.0.clone())?,
                )];
                self.extra_headers(service, &mut headers)?;
                service.recheck(fs)?;
                let epoch = {
                    let mut epoch = self
                        .epoch
                        .lock()
                        .map_err(|_| auth_error("credential cache unavailable"))?;
                    *epoch = epoch
                        .checked_add(1)
                        .ok_or_else(|| auth_error("credential refresh counter exhausted"))?;
                    *epoch
                };
                let valid_for_ms = u64::try_from(valid_until - completed.utc_ms)
                    .map_err(|_| auth_error("credential expiry invalid"))?;
                let final_clock = policy(context, self.options.clock.as_ref(), Some(completed))?;
                if final_clock.utc_ms >= valid_until
                    || final_clock.monotonic_ms - completed.monotonic_ms >= valid_for_ms
                {
                    return Err(auth_error("credential expired during resolution"));
                }
                let entry = CacheEntry {
                    headers: Arc::new(primary),
                    epoch,
                    acquired: completed,
                    valid_until_utc_ms: valid_until,
                    valid_for_ms,
                };
                let answer = CredentialLease {
                    clock: self.options.clock.clone(),
                    headers: Arc::new(headers),
                    epoch,
                    auth_key: service.auth_key().clone(),
                    acquired: completed,
                    valid_for_ms,
                    valid_until_utc_ms: valid_until,
                };
                *cached = Some(entry);
                Ok(answer)
            }
        }
    }
    fn wait_slot<'a>(
        &self,
        slot: &'a Mutex<Option<CacheEntry>>,
        context: &CredentialContext,
        first: ClockReading,
    ) -> Result<MutexGuard<'a, Option<CacheEntry>>> {
        let wall = Instant::now();
        loop {
            policy(context, self.options.clock.as_ref(), Some(first))?;
            match slot.try_lock() {
                Ok(g) => return Ok(g),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(auth_error("credential cache unavailable"));
                }
                Err(TryLockError::WouldBlock) => {
                    if wall.elapsed() >= Duration::from_secs(10) {
                        return Err(auth_error("credential refresh wait exceeded deadline"));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
    fn source(&self, source: &StaticSource) -> Result<SecretBytes> {
        let value = match source {
            StaticSource::Literal(bytes) => SecretBytes::new(bytes.clone())?,
            StaticSource::Env(name) => self
                .options
                .inputs
                .environment(name, providers::MAX_SECRET_BYTES)
                .map_err(|_| auth_error("credential environment source unavailable"))?
                .ok_or_else(|| auth_error("credential environment source missing"))?,
            StaticSource::File(path) => self
                .options
                .inputs
                .file(path, providers::MAX_SECRET_BYTES)
                .map_err(|_| auth_error("credential file unavailable or invalid"))?,
        };
        validate_token(&value)?;
        Ok(value)
    }
    fn extra_headers(
        &self,
        service: &TrustedService,
        out: &mut Vec<(String, SecretBytes)>,
    ) -> Result<()> {
        for (name, reference) in &service.service().secret_headers {
            let source = match reference {
                SecretReference::Env { name } => StaticSource::Env(name.clone()),
                SecretReference::File { path } => StaticSource::File(path.clone()),
            };
            let token = self.source(&source)?;
            out.push((name.clone(), token));
        }
        Ok(())
    }
}
fn redact_runner_error(error: WikiError) -> WikiError {
    match error.code {
        ErrorCode::Cancelled => WikiError::new(ErrorCode::Cancelled, "credential helper cancelled"),
        ErrorCode::BudgetExceeded => WikiError::new(
            ErrorCode::BudgetExceeded,
            "credential helper deadline/clock limit",
        ),
        _ => auth_error("credential helper failed"),
    }
}
fn validate_token(token: &SecretBytes) -> Result<()> {
    let text =
        std::str::from_utf8(&token.0).map_err(|_| auth_error("credential encoding invalid"))?;
    if text.trim().is_empty()
        || text.chars().any(char::is_control)
        || text.len() > providers::MAX_SECRET_BYTES
    {
        return Err(auth_error("credential token empty, invalid, or oversized"));
    }
    Ok(())
}
fn header_value(prefix: &str, token: &SecretBytes) -> Result<SecretBytes> {
    validate_token(token)?;
    let mut value = Vec::with_capacity(prefix.len() + token.0.len());
    value.extend_from_slice(prefix.as_bytes());
    value.extend_from_slice(&token.0);
    if value.len() > providers::MAX_SECRET_BYTES {
        return Err(auth_error("credential header exceeds byte ceiling"));
    }
    SecretBytes::new(value)
}
fn strip_newline(bytes: &mut Vec<u8>) {
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.pop();
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenOutput {
    key: String,
    expires_at: Option<String>,
}
fn parse_output(
    output: HelperOutput,
    mode: HelperOutputMode,
) -> Result<(SecretBytes, Option<i64>)> {
    match mode {
        HelperOutputMode::Text => {
            let mut bytes = output.stdout.0.clone();
            strip_newline(&mut bytes);
            let token = SecretBytes::new(bytes)?;
            validate_token(&token)?;
            Ok((token, None))
        }
        HelperOutputMode::Json => {
            let decoded: TokenOutput =
                crate::changes::prepare::strict_json(output.stdout.as_bytes())
                    .map_err(|_| auth_error("invalid credential helper JSON"))?;
            let expiry = decoded
                .expires_at
                .map(|s| {
                    time::OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
                        .map_err(|_| auth_error("invalid credential expiry"))
                        .and_then(|t| {
                            i64::try_from(t.unix_timestamp_nanos() / 1_000_000)
                                .map_err(|_| auth_error("invalid credential expiry"))
                        })
                })
                .transpose()?;
            let token = SecretBytes::new(decoded.key.into_bytes())?;
            validate_token(&token)?;
            Ok((token, expiry))
        }
    }
}
struct HelperTimer {
    first: ClockReading,
    wall: Instant,
    timeout_ms: u64,
    deadline: i64,
}
impl HelperTimer {
    fn new(limits: &HelperLimits, clock: &dyn JobClock) -> Result<Self> {
        if limits.timeout_ms == 0
            || limits.timeout_ms > 10000
            || limits.max_stdout_bytes == 0
            || limits.max_stdout_bytes > providers::MAX_SECRET_BYTES
        {
            return Err(auth_error("invalid helper limits"));
        }
        Ok(Self {
            first: clock
                .read()
                .map_err(|_| auth_error("credential clock unavailable"))?,
            wall: Instant::now(),
            timeout_ms: limits.timeout_ms,
            deadline: limits.deadline_utc_ms,
        })
    }
    fn check(&self, clock: &dyn JobClock, cancel: &CancellationToken) -> Result<()> {
        if cancel.is_cancelled() {
            return Err(WikiError::new(
                ErrorCode::Cancelled,
                "credential helper cancelled",
            ));
        }
        let now = clock
            .read()
            .map_err(|_| auth_error("credential clock unavailable"))?;
        if now.utc_ms < 0
            || now.utc_ms > 253_402_300_799_999
            || now.utc_ms < self.first.utc_ms
            || now.monotonic_ms < self.first.monotonic_ms
            || now.utc_ms >= self.deadline
            || now.monotonic_ms - self.first.monotonic_ms >= self.timeout_ms
            || self.wall.elapsed() >= Duration::from_millis(self.timeout_ms)
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "credential helper deadline/clock limit",
            ));
        }
        Ok(())
    }
}
impl HelperRunner for NativeHelperRunner {
    fn run(
        &self,
        invocation: &HelperInvocation,
        limits: &HelperLimits,
        clock: &dyn JobClock,
        cancel: &CancellationToken,
    ) -> Result<HelperOutput> {
        #[cfg(test)]
        crate::catalog::query_diagnostics::access("credential_helper");
        let timer = HelperTimer::new(limits, clock)?;
        timer.check(clock, cancel)?;
        native_run(invocation, limits, clock, cancel, &timer)
    }
}

#[cfg(unix)]
fn native_run(
    invocation: &HelperInvocation,
    limits: &HelperLimits,
    clock: &dyn JobClock,
    cancel: &CancellationToken,
    timer: &HelperTimer,
) -> Result<HelperOutput> {
    use std::{
        os::{fd::AsRawFd, unix::process::CommandExt},
        process::{Command, Stdio},
    };
    struct ChildGroup {
        child: std::process::Child,
        armed: bool,
    }
    impl ChildGroup {
        fn exited(&self) -> Result<bool> {
            // WNOWAIT observes exit without releasing the PID before group cleanup.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc != 0 {
                return Err(auth_error("credential helper wait failed"));
            }
            Ok(unsafe { info.si_pid() } != 0)
        }
        fn cleanup(&mut self) {
            if !self.armed {
                return;
            }
            // Group was created for this child; no shell or cleanup subprocess.
            unsafe {
                libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL);
            }
            let _ = self.child.kill();
            let until = Instant::now() + Duration::from_millis(500);
            while Instant::now() < until {
                match self.child.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        self.armed = false;
                        return;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
            self.armed = false;
        }
    }
    impl Drop for ChildGroup {
        fn drop(&mut self) {
            self.cleanup();
        }
    }
    let child = Command::new(&invocation.argv[0])
        .args(&invocation.argv[1..])
        .current_dir(&invocation.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| auth_error("credential helper launch failed"))?;
    let mut group = ChildGroup { child, armed: true };
    let mut stdout = group
        .child
        .stdout
        .take()
        .ok_or_else(|| auth_error("credential helper output unavailable"))?;
    let fd = stdout.as_raw_fd();
    // Owned live pipe descriptor; preserving flags avoids blocking on descendants.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(auth_error("credential helper output polling failed"));
    }
    let mut output = Vec::with_capacity(limits.max_stdout_bytes.min(4096));
    let mut eof = false;
    loop {
        timer.check(clock, cancel)?;
        if !eof {
            let mut chunk = [0u8; 1024];
            match stdout.read(&mut chunk) {
                Ok(0) => eof = true,
                Ok(n) => {
                    if output.len() + n > limits.max_stdout_bytes {
                        return Err(auth_error("credential helper output exceeds byte ceiling"));
                    }
                    output.extend_from_slice(&chunk[..n]);
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(auth_error("credential helper output read failed")),
            }
        }
        if group.exited()? && eof {
            // Kill remaining descendants while the unreaped parent still owns PID.
            unsafe {
                libc::killpg(group.child.id() as libc::pid_t, libc::SIGKILL);
            }
            let status = group
                .child
                .wait()
                .map_err(|_| auth_error("credential helper wait failed"))?;
            // Prevent Drop from signaling a subsequently reused process-group ID.
            group.armed = false;
            if !status.success() {
                return Err(auth_error("credential helper exited unsuccessfully"));
            }
            timer.check(clock, cancel)?;
            return HelperOutput::new(output);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(not(any(unix, windows)))]
fn native_run(
    _: &HelperInvocation,
    _: &HelperLimits,
    _: &dyn JobClock,
    _: &CancellationToken,
    _: &HelperTimer,
) -> Result<HelperOutput> {
    Err(WikiError::new(
        ErrorCode::CapabilityUnavailable,
        "native credential helpers unavailable on this platform",
    ))
}

#[cfg(windows)]
fn native_run(
    invocation: &HelperInvocation,
    limits: &HelperLimits,
    clock: &dyn JobClock,
    cancel: &CancellationToken,
    timer: &HelperTimer,
) -> Result<HelperOutput> {
    use std::{
        fs::{File, OpenOptions},
        os::windows::io::{AsRawHandle, FromRawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::*,
        Security::SECURITY_ATTRIBUTES,
        System::{JobObjects::*, Pipes::*, Threading::*},
    };
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }
    struct Attributes {
        _storage: Vec<usize>,
        list: LPPROC_THREAD_ATTRIBUTE_LIST,
    }
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe {
                DeleteProcThreadAttributeList(self.list);
            }
        }
    }
    struct RunningJob {
        job: Handle,
        process: Handle,
        thread: Handle,
    }
    impl Drop for RunningJob {
        fn drop(&mut self) {
            // The suspended process is assigned before it can create descendants.
            // Cleanup also covers failures before assignment/resume; never infinite-wait.
            unsafe {
                TerminateJobObject(self.job.0, 1);
                TerminateProcess(self.process.0, 1);
                WaitForSingleObject(self.process.0, 500);
            }
        }
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    // Standard Windows CRT argv quoting, including trailing/quote-adjacent slashes.
    fn quote(s: &str) -> String {
        let mut out = String::from("\"");
        let mut slashes = 0;
        for c in s.chars() {
            if c == '\\' {
                slashes += 1;
            } else {
                if c == '"' {
                    out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                } else {
                    out.extend(std::iter::repeat_n('\\', slashes));
                }
                out.push(c);
                slashes = 0;
            }
        }
        out.extend(std::iter::repeat_n('\\', slashes * 2));
        out.push('"');
        out
    }
    let mut security: SECURITY_ATTRIBUTES = unsafe { std::mem::zeroed() };
    security.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
    security.bInheritHandle = 1;
    let mut read = ptr::null_mut();
    let mut write = ptr::null_mut();
    // Anonymous pipe is owned here. Restrict inheritance using an explicit list.
    if unsafe { CreatePipe(&mut read, &mut write, &security, 0) } == 0 {
        return Err(auth_error("credential helper output unavailable"));
    }
    let mut read = Handle(read);
    let write = Handle(write);
    if unsafe { SetHandleInformation(read.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(auth_error("credential helper output unavailable"));
    }
    let null = OpenOptions::new()
        .read(true)
        .write(true)
        .open("NUL")
        .map_err(|_| auth_error("credential helper null stream unavailable"))?;
    let mut inherited_null = ptr::null_mut();
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            null.as_raw_handle(),
            GetCurrentProcess(),
            &mut inherited_null,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(auth_error("credential helper stream isolation failed"));
    }
    let inherited_null = Handle(inherited_null);
    let mut size = 0usize;
    unsafe {
        InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
    }
    if size == 0 || size > 65_536 {
        return Err(auth_error("credential helper attribute setup failed"));
    }
    let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
    let list = storage.as_mut_ptr().cast();
    if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
        return Err(auth_error("credential helper attribute setup failed"));
    }
    let attributes = Attributes {
        _storage: storage,
        list,
    };
    let handles = [write.0, inherited_null.0];
    if unsafe {
        UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast(),
            std::mem::size_of_val(&handles),
            ptr::null_mut(),
            ptr::null(),
        )
    } == 0
    {
        return Err(auth_error("credential helper handle isolation failed"));
    }
    let job = Handle(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) });
    if job.0.is_null() {
        return Err(auth_error("credential helper job unavailable"));
    }
    let mut job_limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    job_limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&job_limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&job_limits) as u32,
        )
    } == 0
    {
        return Err(auth_error("credential helper job setup failed"));
    }
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = inherited_null.0;
    startup.StartupInfo.hStdError = inherited_null.0;
    startup.StartupInfo.hStdOutput = write.0;
    startup.lpAttributeList = attributes.list;
    let application = wide(&invocation.argv[0]);
    let cwd = wide(
        invocation
            .cwd
            .to_str()
            .ok_or_else(|| auth_error("credential helper directory invalid"))?,
    );
    let mut command = wide(
        &invocation
            .argv
            .iter()
            .map(|s| quote(s))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let flags = CREATE_SUSPENDED
        | CREATE_UNICODE_ENVIRONMENT
        | CREATE_NO_WINDOW
        | EXTENDED_STARTUPINFO_PRESENT;
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            flags,
            ptr::null(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut process,
        )
    } == 0
    {
        return Err(auth_error("credential helper launch failed"));
    }
    let running = RunningJob {
        job,
        process: Handle(process.hProcess),
        thread: Handle(process.hThread),
    };
    if unsafe { AssignProcessToJobObject(running.job.0, running.process.0) } == 0 {
        return Err(auth_error("credential helper job assignment failed"));
    }
    timer.check(clock, cancel)?;
    if unsafe { ResumeThread(running.thread.0) } == u32::MAX {
        return Err(auth_error("credential helper resume failed"));
    }
    drop(write);
    drop(inherited_null);
    drop(attributes);
    // Ownership transfers exactly once; PeekNamedPipe prevents blocking reads.
    let mut pipe = unsafe { File::from_raw_handle(read.0) };
    read.0 = ptr::null_mut();
    let mut output = Vec::with_capacity(limits.max_stdout_bytes.min(4096));
    let mut eof = false;
    loop {
        timer.check(clock, cancel)?;
        if !eof {
            let mut available = 0u32;
            if unsafe {
                PeekNamedPipe(
                    pipe.as_raw_handle(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    &mut available,
                    ptr::null_mut(),
                )
            } == 0
            {
                if unsafe { GetLastError() } == ERROR_BROKEN_PIPE {
                    eof = true;
                } else {
                    return Err(auth_error("credential helper output polling failed"));
                }
            } else if available > 0 {
                let mut chunk = [0u8; 1024];
                let count = (available as usize).min(chunk.len());
                let n = pipe
                    .read(&mut chunk[..count])
                    .map_err(|_| auth_error("credential helper output read failed"))?;
                if n == 0 {
                    eof = true;
                } else {
                    if output.len() + n > limits.max_stdout_bytes {
                        return Err(auth_error("credential helper output exceeds byte ceiling"));
                    }
                    output.extend_from_slice(&chunk[..n]);
                    continue;
                }
            }
        }
        if unsafe { WaitForSingleObject(running.process.0, 0) } == WAIT_OBJECT_0 && eof {
            let mut exit = 0u32;
            if unsafe { GetExitCodeProcess(running.process.0, &mut exit) } == 0 || exit != 0 {
                return Err(auth_error("credential helper exited unsuccessfully"));
            }
            timer.check(clock, cancel)?;
            return HelperOutput::new(output);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

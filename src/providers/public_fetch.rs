//! Public acquisition through the dispatcher ledger, pinned DNS and bounded IO.
use super::types::{
    DispatchDisposition, DispatchFailure, RemoteInput, RemoteOperation, RetryDecision,
};
use crate::{
    changes::prepare::read_bounded,
    domain::*,
    graph::packet::canonical_json,
    jobs::*,
    vault::{ExpectedState, VaultFs},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub const PUBLIC_PROFILE: &str = "public-fetch-v1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchLimits {
    pub compressed_bytes: u64,
    pub expanded_bytes: u64,
    pub timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub redirects: u8,
}
impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            compressed_bytes: 4 * 1024 * 1024,
            expanded_bytes: 8 * 1024 * 1024,
            timeout_ms: 30_000,
            connect_timeout_ms: 10_000,
            redirects: 5,
        }
    }
}
impl FetchLimits {
    pub fn validate(&self) -> Result<()> {
        if self.compressed_bytes == 0
            || self.compressed_bytes > 4 * 1024 * 1024
            || self.expanded_bytes == 0
            || self.expanded_bytes > 8 * 1024 * 1024
            || self.timeout_ms == 0
            || self.timeout_ms > 30_000
            || self.connect_timeout_ms == 0
            || self.connect_timeout_ms > self.timeout_ms
            || self.redirects > 5
        {
            return Err(WikiError::invalid("public fetch limits exceed bounds"));
        }
        Ok(())
    }
}
pub fn settings_fingerprint() -> Blake3Hash {
    Blake3Hash::digest(b"lwiki.public-fetch-v1.GET.identity.pinned.no-auth.no-proxy.v1")
}
pub fn input_fingerprint(input: &RemoteInput) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(canonical_json(input)?))
}

/// Conservative URL key: preserves query order and values, removes fragment only.
pub fn validate_url(value: &str, previous: Option<&str>) -> Result<url::Url> {
    if value.len() > 8192 || value.chars().any(char::is_control) {
        return Err(WikiError::invalid("public URL length or controls invalid"));
    }
    let mut url = url::Url::parse(value).map_err(|_| WikiError::invalid("public URL invalid"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(WikiError::invalid(
            "public URL scheme, host, or credentials invalid",
        ));
    }
    if let Some(previous) = previous
        && url::Url::parse(previous).is_ok_and(|p| p.scheme() == "https")
        && url.scheme() != "https"
    {
        return Err(WikiError::invalid("HTTPS downgrade prohibited"));
    }
    match url
        .host()
        .ok_or_else(|| WikiError::invalid("public hostname missing"))?
    {
        url::Host::Ipv4(ip) if !public_ip(IpAddr::V4(ip)) => {
            return Err(WikiError::invalid("nonpublic address prohibited"));
        }
        url::Host::Ipv6(ip) if !public_ip(IpAddr::V6(ip)) => {
            return Err(WikiError::invalid("nonpublic address prohibited"));
        }
        url::Host::Domain(host)
            if host == "localhost"
                || host.ends_with(".localhost")
                || host == "local"
                || host.ends_with(".local")
                || host.ends_with(".internal") =>
        {
            return Err(WikiError::invalid("local hostname prohibited"));
        }
        _ => {}
    }
    url.set_fragment(None);
    Ok(url)
}
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}
pub type ResolveFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<IpAddr>>> + Send + 'a>>;
pub trait PublicResolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a>;
}
pub struct NativeResolver;
impl PublicResolver for NativeResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a> {
        Box::pin(async move {
            let answers = tokio::net::lookup_host((host, port)).await.map_err(|_| {
                WikiError::new(ErrorCode::ProviderUnavailable, "public DNS unavailable")
            })?;
            let mut result = Vec::new();
            for answer in answers {
                if !result.contains(&answer.ip()) {
                    if result.len() == 16 {
                        return Err(WikiError::invalid("DNS answer count exceeds cap"));
                    }
                    result.push(answer.ip());
                }
            }
            Ok(result)
        })
    }
}
/// Only this production boundary constructs a request; no arbitrary headers/auth.
pub struct PinnedRequest {
    url: String,
    host: String,
    addresses: Vec<SocketAddr>,
    limits: FetchLimits,
    observed: Arc<AtomicU64>,
}
impl PinnedRequest {
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn tls_hostname(&self) -> &str {
        &self.host
    }
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }
    pub fn limits(&self) -> &FetchLimits {
        &self.limits
    }
    /// Connection implementations report bytes as chunks arrive, so cancelling
    /// their future cannot erase observed transfer from the durable receipt.
    pub fn observe_body_bytes(&self, total: u64) {
        self.observed.fetch_max(total, Ordering::SeqCst);
    }
}
pub struct PublicReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub bytes: Vec<u8>,
    pub observed_bytes: u64,
    pub complete: bool,
}
impl PublicReply {
    pub fn new(status: u16, headers: Vec<(String, String)>, bytes: Vec<u8>) -> Self {
        let observed_bytes = bytes.len() as u64;
        Self {
            status,
            headers,
            bytes,
            observed_bytes,
            complete: true,
        }
    }
}
pub type ConnectionFuture<'a> = Pin<Box<dyn Future<Output = Result<PublicReply>> + Send + 'a>>;
pub trait PublicConnector: Send + Sync {
    /// Pure request preparation; all IO must remain in the returned future.
    fn connect<'a>(&'a self, request: PinnedRequest) -> Result<ConnectionFuture<'a>>;
}
pub struct NativeConnector;
impl PublicConnector for NativeConnector {
    fn connect<'a>(&'a self, request: PinnedRequest) -> Result<ConnectionFuture<'a>> {
        // Pure setup precedes the dispatcher's final actual-entry proof.
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| WikiError::invalid("TLS unavailable"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .http1_only()
            .http1_max_headers(64)
            .pool_max_idle_per_host(0)
            .resolve_to_addrs(&request.host, &request.addresses)
            .connect_timeout(Duration::from_millis(request.limits.connect_timeout_ms))
            .timeout(Duration::from_millis(request.limits.timeout_ms))
            .build()
            .map_err(|_| {
                WikiError::new(ErrorCode::ProviderUnavailable, "public client unavailable")
            })?;
        let wire = client
            .get(&request.url)
            .header("Accept-Encoding", "identity")
            .header("User-Agent", "lwiki-public-fetch-v1")
            .build()
            .map_err(|_| WikiError::invalid("public request build invalid"))?;
        if wire.url().as_str() != request.url {
            return Err(WikiError::invalid("public built request URL differs"));
        }
        Ok(Box::pin(async move {
            let mut response = client.execute(wire).await.map_err(|_| {
                WikiError::new(
                    ErrorCode::ProviderUnavailable,
                    "public connection unavailable",
                )
            })?;
            let status = response.status().as_u16();
            let mut headers = Vec::new();
            for (name, value) in response.headers() {
                let value = value
                    .to_str()
                    .map_err(|_| WikiError::invalid("public response header invalid"))?;
                headers.push((name.as_str().to_owned(), value.to_owned()));
                if !super::transport::headers_valid(&headers) {
                    return Err(WikiError::invalid("public response header cap"));
                }
            }
            let mut bytes = Vec::new();
            let mut observed = 0u64;
            loop {
                let chunk = match response.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(_) => {
                        return Ok(PublicReply {
                            status,
                            headers,
                            bytes,
                            observed_bytes: observed,
                            complete: false,
                        });
                    }
                };
                observed = observed
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| WikiError::invalid("public byte count overflow"))?;
                request.observe_body_bytes(observed);
                if observed > request.limits.compressed_bytes {
                    return Ok(PublicReply {
                        status,
                        headers,
                        bytes,
                        observed_bytes: observed,
                        complete: false,
                    });
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(PublicReply {
                status,
                headers,
                bytes,
                observed_bytes: observed,
                complete: true,
            })
        }))
    }
}
pub struct PublicFetchOptions {
    pub resolver: Arc<dyn PublicResolver>,
    pub connector: Arc<dyn PublicConnector>,
}
impl Default for PublicFetchOptions {
    fn default() -> Self {
        Self {
            resolver: Arc::new(NativeResolver),
            connector: Arc::new(NativeConnector),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchCapture {
    pub observed_url: String,
    pub requested_url: String,
    pub fetched_at_utc_ms: i64,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub original_hash: Blake3Hash,
    pub original: Vec<u8>,
    pub limits: FetchLimits,
}
/// Protected response metadata. Only required capture fields survive replay;
/// response cookies, credentials and arbitrary headers are never retained here.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicCaptureMetadata {
    pub version: u32,
    pub observed_url: String,
    pub requested_url: String,
    pub fetched_at_utc_ms: i64,
    pub status: u16,
    pub selected_headers: Vec<(String, String)>,
    pub authentication_challenge: bool,
    pub original_hash: Blake3Hash,
    pub limits: FetchLimits,
}
impl std::fmt::Debug for PublicCaptureMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicCaptureMetadata")
            .field("version", &self.version)
            .field("status", &self.status)
            .field("original_hash", &self.original_hash)
            .finish_non_exhaustive()
    }
}
impl PublicCaptureMetadata {
    pub(crate) fn from_capture(capture: &FetchCapture) -> Self {
        Self {
            version: 1,
            observed_url: capture.observed_url.clone(),
            requested_url: capture.requested_url.clone(),
            fetched_at_utc_ms: capture.fetched_at_utc_ms,
            status: capture.status,
            selected_headers: capture
                .headers
                .iter()
                .filter(|(name, _)| {
                    [
                        "content-type",
                        "content-encoding",
                        "location",
                        "x-robots-tag",
                    ]
                    .iter()
                    .any(|key| name.eq_ignore_ascii_case(key))
                })
                .cloned()
                .collect(),
            authentication_challenge: capture.header("www-authenticate").is_some(),
            original_hash: capture.original_hash.clone(),
            limits: capture.limits.clone(),
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        self.limits.validate()?;
        if self.version != 1
            || !(100..600).contains(&self.status)
            || !(0..=253_402_300_799_999).contains(&self.fetched_at_utc_ms)
            || validate_url(&self.observed_url, None)?.as_str() != self.requested_url
            || !super::transport::headers_valid(&self.selected_headers)
            || self
                .selected_headers
                .iter()
                .map(|(n, v)| n.len() + v.len())
                .sum::<usize>()
                > 16 * 1024
        {
            return Err(WikiError::invalid("public capture metadata invalid"));
        }
        let mut keys = BTreeSet::new();
        for (name, _) in &self.selected_headers {
            let name = name.to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "content-type" | "content-encoding" | "location" | "x-robots-tag"
            ) || !keys.insert(name)
            {
                return Err(WikiError::invalid(
                    "public capture header not unique/allowed",
                ));
            }
        }
        Ok(())
    }
    fn into_capture(self, original: Vec<u8>) -> Result<FetchCapture> {
        self.validate()?;
        if original.len() as u64 > self.limits.compressed_bytes
            || Blake3Hash::digest(&original) != self.original_hash
        {
            return Err(WikiError::invalid("public raw capture hash differs"));
        }
        let mut headers = self.selected_headers;
        if self.authentication_challenge {
            headers.push(("www-authenticate".into(), "challenge-observed".into()));
        }
        Ok(FetchCapture {
            observed_url: self.observed_url,
            requested_url: self.requested_url,
            fetched_at_utc_ms: self.fetched_at_utc_ms,
            status: self.status,
            headers,
            original_hash: self.original_hash,
            original,
            limits: self.limits,
        })
    }
}
impl FetchCapture {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn redirect(&self) -> Result<Option<String>> {
        if !matches!(self.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(None);
        }
        let location = self
            .header("location")
            .ok_or_else(|| WikiError::invalid("redirect Location missing"))?;
        let joined = url::Url::parse(&self.requested_url)
            .map_err(|_| WikiError::invalid("redirect base invalid"))?
            .join(location)
            .map_err(|_| WikiError::invalid("redirect Location invalid"))?;
        Ok(Some(
            validate_url(joined.as_str(), Some(&self.requested_url))?.into(),
        ))
    }
}
pub struct PublicFetchOutcome {
    pub attempt: AttemptRef,
    pub spool: SpoolRef,
    pub materialization: MaterializationPlan,
    pub capture: FetchCapture,
}

/// Reads authenticated retained response data only. It cannot reconstruct send
/// authority, and never resolves DNS or polls a connection.
pub fn retained_capture(ledger: &JobLedger, attempt: &AttemptRef) -> Result<FetchCapture> {
    let inspect = ledger.inspect()?;
    let actual = inspect
        .attempts
        .iter()
        .find(|a| &a.attempt == attempt)
        .ok_or_else(|| {
            WikiError::new(ErrorCode::RecoveryRequired, "retained fetch attempt absent")
        })?;
    if actual.bound.capability != Capability::Fetch || actual.bound.profile_id != PUBLIC_PROFILE {
        return Err(WikiError::invalid("retained attempt is not public fetch"));
    }
    let spool = actual
        .spool
        .as_ref()
        .ok_or_else(|| WikiError::new(ErrorCode::RecoveryRequired, "retained raw fetch absent"))?;
    let (fs, _, _, _) = ledger.dispatcher_bindings();
    let read = |reference: &BoundedPayloadRef, cap: usize| -> Result<Vec<u8>> {
        let bytes = read_bounded(&fs, &reference.path, cap)?.ok_or_else(|| {
            WikiError::new(ErrorCode::RecoveryRequired, "retained fetch payload absent")
        })?;
        if bytes.len() as u64 != reference.byte_len || Blake3Hash::digest(&bytes) != reference.hash
        {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "retained fetch payload changed",
            ));
        }
        Ok(bytes)
    };
    let metadata: ResponseMetadata =
        serde_json::from_slice(&read(&spool.metadata, METADATA_MAX_BYTES)?)
            .map_err(|_| WikiError::invalid("retained fetch metadata invalid"))?;
    if !metadata.terminal_response {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "retained fetch response incomplete",
        ));
    }
    let capture = metadata.acquisition.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecoveryRequired,
            "retained public provenance missing",
        )
    })?;
    let task = inspect
        .tasks
        .get(&attempt.task_key)
        .ok_or_else(|| WikiError::invalid("retained fetch task absent"))?;
    let bytes = read(&task.spec.input, 256 * 1024)?;
    let input: RemoteInput = serde_json::from_slice(&bytes)
        .map_err(|_| WikiError::invalid("retained public input invalid"))?;
    let expected = RemoteInput {
        version: 1,
        operation: RemoteOperation::Fetch {
            url: capture.observed_url.clone(),
            limits: capture.limits.clone(),
        },
    };
    if input != expected
        || canonical_json(&input)? != bytes
        || input_fingerprint(&input)? != actual.bound.input_hash
        || metadata.status_code != Some(capture.status)
        || spool.response.hash != capture.original_hash
        || actual.bound.wire_hash
            != Blake3Hash::digest(canonical_json(&(
                "lwiki.public-wire.v1",
                "GET",
                capture.requested_url.as_str(),
                &capture.limits,
                settings_fingerprint(),
            ))?)
    {
        return Err(WikiError::invalid(
            "retained public capture descriptor differs",
        ));
    }
    capture.into_capture(read(&spool.response, 4 * 1024 * 1024)?)
}
pub fn recover_response(ledger: &JobLedger, attempt: &AttemptRef) -> Result<PublicFetchOutcome> {
    let capture = retained_capture(ledger, attempt)?;
    let inspect = ledger.inspect()?;
    let actual = inspect
        .attempts
        .iter()
        .find(|a| &a.attempt == attempt)
        .ok_or_else(|| WikiError::invalid("retained fetch absent"))?;
    if actual.phase != AttemptPhase::Received || actual.receipt.is_some() {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "fetch already materialized",
        ));
    }
    Ok(PublicFetchOutcome {
        attempt: attempt.clone(),
        spool: actual
            .spool
            .clone()
            .ok_or_else(|| WikiError::invalid("retained spool absent"))?,
        materialization: ledger.materialization_plan(attempt)?,
        capture,
    })
}

struct Timer {
    first: ClockReading,
    last: ClockReading,
    wall: Instant,
}
impl Timer {
    fn new(reading: ClockReading) -> Self {
        Self {
            first: reading,
            last: reading,
            wall: Instant::now(),
        }
    }
    fn check(&mut self, options: &JobOptions, deadline: i64, timeout: u64) -> Result<ClockReading> {
        if options.policy.offline || options.policy.dry_run {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "public dispatch prohibited",
            ));
        }
        if options.cancel.is_cancelled() {
            return Err(WikiError::new(
                ErrorCode::Cancelled,
                "public dispatch cancelled",
            ));
        }
        let now = options.clock.read()?;
        if now.utc_ms < self.last.utc_ms || now.monotonic_ms < self.last.monotonic_ms {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "public clock regressed",
            ));
        }
        if now.utc_ms >= deadline
            || now.monotonic_ms - self.first.monotonic_ms >= timeout
            || self.wall.elapsed() >= Duration::from_millis(timeout)
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "public deadline expired",
            ));
        }
        self.last = now;
        Ok(now)
    }
}
async fn bounded<T>(
    mut future: Pin<Box<dyn Future<Output = Result<T>> + Send + '_>>,
    timer: &mut Timer,
    options: &JobOptions,
    deadline: i64,
    timeout: u64,
    mut authorization: Option<&mut SendAuthorization>,
    entered: &mut bool,
) -> Result<T> {
    loop {
        timer.check(options, deadline, timeout)?;
        let poll = std::future::poll_fn(|cx| {
            if let Err(e) = timer.check(options, deadline, timeout) {
                return std::task::Poll::Ready(Err(e));
            }
            if let Some(auth) = authorization.as_deref_mut()
                && !*entered
            {
                match auth.check_before_entry_after(timer.last) {
                    Ok(reading) => {
                        timer.last = reading;
                        if reading.utc_ms >= deadline
                            || reading.monotonic_ms - timer.first.monotonic_ms >= timeout
                            || timer.wall.elapsed() >= Duration::from_millis(timeout)
                        {
                            return std::task::Poll::Ready(Err(WikiError::new(
                                ErrorCode::BudgetExceeded,
                                "public entry deadline expired",
                            )));
                        }
                        *entered = true;
                    }
                    Err(e) => return std::task::Poll::Ready(Err(e)),
                }
            }
            future.as_mut().poll(cx)
        });
        let value = tokio::select! {value=poll=>Some(value),_=tokio::time::sleep(Duration::from_millis(5))=>None};
        if let Some(value) = value {
            timer.check(options, deadline, timeout)?;
            return value;
        }
    }
}
fn fail(
    error: WikiError,
    attempt: Option<AttemptRef>,
    disposition: DispatchDisposition,
) -> Box<DispatchFailure> {
    Box::new(DispatchFailure {
        error: WikiError::new(error.code, "bounded public acquisition failed"),
        disposition,
        attempt,
        spool: None,
        materialization: None,
        retry: RetryDecision::Never,
    })
}

/// Called exclusively by Dispatcher::execute_public. This is not a second ledger.
pub(crate) fn dispatch(
    dispatcher_fs: &VaultFs,
    ledger: &JobLedger,
    key: &Blake3Hash,
    options: &PublicFetchOptions,
    network_used: &AtomicBool,
) -> std::result::Result<PublicFetchOutcome, Box<DispatchFailure>> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(fail(
            WikiError::new(
                ErrorCode::Usage,
                "public dispatch requires application thread",
            ),
            None,
            DispatchDisposition::NotSent,
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            fail(
                WikiError::new(ErrorCode::Internal, "public runtime unavailable"),
                None,
                DispatchDisposition::NotSent,
            )
        })?;
    let out = runtime.block_on(async {
        let prepared = prepare_fetch(dispatcher_fs, ledger, key)?;
        let reservation = ledger
            .reserve(key, prepared.bound.clone())
            .map_err(|e| fail(e, None, DispatchDisposition::NotSent))?;
        send_reserved(
            dispatcher_fs,
            ledger,
            prepared,
            reservation,
            options,
            network_used,
        )
        .await
    });
    runtime.shutdown_timeout(Duration::from_millis(50));
    out
}
fn check_prepared_sources(fs: &VaultFs, task: &TaskSpec, bytes: &[u8]) -> Result<()> {
    for dep in &task.source_bindings {
        let actual = read_bounded(fs, &dep.path, crate::changes::prepare::MAX_PAYLOAD_BYTES)?
            .map_or(ExpectedState::Absent, |b| {
                ExpectedState::Hash(Blake3Hash::digest(b))
            });
        if actual != dep.expected {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "public source changed",
            ));
        }
    }
    let fresh = read_bounded(fs, &task.input.path, 256 * 1024)?
        .ok_or_else(|| WikiError::invalid("public input missing"))?;
    if fresh != bytes {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "public input changed",
        ));
    }
    Ok(())
}
pub(super) struct PreparedFetch {
    pub(super) bound: AttemptBound,
    key: Blake3Hash,
    task: TaskSpec,
    bytes: Vec<u8>,
    url: url::Url,
    observed_url: String,
    limits: FetchLimits,
    currency: Currency,
    deadline: i64,
}
pub(super) fn prepare_fetch(
    dispatcher_fs: &VaultFs,
    ledger: &JobLedger,
    key: &Blake3Hash,
) -> std::result::Result<PreparedFetch, Box<DispatchFailure>> {
    let (fs, _, _, job_options) = ledger.dispatcher_bindings();
    let notsent = |e| fail(e, None, DispatchDisposition::NotSent);
    if fs.root().path() != dispatcher_fs.root().path() {
        return Err(notsent(WikiError::invalid(
            "public dispatcher vault differs",
        )));
    }
    let inspect = ledger.inspect().map_err(notsent)?;
    let deadline = inspect.effective_deadline_utc_ms;
    let first = job_options.clock.read().map_err(notsent)?;
    let mut timer = Timer::new(first);
    timer
        .check(&job_options, deadline, 30_000)
        .map_err(notsent)?;
    let task = inspect
        .tasks
        .get(key)
        .ok_or_else(|| notsent(WikiError::invalid("public task missing")))?
        .spec
        .clone();
    if task.key != crate::jobs::tasks::task_key(&task).map_err(notsent)?
        || task.capability != Some(Capability::Fetch)
        || task.settings_hash != settings_fingerprint()
        || task.model_hash.is_some()
        || task.prompt_hash.is_some()
        || task.schema_hash.is_some()
        || (inspect.research.is_none()
            && inspect.spec.scope.profile_fingerprints.get(PUBLIC_PROFILE)
                != Some(&settings_fingerprint()))
        || inspect
            .research
            .as_ref()
            .is_some_and(|r| !r.active_tasks.contains(key) || !r.task_origins.contains_key(key))
    {
        return Err(notsent(WikiError::invalid("public task bindings differ")));
    }
    if inspect
        .attempts
        .iter()
        .any(|a| a.attempt.task_key == *key && a.spool.is_some() && a.receipt.is_none())
    {
        return Err(notsent(WikiError::new(
            ErrorCode::RecoveryRequired,
            "public prior response needs materialization",
        )));
    }
    let bytes = read_bounded(&fs, &task.input.path, 256 * 1024)
        .map_err(notsent)?
        .ok_or_else(|| notsent(WikiError::invalid("public descriptor missing")))?;
    if bytes.len() as u64 != task.input.byte_len || Blake3Hash::digest(&bytes) != task.input.hash {
        return Err(notsent(WikiError::new(
            ErrorCode::FreshnessConflict,
            "public descriptor changed",
        )));
    }
    let input: RemoteInput = serde_json::from_slice(&bytes)
        .map_err(|_| notsent(WikiError::invalid("public descriptor invalid")))?;
    if canonical_json(&input).map_err(notsent)? != bytes
        || input_fingerprint(&input).map_err(notsent)? != task.input_hash
        || input.version != 1
    {
        return Err(notsent(WikiError::invalid(
            "public descriptor is not canonical",
        )));
    }
    let RemoteOperation::Fetch {
        url: observed_url,
        limits,
    } = &input.operation
    else {
        return Err(notsent(WikiError::invalid(
            "public fetch operation required",
        )));
    };
    limits.validate().map_err(notsent)?;
    let url = validate_url(observed_url, None).map_err(notsent)?;
    check_prepared_sources(&fs, &task, &bytes).map_err(notsent)?;
    let wire_hash = Blake3Hash::digest(
        canonical_json(&(
            "lwiki.public-wire.v1",
            "GET",
            url.as_str(),
            limits,
            settings_fingerprint(),
        ))
        .map_err(notsent)?,
    );
    // This adapter performs unauthenticated website GETs and has no provider API
    // fee. ISP/network charges are outside provider-fee accounting. Search/model
    // fees never inherit this explicit adapter contract.
    let currency = inspect
        .effective_limits
        .max_cost
        .as_ref()
        .map(|m| m.currency().clone())
        .unwrap_or(Currency::new("USD").map_err(notsent)?);
    let mut card = RateCard {
        id: "unmetered-public-fetch-v1".into(),
        version: 1,
        fingerprint: Blake3Hash::digest([]),
        currency: currency.clone(),
        validity: PriceValidity::EntireAttempt {
            valid_from_utc_ms: 0,
            valid_until_utc_ms: deadline,
        },
        request_fee_nanounits: 0,
        rates: BTreeMap::new(),
    };
    card.fingerprint = crate::jobs::budgets::rate_card_fingerprint(&card).map_err(notsent)?;
    let mut bound = AttemptBound {
        codec: None,
        profile_fingerprint: inspect.research.as_ref().map(|_| settings_fingerprint()),
        capability: Capability::Fetch,
        profile_id: PUBLIC_PROFILE.into(),
        endpoint_fingerprint: Blake3Hash::digest(url.as_str()),
        config_fingerprint: inspect
            .research
            .as_ref()
            .map_or(&inspect.spec.config_fingerprint, |r| {
                &r.binding.config_fingerprint
            })
            .clone(),
        input_hash: task.input_hash.clone(),
        wire_hash,
        requested_model: None,
        requested_model_revision: None,
        request_bytes: 0,
        response_bytes: limits.compressed_bytes,
        timeout_ms: limits.timeout_ms,
        applicable_classes: BTreeSet::new(),
        billable_bounds: BTreeMap::new(),
        rate_card: Some(card),
        quoted_allowance: Some(Money::new(currency.clone(), 0)),
        bounds_fingerprint: Blake3Hash::digest([]),
    };
    bound.bounds_fingerprint = crate::jobs::budgets::bound_fingerprint(&bound).map_err(notsent)?;
    Ok(PreparedFetch {
        bound,
        key: key.clone(),
        task,
        bytes,
        url,
        observed_url: observed_url.clone(),
        limits: limits.clone(),
        currency,
        deadline,
    })
}
pub(super) async fn send_reserved(
    dispatcher_fs: &VaultFs,
    ledger: &JobLedger,
    prepared: PreparedFetch,
    reservation: Reservation,
    options: &PublicFetchOptions,
    network_used: &AtomicBool,
) -> std::result::Result<PublicFetchOutcome, Box<DispatchFailure>> {
    let (fs, _, _, job_options) = ledger.dispatcher_bindings();
    let PreparedFetch {
        bound,
        key,
        task,
        bytes,
        url,
        observed_url,
        limits,
        currency,
        deadline,
    } = prepared;
    let attempt = reservation.attempt().clone();
    let notsent = |e| fail(e, Some(attempt.clone()), DispatchDisposition::NotSent);
    let setup = (|| -> Result<Timer> {
        if fs.root().path() != dispatcher_fs.root().path()
            || reservation.attempt().task_key != key
            || reservation.bound() != &bound
        {
            return Err(WikiError::invalid("public reserved binding differs"));
        }
        let first = job_options.clock.read()?;
        let mut timer = Timer::new(first);
        timer.check(&job_options, deadline, limits.timeout_ms)?;
        check_prepared_sources(&fs, &task, &bytes)?;
        Ok(timer)
    })();
    let mut timer = match setup {
        Ok(timer) => timer,
        Err(error) => {
            let _ = ledger.release_not_sent(
                &attempt,
                NotSentObservation {
                    reason: NotSentReason::TransportNotEntered,
                },
            );
            return Err(notsent(error));
        }
    };
    let check_sources = || check_prepared_sources(&fs, &task, &bytes);
    let permit = ledger.dispatch_intent(reservation).map_err(|e| {
        let _ = ledger.release_not_sent(
            &attempt,
            NotSentObservation {
                reason: NotSentReason::TransportNotEntered,
            },
        );
        fail(e, Some(attempt.clone()), DispatchDisposition::NotSent)
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| notsent(WikiError::invalid("public host missing")))?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| notsent(WikiError::invalid("public port missing")))?;
    let mut resolver_entered = false;
    let resolution = match url.host() {
        Some(url::Host::Ipv4(ip)) => Ok(vec![IpAddr::V4(ip)]),
        Some(url::Host::Ipv6(ip)) => Ok(vec![IpAddr::V6(ip)]),
        _ => {
            bounded(
                options.resolver.resolve(&host, port),
                &mut timer,
                &job_options,
                deadline,
                limits.timeout_ms,
                None,
                &mut resolver_entered,
            )
            .await
        }
    };
    let addresses = match resolution.and_then(|ips| {
        if ips.is_empty() || ips.len() > 16 || ips.iter().any(|ip| !public_ip(*ip)) {
            return Err(WikiError::invalid(
                "DNS includes nonpublic or excessive destinations",
            ));
        }
        Ok(ips
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect::<Vec<_>>())
    }) {
        Ok(ips) => ips,
        Err(e) => {
            let _ = ledger.release_not_sent(
                &attempt,
                NotSentObservation {
                    reason: NotSentReason::TransportNotEntered,
                },
            );
            return Err(fail(e, Some(attempt), DispatchDisposition::NotSent));
        }
    };
    if let Err(e) = check_sources() {
        let _ = ledger.release_not_sent(
            &attempt,
            NotSentObservation {
                reason: NotSentReason::TransportNotEntered,
            },
        );
        return Err(fail(e, Some(attempt), DispatchDisposition::NotSent));
    }
    let mut auth = ledger.begin_send(permit).map_err(|e| {
        let _ = ledger.release_not_sent(
            &attempt,
            NotSentObservation {
                reason: NotSentReason::TransportNotEntered,
            },
        );
        fail(e, Some(attempt.clone()), DispatchDisposition::NotSent)
    })?;
    let progress = Arc::new(AtomicU64::new(0));
    let pinned = PinnedRequest {
        url: url.to_string(),
        host,
        addresses,
        limits: limits.clone(),
        observed: progress.clone(),
    };
    let mut entered = false;
    let connection = match options.connector.connect(pinned) {
        Ok(future) => future,
        Err(e) => {
            let _ = ledger.release_not_sent(
                &attempt,
                NotSentObservation {
                    reason: NotSentReason::TransportNotEntered,
                },
            );
            return Err(fail(e, Some(attempt), DispatchDisposition::NotSent));
        }
    };
    let response = bounded(
        connection,
        &mut timer,
        &job_options,
        deadline,
        limits.timeout_ms,
        Some(&mut auth),
        &mut entered,
    )
    .await;
    if entered {
        network_used.store(true, Ordering::Relaxed);
    }
    let reply = match response {
        Ok(reply) => reply,
        Err(e) => {
            if !entered {
                let _ = ledger.release_not_sent(
                    &attempt,
                    NotSentObservation {
                        reason: NotSentReason::TransportNotEntered,
                    },
                );
                return Err(fail(e, Some(attempt), DispatchDisposition::NotSent));
            }
            let _ = ledger.outcome_unknown(&attempt, "public_transport_unknown");
            let _ = ledger.pause(StopReason::OutcomeUnknown);
            let mut failure = fail(
                e,
                Some(attempt.clone()),
                DispatchDisposition::OutcomeUnknown,
            );
            let observed = progress.load(Ordering::SeqCst);
            if observed > 0 {
                let metadata = ResponseMetadata {
                    acquisition: None,
                    provider_request_id: None,
                    returned_model: None,
                    status_code: None,
                    terminal_response: false,
                    usage: KnownOrUnknown::Known(Usage {
                        request_bytes: 0,
                        response_bytes: observed,
                        billable_units: BTreeMap::new(),
                    }),
                    computed_cost: KnownOrUnknown::Known(Money::new(currency.clone(), 0)),
                    failure_code: Some("public_response_incomplete".into()),
                };
                if let Ok(spool) = ledger.record_response(
                    &attempt,
                    ResponseSpoolInput {
                        bytes: vec![],
                        metadata,
                    },
                ) {
                    failure.spool = Some(spool);
                    failure.materialization = ledger.materialization_plan(&attempt).ok();
                }
            }
            return Err(failure);
        }
    };
    let valid = reply.complete
        && (100..600).contains(&reply.status)
        && super::transport::headers_valid(&reply.headers)
        && reply.observed_bytes == reply.bytes.len() as u64
        && reply.observed_bytes <= limits.compressed_bytes
        && ["content-type", "content-encoding", "location"]
            .iter()
            .all(|key| {
                reply
                    .headers
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case(key))
                    .count()
                    <= 1
            });
    let capture = FetchCapture {
        observed_url: observed_url.clone(),
        requested_url: url.to_string(),
        fetched_at_utc_ms: timer.last.utc_ms,
        status: reply.status,
        headers: reply.headers.clone(),
        original_hash: Blake3Hash::digest(&reply.bytes),
        original: reply.bytes.clone(),
        limits: limits.clone(),
    };
    let acquisition = valid.then(|| PublicCaptureMetadata::from_capture(&capture));
    let valid = valid && acquisition.as_ref().is_some_and(|m| m.validate().is_ok());
    let metadata = ResponseMetadata {
        acquisition: if valid { acquisition } else { None },
        provider_request_id: None,
        returned_model: None,
        status_code: Some(reply.status),
        terminal_response: valid,
        usage: KnownOrUnknown::Known(Usage {
            request_bytes: 0,
            response_bytes: reply.observed_bytes,
            billable_units: BTreeMap::new(),
        }),
        computed_cost: KnownOrUnknown::Known(Money::new(currency, 0)),
        failure_code: (!valid).then(|| "public_response_bound".into()),
    };
    let spool = ledger
        .record_response(
            &attempt,
            ResponseSpoolInput {
                bytes: if valid { reply.bytes.clone() } else { vec![] },
                metadata,
            },
        )
        .map_err(|e| {
            let _ = ledger.outcome_unknown(&attempt, "public_persistence_failed");
            fail(
                e,
                Some(attempt.clone()),
                DispatchDisposition::OutcomeUnknown,
            )
        })?;
    let materialization = ledger.materialization_plan(&attempt).map_err(|e| {
        let mut failure = fail(
            e,
            Some(attempt.clone()),
            DispatchDisposition::OutcomeUnknown,
        );
        failure.spool = Some(spool.clone());
        failure
    })?;
    if !valid {
        let mut failure = fail(
            WikiError::new(
                ErrorCode::ProviderResponse,
                "public response exceeded bounds",
            ),
            Some(attempt),
            DispatchDisposition::Rejected,
        );
        failure.spool = Some(spool);
        failure.materialization = Some(materialization);
        return Err(failure);
    }
    Ok(PublicFetchOutcome {
        attempt,
        spool,
        materialization,
        capture,
    })
}

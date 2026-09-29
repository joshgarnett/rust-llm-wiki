//! One native HTTP request, with no authenticated public client constructor.
use super::types::*;
use crate::{domain::*, jobs::ClockReading};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::{
    future::Future,
    time::{Duration, Instant},
};

pub(super) const HEADER_COUNT: usize = 64;
pub(super) const HEADER_BYTES: usize = 16 * 1024;
pub(super) const HEADER_FIELD: usize = 4 * 1024;
impl AuthenticatedRequest<'_> {
    pub fn summary(&self) -> TransportSummary {
        TransportSummary {
            attempt: self.authorization.attempt().clone(),
            role: self.prepared.role,
            endpoint_fingerprint: self.prepared.bound.endpoint_fingerprint.clone(),
            wire_hash: self.prepared.bound.wire_hash.clone(),
            request_bytes: self.prepared.bound.request_bytes,
        }
    }
}
impl TransportReply {
    /// Bounded synthetic responses grant no trust or dispatch authority.
    pub fn new(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<Self> {
        if !(100..600).contains(&status) || body.len() > 8 * 1024 * 1024 || !headers_valid(&headers)
        {
            return Err(WikiError::new(
                ErrorCode::ProviderResponse,
                "mock response exceeds bounds",
            ));
        }
        Ok(Self {
            status,
            headers,
            observed_body_bytes: body.len() as u64,
            body,
            body_exceeded: false,
            headers_exceeded: false,
            terminal: true,
        })
    }
}
impl TransportFailure {
    pub fn new(code: TransportFailureCode) -> Self {
        Self {
            code,
            observed_body_bytes: 0,
            not_entered: false,
        }
    }
}
pub(super) fn headers_valid(headers: &[(String, String)]) -> bool {
    headers.len() <= HEADER_COUNT
        && headers
            .iter()
            .try_fold(0usize, |total, (name, value)| {
                if name.len() > 128
                    || name.is_empty()
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                    || value.len() > HEADER_FIELD
                    || value.chars().any(|c| c.is_control())
                {
                    return None;
                }
                total.checked_add(name.len())?.checked_add(value.len())
            })
            .is_some_and(|total| total <= HEADER_BYTES)
}
struct Timer {
    first: ClockReading,
    last: ClockReading,
    wall: Instant,
}
impl Timer {
    fn new(first: ClockReading) -> Self {
        Self {
            first,
            last: first,
            wall: Instant::now(),
        }
    }
    fn check(
        &mut self,
        context: &TransportContext,
        observed: u64,
    ) -> std::result::Result<(), TransportFailure> {
        if context.cancel.is_cancelled() {
            return Err(TransportFailure {
                code: TransportFailureCode::Cancelled,
                observed_body_bytes: observed,
                not_entered: false,
            });
        }
        let now = context.clock.read().map_err(|_| TransportFailure {
            code: TransportFailureCode::ClockRegression,
            observed_body_bytes: observed,
            not_entered: false,
        })?;
        self.check_reading(context, observed, now)
    }
    // The successful final authorization sample must be consumed without a
    // second clock read between sealing authority and polling HTTP.
    fn check_reading(
        &mut self,
        context: &TransportContext,
        observed: u64,
        now: ClockReading,
    ) -> std::result::Result<(), TransportFailure> {
        let code = if context.cancel.is_cancelled() {
            Some(TransportFailureCode::Cancelled)
        } else if now.utc_ms < self.last.utc_ms
            || now.monotonic_ms < self.last.monotonic_ms
            || now.utc_ms < 0
        {
            Some(TransportFailureCode::ClockRegression)
        } else if now.utc_ms >= context.deadline_utc_ms
            || now.monotonic_ms - self.first.monotonic_ms >= context.timeout_ms
            || self.wall.elapsed() >= Duration::from_millis(context.timeout_ms)
        {
            Some(TransportFailureCode::Timeout)
        } else {
            self.last = now;
            None
        };
        if let Some(code) = code {
            Err(TransportFailure {
                code,
                observed_body_bytes: observed,
                not_entered: false,
            })
        } else {
            Ok(())
        }
    }
}
async fn bounded<F: Future>(
    future: F,
    context: &TransportContext,
    timer: &mut Timer,
    observed: u64,
) -> std::result::Result<F::Output, TransportFailure> {
    tokio::pin!(future);
    bounded_poll(context, timer, observed, |cx, _| future.as_mut().poll(cx)).await
}
async fn bounded_poll<T>(
    context: &TransportContext,
    timer: &mut Timer,
    observed: u64,
    mut poll: impl FnMut(&mut std::task::Context<'_>, &mut Timer) -> std::task::Poll<T>,
) -> std::result::Result<T, TransportFailure> {
    loop {
        timer.check(context, observed)?;
        // The temporary poll future's exclusive timer borrow ends before the
        // post-poll check. The entry closure observes the live floor each poll.
        let value = {
            let future = std::future::poll_fn(|cx| poll(cx, timer));
            tokio::pin!(future);
            tokio::select! {
                value=&mut future=>Some(value),
                _=tokio::time::sleep(Duration::from_millis(5))=>None,
            }
        };
        if let Some(value) = value {
            timer.check(context, observed)?;
            return Ok(value);
        }
    }
}

pub(super) struct NativeTransport;
impl Transport for NativeTransport {
    fn execute<'a>(
        &'a self,
        request: AuthenticatedRequest<'a>,
        context: TransportContext,
    ) -> TransportFuture<'a> {
        Box::pin(async move {
            let mut request = request;
            let mut entered = false;
            let result = async {
                let mut timer = Timer::new(request.authorization.closing_reading());
                timer.check(&context, 0)?;
                if request.authorization.bound() != &request.prepared.bound
                    || request.authorization.attempt().request_hash
                        != request.prepared.bound.wire_hash
                {
                    return Err(TransportFailure::new(TransportFailureCode::InvalidResponse));
                }
                let computed = Blake3Hash::digest(
                    serde_json::to_vec(&(
                        "lwiki.wire.v1",
                        &request.prepared.method,
                        &request.prepared.url,
                        &request.prepared.headers,
                        &request.prepared.body,
                        &request.service.summary().profile_fingerprint,
                    ))
                    .map_err(|_| TransportFailure::new(TransportFailureCode::InvalidResponse))?,
                );
                if computed != request.authorization.bound().wire_hash
                    || request.prepared.body.len() as u64
                        != request.authorization.bound().request_bytes
                    || request.prepared.url != request.service.service().url
                {
                    return Err(TransportFailure::new(TransportFailureCode::InvalidResponse));
                }
                let mut roots = rustls::RootCertStore::empty();
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                if let Some(ca) = &request.tls_ca {
                    let certs = CertificateDer::pem_slice_iter(ca);
                    let mut count = 0usize;
                    for cert in certs {
                        roots
                            .add(cert.map_err(|_| {
                                TransportFailure::new(TransportFailureCode::InvalidResponse)
                            })?)
                            .map_err(|_| {
                                TransportFailure::new(TransportFailureCode::InvalidResponse)
                            })?;
                        count += 1;
                    }
                    if count == 0 {
                        return Err(TransportFailure::new(TransportFailureCode::InvalidResponse));
                    }
                }
                let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(|_| TransportFailure::new(TransportFailureCode::Unavailable))?
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
                    .http1_max_headers(HEADER_COUNT)
                    .pool_max_idle_per_host(0)
                    .connect_timeout(Duration::from_millis(context.connect_timeout_ms))
                    .timeout(Duration::from_millis(context.timeout_ms))
                    .build()
                    .map_err(|_| TransportFailure::new(TransportFailureCode::Unavailable))?;
                let method = reqwest::Method::from_bytes(request.prepared.method.as_bytes())
                    .map_err(|_| TransportFailure::new(TransportFailureCode::InvalidResponse))?;
                let mut builder = client
                    .request(method, &request.prepared.url)
                    .header("Accept-Encoding", "identity");
                for (name, value) in &request.prepared.headers {
                    builder = builder.header(name, value);
                }
                for (name, value) in request.lease.headers() {
                    let mut header = reqwest::header::HeaderValue::from_bytes(value.as_bytes())
                        .map_err(|_| {
                            TransportFailure::new(TransportFailureCode::InvalidResponse)
                        })?;
                    header.set_sensitive(true);
                    builder = builder.header(name, header);
                }
                // Request creation is pure. Nothing polls a network future before the final policy check.
                let wire = builder
                    .body(request.prepared.body.clone())
                    .build()
                    .map_err(|_| TransportFailure::new(TransportFailureCode::InvalidResponse))?;
                if wire.url().as_str() != request.prepared.url {
                    return Err(TransportFailure::new(TransportFailureCode::InvalidResponse));
                }
                request
                    .broker
                    .validate_lease(
                        request.service,
                        &request.fs,
                        &request.lease,
                        &request.credential_context,
                    )
                    .map_err(|_| TransportFailure::new(TransportFailureCode::Unavailable))?;
                request
                    .lease
                    .check_validity()
                    .map_err(|_| TransportFailure::new(TransportFailureCode::Unavailable))?;
                timer.check(&context, 0)?;
                let sending = client.execute(wire);
                tokio::pin!(sending);
                // Mark possible entry only at the first actual HTTP future poll.
                // bounded checks cancellation/clock policy before polling this wrapper.
                let mut response = bounded_poll(&context, &mut timer, 0, |cx, timer| {
                    if !entered {
                        // Recheck after any select sleep, at the actual first HTTP
                        // poll. No await/callback separates the final paid proof
                        // from setting entry and polling the transport future.
                        if let Err(error) = request.broker.validate_lease(
                            request.service,
                            &request.fs,
                            &request.lease,
                            &request.credential_context,
                        ) {
                            return std::task::Poll::Ready(Err(authority_failure(error)));
                        }
                        if let Err(error) = request.lease.check_validity() {
                            return std::task::Poll::Ready(Err(authority_failure(error)));
                        }
                        let reading =
                            match request.authorization.check_before_entry_after(timer.last) {
                                Ok(reading) => reading,
                                Err(error) => {
                                    return std::task::Poll::Ready(Err(authority_failure(error)));
                                }
                            };
                        if let Err(error) = timer.check_reading(&context, 0, reading) {
                            return std::task::Poll::Ready(Err(error));
                        }
                        entered = true;
                    }
                    sending.as_mut().poll(cx).map(|response| {
                        response
                            .map_err(|_| TransportFailure::new(TransportFailureCode::Unavailable))
                    })
                })
                .await??;
                let status = response.status().as_u16();
                let mut headers = Vec::new();
                let mut total = 0usize;
                let mut exceeded = false;
                for (name, value) in response.headers() {
                    let Ok(value) = value.to_str() else {
                        exceeded = true;
                        break;
                    };
                    total = match total
                        .checked_add(name.as_str().len())
                        .and_then(|n| n.checked_add(value.len()))
                    {
                        Some(n) => n,
                        None => {
                            exceeded = true;
                            break;
                        }
                    };
                    if headers.len() == HEADER_COUNT
                        || total > HEADER_BYTES
                        || value.len() > HEADER_FIELD
                        || value.chars().any(char::is_control)
                    {
                        exceeded = true;
                        break;
                    }
                    headers.push((name.as_str().to_owned(), value.to_owned()));
                }
                if exceeded {
                    return Ok(TransportReply {
                        status,
                        headers: Vec::new(),
                        body: Vec::new(),
                        observed_body_bytes: 0,
                        body_exceeded: false,
                        headers_exceeded: true,
                        terminal: false,
                    });
                }
                if headers.iter().any(|(n, v)| {
                    n.eq_ignore_ascii_case("content-encoding")
                        && !v.trim().eq_ignore_ascii_case("identity")
                }) {
                    return Ok(TransportReply {
                        status,
                        headers,
                        body: Vec::new(),
                        observed_body_bytes: 0,
                        body_exceeded: false,
                        headers_exceeded: false,
                        terminal: false,
                    });
                }
                let cap = usize::try_from(context.response_bytes)
                    .map_err(|_| TransportFailure::new(TransportFailureCode::InvalidResponse))?;
                let mut body = Vec::new();
                let mut observed = 0u64;
                loop {
                    let next = bounded(response.chunk(), &context, &mut timer, observed)
                        .await?
                        .map_err(|_| TransportFailure {
                            code: TransportFailureCode::Unavailable,
                            observed_body_bytes: observed,
                            not_entered: false,
                        })?;
                    let Some(chunk) = next else { break };
                    observed = observed.checked_add(chunk.len() as u64).ok_or_else(|| {
                        TransportFailure::new(TransportFailureCode::InvalidResponse)
                    })?;
                    if observed > context.response_bytes {
                        return Ok(TransportReply {
                            status,
                            headers,
                            body: Vec::new(),
                            observed_body_bytes: observed,
                            body_exceeded: true,
                            headers_exceeded: false,
                            terminal: false,
                        });
                    }
                    if body.len().checked_add(chunk.len()).is_none_or(|n| n > cap) {
                        return Err(TransportFailure::new(TransportFailureCode::InvalidResponse));
                    }
                    body.extend_from_slice(&chunk);
                }
                timer.check(&context, observed)?;
                Ok(TransportReply {
                    status,
                    headers,
                    body,
                    observed_body_bytes: observed,
                    body_exceeded: false,
                    headers_exceeded: false,
                    terminal: true,
                })
            }
            .await;
            result.map_err(|mut error: TransportFailure| {
                error.not_entered = !entered;
                error
            })
        })
    }
}

fn authority_failure(error: WikiError) -> TransportFailure {
    TransportFailure::new(match error.code {
        ErrorCode::Cancelled => TransportFailureCode::Cancelled,
        ErrorCode::BudgetExceeded => TransportFailureCode::Timeout,
        _ => TransportFailureCode::Unavailable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{CancellationToken, JobClock};
    use std::sync::Arc;

    struct FixedClock(ClockReading);
    impl JobClock for FixedClock {
        fn read(&self) -> Result<ClockReading> {
            Ok(self.0)
        }
    }

    #[test]
    fn final_reading_advances_live_timer_before_poll_and_is_retained_after_poll() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for final_value in [150, 250] {
            let reading = |value| ClockReading {
                utc_ms: value,
                monotonic_ms: value as u64,
            };
            let context = TransportContext {
                clock: Arc::new(FixedClock(reading(200))),
                cancel: CancellationToken::default(),
                deadline_utc_ms: 1000,
                timeout_ms: 1000,
                connect_timeout_ms: 1000,
                response_bytes: 1024,
            };
            let mut timer = Timer::new(reading(100));
            let mut polls = 0;
            let result = runtime.block_on(bounded_poll(&context, &mut timer, 0, |_, timer| {
                assert_eq!((timer.last.utc_ms, timer.last.monotonic_ms), (200, 200));
                if let Err(error) = timer.check_reading(&context, 0, reading(final_value)) {
                    return std::task::Poll::Ready(Err(error));
                }
                assert_eq!((timer.last.utc_ms, timer.last.monotonic_ms), (250, 250));
                polls += 1;
                std::task::Poll::Ready(Ok::<(), TransportFailure>(()))
            }));
            if final_value == 150 {
                assert_eq!(polls, 0);
                assert_eq!(
                    result.ok().unwrap().err().unwrap().code,
                    TransportFailureCode::ClockRegression
                );
            } else {
                assert_eq!(polls, 1);
                // Post-poll context200 must fail against the final250 sample:
                // losing that successful sample would wrongly return success.
                assert_eq!(
                    result.err().unwrap().code,
                    TransportFailureCode::ClockRegression
                );
                assert_eq!((timer.last.utc_ms, timer.last.monotonic_ms), (250, 250));
            }
        }
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;
    #[test]
    fn response_header_names_accept_http_tokens_and_preserve_bounds() {
        let headers = vec![
            ("llm_provider-x-amzn-requestid".into(), "request".into()),
            ("!#$%&'*+-.^_`|~09AZaz".into(), "value".into()),
        ];
        assert!(TransportReply::new(200, headers, b"{}".to_vec()).is_ok());
        for name in ["bad name", "bad:name", "bad\rname", "", "é"] {
            assert!(!headers_valid(&[(name.into(), "value".into())]));
        }
        assert!(!headers_valid(&[("good".into(), "bad\nvalue".into())]));
        assert!(!headers_valid(&vec![
            ("good".into(), "v".into());
            HEADER_COUNT + 1
        ]));
        assert!(!headers_valid(&[(
            "good".into(),
            "v".repeat(HEADER_FIELD + 1)
        )]));
    }
}

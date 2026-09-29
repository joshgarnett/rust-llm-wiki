use crate as library;
#[path = "../../tests/fixtures/p16b/common.rs"]
mod common;
use super::{
    dispatcher::{Dispatcher, fixture_prepare},
    retry,
    transport::NativeTransport,
    types::*,
};
use crate::{domain::*, jobs::*};
use common::*;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
const GOOD:&[u8]=br#"{"ok":true,"usage":{"input_tokens":1,"cached_input_tokens":0},"model":"test-model","id":"request-1"}"#;
struct Mock {
    calls: AtomicUsize,
    attempts: Mutex<Vec<AttemptRef>>,
    responses: Mutex<VecDeque<std::result::Result<TransportReply, TransportFailure>>>,
}
impl Mock {
    fn new(responses: Vec<std::result::Result<TransportReply, TransportFailure>>) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            attempts: Mutex::new(vec![]),
            responses: Mutex::new(responses.into()),
        })
    }
}
impl Transport for Mock {
    fn execute<'a>(
        &'a self,
        request: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.authorization.bound(), &request.prepared.bound);
        assert_eq!(
            request.authorization.attempt().request_hash,
            request.prepared.bound.wire_hash
        );
        self.attempts
            .lock()
            .unwrap()
            .push(request.summary().attempt);
        assert!(
            request
                .lease
                .headers()
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("authorization")
                    && value.as_bytes() == b"Bearer fixture-token")
        );
        let result = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra HTTP attempt");
        Box::pin(async move { result })
    }
}
fn reply(status: u16, body: &[u8]) -> std::result::Result<TransportReply, TransportFailure> {
    Ok(TransportReply::new(status, vec![], body.to_vec()).unwrap())
}
fn dispatch(c: &Case, mock: Arc<Mock>) -> Dispatcher {
    Dispatcher::new(
        c.fs.clone(),
        DispatchOptions {
            broker: c.broker.clone(),
            transport: mock,
            jitter: Arc::new(ZeroJitter),
        },
    )
}
fn ok(d: &Dispatcher, c: &Case) -> DispatchOutcome {
    d.execute(
        &c.job,
        &c.trusted,
        &c.spec.tasks[0].key,
        DispatchPurpose::Task,
    )
    .unwrap_or_else(|f| panic!("unexpected {}", f.error.code))
}
#[test]
fn retries_have_distinct_authorizations_and_commit_each_failure_receipt() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    let mock = Mock::new(vec![reply(503, GOOD), reply(200, GOOD)]);
    let out = ok(&dispatch(&c, mock.clone()), &c);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
    let attempts = mock.attempts.lock().unwrap();
    assert_ne!(attempts[0].attempt_id, attempts[1].attempt_id);
    assert_eq!(out.attempt.number, 2);
    let i = c.job.inspect().unwrap();
    assert_eq!(i.budget.dispatched_requests, 2);
    assert!(i.attempts[0].receipt.is_some());
    assert_eq!(i.attempts[0].phase, AttemptPhase::Settled);
    assert!(i.attempts[0].spool.is_some());
    assert_eq!(i.attempts[0].billing, BillingDisposition::UnknownReserved);
    assert_eq!(
        i.attempts[1].remote_exposure,
        RemoteExposure::TerminalConfirmed
    );
}
#[test]
fn status_retry_unknown_billing_does_not_imply_zero_or_uncertain_network_outcome() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    let mock = Mock::new(vec![
        reply(429, b"error body SECRET"),
        reply(503, b"another error"),
        reply(200, GOOD),
    ]);
    let out = ok(&dispatch(&c, mock.clone()), &c);
    assert_eq!(out.attempt.number, 3);
    let i = c.job.inspect().unwrap();
    assert_eq!(i.budget.dispatched_requests, 3);
    assert_eq!(i.budget.unknown_attempts.len(), 3);
    assert!(
        i.attempts[..2]
            .iter()
            .all(|a| a.receipt.is_some() && a.spool.is_some())
    );
    for a in &i.attempts[..2] {
        let path =
            c.fs.root()
                .resolve(&a.spool.as_ref().unwrap().response.path)
                .unwrap();
        assert!(std::fs::read(path).unwrap().is_empty());
    }
}
#[test]
fn static_401_403_and_successful_malformed_response_never_retry_or_refresh() {
    for (status, body) in [
        (401, b"SECRET ERROR".as_slice()),
        (403, b"SECRET ERROR".as_slice()),
        (200, b"malformed paid JSON".as_slice()),
    ] {
        let c = case(
            "https://gateway.example/v1/embeddings",
            false,
            ExecutionPolicy::default(),
            |_| {},
            None,
        );
        let mock = Mock::new(vec![reply(status, body)]);
        let f = dispatch(&c, mock.clone())
            .execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(c.runner.calls.load(Ordering::SeqCst), 0);
        assert!(f.materialization.is_some());
        let i = c.job.inspect().unwrap();
        assert!(i.attempts[0].receipt.is_some());
        assert_eq!(i.attempts[0].phase, AttemptPhase::Settled);
        assert!(!serde_json::to_string(&f.error).unwrap().contains("SECRET"));
    }
}
#[test]
fn command_401_refreshes_once_with_new_reservation_and_never_on_403() {
    for statuses in [vec![401, 200], vec![401, 401], vec![403]] {
        let c = case(
            "https://gateway.example/v1/embeddings",
            true,
            ExecutionPolicy::default(),
            |_| {},
            None,
        );
        let mock = Mock::new(statuses.iter().map(|s| reply(*s, GOOD)).collect());
        let _ = dispatch(&c, mock.clone()).execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        );
        assert_eq!(mock.calls.load(Ordering::SeqCst), statuses.len());
        assert_eq!(
            c.runner.calls.load(Ordering::SeqCst),
            if statuses[0] == 401 { 2 } else { 1 }
        );
        assert_eq!(
            c.job.inspect().unwrap().budget.dispatched_requests,
            statuses.len() as u64
        );
    }
}
#[test]
fn unknown_send_timeout_reserves_charge_and_explicit_uncertain_retry_needs_new_attempt() {
    for allowed in [false, true] {
        let c = case(
            "https://gateway.example/v1/embeddings",
            false,
            ExecutionPolicy {
                retry_uncertain: allowed,
                ..ExecutionPolicy::default()
            },
            |_| {},
            None,
        );
        let mut responses = vec![Err(TransportFailure::new(TransportFailureCode::Timeout))];
        if allowed {
            responses.push(reply(200, GOOD));
        }
        let mock = Mock::new(responses);
        let result = dispatch(&c, mock.clone()).execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        );
        assert_eq!(
            mock.calls.load(Ordering::SeqCst),
            if allowed { 2 } else { 1 }
        );
        let i = c.job.inspect().unwrap();
        assert_eq!(i.attempts[0].billing, BillingDisposition::UnknownReserved);
        assert_eq!(
            i.attempts[0].remote_exposure,
            RemoteExposure::PossiblyInFlight
        );
        assert_eq!(i.budget.remote_inflight, 1);
        assert!(result.is_ok() == allowed);
    }
}
#[test]
fn unknown_hard_billable_bound_is_rejected_before_auth_or_reservation() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |spec| spec.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 1)),
        None,
    );
    let mock = Mock::new(vec![]);
    let f = dispatch(&c, mock.clone())
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(f.error.code, ErrorCode::CapabilityUnavailable);
    assert!(c.job.inspect().unwrap().attempts.is_empty());
    assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn modified_semantic_input_or_settings_cannot_forge_prepared_wire() {
    for which in ["model", "settings"] {
        let c = case(
            "https://gateway.example/v1/embeddings",
            false,
            ExecutionPolicy::default(),
            |spec| match which {
                "input" => spec.tasks[0].input_hash = hash("fake"),
                "model" => spec.tasks[0].model_hash = Some(hash("fake")),
                _ => spec.tasks[0].settings_hash = hash("fake"),
            },
            None,
        );
        let mock = Mock::new(vec![]);
        let f = dispatch(&c, mock.clone())
            .execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert_eq!(f.error.code, ErrorCode::FreshnessConflict);
        assert!(c.job.inspect().unwrap().attempts.is_empty());
        assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn probe_requires_underlying_authorized_role_and_separate_probe_task() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |s| s.tasks[0].capability = Some(Capability::Probe),
        None,
    );
    let mock = Mock::new(vec![reply(200, GOOD)]);
    let d = dispatch(&c, mock.clone());
    let f = d
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Probe {
                role: ServiceRole::Generate,
            },
        )
        .err()
        .unwrap();
    assert_eq!(f.error.code, ErrorCode::ProfileUntrusted);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    let out = d
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Probe {
                role: ServiceRole::Embed,
            },
        )
        .unwrap_or_else(|f| panic!("{}", f.error.code));
    assert!(matches!(
        out.output,
        ValidatedOutput::Probe {
            role: ServiceRole::Embed
        }
    ));
    assert_eq!(out.materialization.receipt.capability, Capability::Probe);
}
#[test]
fn retry_after_valid_minimum_dates_invalid_values_and_deadline_are_bounded() {
    let now = ClockReading {
        utc_ms: 1_700_000_000_000,
        monotonic_ms: 10,
    };
    let headers = vec![("Retry-After".into(), "2".into())];
    assert_eq!(
        retry::decide(429, &headers, 1, now, now.utc_ms + 5000, 1000, &ZeroJitter).unwrap(),
        RetryDecision::After {
            delay_ms: 2000,
            reason: "rate_limit".into()
        }
    );
    assert!(matches!(
        retry::decide(503, &headers, 1, now, now.utc_ms + 2000, 1000, &ZeroJitter).unwrap(),
        RetryDecision::Pause { .. }
    ));
    for v in ["-1", "garbage"] {
        assert!(matches!(
            retry::decide(
                503,
                &[("Retry-After".into(), v.into())],
                1,
                now,
                now.utc_ms + 5000,
                1000,
                &ZeroJitter
            )
            .unwrap(),
            RetryDecision::After { delay_ms: 0, .. }
        ));
    }
    let date = httpdate::fmt_http_date(
        std::time::UNIX_EPOCH + Duration::from_millis((now.utc_ms + 2000) as u64),
    );
    assert!(matches!(
        retry::decide(
            503,
            &[("Retry-After".into(), date)],
            1,
            now,
            now.utc_ms + 5000,
            1000,
            &ZeroJitter
        )
        .unwrap(),
        RetryDecision::After { delay_ms: 2000, .. }
    ));
    for status in [200, 301, 400, 401, 403, 404, 501] {
        assert_eq!(
            retry::decide(status, &[], 1, now, now.utc_ms + 5000, 1000, &ZeroJitter).unwrap(),
            RetryDecision::Never
        );
    }
}
#[test]
fn error_body_scrub_and_secret_echo_in_metadata_never_escape_receipt() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    let body=br#"{"id":"fixture-token","model":"fixture-token","usage":{"input_tokens":1,"cached_input_tokens":0},"SECRET_BODY":"fixture-token"}"#;
    let mock = Mock::new(vec![reply(403, body)]);
    let f = dispatch(&c, mock)
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    let receipt = &f.materialization.as_ref().unwrap().receipt;
    assert!(receipt.provider_request_id.is_none());
    assert!(receipt.returned_model.is_none());
    assert!(
        !serde_json::to_string(receipt)
            .unwrap()
            .contains("fixture-token")
    );
    assert!(
        std::fs::read(
            c.fs.root()
                .resolve(&f.spool.unwrap().response.path)
                .unwrap()
        )
        .unwrap()
        .is_empty()
    );
}
#[test]
fn source_change_during_helper_is_definitely_unentered_and_releases_only_that_attempt() {
    let path = Arc::new(Mutex::new(None::<std::path::PathBuf>));
    let copy = path.clone();
    let hook = Arc::new(move || {
        std::fs::write(copy.lock().unwrap().as_ref().unwrap(), b"foreign input").unwrap()
    });
    let c = case(
        "https://gateway.example/v1/embeddings",
        true,
        ExecutionPolicy::default(),
        |_| {},
        Some(hook),
    );
    *path.lock().unwrap() = Some(c.fs.root().path().join("inputs/task.json"));
    let mock = Mock::new(vec![]);
    let f = dispatch(&c, mock.clone())
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(f.error.code, ErrorCode::FreshnessConflict);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    let i = c.job.inspect().unwrap();
    assert_eq!(i.attempts[0].billing, BillingDisposition::ReleasedNotSent);
    assert_eq!(i.budget.dispatched_requests, 0);
}
#[test]
fn observed_response_over_bound_stops_and_keeps_actual_bytes_without_large_spool() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    let mock = Mock::new(vec![Ok(TransportReply {
        status: 200,
        headers: vec![],
        body: vec![],
        observed_body_bytes: 8 * 1024 * 1024 + 1,
        body_exceeded: true,
        headers_exceeded: false,
        terminal: false,
    })]);
    let f = dispatch(&c, mock)
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    let i = c.job.inspect().unwrap();
    assert!(!i.budget.guarantee_intact);
    assert_eq!(i.state, RunState::Paused);
    assert!(c.job.resume(None).is_err());
    let KnownOrUnknown::Known(usage) = &f.materialization.unwrap().receipt.usage else {
        panic!("actual bytes lost")
    };
    assert_eq!(usage.response_bytes, 8 * 1024 * 1024 + 1);
    assert!(
        std::fs::read(
            c.fs.root()
                .resolve(&f.spool.unwrap().response.path)
                .unwrap()
        )
        .unwrap()
        .is_empty()
    );
}

// Native loopback mocks never leave literal127.0.0.1. Fixture IO has finite socket deadlines.
type ServerReply = (u16, Vec<(String, String)>, Vec<u8>);
#[derive(Clone)]
enum ServerMode {
    Replies(Vec<ServerReply>),
    SlowHeaders,
    SlowBody,
    ParserHeaders,
    CloseAfterRequest,
    Informational,
}
struct Server {
    url: String,
    records: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn start(mode: ServerMode, tls: bool) -> Self {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let records = Arc::new(Mutex::new(vec![]));
        let saved = records.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let end = stop.clone();
        let tls_config = if tls {
            use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
            let cert = CertificateDer::pem_slice_iter(include_bytes!(
                "../../tests/fixtures/p16b/server.pem"
            ))
            .map(|c| c.unwrap())
            .collect::<Vec<_>>();
            let key = PrivateKeyDer::from_pem_slice(include_bytes!(
                "../../tests/fixtures/p16b/server-key.pem"
            ))
            .unwrap();
            Some(Arc::new(
                rustls::ServerConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(cert, key)
                .unwrap(),
            ))
        } else {
            None
        };
        let thread = std::thread::spawn(move || {
            let mut index = 0usize;
            while !end.load(Ordering::SeqCst) {
                let (socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_millis(300)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_millis(300)))
                    .unwrap();
                trait Socket: Read + Write {
                    fn close(&mut self) {}
                }
                impl Socket for std::net::TcpStream {}
                impl Socket for rustls::StreamOwned<rustls::ServerConnection, std::net::TcpStream> {
                    fn close(&mut self) {
                        self.conn.send_close_notify();
                        let _ = self.flush();
                    }
                }
                let mut stream: Box<dyn Socket> = if let Some(config) = &tls_config {
                    Box::new(rustls::StreamOwned::new(
                        rustls::ServerConnection::new(config.clone()).unwrap(),
                        socket,
                    ))
                } else {
                    Box::new(socket)
                };
                let mut request = vec![];
                let mut byte = [0u8; 1];
                let mut failed = false;
                while !request.ends_with(b"\r\n\r\n") {
                    if request.len() > 65536 || stream.read_exact(&mut byte).is_err() {
                        failed = true;
                        break;
                    }
                    request.push(byte[0]);
                }
                if failed {
                    continue;
                }
                let header = String::from_utf8_lossy(&request);
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if length > 256 * 1024 {
                    continue;
                }
                let mut body = vec![0u8; length];
                if stream.read_exact(&mut body).is_err() {
                    continue;
                }
                request.extend(body);
                saved.lock().unwrap().push(request);
                match &mode {
                    ServerMode::Replies(replies) => {
                        let (status, headers, body) = &replies[index.min(replies.len() - 1)];
                        index += 1;
                        let mut head = format!(
                            "HTTP/1.1 {status} Mock\r\nContent-Length: {}\r\nConnection: close\r\n",
                            body.len()
                        );
                        for (name, value) in headers {
                            head.push_str(&format!("{name}: {value}\r\n"));
                        }
                        head.push_str("\r\n");
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(body);
                        let _ = stream.flush();
                    }
                    ServerMode::SlowHeaders => {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nX-Slow: ");
                        for _ in 0..400 {
                            if end.load(Ordering::SeqCst) || stream.write_all(b"x").is_err() {
                                break;
                            }
                            let _ = stream.flush();
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    ServerMode::SlowBody => {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 400\r\n\r\n");
                        for _ in 0..400 {
                            if end.load(Ordering::SeqCst) || stream.write_all(b"x").is_err() {
                                break;
                            }
                            let _ = stream.flush();
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    ServerMode::ParserHeaders => {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nX-Excess: ");
                        // Leave the oversized header incomplete to exercise Hyper's partial-parser cap.
                        let _ = stream.write_all(&vec![b'x'; 450_000]);
                    }
                    ServerMode::Informational => {
                        for _ in 0..32 {
                            if end.load(Ordering::SeqCst) {
                                break;
                            }
                            let mut frame = b"HTTP/1.1 103 Early Hints\r\nX-Fixture: ".to_vec();
                            frame.extend(vec![b'x'; 100_000]);
                            frame.extend(b"\r\n\r\n");
                            if stream.write_all(&frame).is_err() {
                                break;
                            }
                            let _ = stream.flush();
                        }
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            GOOD.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(GOOD);
                        let _ = stream.flush();
                    }
                    ServerMode::CloseAfterRequest => {}
                }
                stream.close();
            }
        });
        Self {
            url: format!(
                "{}://{address}/configured/full?tenant=one%2Ftwo",
                if tls { "https" } else { "http" }
            ),
            records,
            stop,
            thread: Some(thread),
        }
    }
    fn count(&self) -> usize {
        self.records.lock().unwrap().len()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
fn native_case(server: &Server, tls: bool) -> Case {
    let ca = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/p16b/ca.pem");
    let extra = if tls {
        format!("ca_file={}", quote(ca.to_str().unwrap()))
    } else {
        String::new()
    };
    case_extra(
        &server.url,
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
        &extra,
    )
}
#[test]
fn full_url_no_appended_path_and_no_auth_redirect() {
    let target = Server::start(
        ServerMode::Replies(vec![(200, vec![], GOOD.to_vec())]),
        false,
    );
    let first = Server::start(
        ServerMode::Replies(vec![(
            302,
            vec![("Location".into(), target.url.clone())],
            b"SECRET REDIRECT BODY".to_vec(),
        )]),
        false,
    );
    let c = native_case(&first, false);
    let f = Dispatcher::native(c.fs.clone(), c.broker.clone())
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(first.count(), 1);
    assert_eq!(target.count(), 0);
    let records = first.records.lock().unwrap();
    let text = String::from_utf8_lossy(&records[0]);
    assert!(text.starts_with("POST /configured/full?tenant=one%2Ftwo HTTP/1.1\r\n"));
    assert!(
        text.to_ascii_lowercase()
            .contains("authorization: bearer fixture-token")
    );
    assert_eq!(f.disposition, DispatchDisposition::Rejected);
    assert!(c.job.inspect().unwrap().attempts[0].receipt.is_some());
}
#[test]
fn native_custom_ca_verifies_certificate_and_untrusted_tls_never_receives_auth() {
    let server = Server::start(
        ServerMode::Replies(vec![(200, vec![], GOOD.to_vec())]),
        true,
    );
    let c = native_case(&server, true);
    let out = ok(&Dispatcher::native(c.fs.clone(), c.broker.clone()), &c);
    assert_eq!(server.count(), 1);
    assert_eq!(out.attempt.number, 1);
    let untrusted = case(&server.url, false, ExecutionPolicy::default(), |_| {}, None);
    let f = Dispatcher::native(untrusted.fs.clone(), untrusted.broker.clone())
        .execute(
            &untrusted.job,
            &untrusted.trusted,
            &untrusted.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(server.count(), 1);
    assert_eq!(f.disposition, DispatchDisposition::OutcomeUnknown);
    assert_eq!(
        untrusted.job.inspect().unwrap().attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
}
#[test]
fn native_http_has_no_automatic_retry_after_reset() {
    let server = Server::start(ServerMode::CloseAfterRequest, false);
    let c = native_case(&server, false);
    let f = Dispatcher::native(c.fs.clone(), c.broker.clone())
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(server.count(), 1);
    assert_eq!(c.job.inspect().unwrap().budget.dispatched_requests, 1);
    assert_eq!(f.disposition, DispatchDisposition::OutcomeUnknown);
}
#[test]
fn native_slow_headers_body_and_parser_excess_return_within_deadline() {
    for (mode_index, mode) in [
        ServerMode::SlowHeaders,
        ServerMode::SlowBody,
        ServerMode::ParserHeaders,
    ]
    .into_iter()
    .enumerate()
    {
        let server = Server::start(mode, false);
        let c = native_case(&server, false);
        let started = Instant::now();
        let f = Dispatcher::native(c.fs.clone(), c.broker.clone())
            .execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(server.count(), 1);
        assert_eq!(
            f.disposition,
            DispatchDisposition::OutcomeUnknown,
            "native mode {mode_index}"
        );
        assert_eq!(
            c.job.inspect().unwrap().attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
    }
}
#[test]
fn native_compression_header_and_body_caps_never_decompress_or_activate() {
    for (headers, body) in [
        (
            vec![("Content-Encoding".into(), "gzip".into())],
            GOOD.to_vec(),
        ),
        (vec![("X-Excess".into(), "x".repeat(4097))], GOOD.to_vec()),
        (vec![], vec![b'x'; 8 * 1024 * 1024 + 1]),
    ] {
        let server = Server::start(ServerMode::Replies(vec![(200, headers, body)]), false);
        let c = native_case(&server, false);
        let f = Dispatcher::native(c.fs.clone(), c.broker.clone())
            .execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert_eq!(server.count(), 1);
        assert!(f.materialization.is_some());
        assert!(
            c.job
                .inspect()
                .unwrap()
                .tasks
                .values()
                .all(|t| t.outputs.is_empty())
        );
        assert!(
            std::fs::read(
                c.fs.root()
                    .resolve(&f.spool.unwrap().response.path)
                    .unwrap()
            )
            .unwrap()
            .is_empty()
        );
    }
}

#[test]
fn enormous_valid_retry_after_pauses_instead_of_retrying_early() {
    let now = ClockReading {
        utc_ms: 1_700_000_000_000,
        monotonic_ms: 10,
    };
    for value in ["18446744073709551615", "18446744073709551616"] {
        assert!(matches!(
            retry::decide(
                429,
                &[("Retry-After".into(), value.into())],
                1,
                now,
                now.utc_ms + 5000,
                1000,
                &ZeroJitter
            )
            .unwrap(),
            RetryDecision::Pause { .. }
        ));
    }
}

#[test]
fn pinned_hyper_buffer_growth_reclaims_informational_prefixes_with_bounded_overshoot() {
    // Hyper 1.11.1 io::poll_read_from_io reserves min(next, max-len), then
    // exposes all BytesMut spare capacity. Model that actual operation, including
    // whole-capacity reads and split/freeze/drop for non-retained 1xx headers.
    // This is a pinned allocation regression, not an assertion that max is an
    // exact preparse byte limit. Native transport has no informational callback.
    use bytes::BytesMut;
    const THRESHOLD: usize = 8192 + 4096 * 100;
    const BACKING_BOUND: usize = 4 * THRESHOLD;
    let mut greatest_capacity = 0usize;
    for fragment in [1, 17, 8191, 8192, 32768, 262144, usize::MAX] {
        let mut buf = BytesMut::with_capacity(0);
        let mut next = 8192usize;
        for turn in 0..256usize {
            if buf.len() >= THRESHOLD {
                // A partial header is rejected; a complete informational header
                // can instead be consumed before checking the threshold.
                let consume = if turn % 2 == 0 {
                    buf.len()
                } else {
                    buf.len() / 2
                };
                drop(buf.split_to(consume).freeze());
            }
            if buf.len() >= THRESHOLD {
                break;
            }
            let reserve = next.min(THRESHOLD - buf.len());
            if buf.capacity() - buf.len() < reserve {
                buf.reserve(reserve);
            }
            greatest_capacity = greatest_capacity.max(buf.capacity());
            assert!(buf.capacity() <= BACKING_BOUND);
            let read = fragment.min(buf.capacity() - buf.len());
            let length = buf.len() + read;
            buf.resize(length, 0);
            if read >= next {
                next = next.saturating_mul(2).min(THRESHOLD);
            }
            if turn % 3 != 0 && !buf.is_empty() {
                // Different prefix lengths leave both small and large offsets,
                // covering reserve's shift/reclaim and grow paths.
                let consume = if turn % 3 == 1 {
                    buf.len() / 2
                } else {
                    buf.len() - 1
                };
                drop(buf.split_to(consume).freeze());
            }
        }
    }
    // Adversarial pipeline: fill a large backing with a completed informational
    // prefix followed by almost M bytes of the next incomplete header. The small
    // consumed prefix cannot be reclaimed cheaply (offset < len), so reserve(1)
    // must exercise the doubled backing path, not only the initial overshoot.
    let mut pipelined = BytesMut::with_capacity(0);
    let mut next = 8192usize;
    while pipelined.len() < THRESHOLD {
        let reserve = next.min(THRESHOLD - pipelined.len());
        if pipelined.capacity() - pipelined.len() < reserve {
            pipelined.reserve(reserve);
        }
        let read = pipelined.capacity() - pipelined.len();
        pipelined.resize(pipelined.len() + read, 0);
        if read >= next {
            next = next.saturating_mul(2).min(THRESHOLD);
        }
    }
    let prefix = pipelined.len() - (THRESHOLD - 1);
    drop(pipelined.split_to(prefix).freeze());
    assert_eq!(pipelined.len(), THRESHOLD - 1);
    assert_eq!(pipelined.capacity(), pipelined.len());
    pipelined.reserve(1);
    assert!(pipelined.capacity() <= BACKING_BOUND);
    assert!(pipelined.capacity() > 2 * THRESHOLD);
    greatest_capacity = greatest_capacity.max(pipelined.capacity());
    assert!(greatest_capacity > THRESHOLD);
    eprintln!("pinned raw-buffer modeled largest exposed capacity: {greatest_capacity}");
}

#[test]
fn later_retry_refusal_keeps_prior_paid_attempt_spool_and_receipt_plan() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |spec| spec.limits.requests = 1,
        None,
    );
    let mock = Mock::new(vec![reply(503, GOOD)]);
    let f = dispatch(&c, mock.clone())
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.error.code, ErrorCode::BudgetExceeded);
    assert_eq!(f.disposition, DispatchDisposition::Rejected);
    let attempt = f.attempt.unwrap();
    assert_eq!(attempt.number, 1);
    assert!(f.spool.is_some());
    assert!(f.materialization.is_some());
    let paid: Vec<AttemptRef> =
        serde_json::from_value(f.error.details["retained_paid_attempts"].clone()).unwrap();
    assert_eq!(paid, vec![attempt]);
    let inspect = c.job.inspect().unwrap();
    assert_eq!(inspect.attempts[0].phase, AttemptPhase::Settled);
    assert_eq!(
        inspect.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
}

#[test]
fn failed_receipt_apply_keeps_actual_prepared_identity_and_never_retries() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    let cache = c.fs.root().path().join(".wiki/cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("index.sqlite"), b"not sqlite").unwrap();
    let mock = Mock::new(vec![reply(503, GOOD)]);
    let f = dispatch(&c, mock.clone())
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(f.error.code, ErrorCode::IndexCorrupt);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let change: RecordId =
        serde_json::from_value(f.error.details["receipt_change_id"].clone()).unwrap();
    let fingerprint: Blake3Hash =
        serde_json::from_value(f.error.details["receipt_manifest_hash"].clone()).unwrap();
    let engine = crate::changes::ChangeEngine::new(c.fs.clone()).unwrap();
    let retained = engine.inspect(&change).unwrap();
    assert_eq!(retained.prepared.manifest_hash, fingerprint);
    assert!(f.spool.is_some());
    assert!(f.materialization.is_some());
    assert_eq!(
        c.job.inspect().unwrap().attempts[0].phase,
        AttemptPhase::Received
    );
    let safe = serde_json::to_string(&f.error).unwrap();
    assert!(!safe.contains("fixture-token"));
}

#[test]
fn native_final_guard_rejects_expired_lease_changed_config_and_cancel_before_first_poll() {
    use super::credentials::CredentialContext;
    struct CancelAtFinal {
        clock: Arc<Clock>,
        cancel: CancellationToken,
        reads: AtomicUsize,
    }
    impl JobClock for CancelAtFinal {
        fn read(&self) -> Result<ClockReading> {
            // Native initial check + final closing check. Cancel
            // during the final read; bounded must refuse before polling HTTP.
            if self.reads.fetch_add(1, Ordering::SeqCst) == 1 {
                self.cancel.cancel();
            }
            self.clock.read()
        }
    }
    for variant in 0..3 {
        let server = Server::start(
            ServerMode::Replies(vec![(200, vec![], GOOD.to_vec())]),
            false,
        );
        let c = case(
            &server.url,
            variant == 0,
            ExecutionPolicy::default(),
            |_| {},
            None,
        );
        let prepared = fixture_prepare(
            &c.trusted,
            &c.spec.tasks[0],
            serde_json::from_slice(
                &std::fs::read(c.fs.root().resolve(&c.spec.tasks[0].input.path).unwrap()).unwrap(),
            )
            .unwrap(),
            ServiceRole::Embed,
            DispatchPurpose::Task,
            Capability::Embed,
        )
        .unwrap();
        let (_, _, _, options) = c.job.dispatcher_bindings();
        let context = CredentialContext {
            policy: options.policy,
            cancel: options.cancel.clone(),
            deadline_utc_ms: c.spec.deadline_utc_ms,
        };
        let reservation = c
            .job
            .reserve(&c.spec.tasks[0].key, prepared.bound.clone())
            .unwrap();
        let permit = c.job.dispatch_intent(reservation).unwrap();
        let lease = c.broker.resolve(&c.trusted, &c.fs, &context).unwrap();
        let authorization = c.job.begin_send(permit).unwrap();
        let attempt = authorization.attempt().clone();
        let clock: Arc<dyn JobClock> = if variant == 2 {
            Arc::new(CancelAtFinal {
                clock: c.clock.clone(),
                cancel: options.cancel.clone(),
                reads: AtomicUsize::new(0),
            })
        } else {
            c.clock.clone()
        };
        if variant == 0 {
            c.clock.utc.fetch_add(301000, Ordering::SeqCst);
            c.clock.mono.fetch_add(301000, Ordering::SeqCst);
        }
        if variant == 1 {
            let mut bytes = std::fs::read(&c.config).unwrap();
            bytes.extend(b"\n# foreign config edit\n");
            private(&c.config, bytes);
        }
        let request = AuthenticatedRequest {
            authorization,
            prepared: &prepared,
            lease,
            tls_ca: None,
            service: &c.trusted,
            fs: c.fs.clone(),
            broker: c.broker.clone(),
            credential_context: context,
        };
        let limits = TransportContext {
            clock,
            cancel: options.cancel.clone(),
            deadline_utc_ms: c.spec.deadline_utc_ms,
            timeout_ms: 1000,
            connect_timeout_ms: 1000,
            response_bytes: prepared.bound.response_bytes,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(NativeTransport.execute(request, limits))
            .err()
            .unwrap();
        assert!(error.not_entered, "variant {variant}");
        assert_eq!(error.observed_body_bytes, 0);
        assert_eq!(server.count(), 0);
        c.job
            .release_not_sent(
                &attempt,
                NotSentObservation {
                    reason: NotSentReason::TransportNotEntered,
                },
            )
            .unwrap();
        assert_eq!(
            c.job.inspect().unwrap().attempts[0].billing,
            BillingDisposition::ReleasedNotSent
        );
    }
}

#[test]
fn native_cancel_during_headers_or_body_retains_possible_charge_without_retry() {
    for mode in [ServerMode::SlowHeaders, ServerMode::SlowBody] {
        let server = Server::start(mode, false);
        let c = native_case(&server, false);
        let (_, _, _, options) = c.job.dispatcher_bindings();
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                let limit = Instant::now() + Duration::from_secs(2);
                while server.count() == 0 && Instant::now() < limit {
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(server.count(), 1);
                options.cancel.cancel();
            });
            Dispatcher::native(c.fs.clone(), c.broker.clone()).execute(
                &c.job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
        });
        let f = result.err().unwrap();
        assert_eq!(f.error.code, ErrorCode::Cancelled);
        assert_eq!(f.disposition, DispatchDisposition::OutcomeUnknown);
        assert_eq!(server.count(), 1);
        let state = c.job.inspect().unwrap();
        assert_eq!(state.state, RunState::Stopped);
        assert_eq!(state.budget.dispatched_requests, 1);
        assert_eq!(
            state.attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
    }
}

#[test]
fn cancel_retry_wait_keeps_committed_receipt_and_stops_without_next_reservation() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    let (_, _, _, options) = c.job.dispatcher_bindings();
    let mock = Mock::new(vec![Ok(TransportReply::new(
        503,
        vec![("Retry-After".into(), "2".into())],
        GOOD.to_vec(),
    )
    .unwrap())]);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            let until = Instant::now() + Duration::from_secs(4);
            while Instant::now() < until {
                let state = c.job.inspect().unwrap();
                if state
                    .attempts
                    .first()
                    .is_some_and(|a| a.phase == AttemptPhase::Settled)
                {
                    options.cancel.cancel();
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            panic!("receipt was not settled before wait cancellation");
        });
        dispatch(&c, mock.clone()).execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
    });
    let f = result.err().unwrap();
    assert_eq!(f.error.code, ErrorCode::Cancelled);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    assert!(f.spool.is_some());
    assert!(f.materialization.is_some());
    let state = c.job.inspect().unwrap();
    assert_eq!(state.state, RunState::Stopped);
    assert_eq!(state.budget.dispatched_requests, 1);
    assert_eq!(state.attempts[0].phase, AttemptPhase::Settled);
    assert_eq!(
        state.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
}

#[test]
#[ignore = "invoked explicitly by subprocess proxy test"]
fn native_proxy_child() {
    let url = std::env::var("LWIKI_DISPOSABLE_TARGET_URL").unwrap();
    let c = case(&url, false, ExecutionPolicy::default(), |_| {}, None);
    let out = ok(&Dispatcher::native(c.fs.clone(), c.broker.clone()), &c);
    assert_eq!(out.attempt.number, 1);
}

#[test]
fn native_ambient_proxy_variables_are_ignored_in_disposable_subprocess() {
    let target = Server::start(
        ServerMode::Replies(vec![(200, vec![], GOOD.to_vec())]),
        false,
    );
    let proxy = Server::start(
        ServerMode::Replies(vec![(403, vec![], b"not a proxy".to_vec())]),
        false,
    );
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "providers::dispatch_tests::native_proxy_child",
            "--ignored",
            "--nocapture",
        ])
        .env("LWIKI_DISPOSABLE_TARGET_URL", &target.url)
        .env("HTTP_PROXY", &proxy.url)
        .env("HTTPS_PROXY", &proxy.url)
        .env("ALL_PROXY", &proxy.url)
        .env("http_proxy", &proxy.url)
        .env("https_proxy", &proxy.url)
        .env("all_proxy", &proxy.url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child status {}, stderr {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(target.count(), 1);
    assert_eq!(proxy.count(), 0);
}

#[test]
fn native_repeated_informational_headers_preserve_final_response_and_one_send() {
    let server = Server::start(ServerMode::Informational, false);
    let c = native_case(&server, false);
    let out = ok(&Dispatcher::native(c.fs.clone(), c.broker.clone()), &c);
    assert_eq!(server.count(), 1);
    assert_eq!(out.attempt.number, 1);
    let state = c.job.inspect().unwrap();
    assert_eq!(state.budget.dispatched_requests, 1);
    assert_eq!(state.attempts[0].phase, AttemptPhase::Received);
}

#[test]
fn concurrent_dispatchers_share_the_last_request_slot_without_extra_credentials_or_send() {
    struct LastSlot {
        released: std::sync::atomic::AtomicBool,
        calls: AtomicUsize,
    }
    impl Transport for LastSlot {
        fn execute<'a>(
            &'a self,
            _: AuthenticatedRequest<'a>,
            _: TransportContext,
        ) -> TransportFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                while !self.released.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                TransportReply::new(200, vec![], GOOD.to_vec())
                    .map_err(|_| TransportFailure::new(TransportFailureCode::InvalidResponse))
            })
        }
    }
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |spec| {
            spec.limits.requests = 1;
            let mut second = spec.tasks[0].clone();
            second.input.path = VaultRelativePath::new("inputs/task2.json").unwrap();
            spec.tasks.push(second);
        },
        None,
    );
    let transport = Arc::new(LastSlot {
        released: std::sync::atomic::AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let barrier = std::sync::Barrier::new(3);
    let results = std::thread::scope(|scope| {
        let spawn = |task_index: usize| {
            let c = &c;
            let transport = &transport;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                let result = Dispatcher::new(
                    c.fs.clone(),
                    DispatchOptions {
                        broker: c.broker.clone(),
                        transport: transport.clone(),
                        jitter: Arc::new(ZeroJitter),
                    },
                )
                .execute(
                    &c.job,
                    &c.trusted,
                    &c.spec.tasks[task_index].key,
                    DispatchPurpose::Task,
                );
                transport.released.store(true, Ordering::SeqCst);
                result
            })
        };
        let first = spawn(0);
        let second = spawn(1);
        barrier.wait();
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r
                .as_ref()
                .err()
                .is_some_and(|f| f.error.code == ErrorCode::BudgetExceeded))
            .count(),
        1
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert_eq!(c.inputs.0.load(Ordering::SeqCst), 1);
    assert_eq!(c.job.inspect().unwrap().budget.dispatched_requests, 1);
}

#[test]
fn response_and_receipt_fault_boundaries_replay_without_transport_or_refund() {
    struct Once {
        point: LedgerCheckpoint,
        fired: std::sync::atomic::AtomicBool,
    }
    impl LedgerFault for Once {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            if point == self.point && !self.fired.swap(true, Ordering::SeqCst) {
                return Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected local durability interruption",
                ));
            }
            Ok(())
        }
    }
    let points = [
        LedgerCheckpoint::AfterSpoolBytesSync,
        LedgerCheckpoint::AfterSpoolMetadataSync,
        LedgerCheckpoint::BeforeReceived,
        LedgerCheckpoint::AfterReceived,
        LedgerCheckpoint::BeforeOutputsCommitted,
        LedgerCheckpoint::AfterOutputsCommitted,
        LedgerCheckpoint::BeforeSettlement,
        LedgerCheckpoint::AfterSettlement,
    ];
    for point in points {
        let c = case(
            "https://gateway.example/v1/embeddings",
            false,
            ExecutionPolicy::default(),
            |_| {},
            None,
        );
        let (_, vault, run, mut options) = c.job.dispatcher_bindings();
        let fault = Arc::new(Once {
            point,
            fired: std::sync::atomic::AtomicBool::new(false),
        });
        options.fault = Some(fault.clone());
        let ledger =
            JobLedger::new(c.fs.clone(), vault.clone(), run.clone(), options.clone()).unwrap();
        let mock = Mock::new(vec![reply(401, GOOD)]);
        let f = dispatch(&c, mock.clone())
            .execute(
                &ledger,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert!(fault.fired.load(Ordering::SeqCst), "point {point:?}");
        assert_ne!(
            f.disposition,
            DispatchDisposition::NotSent,
            "point {point:?}"
        );
        assert!(f.attempt.is_some());
        options.fault = None;
        let reopened = JobLedger::new(c.fs.clone(), vault, run, options).unwrap();
        let first = reopened.replay().unwrap();
        let second = reopened.replay().unwrap();
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(first.inspection.budget.dispatched_requests, 1);
        assert_eq!(second.inspection.budget.dispatched_requests, 1);
        assert_eq!(first.inspection.attempts.len(), 1);
        assert_eq!(
            second.inspection.attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
        eprintln!("dispatcher durable fault replay: {point:?}");
    }
}

#[cfg(unix)]
#[test]
#[ignore = "invoked explicitly by native SIGKILL wrapper"]
fn native_interruption_child() {
    fn publish_ready(ready: &std::path::Path, root: &std::path::Path) {
        let partial = ready.with_extension("partial");
        std::fs::write(&partial, root.to_str().unwrap()).unwrap();
        std::fs::rename(partial, ready).unwrap();
    }
    struct KillPoint {
        point: String,
        ready: std::path::PathBuf,
        root: std::path::PathBuf,
        appends: AtomicUsize,
    }
    impl LedgerFault for KillPoint {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            let authorized = point == LedgerCheckpoint::AfterJournalSync
                && self.appends.fetch_add(1, Ordering::SeqCst) == 2;
            if (self.point == "authorized" && authorized) || self.point == format!("{point:?}") {
                publish_ready(&self.ready, &self.root);
                loop {
                    std::thread::park_timeout(Duration::from_secs(1));
                }
            }
            Ok(())
        }
    }
    let url = std::env::var("LWIKI_DISPOSABLE_TARGET_URL").unwrap();
    let ready = std::path::PathBuf::from(std::env::var("LWIKI_DISPOSABLE_READY").unwrap());
    let point = std::env::var("LWIKI_DISPOSABLE_KILL_POINT").unwrap();
    let c = case(&url, false, ExecutionPolicy::default(), |_| {}, None);
    let (_, vault, run, mut options) = c.job.dispatcher_bindings();
    options.fault = Some(Arc::new(KillPoint {
        point: point.clone(),
        ready: ready.clone(),
        root: c.fs.root().path().to_path_buf(),
        appends: AtomicUsize::new(0),
    }));
    let ledger = JobLedger::new(c.fs.clone(), vault, run, options).unwrap();
    if point == "body" {
        publish_ready(&ready, c.fs.root().path());
    }
    let _ = Dispatcher::native(c.fs.clone(), c.broker.clone()).execute(
        &ledger,
        &c.trusted,
        &c.spec.tasks[0].key,
        DispatchPurpose::Task,
    );
    panic!("native child failed to stop at requested checkpoint");
}

#[cfg(unix)]
#[test]
fn native_sigkill_after_authorization_body_spool_and_receipt_replays_without_resend() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for point in [
        "authorized",
        "body",
        "AfterSpoolBytesSync",
        "AfterSpoolMetadataSync",
        "AfterReceived",
        "AfterOutputsCommitted",
        "AfterSettlement",
    ] {
        let holder = tempfile::tempdir().unwrap();
        let ready = holder.path().join("ready");
        let server = Server::start(
            if point == "body" {
                ServerMode::SlowBody
            } else {
                ServerMode::Replies(vec![(401, vec![], GOOD.to_vec())])
            },
            false,
        );
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "providers::dispatch_tests::native_interruption_child",
                "--ignored",
                "--nocapture",
            ])
            .env("LWIKI_DISPOSABLE_TARGET_URL", &server.url)
            .env("LWIKI_DISPOSABLE_READY", &ready)
            .env("LWIKI_DISPOSABLE_KILL_POINT", point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut guard = ChildGuard(child);
        let child = &mut guard.0;
        let until = Instant::now() + Duration::from_secs(10);
        while !ready.exists() || (point == "body" && server.count() == 0) {
            assert!(Instant::now() < until, "child did not reach {point}");
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before {point}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let path = std::path::PathBuf::from(std::fs::read_to_string(&ready).unwrap());
        child.kill().unwrap();
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9));
        let fs = crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&path).unwrap());
        let clock = Arc::new(Clock::new());
        clock.mono.store(1_000_000, Ordering::SeqCst);
        let options = JobOptions {
            clock,
            fault: None,
            cancel: CancellationToken::default(),
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        };
        let ledger = JobLedger::new(fs, id("vault_test"), id("run_dispatch"), options).unwrap();
        let before = server.count();
        let replay = ledger.replay().unwrap();
        let second = ledger.replay().unwrap();
        assert_eq!(server.count(), before);
        assert_eq!(before, usize::from(point != "authorized"));
        assert_eq!(replay.inspection.budget.dispatched_requests, 1);
        assert_eq!(second.inspection.budget.dispatched_requests, 1);
        assert_eq!(
            second.inspection.attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
        assert_eq!(path.file_name().unwrap(), "vault");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        eprintln!("dispatcher native SIGKILL/replay: {point}");
    }
}

#[test]
fn native_sealed_closing_clock_and_rate_quote_refuse_late_entry_and_survive_reopen() {
    use super::credentials::CredentialContext;
    for variant in 0..3 {
        let server = Server::start(
            ServerMode::Replies(vec![(200, vec![], GOOD.to_vec())]),
            false,
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let validity = if variant == 2 {
            PriceValidity::EntireAttempt {
                valid_from_utc_ms: now - 10000,
                valid_until_utc_ms: now + 30000,
            }
        } else {
            PriceValidity::DispatchLocked {
                valid_from_utc_ms: now - 10000,
                valid_until_utc_ms: now + 5000,
            }
        };
        let mut card = RateCard {
            id: "fixture-rate-v2".into(),
            version: 2,
            fingerprint: hash([]),
            currency: Currency::new("USD").unwrap(),
            validity,
            request_fee_nanounits: 0,
            rates: std::collections::BTreeMap::new(),
        };
        card.fingerprint = crate::jobs::budgets::rate_card_fingerprint(&card).unwrap();
        let (pricing, until) = if variant == 2 {
            ("entire_attempt", now + 30000)
        } else {
            ("dispatch_locked", now + 5000)
        };
        let extra = if variant == 0 {
            "timeout_seconds=20".to_owned()
        } else {
            format!(
                "timeout_seconds=20\nrate_card={{id=\"fixture-rate-v2\",version=2,fingerprint={},currency=\"USD\",validity={{pricing={},valid_from_utc_ms={},valid_until_utc_ms={}}},request_fee_nanounits=0,rates={{}}}}",
                quote(card.fingerprint.as_str()),
                quote(pricing),
                now - 10000,
                until
            )
        };
        let c = case_extra(
            &server.url,
            true,
            ExecutionPolicy::default(),
            |_| {},
            None,
            &extra,
        );
        let prepared = fixture_prepare(
            &c.trusted,
            &c.spec.tasks[0],
            serde_json::from_slice(
                &std::fs::read(c.fs.root().resolve(&c.spec.tasks[0].input.path).unwrap()).unwrap(),
            )
            .unwrap(),
            ServiceRole::Embed,
            DispatchPurpose::Task,
            Capability::Embed,
        )
        .unwrap();
        let (_, vault, run, options) = c.job.dispatcher_bindings();
        let context = CredentialContext {
            policy: options.policy,
            cancel: options.cancel.clone(),
            deadline_utc_ms: c.spec.deadline_utc_ms,
        };
        let reservation = c
            .job
            .reserve(&c.spec.tasks[0].key, prepared.bound.clone())
            .unwrap();
        let permit = c.job.dispatch_intent(reservation).unwrap();
        let lease = c.broker.resolve(&c.trusted, &c.fs, &context).unwrap();
        // Move closing proof forward after acquisition, then regress to a
        // reading still later than credential acquisition. Old native Timer
        // restarted its baseline and would accept that regression.
        if variant == 0 {
            c.clock.utc.store(5000, Ordering::SeqCst);
            c.clock.mono.store(5000, Ordering::SeqCst);
        }
        let authorization = c.job.begin_send(permit).unwrap();
        let attempt = authorization.attempt().clone();
        let shift = if variant == 0 {
            0
        } else if variant == 1 {
            7000
        } else {
            15000
        };
        c.clock.utc.store(shift, Ordering::SeqCst);
        c.clock.mono.store(shift as u64, Ordering::SeqCst);
        // Lease remains valid: the final failure is the sealed clock or pricing
        // interval, not credential expiry or the 20-second HTTP timeout.
        lease.check_validity().unwrap();
        let request = AuthenticatedRequest {
            authorization,
            prepared: &prepared,
            lease,
            tls_ca: None,
            service: &c.trusted,
            fs: c.fs.clone(),
            broker: c.broker.clone(),
            credential_context: context,
        };
        let transport_context = TransportContext {
            clock: options.clock.clone(),
            cancel: options.cancel.clone(),
            deadline_utc_ms: c.spec.deadline_utc_ms,
            timeout_ms: prepared.bound.timeout_ms,
            connect_timeout_ms: 1000,
            response_bytes: prepared.bound.response_bytes,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(NativeTransport.execute(request, transport_context))
            .err()
            .unwrap();
        assert!(error.not_entered, "variant {variant}");
        assert_eq!(server.count(), 0);
        // Restore monotonic forward progress before durably acknowledging the
        // native proof of no entry. Never release by inspecting error text.
        if variant == 0 {
            c.clock.utc.store(6000, Ordering::SeqCst);
            c.clock.mono.store(6000, Ordering::SeqCst);
        }
        c.job
            .release_not_sent(
                &attempt,
                NotSentObservation {
                    reason: NotSentReason::TransportNotEntered,
                },
            )
            .unwrap();
        let reopened = JobLedger::new(c.fs.clone(), vault, run, options).unwrap();
        assert_eq!(
            reopened.replay().unwrap().inspection.attempts[0].billing,
            BillingDisposition::ReleasedNotSent
        );
        assert_eq!(server.count(), 0);
    }
}

#[test]
fn native_validity_change_after_actual_entry_keeps_paid_exposure_without_refund() {
    struct OneRegression {
        clock: Arc<Clock>,
        trigger: std::sync::atomic::AtomicBool,
    }
    impl JobClock for OneRegression {
        fn read(&self) -> Result<ClockReading> {
            let mut now = self.clock.read()?;
            if self.trigger.swap(false, Ordering::SeqCst) {
                now.utc_ms -= 2000;
                now.monotonic_ms = now.monotonic_ms.saturating_sub(2000);
            }
            Ok(now)
        }
    }
    for variant in 0..2 {
        let server = Server::start(ServerMode::SlowBody, false);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut card = RateCard {
            id: "fixture-locked".into(),
            version: 2,
            fingerprint: hash([]),
            currency: Currency::new("USD").unwrap(),
            validity: PriceValidity::DispatchLocked {
                valid_from_utc_ms: now - 10000,
                valid_until_utc_ms: now + 5000,
            },
            request_fee_nanounits: 0,
            rates: std::collections::BTreeMap::new(),
        };
        card.fingerprint = crate::jobs::budgets::rate_card_fingerprint(&card).unwrap();
        let extra = if variant == 0 {
            "timeout_seconds=20".to_owned()
        } else {
            format!(
                "timeout_seconds=20\nrate_card={{id=\"fixture-locked\",version=2,fingerprint={},currency=\"USD\",validity={{pricing=\"dispatch_locked\",valid_from_utc_ms={},valid_until_utc_ms={}}},request_fee_nanounits=0,rates={{}}}}",
                quote(card.fingerprint.as_str()),
                now - 10000,
                now + 5000
            )
        };
        let c = case_extra(
            &server.url,
            false,
            ExecutionPolicy::default(),
            |_| {},
            None,
            &extra,
        );
        let (_, vault, run, mut options) = c.job.dispatcher_bindings();
        let clock = Arc::new(OneRegression {
            clock: c.clock.clone(),
            trigger: std::sync::atomic::AtomicBool::new(false),
        });
        if variant == 0 {
            options.clock = clock.clone();
        }
        let ledger = JobLedger::new(c.fs.clone(), vault, run, options).unwrap();
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                let until = Instant::now() + Duration::from_secs(4);
                while server.count() == 0 && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(server.count(), 1);
                if variant == 0 {
                    clock.trigger.store(true, Ordering::SeqCst);
                } else {
                    c.clock.utc.store(7000, Ordering::SeqCst);
                    c.clock.mono.store(7000, Ordering::SeqCst);
                }
            });
            Dispatcher::native(c.fs.clone(), c.broker.clone()).execute(
                &ledger,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
        });
        let f = result.err().unwrap();
        assert_ne!(f.disposition, DispatchDisposition::NotSent);
        assert_eq!(server.count(), 1);
        let state = ledger.replay().unwrap().inspection;
        assert_eq!(state.budget.dispatched_requests, 1);
        assert_eq!(
            state.attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
        if variant == 0 {
            assert_eq!(f.disposition, DispatchDisposition::OutcomeUnknown);
        } else {
            assert!(state.attempts[0].receipt.is_some());
        }
    }
}

#[test]
fn native_live_timer_floor_refuses_owned_sample_regression_in_either_component() {
    use super::credentials::CredentialContext;
    struct ControlledClock(Mutex<ClockReading>);
    impl JobClock for ControlledClock {
        fn read(&self) -> Result<ClockReading> {
            Ok(*self.0.lock().unwrap())
        }
    }
    struct AheadClock {
        owned: Arc<ControlledClock>,
        utc: bool,
    }
    impl JobClock for AheadClock {
        fn read(&self) -> Result<ClockReading> {
            let mut reading = self.owned.read()?;
            if self.utc {
                reading.utc_ms += 50;
            } else {
                reading.monotonic_ms += 50;
            }
            Ok(reading)
        }
    }
    for utc in [true, false] {
        let server = Server::start(
            ServerMode::Replies(vec![(200, vec![], GOOD.to_vec())]),
            false,
        );
        let c = native_case(&server, false);
        let prepared = fixture_prepare(
            &c.trusted,
            &c.spec.tasks[0],
            serde_json::from_slice(
                &std::fs::read(c.fs.root().resolve(&c.spec.tasks[0].input.path).unwrap()).unwrap(),
            )
            .unwrap(),
            ServiceRole::Embed,
            DispatchPurpose::Task,
            Capability::Embed,
        )
        .unwrap();
        let (_, vault, run, mut options) = c.job.dispatcher_bindings();
        let base = c.clock.read().unwrap();
        let closing = ClockReading {
            utc_ms: base.utc_ms + 100,
            monotonic_ms: base.monotonic_ms + 100,
        };
        let owned = Arc::new(ControlledClock(Mutex::new(closing)));
        options.clock = owned.clone();
        let job =
            JobLedger::new(c.fs.clone(), vault.clone(), run.clone(), options.clone()).unwrap();
        let credential_context = CredentialContext {
            policy: options.policy,
            cancel: options.cancel.clone(),
            deadline_utc_ms: c.spec.deadline_utc_ms,
        };
        let reservation = job
            .reserve(&c.spec.tasks[0].key, prepared.bound.clone())
            .unwrap();
        let permit = job.dispatch_intent(reservation).unwrap();
        let lease = c
            .broker
            .resolve(&c.trusted, &c.fs, &credential_context)
            .unwrap();
        let authorization = job.begin_send(permit).unwrap();
        assert_eq!(authorization.closing_reading().utc_ms, closing.utc_ms);
        assert_eq!(
            authorization.closing_reading().monotonic_ms,
            closing.monotonic_ms
        );
        let attempt = authorization.attempt().clone();
        let mut current = closing;
        if utc {
            current.utc_ms += 50;
        } else {
            current.monotonic_ms += 50;
        }
        *owned.0.lock().unwrap() = current;
        // Count-free interleaving model: the transport has observed T+200,
        // while the authoritative sample is T+150 and its sealed floor T+100.
        // Only the live componentwise floor prevents the old successful send.
        let transport_context = TransportContext {
            clock: Arc::new(AheadClock {
                owned: owned.clone(),
                utc,
            }),
            cancel: options.cancel.clone(),
            deadline_utc_ms: c.spec.deadline_utc_ms,
            timeout_ms: prepared.bound.timeout_ms,
            connect_timeout_ms: 1000,
            response_bytes: prepared.bound.response_bytes,
        };
        let request = AuthenticatedRequest {
            authorization,
            prepared: &prepared,
            lease,
            tls_ca: None,
            service: &c.trusted,
            fs: c.fs.clone(),
            broker: c.broker.clone(),
            credential_context,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(NativeTransport.execute(request, transport_context))
            .err()
            .unwrap();
        assert!(error.not_entered);
        assert_eq!(error.observed_body_bytes, 0);
        assert_eq!(server.count(), 0);
        *owned.0.lock().unwrap() = ClockReading {
            utc_ms: closing.utc_ms + 200,
            monotonic_ms: closing.monotonic_ms + 200,
        };
        job.release_not_sent(
            &attempt,
            NotSentObservation {
                reason: NotSentReason::TransportNotEntered,
            },
        )
        .unwrap();
        let reopened = JobLedger::new(c.fs.clone(), vault, run, options).unwrap();
        assert_eq!(
            reopened.replay().unwrap().inspection.attempts[0].billing,
            BillingDisposition::ReleasedNotSent
        );
        assert_eq!(server.count(), 0);
    }
}

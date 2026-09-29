use lwiki as library;
#[path = "fixtures/p16b/common.rs"]
mod common;
use common::*;
use lwiki::{
    domain::ErrorCode,
    jobs::*,
    providers::{dispatcher::Dispatcher, types::*},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct NeverTransport(AtomicUsize);
impl Transport for NeverTransport {
    fn execute<'a>(
        &'a self,
        _: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(TransportFailure::new(TransportFailureCode::Unavailable)) })
    }
}
#[test]
fn offline_dry_run_before_secret_resolution_zero_dns_http_helpers() {
    for policy in [
        ExecutionPolicy {
            offline: true,
            ..ExecutionPolicy::default()
        },
        ExecutionPolicy {
            dry_run: true,
            ..ExecutionPolicy::default()
        },
    ] {
        // Bootstrap explicitly online, then reopen with the prohibited execution policy.
        let c = case(
            "https://gateway.example/v1/embeddings",
            true,
            ExecutionPolicy::default(),
            |_| {},
            None,
        );
        let job = JobLedger::new(
            c.fs.clone(),
            c.spec.vault_id.clone(),
            c.spec.run_id.clone(),
            JobOptions {
                clock: c.clock.clone(),
                fault: None,
                cancel: CancellationToken::default(),
                policy,
                lock_timeout_ms: 5000,
            },
        )
        .unwrap();
        let transport = Arc::new(NeverTransport(AtomicUsize::new(0)));
        let d = Dispatcher::new(
            c.fs.clone(),
            DispatchOptions {
                broker: c.broker.clone(),
                transport: transport.clone(),
                jitter: Arc::new(ZeroJitter),
            },
        );
        let before = tree(c.temp.path());
        let f = d
            .execute(
                &job,
                &c.trusted,
                &c.spec.tasks[0].key,
                DispatchPurpose::Task,
            )
            .err()
            .unwrap();
        assert_eq!(f.error.code, ErrorCode::OfflineUnavailable);
        assert_eq!(transport.0.load(Ordering::SeqCst), 0);
        assert_eq!(c.runner.calls.load(Ordering::SeqCst), 0);
        assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
        assert_eq!(tree(c.temp.path()), before);
    }
}
#[test]
fn noncanonical_retained_descriptor_refuses_before_auth_or_reservation() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    let bytes = std::fs::read(c.fs.root().path().join("inputs/task.json")).unwrap();
    let input: RemoteInput = serde_json::from_slice(&bytes).unwrap();
    assert_ne!(bytes, lwiki::graph::packet::canonical_json(&input).unwrap());
    let transport = Arc::new(NeverTransport(AtomicUsize::new(0)));
    let d = Dispatcher::new(
        c.fs.clone(),
        DispatchOptions {
            broker: c.broker.clone(),
            transport: transport.clone(),
            jitter: Arc::new(ZeroJitter),
        },
    );
    let before = tree(c.temp.path());
    let f = d
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(f.error.code, ErrorCode::RecordInvalid);
    assert_eq!(transport.0.load(Ordering::SeqCst), 0);
    assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
    assert_eq!(c.runner.calls.load(Ordering::SeqCst), 0);
    assert_eq!(tree(c.temp.path()), before);
    assert!(c.job.inspect().unwrap().attempts.is_empty());
}
#[test]
fn malformed_retained_input_never_uses_caller_hash_or_transport() {
    let c = case(
        "https://gateway.example/v1/embeddings",
        false,
        ExecutionPolicy::default(),
        |_| {},
        None,
    );
    std::fs::write(
        c.fs.root().path().join("inputs/task.json"),
        b"forged payload",
    )
    .unwrap();
    let d = Dispatcher::native(c.fs.clone(), c.broker.clone());
    let f = d
        .execute(
            &c.job,
            &c.trusted,
            &c.spec.tasks[0].key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(f.error.code, ErrorCode::FreshnessConflict);
    assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
    assert!(c.job.inspect().unwrap().attempts.is_empty());
}
#[test]
fn bounded_mock_reply_constructor_and_errors_do_not_echo_body() {
    assert!(TransportReply::new(200, vec![("Bad Name".into(), "secret".into())], vec![]).is_err());
    assert!(TransportReply::new(200, vec![], vec![1; 8 * 1024 * 1024 + 1]).is_err());
    assert!(TransportReply::new(200, vec![("X-Test".into(), "x".repeat(4097))], vec![]).is_err());
}

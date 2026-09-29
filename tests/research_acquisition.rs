use lwiki as library;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod common;
use common::{Clock, Inputs, Runner, ZeroJitter, hash, id, private, quote};
use lwiki::{
    config::providers::ProviderConfig,
    domain::*,
    graph::packet::canonical_json,
    jobs::*,
    providers::{
        credentials::{CredentialBroker, CredentialOptions},
        dispatcher::Dispatcher,
        public_fetch::*,
        search_wire,
        types::*,
    },
    research::acquire::{self, AcquisitionResult},
    sources::{SourceStore, web_normalize::WebGap},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    net::IpAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    job: JobLedger,
    dispatcher: Dispatcher,
    inputs: Arc<Inputs>,
    clock: Arc<Clock>,
}
fn fixture(edit: impl FnOnce(&mut RunSpec)) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("WIKI.md"),"---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Acquisition fixture\n---\n").unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(&root).unwrap());
    let clock = Arc::new(Clock::new());
    let now = clock.read().unwrap().utc_ms;
    let mut spec = RunSpec {
        version: 1,
        run_id: id("run_acquire"),
        vault_id: id("vault_test"),
        title: "Acquisition fixture".into(),
        created_at_utc_ms: now,
        deadline_utc_ms: now + 900_000,
        scope: RunScope {
            operation: "research".into(),
            question: Some("fixture".into()),
            exclusions: vec![],
            source_snapshot: None,
            input_records: vec![],
            read_preconditions: vec![],
            profile_fingerprints: BTreeMap::from([(PUBLIC_PROFILE.into(), settings_fingerprint())]),
            scope_payload_hash: None,
        },
        config_fingerprint: hash("no provider config"),
        input_fingerprint: hash([]),
        limits: LifetimeLimits::default(),
        tasks: vec![],
        prior_accounting: PriorAccounting::None,
    };
    edit(&mut spec);
    spec.input_fingerprint = lwiki::jobs::tasks::input_fingerprint(&spec).unwrap();
    let job = JobLedger::new(
        fs.clone(),
        id("vault_test"),
        spec.run_id.clone(),
        JobOptions {
            clock: clock.clone(),
            cancel: CancellationToken::default(),
            fault: None,
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        },
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    job.create(&writer, spec).unwrap();
    drop(writer);
    job.start().unwrap();
    let inputs = Arc::new(Inputs(AtomicUsize::new(0)));
    let broker = Arc::new(CredentialBroker::new(CredentialOptions {
        clock: clock.clone(),
        inputs: inputs.clone(),
        runner: Arc::new(Runner {
            calls: AtomicUsize::new(0),
            hook: None,
        }),
    }));
    let dispatcher = Dispatcher::native(fs.clone(), broker);
    Fixture {
        _temp: temp,
        fs,
        job,
        dispatcher,
        inputs,
        clock,
    }
}
struct Resolver {
    answers: Mutex<VecDeque<Vec<IpAddr>>>,
    calls: AtomicUsize,
}
impl Resolver {
    fn new(answers: Vec<Vec<&str>>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(
                answers
                    .into_iter()
                    .map(|v| v.into_iter().map(|s| s.parse().unwrap()).collect())
                    .collect(),
            ),
            calls: AtomicUsize::new(0),
        })
    }
}
impl PublicResolver for Resolver {
    fn resolve<'a>(&'a self, _: &'a str, _: u16) -> ResolveFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .expect("unplanned DNS"))
        })
    }
}
struct Connector {
    replies: Mutex<VecDeque<PublicReply>>,
    requests: Mutex<Vec<(String, String, Vec<std::net::SocketAddr>)>>,
    calls: AtomicUsize,
    hook: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl Connector {
    fn new(replies: Vec<PublicReply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
            hook: None,
        })
    }
}
impl PublicConnector for Connector {
    fn connect<'a>(&'a self, request: PinnedRequest) -> Result<ConnectionFuture<'a>> {
        Ok(Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(request.addresses().iter().all(|a| public_ip(a.ip())));
            assert_eq!(
                request.tls_hostname(),
                url::Url::parse(request.url()).unwrap().host_str().unwrap()
            );
            self.requests.lock().unwrap().push((
                request.url().into(),
                request.tls_hostname().into(),
                request.addresses().to_vec(),
            ));
            if let Some(hook) = &self.hook {
                hook();
            }
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unplanned public GET"))
        }))
    }
}
fn reply(status: u16, media: &str, body: &[u8]) -> PublicReply {
    PublicReply::new(
        status,
        vec![("Content-Type".into(), media.into())],
        body.to_vec(),
    )
}
fn options(resolver: Arc<Resolver>, connector: Arc<Connector>) -> PublicFetchOptions {
    PublicFetchOptions {
        resolver,
        connector,
    }
}
fn task(f: &Fixture, url: &str, limits: FetchLimits) -> Blake3Hash {
    let t = acquire::plan_task(&f.fs, url, limits, vec![]).unwrap();
    let key = t.key.clone();
    f.job.add_tasks(vec![t]).unwrap();
    key
}
fn captured(result: AcquisitionResult) -> acquire::CapturedSource {
    match result {
        AcquisitionResult::Captured(source) => *source,
        _ => panic!("expected capture"),
    }
}
fn raw_content(
    f: &Fixture,
    source: &acquire::CapturedSource,
) -> (Vec<u8>, Option<Vec<u8>>, Vec<u8>) {
    let base = format!(
        "sources/{}/revisions/{}",
        source.source_id, source.revision_id
    );
    let read = |name: &str| std::fs::read(f.fs.root().path().join(format!("{base}/{name}")));
    (
        read("original.bin").unwrap(),
        read("content.md").ok(),
        read("revision.md").unwrap(),
    )
}

#[test]
fn explicit_url_without_search_capability() {
    let f = fixture(|s| s.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 100)));
    let dns = Resolver::new(vec![vec!["93.184.216.34"]]);
    let connector = Connector::new(vec![reply(
        200,
        "text/plain; charset=utf-8",
        b"Exact public evidence\n",
    )]);
    let source = captured(
        acquire::fetch_url(
            &f.fs,
            &f.dispatcher,
            &f.job,
            "https://example.org/article?q=one&q=two#heading",
            FetchLimits::default(),
            &options(dns.clone(), connector.clone()),
        )
        .unwrap(),
    );
    assert_eq!(raw_content(&f, &source).0, b"Exact public evidence\n");
    assert_eq!(
        source.provenance.observed_url,
        "https://example.org/article?q=one&q=two#heading"
    );
    assert_eq!(
        source.provenance.final_url,
        "https://example.org/article?q=one&q=two"
    );
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(dns.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.inputs.0.load(Ordering::SeqCst), 0);
    let i = f.job.inspect().unwrap();
    assert_eq!(i.budget.dispatched_requests, 1);
    assert_eq!(i.attempts[0].billing, BillingDisposition::KnownSettled);
    assert_eq!(i.budget.known_costs.values().sum::<u64>(), 0);
    assert!(i.budget.guarantee_intact);
}

#[test]
fn dns_rebind_private_redirect_ipv4_mapped_ipv6_rejected() {
    for answers in [
        vec!["127.0.0.1"],
        vec!["93.184.216.34", "10.0.0.1"],
        vec!["::ffff:93.184.216.34"],
        vec!["2001:db8::1"],
        vec!["169.254.1.1"],
    ] {
        let f = fixture(|_| {});
        let connector = Connector::new(vec![]);
        let key = task(&f, "https://example.org/", FetchLimits::default());
        assert!(
            f.dispatcher
                .execute_public(
                    &f.job,
                    &key,
                    &options(Resolver::new(vec![answers]), connector.clone())
                )
                .is_err()
        );
        assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
        assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 0);
    }
    let f = fixture(|_| {});
    let dns = Resolver::new(vec![vec!["93.184.216.34"], vec!["10.0.0.1"]]);
    let connector = Connector::new(vec![PublicReply::new(
        302,
        vec![("location".into(), "https://example.org/next".into())],
        b"redirect raw".to_vec(),
    )]);
    assert!(
        acquire::fetch_url(
            &f.fs,
            &f.dispatcher,
            &f.job,
            "https://example.org/",
            FetchLimits::default(),
            &options(dns.clone(), connector.clone())
        )
        .is_err()
    );
    assert_eq!(dns.calls.load(Ordering::SeqCst), 2);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
    for url in [
        "https://127.0.0.1/",
        "https://[::ffff:8.8.8.8]/",
        "https://[2002:0808:0808::]/",
    ] {
        assert!(validate_url(url, None).is_err());
    }
}

#[test]
fn ambient_proxy_auth_downgrade_credentials_rejected() {
    for url in [
        "https://user:password@example.org/",
        "http://user@example.org/",
        "file:///etc/passwd",
        "https://localhost/",
        "https://192.168.1.1/",
    ] {
        assert!(validate_url(url, None).is_err());
    }
    assert!(validate_url("http://example.org/", Some("https://example.org/")).is_err());
    let f = fixture(|_| {});
    let connector = Connector::new(vec![PublicReply::new(
        302,
        vec![
            ("Location".into(), "http://example.org/next".into()),
            ("Set-Cookie".into(), "session=ignored".into()),
        ],
        vec![],
    )]);
    let result = acquire::fetch_url(
        &f.fs,
        &f.dispatcher,
        &f.job,
        "https://example.org/",
        FetchLimits::default(),
        &options(
            Resolver::new(vec![vec!["93.184.216.34"]]),
            connector.clone(),
        ),
    )
    .unwrap();
    assert!(matches!(
        result,
        AcquisitionResult::Gap {
            kind: WebGap::RedirectRejected,
            ..
        }
    ));
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.inputs.0.load(Ordering::SeqCst), 0);
    let requests = connector.requests.lock().unwrap();
    assert_eq!(requests[0].0, "https://example.org/");
    assert_eq!(requests[0].1, "example.org");
    assert_eq!(
        requests[0].2[0].ip(),
        "93.184.216.34".parse::<IpAddr>().unwrap()
    );
}

#[test]
fn redirect_and_decompression_caps_accounted() {
    let f = fixture(|_| {});
    let connector = Connector::new(vec![
        PublicReply::new(302, vec![("location".into(), "/next".into())], vec![]),
        PublicReply::new(302, vec![("location".into(), "/last".into())], vec![]),
    ]);
    let result = acquire::fetch_url(
        &f.fs,
        &f.dispatcher,
        &f.job,
        "https://example.org/start",
        FetchLimits {
            redirects: 1,
            ..FetchLimits::default()
        },
        &options(
            Resolver::new(vec![vec!["93.184.216.34"], vec!["93.184.216.34"]]),
            connector.clone(),
        ),
    )
    .unwrap();
    assert!(matches!(
        result,
        AcquisitionResult::Gap {
            kind: WebGap::RedirectLimit,
            ..
        }
    ));
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 2);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 2);
    use std::io::Write;
    let mut gzip = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gzip.write_all(&[b'a'; 4096]).unwrap();
    let compressed = gzip.finish().unwrap();
    let f = fixture(|_| {});
    let connector = Connector::new(vec![PublicReply::new(
        200,
        vec![
            ("Content-Type".into(), "text/plain".into()),
            ("Content-Encoding".into(), "gzip".into()),
        ],
        compressed.clone(),
    )]);
    let source = captured(
        acquire::fetch_url(
            &f.fs,
            &f.dispatcher,
            &f.job,
            "https://example.org/compressed",
            FetchLimits {
                expanded_bytes: 64,
                ..FetchLimits::default()
            },
            &options(Resolver::new(vec![vec!["93.184.216.34"]]), connector),
        )
        .unwrap(),
    );
    assert_eq!(source.gap, Some(WebGap::DecompressionLimit));
    assert_eq!(raw_content(&f, &source).0, compressed);
    assert!(raw_content(&f, &source).1.is_none());
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
    let f = fixture(|_| {});
    let key = task(
        &f,
        "https://example.org/oversized",
        FetchLimits {
            compressed_bytes: 8,
            ..FetchLimits::default()
        },
    );
    let connector = Connector::new(vec![reply(200, "text/plain", b"more than eight bytes")]);
    let mut failure = f
        .dispatcher
        .execute_public(
            &f.job,
            &key,
            &options(Resolver::new(vec![vec!["93.184.216.34"]]), connector),
        )
        .err()
        .unwrap();
    assert!(failure.spool.is_some());
    acquire::settle_receipt(&f.fs, &f.job, failure.materialization.take().unwrap()).unwrap();
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
    assert!(!f.job.inspect().unwrap().budget.guarantee_intact);
}

#[test]
fn capture_raw_before_normalize_and_unsupported_gap() {
    let f = fixture(|_| {});
    let html=b"<html><body><h1>Evidence &amp; traces</h1><script>fake()</script><style>hidden</style><p>Exact &#233; paragraph.</p></body></html>";
    let key = task(&f, "https://example.org/html", FetchLimits::default());
    let outcome = f
        .dispatcher
        .execute_public(
            &f.job,
            &key,
            &options(
                Resolver::new(vec![vec!["93.184.216.34"]]),
                Connector::new(vec![reply(200, "text/html; charset=UTF-8", html)]),
            ),
        )
        .ok()
        .unwrap();
    assert_eq!(
        std::fs::read(f.fs.root().resolve(&outcome.spool.response.path).unwrap()).unwrap(),
        html
    );
    assert_eq!(
        f.job.inspect().unwrap().attempts[0].phase,
        AttemptPhase::Received
    );
    let source =
        acquire::capture_outcome(&f.fs, &f.job, outcome, "https://example.org/html", vec![])
            .unwrap();
    let (raw, content, manifest) = raw_content(&f, &source);
    assert_eq!(raw, html);
    assert_eq!(
        content.unwrap(),
        "Evidence & traces\nExact é paragraph.\n".as_bytes()
    );
    assert!(
        String::from_utf8(manifest)
            .unwrap()
            .contains("lwiki-acquisition-v1")
    );
    let immutable_before = std::fs::read(f.fs.root().path().join(format!(
        "sources/{}/revisions/{}/revision.md",
        source.source_id, source.revision_id
    )))
    .unwrap();
    let result = captured(
        acquire::fetch_url(
            &f.fs,
            &f.dispatcher,
            &f.job,
            "https://example.org/binary",
            FetchLimits::default(),
            &options(
                Resolver::new(vec![vec!["93.184.216.34"]]),
                Connector::new(vec![reply(200, "application/pdf", b"%PDF-fixture")]),
            ),
        )
        .unwrap(),
    );
    assert_eq!(result.gap, Some(WebGap::UnsupportedMedia));
    assert!(raw_content(&f, &result).1.is_none());
    assert_eq!(raw_content(&f, &source).2, immutable_before);
    let captures = [
        (401, "text/html", b"login".as_slice(), WebGap::AuthWall),
        (
            503,
            "text/plain",
            b"unavailable".as_slice(),
            WebGap::Unavailable,
        ),
    ];
    for (status, media, body, gap) in captures {
        let f = fixture(|_| {});
        let source = captured(
            acquire::fetch_url(
                &f.fs,
                &f.dispatcher,
                &f.job,
                "https://example.org/gap",
                FetchLimits::default(),
                &options(
                    Resolver::new(vec![vec!["93.184.216.34"]]),
                    Connector::new(vec![reply(status, media, body)]),
                ),
            )
            .unwrap(),
        );
        assert_eq!(source.gap, Some(gap));
        assert_eq!(raw_content(&f, &source).0, body);
    }
}

struct SearchTransport {
    bodies: Mutex<VecDeque<Value>>,
    calls: AtomicUsize,
}
impl Transport for SearchTransport {
    fn execute<'a>(
        &'a self,
        request: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        Box::pin(async move {
            assert_eq!(request.summary().role, ServiceRole::Search);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(TransportReply::new(
                200,
                vec![],
                serde_json::to_vec(&self.bodies.lock().unwrap().pop_front().unwrap()).unwrap(),
            )
            .unwrap())
        })
    }
}
fn search_case(
    inputs: Vec<RemoteInput>,
    bodies: Vec<Value>,
) -> (
    Fixture,
    lwiki::config::providers::TrustedService,
    Arc<SearchTransport>,
    Vec<Blake3Hash>,
) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("WIKI.md"),"---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Search fixture\n---\n").unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(&root).unwrap());
    let config = temp.path().join("providers.toml");
    private(
        &config,
        format!(
            "version=1\n[profiles.primary]\nsearch=\"service\"\n[services.service]\nadapter=\"brave-web-v1\"\nurl=\"https://api.search.brave.com/res/v1/web/search\"\n[services.service.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\nheader=\"X-Subscription-Token\"\nprefix=\"\"\n[vault_bindings.test]\nroot={}\nwiki_id=\"vault_test\"\nallowed_profiles=[\"primary\"]\n",
            quote(root.to_str().unwrap())
        ),
    );
    let trusted = ProviderConfig::load(&config)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Search)
        .unwrap();
    let mut tasks = vec![];
    std::fs::create_dir(root.join("inputs")).unwrap();
    for (i, input) in inputs.into_iter().enumerate() {
        let fp = lwiki::providers::wire::task_fingerprints(&trusted, &input).unwrap();
        let bytes = canonical_json(&input).unwrap();
        let path = VaultRelativePath::new(format!("inputs/{i}.json")).unwrap();
        private(&fs.root().resolve(&path).unwrap(), &bytes);
        let mut task = TaskSpec {
            key: hash([]),
            stage: TaskStage::Discover,
            capability: Some(Capability::Search),
            priority: i as i32,
            dependencies: vec![],
            input_hash: fp.input,
            prompt_hash: fp.prompt,
            schema_hash: fp.schema,
            model_hash: Some(fp.model),
            settings_hash: fp.settings,
            source_bindings: vec![],
            input: BoundedPayloadRef {
                path,
                hash: hash(&bytes),
                byte_len: bytes.len() as u64,
            },
        };
        task.key = lwiki::jobs::tasks::task_key(&task).unwrap();
        tasks.push(task);
    }
    let keys = tasks.iter().map(|t| t.key.clone()).collect();
    let summary = trusted.summary();
    let clock = Arc::new(Clock::new());
    let now = clock.read().unwrap().utc_ms;
    let mut spec = RunSpec {
        version: 1,
        run_id: id("run_search"),
        vault_id: id("vault_test"),
        title: "Search fixture".into(),
        created_at_utc_ms: now,
        deadline_utc_ms: now + 900_000,
        scope: RunScope {
            operation: "research".into(),
            question: Some("fixture".into()),
            exclusions: vec![],
            source_snapshot: None,
            input_records: vec![],
            read_preconditions: vec![],
            profile_fingerprints: BTreeMap::from([("primary".into(), summary.profile_fingerprint)]),
            scope_payload_hash: None,
        },
        config_fingerprint: summary.config_fingerprint,
        input_fingerprint: hash([]),
        limits: LifetimeLimits::default(),
        tasks,
        prior_accounting: PriorAccounting::None,
    };
    spec.input_fingerprint = lwiki::jobs::tasks::input_fingerprint(&spec).unwrap();
    let job = JobLedger::new(
        fs.clone(),
        id("vault_test"),
        spec.run_id.clone(),
        JobOptions {
            clock: clock.clone(),
            cancel: CancellationToken::default(),
            fault: None,
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        },
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    job.create(&writer, spec).unwrap();
    drop(writer);
    job.start().unwrap();
    let inputs = Arc::new(Inputs(AtomicUsize::new(0)));
    let broker = Arc::new(CredentialBroker::new(CredentialOptions {
        clock: clock.clone(),
        inputs: inputs.clone(),
        runner: Arc::new(Runner {
            calls: AtomicUsize::new(0),
            hook: None,
        }),
    }));
    let transport = Arc::new(SearchTransport {
        bodies: Mutex::new(bodies.into()),
        calls: AtomicUsize::new(0),
    });
    let dispatcher = Dispatcher::new(
        fs.clone(),
        DispatchOptions {
            broker,
            transport: transport.clone(),
            jitter: Arc::new(ZeroJitter),
        },
    );
    (
        Fixture {
            _temp: temp,
            fs,
            job,
            dispatcher,
            inputs,
            clock,
        },
        trusted,
        transport,
        keys,
    )
}
fn search_input(page: u8) -> RemoteInput {
    RemoteInput {
        version: 1,
        operation: RemoteOperation::Search {
            query: "encoded phrase & question".into(),
            count: 2,
            page,
        },
    }
}
#[test]
fn search_limits_pages_dedup_snippets_are_not_evidence() {
    for (q, count, page) in [
        (" ".to_owned(), 1, 0),
        ("a".repeat(601), 1, 0),
        ((0..76).map(|_| "word").collect::<Vec<_>>().join(" "), 1, 0),
        ("x".into(), 0, 0),
        ("x".into(), 21, 0),
        ("x".into(), 1, 10),
    ] {
        assert!(search_wire::validate(&q, count, page).is_err());
    }
    let (invalid, trusted, transport, keys) = search_case(vec![search_input(10)], vec![]);
    assert!(
        invalid
            .dispatcher
            .execute(&invalid.job, &trusted, &keys[0], DispatchPurpose::Task)
            .is_err()
    );
    assert_eq!(invalid.inputs.0.load(Ordering::SeqCst), 0);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert_eq!(invalid.job.inspect().unwrap().budget.dispatched_requests, 0);
    let result = |urls: &[&str]| json!({"web":{"results":urls.iter().map(|url|json!({"url":url,"title":"Lead","description":"Snippet is a lead"})).collect::<Vec<_>>()}});
    let (f, trusted, transport, keys) = search_case(
        vec![search_input(0), search_input(1)],
        vec![
            result(&["https://example.org/a?q=1", "https://example.org/b"]),
            result(&[
                "https://example.org/a?q=1#part",
                "https://example.org/a?q=2",
            ]),
        ],
    );
    let mut pages = vec![];
    for (page, key) in keys.iter().enumerate() {
        let outcome = f
            .dispatcher
            .execute(&f.job, &trusted, key, DispatchPurpose::Task)
            .ok()
            .unwrap();
        let ValidatedOutput::Search { leads } = outcome.output else {
            panic!("search leads required")
        };
        assert_eq!(leads[0].rank, 1 + page as u32 * 2);
        pages.push((format!("page{page}"), leads));
        acquire::settle_receipt(&f.fs, &f.job, outcome.materialization).unwrap();
    }
    let leads = acquire::deduplicate_leads(pages).unwrap();
    assert_eq!(leads.len(), 3);
    assert_eq!(leads[0].origins, vec!["page0", "page1"]);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 2);
    assert!(
        SourceStore::new(f.fs.clone())
            .view()
            .unwrap()
            .verify(
                &CitationRef::Source(SourceSpanRef {
                    source_id: id("src_missing"),
                    source_revision: id("rev_missing"),
                    span: ByteSpan::new(0, 7).unwrap(),
                    quote_hash: hash("Snippet")
                }),
                lwiki::sources::CitationScope::Current
            )
            .is_err()
    );
    assert!(
        serde_json::to_value(&leads[0].lead)
            .unwrap()
            .get("source_revision")
            .is_none()
    );
}

#[test]
fn malformed_search_is_paid_rejection_and_count_breach_invalidates_guarantee() {
    for (body, intact) in [
        (
            json!({"web":{"results":[{"url":"https://example.org/","title":7,"description":"lead"}]}}),
            true,
        ),
        (json!({"web":{"results":[{}, {}, {}]}}), false),
    ] {
        let (f, trusted, transport, keys) = search_case(vec![search_input(0)], vec![body]);
        let failure = f
            .dispatcher
            .execute(&f.job, &trusted, &keys[0], DispatchPurpose::Task)
            .err()
            .unwrap();
        assert!(failure.spool.is_some());
        // Authenticated Dispatcher already durably commits rejected receipts.
        assert!(failure.materialization.is_some());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        let inspect = f.job.inspect().unwrap();
        assert_eq!(inspect.budget.dispatched_requests, 1);
        assert_eq!(inspect.budget.guarantee_intact, intact);
    }
}

struct InterruptedConnector(Arc<Clock>);
impl PublicConnector for InterruptedConnector {
    fn connect<'a>(&'a self, request: PinnedRequest) -> Result<ConnectionFuture<'a>> {
        Ok(Box::pin(async move {
            request.observe_body_bytes(37);
            self.0.utc.fetch_add(1_000_000, Ordering::SeqCst);
            std::future::pending::<Result<PublicReply>>().await
        }))
    }
}
#[test]
fn deadline_preserves_observed_bytes_and_never_resends() {
    let f = fixture(|_| {});
    let key = task(&f, "https://example.org/slow", FetchLimits::default());
    let options = PublicFetchOptions {
        resolver: Resolver::new(vec![vec!["93.184.216.34"]]),
        connector: Arc::new(InterruptedConnector(f.clock.clone())),
    };
    let mut failure = f
        .dispatcher
        .execute_public(&f.job, &key, &options)
        .err()
        .unwrap();
    assert_eq!(failure.disposition, DispatchDisposition::OutcomeUnknown);
    assert!(failure.spool.is_some());
    let plan = failure.materialization.take().unwrap();
    let KnownOrUnknown::Known(usage) = &plan.receipt.usage else {
        panic!("observed usage required")
    };
    assert_eq!(usage.response_bytes, 37);
    acquire::settle_receipt(&f.fs, &f.job, plan).unwrap();
    assert!(f.dispatcher.execute_public(&f.job, &key, &options).is_err());
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
}

#[test]
fn offline_and_dry_run_do_not_resolve_or_connect() {
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
        let f = fixture(|_| {});
        let key = task(&f, "https://example.org/", FetchLimits::default());
        let job = JobLedger::new(
            f.fs.clone(),
            id("vault_test"),
            id("run_acquire"),
            JobOptions {
                clock: f.clock.clone(),
                cancel: CancellationToken::default(),
                fault: None,
                policy,
                lock_timeout_ms: 5000,
            },
        )
        .unwrap();
        let dns = Resolver::new(vec![]);
        let connector = Connector::new(vec![]);
        assert!(
            f.dispatcher
                .execute_public(&job, &key, &options(dns.clone(), connector.clone()))
                .is_err()
        );
        assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
        assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
        assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 0);
    }
}

#[test]
fn robots_auth_and_distinct_content_origins_remain_explicit() {
    let f = fixture(|_| {});
    let key = task(&f, "https://example.org/robots", FetchLimits::default());
    let outcome = f
        .dispatcher
        .execute_public(
            &f.job,
            &key,
            &options(
                Resolver::new(vec![vec!["93.184.216.34"]]),
                Connector::new(vec![PublicReply::new(
                    200,
                    vec![
                        ("content-type".into(), "text/html".into()),
                        ("x-robots-tag".into(), "noindex, nosnippet".into()),
                    ],
                    b"<p>blocked</p>".to_vec(),
                )]),
            ),
        )
        .ok()
        .unwrap();
    let mut other = outcome.capture.clone();
    other.observed_url = "https://another.example/same".into();
    let groups = acquire::group_content_origins(&[outcome.capture.clone(), other]);
    assert_eq!(groups[&outcome.capture.original_hash].len(), 2);
    let source =
        acquire::capture_outcome(&f.fs, &f.job, outcome, "https://example.org/robots", vec![])
            .unwrap();
    assert_eq!(source.gap, Some(WebGap::Robots));
    assert!(raw_content(&f, &source).1.is_none());
    let f = fixture(|_| {});
    let source = captured(
        acquire::fetch_url(
            &f.fs,
            &f.dispatcher,
            &f.job,
            "https://example.org/login",
            FetchLimits::default(),
            &options(
                Resolver::new(vec![vec!["93.184.216.34"]]),
                Connector::new(vec![reply(
                    200,
                    "text/html",
                    b"<form><input type='password'>Sign in</form>",
                )]),
            ),
        )
        .unwrap(),
    );
    assert_eq!(source.gap, Some(WebGap::AuthWall));
}

#[test]
fn protected_fetch_replay_recovers_headers_time_bytes_without_resend() {
    use std::io::Write;
    let mut gzip = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gzip.write_all(b"<p>Recovered &amp; supported</p>").unwrap();
    let original = gzip.finish().unwrap();
    let f = fixture(|_| {});
    let key = task(
        &f,
        "https://example.org/replay?meaning=preserved",
        FetchLimits::default(),
    );
    let dns = Resolver::new(vec![vec!["93.184.216.34"]]);
    let connector = Connector::new(vec![PublicReply::new(
        200,
        vec![
            ("content-type".into(), "text/html; charset=utf-8".into()),
            ("content-encoding".into(), "gzip".into()),
            ("set-cookie".into(), "never-retained-or-sent".into()),
        ],
        original.clone(),
    )]);
    let outcome = f
        .dispatcher
        .execute_public(&f.job, &key, &options(dns.clone(), connector.clone()))
        .ok()
        .unwrap();
    let attempt = outcome.attempt.clone();
    let at = outcome.capture.fetched_at_utc_ms;
    let meta = std::fs::read(f.fs.root().resolve(&outcome.spool.metadata.path).unwrap()).unwrap();
    assert!(!String::from_utf8_lossy(&meta).contains("never-retained-or-sent"));
    drop(outcome);
    let reopened = JobLedger::new(
        f.fs.clone(),
        id("vault_test"),
        id("run_acquire"),
        JobOptions {
            clock: f.clock.clone(),
            cancel: CancellationToken::default(),
            fault: None,
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        },
    )
    .unwrap();
    reopened.replay().unwrap();
    let restored = recover_response(&reopened, &attempt).unwrap();
    assert_eq!(restored.capture.fetched_at_utc_ms, at);
    assert_eq!(restored.capture.header("content-encoding"), Some("gzip"));
    assert_eq!(restored.capture.original, original);
    let source = acquire::capture_outcome(
        &f.fs,
        &reopened,
        restored,
        "https://example.org/replay?meaning=preserved",
        vec![],
    )
    .unwrap();
    assert_eq!(
        raw_content(&f, &source).1.unwrap(),
        b"Recovered & supported\n"
    );
    assert_eq!(dns.calls.load(Ordering::SeqCst), 1);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(reopened.inspect().unwrap().budget.dispatched_requests, 1);
}

#[test]
fn forged_capture_metadata_cannot_reinterpret_durable_raw_response() {
    let f = fixture(|_| {});
    let key = task(
        &f,
        "https://example.org/unsupported",
        FetchLimits::default(),
    );
    let mut outcome = f
        .dispatcher
        .execute_public(
            &f.job,
            &key,
            &options(
                Resolver::new(vec![vec!["93.184.216.34"]]),
                Connector::new(vec![reply(200, "application/pdf", b"unsupported bytes")]),
            ),
        )
        .ok()
        .unwrap();
    outcome.capture.headers = vec![("content-type".into(), "text/plain".into())];
    assert!(
        acquire::capture_outcome(
            &f.fs,
            &f.job,
            outcome,
            "https://example.org/unsupported",
            vec![]
        )
        .is_err()
    );
    assert_eq!(
        f.job.inspect().unwrap().attempts[0].phase,
        AttemptPhase::Received
    );
}

struct ExpiringPreparation {
    clock: Arc<Clock>,
    polls: Arc<AtomicUsize>,
}
impl PublicConnector for ExpiringPreparation {
    fn connect<'a>(&'a self, _: PinnedRequest) -> Result<ConnectionFuture<'a>> {
        // Preparation is pure with respect to network IO; expiry is injected
        // before the boundary's actual transport poll, as native setup can take
        // time without being allowed to weaken the authorization clock.
        self.clock.utc.fetch_add(1_000_000, Ordering::SeqCst);
        Ok(Box::pin(async move {
            self.polls.fetch_add(1, Ordering::SeqCst);
            Ok(reply(200, "text/plain", b"must not send"))
        }))
    }
}
#[test]
fn final_entry_proof_runs_after_connection_preparation() {
    let f = fixture(|_| {});
    let key = task(&f, "https://example.org/", FetchLimits::default());
    let polls = Arc::new(AtomicUsize::new(0));
    let opts = PublicFetchOptions {
        resolver: Resolver::new(vec![vec!["93.184.216.34"]]),
        connector: Arc::new(ExpiringPreparation {
            clock: f.clock.clone(),
            polls: polls.clone(),
        }),
    };
    let failure = f
        .dispatcher
        .execute_public(&f.job, &key, &opts)
        .err()
        .unwrap();
    assert_eq!(failure.disposition, DispatchDisposition::NotSent);
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 0);
}

#[test]
fn two_hop_capture_completes_redirect_and_retains_bound_origin_chain() {
    let f = fixture(|_| {});
    let dns = Resolver::new(vec![vec!["93.184.216.34"], vec!["93.184.216.35"]]);
    let connector = Connector::new(vec![
        PublicReply::new(
            302,
            vec![(
                "location".into(),
                "/final?meaning=one&meaning=two#part".into(),
            )],
            b"raw redirect body".to_vec(),
        ),
        reply(200, "text/plain", b"Final captured evidence"),
    ]);
    let source = captured(
        acquire::fetch_url(
            &f.fs,
            &f.dispatcher,
            &f.job,
            "https://example.org/start#lead",
            FetchLimits::default(),
            &options(dns.clone(), connector.clone()),
        )
        .unwrap(),
    );
    assert_eq!(
        source.provenance.observed_url,
        "https://example.org/start#lead"
    );
    assert_eq!(
        source.provenance.final_url,
        "https://example.org/final?meaning=one&meaning=two"
    );
    assert_eq!(source.provenance.redirects.len(), 1);
    assert_eq!(
        source.provenance.redirects[0].original_hash,
        hash(b"raw redirect body")
    );
    assert_eq!(raw_content(&f, &source).0, b"Final captured evidence");
    let inspect = f.job.inspect().unwrap();
    assert_eq!(inspect.budget.dispatched_requests, 2);
    assert!(
        inspect
            .tasks
            .values()
            .all(|t| t.state == TaskState::Completed)
    );
    assert!(
        inspect
            .attempts
            .iter()
            .all(|a| a.phase == AttemptPhase::Settled)
    );
    assert_eq!(dns.calls.load(Ordering::SeqCst), 2);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn ampersand_heavy_html_has_bounded_entity_lookahead() {
    let text = "&".repeat(128 * 1024);
    let raw = format!("<p>{text}é</p>").into_bytes();
    let f = fixture(|_| {});
    let source = captured(
        acquire::fetch_url(
            &f.fs,
            &f.dispatcher,
            &f.job,
            "https://example.org/entities",
            FetchLimits::default(),
            &options(
                Resolver::new(vec![vec!["93.184.216.34"]]),
                Connector::new(vec![reply(200, "text/html", &raw)]),
            ),
        )
        .unwrap(),
    );
    assert_eq!(
        raw_content(&f, &source).1.unwrap(),
        format!("{text}é\n").as_bytes()
    );
}

struct ChangeCanonicalRunAfterReceived {
    path: std::path::PathBuf,
    used: AtomicUsize,
}
impl LedgerFault for ChangeCanonicalRunAfterReceived {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterReceived && self.used.fetch_add(1, Ordering::SeqCst) == 0
        {
            std::fs::write(&self.path, b"edited canonical run after paid capture").unwrap();
        }
        Ok(())
    }
}
#[test]
fn local_materialization_failure_retains_paid_raw_spool_for_recovery() {
    let f = fixture(|_| {});
    let key = task(&f, "https://example.org/received", FetchLimits::default());
    let run = f.job.inspect().unwrap().run_note.unwrap();
    let path = f.fs.root().resolve(&run.path).unwrap();
    let before = std::fs::read(&path).unwrap();
    let fault = Arc::new(ChangeCanonicalRunAfterReceived {
        path: path.clone(),
        used: AtomicUsize::new(0),
    });
    let job = JobLedger::new(
        f.fs.clone(),
        id("vault_test"),
        id("run_acquire"),
        JobOptions {
            clock: f.clock.clone(),
            cancel: CancellationToken::default(),
            fault: Some(fault),
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        },
    )
    .unwrap();
    let dns = Resolver::new(vec![vec!["93.184.216.34"]]);
    let connector = Connector::new(vec![reply(
        200,
        "text/plain",
        b"paid bytes survive local planning",
    )]);
    let failure = f
        .dispatcher
        .execute_public(&job, &key, &options(dns.clone(), connector.clone()))
        .err()
        .unwrap();
    assert_eq!(failure.disposition, DispatchDisposition::OutcomeUnknown);
    let spool = failure.spool.unwrap();
    assert_eq!(
        std::fs::read(f.fs.root().resolve(&spool.response.path).unwrap()).unwrap(),
        b"paid bytes survive local planning"
    );
    std::fs::write(path, before).unwrap();
    let recovered = recover_response(&f.job, &spool.attempt).unwrap();
    let source = acquire::capture_outcome(
        &f.fs,
        &f.job,
        recovered,
        "https://example.org/received",
        vec![],
    )
    .unwrap();
    assert_eq!(
        raw_content(&f, &source).0,
        b"paid bytes survive local planning"
    );
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(dns.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
}

fn tree_hashes(root: &std::path::Path) -> BTreeMap<String, Option<Blake3Hash>> {
    fn walk(
        root: &std::path::Path,
        path: &std::path::Path,
        out: &mut BTreeMap<String, Option<Blake3Hash>>,
    ) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let key = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if entry.file_type().unwrap().is_dir() {
                out.insert(key, None);
                walk(root, &path, out);
            } else {
                out.insert(key, Some(hash(std::fs::read(path).unwrap())));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
#[test]
fn high_level_dry_run_and_offline_leave_full_vault_tree_unchanged() {
    for policy in [
        ExecutionPolicy {
            dry_run: true,
            ..ExecutionPolicy::default()
        },
        ExecutionPolicy {
            offline: true,
            ..ExecutionPolicy::default()
        },
    ] {
        let f = fixture(|_| {});
        let job = JobLedger::new(
            f.fs.clone(),
            id("vault_test"),
            id("run_acquire"),
            JobOptions {
                clock: f.clock.clone(),
                cancel: CancellationToken::default(),
                fault: None,
                policy,
                lock_timeout_ms: 5000,
            },
        )
        .unwrap();
        let before = tree_hashes(f.fs.root().path());
        let dns = Resolver::new(vec![]);
        let connector = Connector::new(vec![]);
        assert!(
            acquire::fetch_url(
                &f.fs,
                &f.dispatcher,
                &job,
                "https://example.org/new-unstored",
                FetchLimits::default(),
                &options(dns.clone(), connector.clone())
            )
            .is_err()
        );
        assert_eq!(tree_hashes(f.fs.root().path()), before);
        assert_eq!(dns.calls.load(Ordering::SeqCst), 0);
        assert_eq!(connector.calls.load(Ordering::SeqCst), 0);
    }
}

struct FailAfterProtectedMetadata(AtomicUsize);
impl LedgerFault for FailAfterProtectedMetadata {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterSpoolMetadataSync
            && self.0.fetch_add(1, Ordering::SeqCst) == 0
        {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "injected metadata-to-Received interruption",
            ));
        }
        Ok(())
    }
}
#[test]
fn protected_metadata_before_received_replays_without_resend() {
    let f = fixture(|_| {});
    let key = task(
        &f,
        "https://example.org/before-received",
        FetchLimits::default(),
    );
    let job = JobLedger::new(
        f.fs.clone(),
        id("vault_test"),
        id("run_acquire"),
        JobOptions {
            clock: f.clock.clone(),
            cancel: CancellationToken::default(),
            fault: Some(Arc::new(FailAfterProtectedMetadata(AtomicUsize::new(0)))),
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        },
    )
    .unwrap();
    let dns = Resolver::new(vec![vec!["93.184.216.34"]]);
    let connector = Connector::new(vec![reply(
        200,
        "text/html; charset=utf-8",
        b"<p>Durable before Received</p>",
    )]);
    let failure = f
        .dispatcher
        .execute_public(&job, &key, &options(dns.clone(), connector.clone()))
        .err()
        .unwrap();
    assert_eq!(failure.disposition, DispatchDisposition::OutcomeUnknown);
    let attempt = failure.attempt.unwrap();
    assert!(f.job.inspect().unwrap().attempts[0].spool.is_none());
    let replay = f.job.replay().unwrap();
    assert_eq!(replay.orphan_spools.len(), 1);
    let outcome = recover_response(&f.job, &attempt).unwrap();
    assert_eq!(
        outcome.capture.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    let source = acquire::capture_outcome(
        &f.fs,
        &f.job,
        outcome,
        "https://example.org/before-received",
        vec![],
    )
    .unwrap();
    assert_eq!(
        raw_content(&f, &source).1.unwrap(),
        b"Durable before Received\n"
    );
    assert_eq!(dns.calls.load(Ordering::SeqCst), 1);
    assert_eq!(connector.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.job.inspect().unwrap().budget.dispatched_requests, 1);
}

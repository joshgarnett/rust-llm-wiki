#![allow(dead_code)]
use super::provider;
use lwiki::{
    app::*,
    catalog::*,
    changes::*,
    config::providers::*,
    domain::*,
    graph::{api_extract::*, *},
    jobs::*,
    providers::{credentials::*, dispatcher::Dispatcher, types::*},
    sources::*,
    vault::*,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub fs: VaultFs,
    pub app: OfflineApp,
    pub engine: ChangeEngine,
    pub catalog: Catalog,
    pub service: TrustedService,
    pub request: ApiExtractionRequest,
    pub options: JobOptions,
    pub broker: Arc<CredentialBroker>,
    pub packet: ExtractionPacket,
}
pub fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: API fixture\n---\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let engine = ChangeEngine::new(fs.clone()).unwrap();
        let catalog = Catalog::new(fs.clone(), id("vault_test"));
        let clock = Arc::new(provider::Clock::new());
        let now = clock.read().unwrap().utc_ms;
        let options = JobOptions {
            clock: clock.clone(),
            fault: None,
            cancel: CancellationToken::default(),
            policy: ExecutionPolicy::default(),
            lock_timeout_ms: 5000,
        };
        let capture = SourceStore::new(fs.clone())
            .plan_capture(CaptureRequest {
                title: "Synthetic API source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fixture.md".into(),
                original: b"Ada works for Acme.\nAda uses Tool.\nBob maintains Tool.".to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            })
            .unwrap();
        let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(2)).unwrap();
        let prepared = engine
            .prepare(&writer, capture.draft.unwrap())
            .unwrap()
            .prepared;
        engine
            .apply(&writer, &prepared, &CatalogGraphValidator, &catalog)
            .unwrap();
        drop(writer);
        let app = OfflineApp::new(
            fs.clone(),
            OperationOptions {
                lock_timeout_ms: 5000,
                ..Default::default()
            },
        )
        .unwrap();
        let export = ExportRequest {
            source_id: capture.source_id,
            revision_id: Some(capture.revision_id),
            windows: vec![],
            limits: ExtractionLimits::default(),
            candidate_context: vec![],
        };
        let view = SourceView::from_fs_bounded(&fs, 64 * 1024 * 1024, 4096).unwrap();
        let packet = lwiki::graph::packet::build_packet(&view, &export)
            .unwrap()
            .packet;
        let config = temp.path().join("providers.toml");
        provider::private(
            &config,
            format!(
                "version=1\n[profiles.primary]\ngeneration=\"service\"\n[services.service]\nadapter=\"chat-completions-v1\"\nurl=\"https://gateway.example/chat\"\nmodel=\"test-model\"\nrevision=\"r1\"\n[services.service.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\n[vault_bindings.test]\nroot={}\nwiki_id=\"vault_test\"\nallowed_profiles=[\"primary\"]\n",
                provider::quote(temp.path().to_str().unwrap())
            ),
        );
        let service = ProviderConfig::load(&config)
            .unwrap()
            .authorize(&fs, &id("vault_test"), "primary", Capability::Generate)
            .unwrap();
        let broker = Arc::new(CredentialBroker::new(CredentialOptions {
            clock,
            inputs: Arc::new(provider::Inputs(AtomicUsize::new(0))),
            runner: Arc::new(provider::Runner {
                calls: AtomicUsize::new(0),
                hook: None,
            }),
        }));
        let request = ApiExtractionRequest {
            requested_limits: None,
            export,
            run_id: id("run_api"),
            created_at_utc_ms: now,
            deadline_utc_ms: now + 900_000,
            limits: LifetimeLimits {
                requests: 3,
                concurrency: 1,
                attempts_per_task: 1,
                ..Default::default()
            },
            max_output_tokens: 1024,
            new_extraction: false,
        };
        Self {
            temp,
            fs,
            app,
            engine,
            catalog,
            service,
            request,
            options,
            broker,
            packet,
        }
    }
    pub fn response(&self) -> Value {
        let text = &self.packet.windows[0].text;
        let mention = |id: &str, label: &str, kind: &str| {
            let start = text.find(label).unwrap();
            json!({"id":id,"window_id":"w1","label":label,"type":kind,"quote":label,"span":{"start":start,"end":start+label.len()}})
        };
        let assertion = |id: &str, subject: &str, predicate: &str, object: &str, quote: &str| json!({"id":id,"subject":subject,"predicate":predicate,"object":{"kind":"mention","mention_id":object},"negated":false,"modality":"asserted","evidence":[{"window_id":"w1","stance":"supports","quote":quote}]});
        json!({"schema":EXTRACTION_SCHEMA,"packet_id":self.packet.packet_id,"packet_fingerprint":self.packet.packet_fingerprint,"mentions":[mention("m1","Ada","person"),mention("m2","Acme","organization"),mention("m3","Tool","component"),mention("m4","Bob","person")],"assertions":[assertion("a1","m1","works_for","m2","Ada works for Acme."),assertion("a2","m1","uses","m3","Ada uses Tool."),assertion("a3","m4","maintains","m3","Bob maintains Tool.")],"unresolved":[]})
    }
    pub fn dispatcher(&self, mock: Arc<Mock>) -> Dispatcher {
        Dispatcher::new(
            self.fs.clone(),
            DispatchOptions {
                broker: self.broker.clone(),
                transport: mock,
                jitter: Arc::new(provider::ZeroJitter),
            },
        )
    }
    pub fn ledger(&self) -> JobLedger {
        JobLedger::new(
            self.fs.clone(),
            id("vault_test"),
            self.request.run_id.clone(),
            self.options.clone(),
        )
        .unwrap()
    }
    pub fn apply(&self, prepared: &PreparedChange) {
        let writer = WriterPermit::acquire(self.fs.root(), Duration::from_secs(2)).unwrap();
        self.engine
            .apply(&writer, prepared, &CatalogGraphValidator, &self.catalog)
            .unwrap();
    }
    pub fn record(&self, record_id: &RecordId) -> lwiki::records::ParsedNote {
        self.fs
            .root()
            .scan_markdown()
            .unwrap()
            .into_iter()
            .map(|p| {
                lwiki::records::parse_note(
                    &std::fs::read(self.fs.root().path().join(p.as_str())).unwrap(),
                )
            })
            .find(|n| n.canonical.as_ref().is_some_and(|r| r.id() == record_id))
            .unwrap()
    }
}
pub struct Mock {
    pub calls: AtomicUsize,
    pub body: Mutex<Option<Value>>,
    pub cancel: Option<CancellationToken>,
    pub fail: bool,
}
impl Mock {
    pub fn response(value: Value) -> Arc<Self> {
        Self::content(serde_json::to_string(&value).unwrap(), "stop")
    }
    pub fn content(content: String, finish: &str) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            body: Mutex::new(Some(
                json!({"model":"test-model","choices":[{"index":0,"finish_reason":finish,"message":{"role":"assistant","content":content}}],"usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0}}}),
            )),
            cancel: None,
            fail: false,
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
        assert!(request.summary().request_bytes > 0);
        if let Some(c) = &self.cancel {
            c.cancel();
        }
        let body = self.body.lock().unwrap().take();
        Box::pin(async move {
            if self.fail {
                Err(TransportFailure::new(TransportFailureCode::Cancelled))
            } else {
                Ok(
                    TransportReply::new(200, vec![], serde_json::to_vec(&body.unwrap()).unwrap())
                        .unwrap(),
                )
            }
        })
    }
}

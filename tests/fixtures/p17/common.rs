#![allow(dead_code)]
use lwiki::{
    app::{OfflineApp, OperationOptions, embeddings::EmbeddingRuntime},
    catalog::Catalog,
    config::providers::{ProviderConfig, TrustedService},
    domain::*,
    jobs::*,
    providers::{credentials::*, dispatcher::Dispatcher, types::*},
    retrieval::{
        render::{self, RenderedUnit},
        spaces::*,
        vectors::*,
    },
    vault::*,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
pub fn private_write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
pub fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let p = entry.unwrap().path();
        let t = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy(&p, &t);
        } else {
            std::fs::copy(p, t).unwrap();
        }
    }
}
pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub fs: VaultFs,
    pub app: OfflineApp,
    pub config: PathBuf,
    pub service: TrustedService,
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        lwiki::app::offline::init(&root, "Semantic tests", OperationOptions::default()).unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        let app = OfflineApp::new(fs.clone(), OperationOptions::default()).unwrap();
        let config = temp.path().join("providers.toml");
        let text = format!(
            "version=1\n[profiles.primary]\nembedding='embed'\n[services.embed]\nadapter='embeddings-v1'\nurl='https://mock.example/v1/embeddings'\nmodel='test-model'\nrevision='r1'\nmax_batch_items=1\nmax_batch_bytes=262144\n[services.embed.auth]\nkind='static'\nkey='mock-secret'\n[vault_bindings.main]\nroot={}\nwiki_id={}\nallowed_profiles=['primary']\n",
            serde_json::to_string(root.to_str().unwrap()).unwrap(),
            serde_json::to_string(app.vault_id().as_str()).unwrap()
        );
        private_write(&config, text);
        let service = ProviderConfig::load(&config)
            .unwrap()
            .authorize(&fs, app.vault_id(), "primary", Capability::Embed)
            .unwrap();
        Self {
            temp,
            fs,
            app,
            config,
            service,
        }
    }
    pub fn page(&self, name: &str, body: &str) {
        let path = self
            .fs
            .root()
            .path()
            .join(format!("knowledge/pages/{name}.md"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path,format!("---\nwiki_schema: \"1\"\nwiki_id: page_{name}\nwiki_kind: page\ntitle: {name}\nwiki_status: reviewed\n---\n{body}")).unwrap();
    }
    pub fn reader(&self) -> lwiki::catalog::ReaderSnapshot {
        let catalog = Catalog::new(self.fs.clone(), self.app.vault_id().clone());
        let writer = self.writer();
        catalog.verified_snapshot(Some(&writer)).unwrap()
    }
    pub fn writer(&self) -> WriterPermit {
        WriterPermit::acquire(self.fs.root(), Duration::from_secs(1)).unwrap()
    }
    pub fn spec(&self) -> SpaceSpec {
        SpaceSpec::from_service(&self.service, EmbeddingSettings::default()).unwrap()
    }
    pub fn seed(&self, spec: &SpaceSpec, units: &[RenderedUnit], activate: bool) {
        let reader = self.reader();
        let writer = self.writer();
        let mut store = VectorStore::open(&self.fs, Some(&writer)).unwrap();
        let space = store.prepare_space(spec).unwrap();
        for (index, unit) in units.iter().enumerate() {
            let vector = if index % 2 == 0 {
                vec![1.0, 0.0]
            } else {
                vec![0.0, 1.0]
            };
            store
                .put_batch(
                    &space,
                    std::slice::from_ref(&unit.input_hash),
                    &[vector],
                    true,
                    &unit.dependency_fingerprint,
                )
                .unwrap();
        }
        store
            .memberships(&space, reader.snapshot(), units, activate)
            .unwrap();
    }
    pub fn query_seed(&self, spec: &SpaceSpec, text: &str, vector: Vec<f32>) {
        let writer = self.writer();
        let mut store = VectorStore::open(&self.fs, Some(&writer)).unwrap();
        let space = spec.id().unwrap();
        let input = spec.query(text).unwrap();
        store
            .put_batch(
                &space,
                &[input.input_hash],
                &[vector],
                false,
                &Blake3Hash::digest([]),
            )
            .unwrap();
    }
    pub fn offline(&self) -> OfflineApp {
        OfflineApp::new(
            self.fs.clone(),
            OperationOptions {
                offline: true,
                ..OperationOptions::default()
            },
        )
        .unwrap()
    }
}
pub struct TestClock;
impl JobClock for TestClock {
    fn read(&self) -> Result<ClockReading> {
        Ok(ClockReading {
            utc_ms: 1_800_000_000_000,
            monotonic_ms: 1000,
        })
    }
}
pub fn options() -> JobOptions {
    JobOptions {
        clock: Arc::new(TestClock),
        fault: None,
        cancel: CancellationToken::default(),
        policy: ExecutionPolicy::default(),
        lock_timeout_ms: 1000,
    }
}
pub struct Responses {
    pub calls: AtomicUsize,
    pub hook: Option<Box<dyn Fn(usize) + Send + Sync>>,
    pub vectors: Vec<Vec<f32>>,
}
impl Responses {
    pub fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            hook: None,
            vectors: vec![],
        }
    }
}
impl Transport for Responses {
    fn execute<'a>(
        &'a self,
        _: AuthenticatedRequest<'a>,
        _: TransportContext,
    ) -> TransportFuture<'a> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(hook) = &self.hook {
            hook(index);
        }
        let vector = self.vectors.get(index).cloned().unwrap_or(vec![1.0, 0.0]);
        Box::pin(async move {
            Ok(TransportReply::new(200,vec![],serde_json::to_vec(&serde_json::json!({"model":"test-model","data":[{"index":0,"embedding":vector}],"usage":{"prompt_tokens":2,"total_tokens":2}})).unwrap()).unwrap())
        })
    }
}
pub fn dispatcher(fs: &VaultFs, transport: Arc<Responses>) -> Dispatcher {
    Dispatcher::new(
        fs.clone(),
        DispatchOptions {
            broker: Arc::new(CredentialBroker::new(CredentialOptions {
                clock: Arc::new(TestClock),
                inputs: Arc::new(NativeSecretInputs),
                runner: Arc::new(NativeHelperRunner),
            })),
            transport,
            jitter: Arc::new(ZeroJitter),
        },
    )
}
pub fn runtime<'a>(
    service: &'a TrustedService,
    dispatcher: &'a Dispatcher,
) -> EmbeddingRuntime<'a> {
    EmbeddingRuntime {
        service,
        dispatcher,
        job_options: options(),
        limits: LifetimeLimits {
            requests: 64,
            attempts_per_task: 1,
            concurrency: 1,
            ..Default::default()
        },
        deadline_ms: 900000,
    }
}
pub fn corpus(f: &Fixture) -> Vec<RenderedUnit> {
    render::corpus(&f.reader(), &EmbeddingSettings::default()).unwrap()
}

struct ZeroJitter;
impl JitterSource for ZeroJitter {
    fn sample_inclusive(&self, _: u64) -> Result<u64> {
        Ok(0)
    }
}

use super::library::{
    config::providers::{ProviderConfig, TrustedService},
    domain::*,
    jobs::{self, *},
    providers::{
        credentials::{
            CredentialBroker, CredentialOptions, HelperInvocation, HelperLimits, HelperOutput,
            HelperRunner, SecretBytes, SecretInputs,
        },
        types::*,
    },
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub fn hash(bytes: impl AsRef<[u8]>) -> Blake3Hash {
    Blake3Hash::digest(bytes)
}
pub fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
pub fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}
pub fn private(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
pub struct Clock {
    pub utc: AtomicI64,
    pub mono: AtomicU64,
    start: Instant,
    base: i64,
}
impl Clock {
    pub fn new() -> Self {
        Self {
            utc: AtomicI64::new(0),
            mono: AtomicU64::new(0),
            start: Instant::now(),
            base: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
        }
    }
}
impl JobClock for Clock {
    fn read(&self) -> Result<ClockReading> {
        Ok(ClockReading {
            utc_ms: self.base
                + self.start.elapsed().as_millis() as i64
                + self.utc.load(Ordering::SeqCst),
            monotonic_ms: self.start.elapsed().as_millis() as u64
                + self.mono.load(Ordering::SeqCst),
        })
    }
}
pub struct Inputs(pub AtomicUsize);
impl SecretInputs for Inputs {
    fn environment(&self, _: &str, max_bytes: usize) -> Result<Option<SecretBytes>> {
        assert_eq!(max_bytes, 16384);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Some(SecretBytes::new(b"fixture-token".to_vec())?))
    }
    fn file(&self, _: &Path, max_bytes: usize) -> Result<SecretBytes> {
        assert_eq!(max_bytes, 16384);
        self.0.fetch_add(1, Ordering::SeqCst);
        SecretBytes::new(b"fixture-token".to_vec())
    }
}
pub struct Runner {
    pub calls: AtomicUsize,
    pub hook: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl HelperRunner for Runner {
    fn run(
        &self,
        _: &HelperInvocation,
        _: &HelperLimits,
        _: &dyn JobClock,
        _: &CancellationToken,
    ) -> Result<HelperOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(hook) = &self.hook {
            hook();
        }
        HelperOutput::new(br#"{"key":"fixture-token"}"#.to_vec())
    }
}
pub struct Case {
    #[allow(
        dead_code,
        reason = "owns fixture lifetime; some consumers inspect its tree"
    )]
    pub temp: tempfile::TempDir,
    pub fs: VaultFs,
    #[allow(
        dead_code,
        reason = "shared fixture configuration path used by wire and trust consumers"
    )]
    pub config: std::path::PathBuf,
    pub trusted: TrustedService,
    pub job: JobLedger,
    pub spec: RunSpec,
    pub clock: Arc<Clock>,
    pub inputs: Arc<Inputs>,
    pub runner: Arc<Runner>,
    pub broker: Arc<CredentialBroker>,
}
pub fn document(root: &Path, url: &str, command: bool, extra: &str) -> String {
    let timeout = if extra
        .lines()
        .any(|line| line.starts_with("timeout_seconds="))
    {
        ""
    } else {
        "timeout_seconds=1\n"
    };
    format!(
        "version=1\n[profiles.primary]\nembedding=\"service\"\n[services.service]\nadapter=\"embeddings-v1\"\nurl={}\nmodel=\"test-model\"\nrevision=\"r1\"\nmax_batch_items=32\nmax_batch_bytes=262144\nallow_loopback_http=true\n{timeout}connect_timeout_seconds=1\n{}\n[services.service.auth]\n{}\n[vault_bindings.test]\nroot={}\nwiki_id=\"vault_test\"\nallowed_profiles=[\"primary\"]\n",
        quote(url),
        extra,
        if command {
            "kind=\"command\"\ncommand=[\"/fixture/helper\"]\noutput=\"json\"\nttl_seconds=300\nrefresh_skew_seconds=60"
        } else {
            "kind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\""
        },
        quote(root.to_str().unwrap())
    )
}
pub fn case(
    url: &str,
    command: bool,
    policy: ExecutionPolicy,
    edit: impl FnOnce(&mut RunSpec),
    hook: Option<Arc<dyn Fn() + Send + Sync>>,
) -> Case {
    case_extra(url, command, policy, edit, hook, "")
}
pub fn case_extra(
    url: &str,
    command: bool,
    policy: ExecutionPolicy,
    edit: impl FnOnce(&mut RunSpec),
    hook: Option<Arc<dyn Fn() + Send + Sync>>,
    extra: &str,
) -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("WIKI.md"),"---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Dispatcher fixture\n---\nFixture\n").unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(&root).unwrap());
    let config = temp.path().join("providers.toml");
    private(&config, document(fs.root().path(), url, command, extra));
    let trusted = ProviderConfig::load(&config)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap();
    let input = RemoteInput {
        version: 1,
        operation: RemoteOperation::Embed {
            inputs: vec![EmbeddingInput {
                input_hash: hash("bounded input"),
                utf8: "bounded input".into(),
            }],
            expected_dimensions: None,
            representation_fingerprint: hash("render.v1"),
        },
    };
    let bytes = serde_json::to_vec(&input).unwrap();
    std::fs::create_dir(root.join("inputs")).unwrap();
    std::fs::write(root.join("inputs/task.json"), &bytes).unwrap();
    let summary = trusted.summary();
    let mut task = TaskSpec {
        key: hash([]),
        stage: TaskStage::Extract,
        capability: Some(Capability::Embed),
        priority: 0,
        dependencies: vec![],
        input_hash: hash(&bytes),
        prompt_hash: None,
        schema_hash: None,
        model_hash: Some(hash(
            serde_json::to_vec(&(summary.model.clone(), summary.revision.clone())).unwrap(),
        )),
        settings_hash: hash(summary.profile_fingerprint.as_str()),
        source_bindings: vec![],
        input: BoundedPayloadRef {
            path: VaultRelativePath::new("inputs/task.json").unwrap(),
            hash: hash(&bytes),
            byte_len: bytes.len() as u64,
        },
    };
    task.key = jobs::tasks::task_key(&task).unwrap();
    let clock = Arc::new(Clock::new());
    let now = clock.read().unwrap().utc_ms;
    let mut spec = RunSpec {
        version: 1,
        run_id: id("run_dispatch"),
        vault_id: id("vault_test"),
        title: "Mock dispatcher".into(),
        created_at_utc_ms: now,
        deadline_utc_ms: now + 900_000,
        scope: RunScope {
            research: None,
            operation: "embedding".into(),
            question: None,
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
        tasks: vec![task],
        prior_accounting: PriorAccounting::None,
    };
    edit(&mut spec);
    // An explicitly requested duplicate descriptor at another contained path
    // lets concurrency fixtures use distinct immutable task identities.
    for task in &spec.tasks {
        let path = fs.root().resolve(&task.input.path).unwrap();
        if !path.exists()
            && task.input.hash == hash(&bytes)
            && task.input.byte_len == bytes.len() as u64
        {
            private(&path, &bytes);
        }
    }
    for task in &mut spec.tasks {
        task.key = jobs::tasks::task_key(task).unwrap();
    }
    spec.input_fingerprint = jobs::tasks::input_fingerprint(&spec).unwrap();
    let options = JobOptions {
        clock: clock.clone(),
        fault: None,
        cancel: CancellationToken::default(),
        policy,
        lock_timeout_ms: 5000,
    };
    let job = JobLedger::new(fs.clone(), id("vault_test"), spec.run_id.clone(), options).unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    job.create(&writer, spec.clone()).unwrap();
    drop(writer);
    if !policy.offline && !policy.dry_run {
        job.start().unwrap();
    }
    let inputs = Arc::new(Inputs(AtomicUsize::new(0)));
    let runner = Arc::new(Runner {
        calls: AtomicUsize::new(0),
        hook,
    });
    let broker = Arc::new(CredentialBroker::new(CredentialOptions {
        clock: clock.clone(),
        inputs: inputs.clone(),
        runner: runner.clone(),
    }));
    Case {
        temp,
        fs,
        config,
        trusted,
        job,
        spec,
        clock,
        inputs,
        runner,
        broker,
    }
}
#[allow(
    dead_code,
    reason = "shared fixture helper used by invariance consumers"
)]
pub fn tree(path: &Path) -> Vec<(String, Vec<u8>, u128)> {
    fn walk(root: &Path, path: &Path, out: &mut Vec<(String, Vec<u8>, u128)>) {
        let mut entries = std::fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for p in entries {
            let meta = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().to_str().unwrap().to_owned();
            let time = meta
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            if meta.is_dir() {
                out.push((rel, vec![], time));
                walk(root, &p, out);
            } else {
                out.push((rel, std::fs::read(p).unwrap(), time));
            }
        }
    }
    let mut out = vec![];
    walk(path, path, &mut out);
    out
}
pub struct ZeroJitter;
impl JitterSource for ZeroJitter {
    fn sample_inclusive(&self, _: u64) -> Result<u64> {
        Ok(0)
    }
}

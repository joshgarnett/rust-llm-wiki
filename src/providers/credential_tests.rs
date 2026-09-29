use crate as library;
#[path = "../../tests/fixtures/p16/common.rs"]
mod common;
use super::credentials::*;
use crate::{
    config::providers::ProviderConfig,
    domain::*,
    jobs::{CancellationToken, Capability, ClockReading, ExecutionPolicy, JobClock},
};
use common::*;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
struct Clock {
    utc: AtomicI64,
    mono: AtomicU64,
}
impl Clock {
    fn new() -> Self {
        Self {
            utc: AtomicI64::new(1_700_000_000_000),
            mono: AtomicU64::new(10),
        }
    }
}
impl JobClock for Clock {
    fn read(&self) -> Result<ClockReading> {
        Ok(ClockReading {
            utc_ms: self.utc.load(Ordering::SeqCst),
            monotonic_ms: self.mono.load(Ordering::SeqCst),
        })
    }
}
#[derive(Default)]
struct Inputs {
    calls: AtomicUsize,
    values: Mutex<BTreeMap<String, Vec<u8>>>,
}
impl SecretInputs for Inputs {
    fn environment(&self, name: &str, max: usize) -> Result<Option<SecretBytes>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let data = self.values.lock().unwrap().get(name).cloned();
        assert_eq!(max, 16384);
        data.map(SecretBytes::new).transpose()
    }
    fn file(&self, path: &std::path::Path, max: usize) -> Result<SecretBytes> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(max, 16384);
        let data = self
            .values
            .lock()
            .unwrap()
            .get(path.to_str().unwrap())
            .cloned()
            .ok_or_else(|| WikiError::new(ErrorCode::ProviderAuth, "SECRET-FILE-ERROR"))?;
        SecretBytes::new(data)
    }
}
struct Runner {
    calls: AtomicUsize,
    bytes: Mutex<Vec<u8>>,
    delay: Duration,
    after: Option<Arc<dyn Fn() + Send + Sync>>,
    fail: bool,
}
impl Runner {
    fn new(bytes: &[u8]) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            bytes: Mutex::new(bytes.to_vec()),
            delay: Duration::ZERO,
            after: None,
            fail: false,
        }
    }
}
impl HelperRunner for Runner {
    fn run(
        &self,
        i: &HelperInvocation,
        l: &HelperLimits,
        _: &dyn JobClock,
        _: &CancellationToken,
    ) -> Result<HelperOutput> {
        assert!(i.cwd().is_absolute());
        assert_eq!(i.argv()[0], "/fixture/helper");
        assert!(l.timeout_ms <= 10000);
        assert_eq!(l.max_stdout_bytes, 16384);
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        if let Some(after) = &self.after {
            after();
        }
        if self.fail {
            let mut e = WikiError::new(ErrorCode::ProviderAuth, "SECRET-STDERR-OUTPUT");
            e.details = serde_json::json!({"stdout":"SECRET-STDOUT","argv":"SECRET-ARGV"});
            return Err(e);
        }
        HelperOutput::new(self.bytes.lock().unwrap().clone())
    }
}
fn command(mode: &str) -> String {
    format!(
        "kind=\"command\"\ncommand=[\"/fixture/helper\",\"SECRET-ARGV\"]\noutput=\"{mode}\"\ntimeout_seconds=10\nttl_seconds=300\nrefresh_skew_seconds=60"
    )
}
fn context() -> CredentialContext {
    CredentialContext {
        policy: ExecutionPolicy::default(),
        cancel: CancellationToken::default(),
        deadline_utc_ms: 1_700_000_900_000,
    }
}
fn new_broker(clock: Arc<Clock>, inputs: Arc<Inputs>, runner: Arc<Runner>) -> CredentialBroker {
    CredentialBroker::new(CredentialOptions {
        clock,
        inputs,
        runner,
    })
}

#[test]
fn offline_dry_run_before_secret_resolution_zero_dns_http_helpers() {
    for auth in [
        STATIC.to_string(),
        "kind=\"static\"\nkey_env=\"FAKE_TOKEN\"".into(),
        "kind=\"static\"\nkey_file=\"/fixture/secret\"".into(),
        command("json"),
    ] {
        let (t, fs, _path, trusted) = fixture(&auth);
        let clock = Arc::new(Clock::new());
        let inputs = Arc::new(Inputs::default());
        let runner = Arc::new(Runner::new(b"{\"key\":\"fake\"}"));
        let broker = new_broker(clock, inputs.clone(), runner.clone());
        let before = tree(t.path());
        for dry in [false, true] {
            let mut ctx = context();
            ctx.policy.offline = !dry;
            ctx.policy.dry_run = dry;
            assert_eq!(
                broker.resolve(&trusted, &fs, &ctx).err().unwrap().code,
                ErrorCode::OfflineUnavailable
            );
            assert!(broker.refresh_after_401(&trusted, &fs, 0, &ctx).is_err());
        }
        assert_eq!(inputs.calls.load(Ordering::SeqCst), 0);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
        assert_eq!(broker.cache_stats().0, 0);
        assert_eq!(tree(t.path()), before);
    }
}
fn tree(path: &std::path::Path) -> BTreeMap<String, (u64, Blake3Hash, std::time::SystemTime)> {
    fn walk(
        base: &std::path::Path,
        p: &std::path::Path,
        out: &mut BTreeMap<String, (u64, Blake3Hash, std::time::SystemTime)>,
    ) {
        let meta = std::fs::symlink_metadata(p).unwrap();
        let relative = p.strip_prefix(base).unwrap().to_str().unwrap().to_string();
        let bytes = if meta.is_file() {
            std::fs::read(p).unwrap()
        } else {
            vec![]
        };
        out.insert(
            relative,
            (
                meta.len(),
                Blake3Hash::digest(bytes),
                meta.modified().unwrap(),
            ),
        );
        if meta.is_dir() {
            for e in std::fs::read_dir(p).unwrap() {
                walk(base, &e.unwrap().path(), out);
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(path, path, &mut result);
    result
}
#[test]
fn cloned_or_changed_trust_refuses_before_input_or_helper() {
    let (t, fs, path, trusted) = fixture(&command("json"));
    let clone = vault(&t.path().join("clone"));
    let clock = Arc::new(Clock::new());
    let inputs = Arc::new(Inputs::default());
    let runner = Arc::new(Runner::new(b"{\"key\":\"fake\"}"));
    let broker = new_broker(clock, inputs.clone(), runner.clone());
    assert!(broker.resolve(&trusted, &clone, &context()).is_err());
    private_write(&path, document(fs.root().path(), STATIC));
    assert!(broker.resolve(&trusted, &fs, &context()).is_err());
    assert_eq!(inputs.calls.load(Ordering::SeqCst), 0);
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn bounded_static_env_file_missing_empty_and_control_tokens() {
    for value in [
        vec![],
        b"bad\nsecret".to_vec(),
        b"valid-secret".to_vec(),
        vec![b'x'; 16385],
    ] {
        let (_t, fs, _path, trusted) = fixture("kind=\"static\"\nkey_env=\"FAKE_TOKEN\"");
        let inputs = Arc::new(Inputs::default());
        inputs
            .values
            .lock()
            .unwrap()
            .insert("FAKE_TOKEN".into(), value.clone());
        let runner = Arc::new(Runner::new(b"unused"));
        let broker = new_broker(Arc::new(Clock::new()), inputs.clone(), runner.clone());
        let result = broker.resolve(&trusted, &fs, &context());
        if value == b"valid-secret" {
            let lease = result.unwrap();
            assert_eq!(lease.headers()[0].1.as_bytes(), b"Bearer valid-secret");
        } else {
            assert!(result.is_err());
        }
        assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
    }
    let (t, fs, path, _) = fixture(STATIC);
    let key = t.path().join("key");
    private_write(&key, b"file-token\r\n");
    private_write(
        &path,
        document(
            fs.root().path(),
            &format!("kind=\"static\"\nkey_file={}", quote(key.to_str().unwrap())),
        ),
    );
    let trusted = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap();
    let clock = Arc::new(Clock::new());
    let broker = CredentialBroker::new(CredentialOptions {
        clock,
        inputs: Arc::new(NativeSecretInputs),
        runner: Arc::new(NativeHelperRunner),
    });
    assert_eq!(
        broker.resolve(&trusted, &fs, &context()).unwrap().headers()[0]
            .1
            .as_bytes(),
        b"Bearer file-token"
    );
}
#[test]
fn helper_expiry_size_control_stderr_redaction_and_one_401_refresh() {
    let (_t, fs, _path, trusted) = fixture(&command("json"));
    let clock = Arc::new(Clock::new());
    let inputs = Arc::new(Inputs::default());
    let runner = Arc::new(Runner::new(
        b"{\"key\":\"first\",\"expires_at\":\"2023-11-14T22:23:20Z\"}",
    ));
    let broker = new_broker(clock, inputs, runner.clone());
    let first = broker.resolve(&trusted, &fs, &context()).unwrap();
    assert_eq!(first.valid_until_utc_ms(), 1_700_000_300_000);
    assert_eq!(
        broker.resolve(&trusted, &fs, &context()).unwrap().epoch(),
        first.epoch()
    );
    assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
    *runner.bytes.lock().unwrap() = b"{\"key\":\"second\"}".to_vec();
    let second = broker
        .refresh_after_401(&trusted, &fs, first.epoch(), &context())
        .unwrap();
    assert!(second.epoch() > first.epoch());
    assert_eq!(second.headers()[0].1.as_bytes(), b"Bearer second");
    assert_eq!(
        broker
            .refresh_after_401(&trusted, &fs, first.epoch(), &context())
            .unwrap()
            .epoch(),
        second.epoch()
    );
    assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
    for output in [
        b"{\"key\":\"SECRET-STDOUT\",\"expires_at\":\"bad\"}".to_vec(),
        b"{\"key\":\"x\",\"expires_at\":\"2023-11-14T22:13:30Z\"}".to_vec(),
        b"{\"key\":\"x\",\"key\":\"SECRET-STDOUT\"}".to_vec(),
        b"{\"key\":\"x\",\"extra\":\"SECRET-STDOUT\"}".to_vec(),
        b"{\"key\":\"bad\\nsecret\"}".to_vec(),
        vec![b'x'; 16385],
    ] {
        let runner = Arc::new(Runner::new(&output));
        let broker = new_broker(Arc::new(Clock::new()), Arc::new(Inputs::default()), runner);
        let error = broker.resolve(&trusted, &fs, &context()).err().unwrap();
        let json = serde_json::to_string(&error).unwrap();
        assert!(!json.contains("SECRET-STDOUT"));
        assert!(!json.contains("SECRET-ARGV"));
    }
    let mut runner = Runner::new(b"unused");
    runner.fail = true;
    let broker = new_broker(
        Arc::new(Clock::new()),
        Arc::new(Inputs::default()),
        Arc::new(runner),
    );
    let error =
        serde_json::to_string(&broker.resolve(&trusted, &fs, &context()).err().unwrap()).unwrap();
    assert!(!error.contains("SECRET"));
}
#[test]
fn static_401_does_not_refresh_and_text_removes_only_one_terminal_newline() {
    let (_t, fs, _p, static_service) = fixture(STATIC);
    let runner = Arc::new(Runner::new(b"unused"));
    let broker = new_broker(
        Arc::new(Clock::new()),
        Arc::new(Inputs::default()),
        runner.clone(),
    );
    assert!(
        broker
            .refresh_after_401(&static_service, &fs, 0, &context())
            .is_err()
    );
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
    let (_t, fs, _p, trusted) = fixture(&command("text"));
    for data in [
        b"token\n".as_slice(),
        b"token\r\n",
        b"token\n\n",
        b"token\t",
        b"",
    ] {
        let broker = new_broker(
            Arc::new(Clock::new()),
            Arc::new(Inputs::default()),
            Arc::new(Runner::new(data)),
        );
        let result = broker.resolve(&trusted, &fs, &context());
        if data == b"token\n" || data == b"token\r\n" {
            assert_eq!(result.unwrap().headers()[0].1.as_bytes(), b"Bearer token");
        } else {
            assert!(result.is_err());
        }
    }
}
#[test]
fn process_refreshes_serialize_and_waiters_reuse_fresh_epoch() {
    let (_t, fs, _p, trusted) = fixture(&command("json"));
    let mut runner = Runner::new(b"{\"key\":\"one\"}");
    runner.delay = Duration::from_millis(75);
    let runner = Arc::new(runner);
    let broker = Arc::new(new_broker(
        Arc::new(Clock::new()),
        Arc::new(Inputs::default()),
        runner.clone(),
    ));
    std::thread::scope(|scope| {
        let mut handles = vec![];
        for _ in 0..8 {
            let b = broker.clone();
            let s = &trusted;
            let f = &fs;
            handles.push(scope.spawn(move || b.resolve(s, f, &context()).unwrap().epoch()));
        }
        let epochs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(epochs.iter().all(|e| *e == epochs[0]));
    });
    assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn cancellation_clock_and_trust_changes_after_helper_return_no_lease_or_cache() {
    for mode in 0..4 {
        let (_t, fs, path, trusted) = fixture(&command("json"));
        let clock = Arc::new(Clock::new());
        let mut ctx = context();
        ctx.deadline_utc_ms = 1_700_000_000_100;
        let cancel = ctx.cancel.clone();
        let c = clock.clone();
        let p = path.clone();
        let root = fs.root().path().to_path_buf();
        let mut runner = Runner::new(b"{\"key\":\"one\"}");
        runner.after = Some(Arc::new(move || match mode {
            0 => cancel.cancel(),
            1 => {
                c.utc.store(1_700_000_000_100, Ordering::SeqCst);
            }
            2 => {
                c.mono.store(9, Ordering::SeqCst);
            }
            _ => private_write(&p, document(&root, STATIC)),
        }));
        let broker = new_broker(clock, Arc::new(Inputs::default()), Arc::new(runner));
        assert!(broker.resolve(&trusted, &fs, &ctx).is_err());
        assert_eq!(broker.cache_stats().1, 0);
    }
}
#[test]
fn expired_or_regressed_cache_and_offline_warm_cache_cannot_bypass_policy() {
    let (_t, fs, _p, trusted) = fixture(&command("json"));
    let clock = Arc::new(Clock::new());
    let runner = Arc::new(Runner::new(b"{\"key\":\"one\"}"));
    let broker = new_broker(clock.clone(), Arc::new(Inputs::default()), runner.clone());
    let first = broker.resolve(&trusted, &fs, &context()).unwrap();
    let mut offline = context();
    offline.policy.offline = true;
    assert!(broker.resolve(&trusted, &fs, &offline).is_err());
    assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
    clock.utc.store(1_699_999_999_999, Ordering::SeqCst);
    assert!(broker.resolve(&trusted, &fs, &context()).is_err());
    clock.utc.store(1_700_000_300_001, Ordering::SeqCst);
    clock.mono.store(300_011, Ordering::SeqCst);
    let second = broker.resolve(&trusted, &fs, &context()).unwrap();
    assert!(second.epoch() > first.epoch());
    assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
}
#[test]
fn cached_command_still_resolves_rotated_extra_secret_headers() {
    let (_t, fs, path, _) = fixture(&command("json"));
    let doc = document(fs.root().path(), &command("json")).replace(
        "[services.embed.auth]",
        "secret_headers={X-Extra={kind=\"env\",name=\"EXTRA\"}}\n[services.embed.auth]",
    );
    private_write(&path, doc);
    let trusted = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap();
    let inputs = Arc::new(Inputs::default());
    inputs
        .values
        .lock()
        .unwrap()
        .insert("EXTRA".into(), b"old".to_vec());
    let runner = Arc::new(Runner::new(b"{\"key\":\"one\"}"));
    let broker = new_broker(Arc::new(Clock::new()), inputs.clone(), runner.clone());
    assert_eq!(
        broker.resolve(&trusted, &fs, &context()).unwrap().headers()[1]
            .1
            .as_bytes(),
        b"old"
    );
    inputs
        .values
        .lock()
        .unwrap()
        .insert("EXTRA".into(), b"new".to_vec());
    assert_eq!(
        broker.resolve(&trusted, &fs, &context()).unwrap().headers()[1]
            .1
            .as_bytes(),
        b"new"
    );
    assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn fresh_command_header_lookup_cannot_return_expired_or_regressed_credentials() {
    struct AdvancingInputs {
        clock: Arc<Clock>,
        mode: u8,
    }
    impl SecretInputs for AdvancingInputs {
        fn environment(&self, name: &str, max: usize) -> Result<Option<SecretBytes>> {
            assert_eq!(name, "EXTRA");
            assert_eq!(max, 16384);
            match self.mode {
                // UTC need not advance with the monotonic clock. The token's
                // completed-helper TTL must still expire before its first lease.
                0 => self.clock.mono.store(300_011, Ordering::SeqCst),
                1 => self.clock.utc.store(1_700_000_900_000, Ordering::SeqCst),
                _ => self.clock.mono.store(9, Ordering::SeqCst),
            }
            SecretBytes::new(b"extra-header".to_vec()).map(Some)
        }
        fn file(&self, _: &std::path::Path, _: usize) -> Result<SecretBytes> {
            panic!("fixture has no file credential source")
        }
    }
    for mode in 0..3 {
        let (_t, fs, path, _) = fixture(&command("json"));
        private_write(
            &path,
            document(fs.root().path(), &command("json")).replace(
                "[services.embed.auth]",
                "secret_headers={X-Extra={kind=\"env\",name=\"EXTRA\"}}\n[services.embed.auth]",
            ),
        );
        let trusted = ProviderConfig::load(&path)
            .unwrap()
            .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
            .unwrap();
        let clock = Arc::new(Clock::new());
        let runner = Arc::new(Runner::new(b"{\"key\":\"one\"}"));
        let broker = CredentialBroker::new(CredentialOptions {
            clock: clock.clone(),
            inputs: Arc::new(AdvancingInputs { clock, mode }),
            runner: runner.clone(),
        });
        let error = broker.resolve(&trusted, &fs, &context()).err().unwrap();
        assert_eq!(
            error.code,
            if mode == 0 {
                ErrorCode::ProviderAuth
            } else {
                ErrorCode::BudgetExceeded
            },
            "mode {mode}"
        );
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(broker.cache_stats().1, 0);
    }
}
#[test]
fn post_resolve_command_lease_checks_both_clocks_trust_and_cached_acquisition() {
    for cached in [false, true] {
        for mode in 0..7 {
            let (_t, fs, path, trusted) = fixture(&command("json"));
            let clock = Arc::new(Clock::new());
            let runner = Arc::new(Runner::new(b"{\"key\":\"one\"}"));
            let broker = new_broker(clock.clone(), Arc::new(Inputs::default()), runner.clone());
            let first = broker.resolve(&trusted, &fs, &context()).unwrap();
            broker
                .validate_lease(&trusted, &fs, &first, &context())
                .unwrap();
            first.check_validity().unwrap();
            let lease = if cached {
                clock.mono.store(200_010, Ordering::SeqCst);
                let cached = broker.resolve(&trusted, &fs, &context()).unwrap();
                assert_eq!(cached.epoch(), first.epoch());
                assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
                cached
            } else {
                first
            };
            let expected = match mode {
                0 => {
                    // Fixed UTC cannot extend the original TTL, including when
                    // another lease was obtained from the cache later.
                    clock.mono.store(300_011, Ordering::SeqCst);
                    ErrorCode::ProviderAuth
                }
                1 => {
                    clock.utc.store(1_700_000_300_001, Ordering::SeqCst);
                    ErrorCode::ProviderAuth
                }
                2 => {
                    clock.utc.store(1_700_000_900_000, Ordering::SeqCst);
                    ErrorCode::BudgetExceeded
                }
                3 => {
                    clock.mono.store(9, Ordering::SeqCst);
                    ErrorCode::BudgetExceeded
                }
                4 => {
                    clock.utc.store(1_699_999_999_999, Ordering::SeqCst);
                    ErrorCode::BudgetExceeded
                }
                5 => {
                    private_write(&path, document(fs.root().path(), STATIC));
                    ErrorCode::ProfileUntrusted
                }
                _ => {
                    std::fs::write(
                        fs.root().path().join("WIKI.md"),
                        b"---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Changed marker\n---\n",
                    )
                    .unwrap();
                    ErrorCode::ProfileUntrusted
                }
            };
            let error = broker
                .validate_lease(&trusted, &fs, &lease, &context())
                .unwrap_err();
            assert_eq!(error.code, expected, "cached {cached}, mode {mode}");
            if mode <= 4 {
                assert_eq!(
                    lease.check_validity().unwrap_err().code,
                    if mode <= 2 {
                        ErrorCode::ProviderAuth
                    } else {
                        ErrorCode::BudgetExceeded
                    },
                    "native entry clock check, cached {cached}, mode {mode}"
                );
            }
            assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        }
    }
}
#[test]
fn static_lease_has_a_monotonic_deadline_and_cannot_cross_services() {
    let (_t, fs, path, _) = fixture(STATIC);
    let doc = document(fs.root().path(), STATIC)
        .replace(
            "[vault_bindings.main]",
            "[profiles.secondary]\nembedding=\"other\"\n[services.other]\nadapter=\"embeddings-v1\"\nurl=\"https://other.example/v1/embeddings\"\nmodel=\"other-model\"\n[services.other.auth]\nkind=\"static\"\nkey=\"other-secret\"\n[vault_bindings.main]",
        )
        .replace(
            "allowed_profiles = [\"primary\"]",
            "allowed_profiles = [\"primary\",\"secondary\"]",
        );
    private_write(&path, doc);
    let config = ProviderConfig::load(&path).unwrap();
    let primary = config
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap();
    let secondary = config
        .authorize(&fs, &id("vault_test"), "secondary", Capability::Embed)
        .unwrap();
    let clock = Arc::new(Clock::new());
    let runner = Arc::new(Runner::new(b"unused"));
    let broker = new_broker(clock.clone(), Arc::new(Inputs::default()), runner.clone());
    let lease = broker.resolve(&primary, &fs, &context()).unwrap();
    broker
        .validate_lease(&primary, &fs, &lease, &context())
        .unwrap();
    assert_eq!(
        broker
            .validate_lease(&secondary, &fs, &lease, &context())
            .unwrap_err()
            .code,
        ErrorCode::ProviderAuth
    );
    clock.mono.store(900_011, Ordering::SeqCst);
    assert_eq!(
        broker
            .validate_lease(&primary, &fs, &lease, &context())
            .unwrap_err()
            .code,
        ErrorCode::ProviderAuth
    );
    assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn exact_service_url_is_preserved_without_inferred_path() {
    let (_t, _fs, _p, trusted) = fixture(STATIC);
    assert_eq!(
        trusted.service().url,
        "https://gateway.example/v1/embeddings?tenant=one"
    );
    assert!(!trusted.service().allow_loopback_http);
}

#[cfg(unix)]
fn script(t: &tempfile::TempDir, name: &str, text: &str) -> std::path::PathBuf {
    let path = t.path().join(name);
    private_write(&path, text);
    path
}
#[cfg(unix)]
#[test]
fn native_helper_executes_explicit_interpreter_with_closed_stdin_and_exact_argv() {
    let t = tempfile::tempdir().unwrap();
    let file = script(
        &t,
        "helper.sh",
        "if IFS= read -r value; then exit 1; fi\nprintf '%s' \"$1\"\nprintf 'SECRET-STDERR' >&2\n",
    );
    let argv = vec![
        "/bin/sh".into(),
        file.to_str().unwrap().into(),
        "quoted space\\\"argument".into(),
    ];
    let i = HelperInvocation::new(argv, t.path().to_path_buf()).unwrap();
    let clock = NativeCredentialClock::default();
    let now = clock.read().unwrap();
    let out = NativeHelperRunner
        .run(
            &i,
            &HelperLimits {
                timeout_ms: 1000,
                max_stdout_bytes: 16384,
                deadline_utc_ms: now.utc_ms + 5000,
            },
            &clock,
            &CancellationToken::default(),
        )
        .unwrap();
    assert_eq!(out.stdout(), b"quoted space\\\"argument");
}

#[cfg(unix)]
#[test]
fn native_broker_helper_json_cache_and_offline_warm_cache_do_not_leak_or_relaunch() {
    let (t, fs, path, _) = fixture(STATIC);
    let counter = t.path().join("launches");
    let text = format!(
        "printf x >> {}\nprintf '{{\"key\":\"native-fixture-token\"}}'\nprintf 'SECRET-STDERR' >&2\n",
        quote(counter.to_str().unwrap())
    );
    let file = script(&t, "broker.sh", &text);
    let auth = format!(
        "kind=\"command\"\ncommand=[\"/bin/sh\",{}]\noutput=\"json\"",
        quote(file.to_str().unwrap())
    );
    private_write(&path, document(fs.root().path(), &auth));
    let trusted = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap();
    let clock = Arc::new(NativeCredentialClock::default());
    let ctx = CredentialContext {
        policy: ExecutionPolicy::default(),
        cancel: CancellationToken::default(),
        deadline_utc_ms: clock.read().unwrap().utc_ms + 5000,
    };
    let broker = CredentialBroker::new(CredentialOptions {
        clock,
        inputs: Arc::new(NativeSecretInputs),
        runner: Arc::new(NativeHelperRunner),
    });
    let lease = broker.resolve(&trusted, &fs, &ctx).unwrap();
    assert_eq!(
        lease.headers()[0].1.as_bytes(),
        b"Bearer native-fixture-token"
    );
    assert_eq!(
        broker.resolve(&trusted, &fs, &ctx).unwrap().epoch(),
        lease.epoch()
    );
    assert_eq!(std::fs::read(&counter).unwrap(), b"x");
    let before = tree(t.path());
    let offline = CredentialContext {
        policy: ExecutionPolicy {
            offline: true,
            ..ExecutionPolicy::default()
        },
        cancel: CancellationToken::default(),
        deadline_utc_ms: ctx.deadline_utc_ms,
    };
    assert!(broker.resolve(&trusted, &fs, &offline).is_err());
    assert_eq!(tree(t.path()), before);
}

#[test]
fn helper_limits_ttl_args_and_private_prefixes_are_validated_without_secret_hashes() {
    let (_t, fs, path, before) = fixture(STATIC);
    for auth in [
        command("json").replace("timeout_seconds=10", "timeout_seconds=11"),
        command("json").replace("ttl_seconds=300", "ttl_seconds=3601"),
        command("json").replace("refresh_skew_seconds=60", "refresh_skew_seconds=300"),
        command("json").replace("/fixture/helper", "relative-helper"),
        command("json").replace("/fixture/helper", "/fixture/helper.cmd"),
        format!(
            "kind=\"command\"\ncommand=[\"/fixture/helper\",{}]\noutput=\"json\"",
            quote(&"x".repeat(4097))
        ),
    ] {
        private_write(&path, document(fs.root().path(), &auth));
        assert!(ProviderConfig::load(&path).is_err());
    }
    private_write(
        &path,
        document(
            fs.root().path(),
            "kind=\"static\"\nkey=\"changed-secret\"\nprefix=\"private-prefix-secret\"",
        ),
    );
    let after = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap()
        .summary();
    assert_eq!(
        before.summary().config_fingerprint,
        after.config_fingerprint
    );
}
#[cfg(unix)]
#[test]
fn native_helper_descendant_held_stdout_returns_bounded_and_kills_group() {
    let t = tempfile::tempdir().unwrap();
    let survived = t.path().join("survived");
    let text = format!(
        "(/bin/sleep 1; printf escaped > {}) &\nprintf key\nexit 0\n",
        quote(survived.to_str().unwrap())
    );
    let file = script(&t, "descendant.sh", &text);
    let i = HelperInvocation::new(
        vec!["/bin/sh".into(), file.to_str().unwrap().into()],
        t.path().to_path_buf(),
    )
    .unwrap();
    let clock = NativeCredentialClock::default();
    let now = clock.read().unwrap();
    let started = Instant::now();
    let result = NativeHelperRunner.run(
        &i,
        &HelperLimits {
            timeout_ms: 150,
            max_stdout_bytes: 16384,
            deadline_utc_ms: now.utc_ms + 5000,
        },
        &clock,
        &CancellationToken::default(),
    );
    assert_eq!(result.err().unwrap().code, ErrorCode::BudgetExceeded);
    assert!(started.elapsed() < Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(1100));
    assert!(!survived.exists());
}
#[cfg(unix)]
#[test]
fn native_helper_output_ceiling_timeout_cancel_and_stderr_are_bounded() {
    let t = tempfile::tempdir().unwrap();
    for (name, text) in [
        (
            "oversize.sh",
            "printf 'SECRET-STDERR' >&2\nwhile :; do printf 1234567890; done\n",
        ),
        ("sleep.sh", "/bin/sleep 30\n"),
    ] {
        let file = script(&t, name, text);
        let i = HelperInvocation::new(
            vec!["/bin/sh".into(), file.to_str().unwrap().into()],
            t.path().to_path_buf(),
        )
        .unwrap();
        let clock = NativeCredentialClock::default();
        let now = clock.read().unwrap();
        let started = Instant::now();
        let error = NativeHelperRunner
            .run(
                &i,
                &HelperLimits {
                    timeout_ms: 150,
                    max_stdout_bytes: 128,
                    deadline_utc_ms: now.utc_ms + 5000,
                },
                &clock,
                &CancellationToken::default(),
            )
            .err()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!serde_json::to_string(&error).unwrap().contains("SECRET"));
    }
    let file = script(&t, "cancel.sh", "/bin/sleep 30\n");
    let i = HelperInvocation::new(
        vec!["/bin/sh".into(), file.to_str().unwrap().into()],
        t.path().to_path_buf(),
    )
    .unwrap();
    let clock = NativeCredentialClock::default();
    let now = clock.read().unwrap();
    let cancel = CancellationToken::default();
    std::thread::scope(|scope| {
        let c = cancel.clone();
        scope.spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            c.cancel();
        });
        assert_eq!(
            NativeHelperRunner
                .run(
                    &i,
                    &HelperLimits {
                        timeout_ms: 1000,
                        max_stdout_bytes: 128,
                        deadline_utc_ms: now.utc_ms + 5000
                    },
                    &clock,
                    &cancel
                )
                .err()
                .unwrap()
                .code,
            ErrorCode::Cancelled
        );
    });
}

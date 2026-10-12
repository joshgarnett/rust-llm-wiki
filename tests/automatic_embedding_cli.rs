#[path = "../test_support/paths.rs"]
mod test_paths;

use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    fail: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Mock {
    fn new() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/embeddings", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let fail = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (captured, failing, stopping) = (requests.clone(), fail.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let started = Instant::now();
                let request = loop {
                    assert!(started.elapsed() < Duration::from_secs(6));
                    let read = stream.read(&mut buffer).unwrap();
                    assert_ne!(read, 0);
                    bytes.extend_from_slice(&buffer[..read]);
                    assert!(bytes.len() < 2 * 1024 * 1024);
                    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break serde_json::from_slice::<Value>(
                                &bytes[end + 4..end + 4 + length],
                            )
                            .unwrap();
                        }
                    }
                };
                let inputs = request["input"].as_array().unwrap();
                let count = inputs.len();
                captured.lock().unwrap().push(request);
                let (status, response) = if failing.load(Ordering::SeqCst) {
                    (
                        "503 Service Unavailable",
                        json!({"error":"fixture provider failure"}),
                    )
                } else {
                    (
                        "200 OK",
                        json!({"model":"test-model","data":(0..count).map(|index|
                        json!({"index":index,"embedding":[1.0,0.0]})).collect::<Vec<_>>(),
                        "usage":{"prompt_tokens":count,"total_tokens":count}}),
                    )
                };
                let body = serde_json::to_vec(&response).unwrap();
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            url,
            requests,
            fail,
            stop,
            worker: Some(worker),
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    preferences: PathBuf,
    providers: PathBuf,
    mock: Mock,
}
impl Fixture {
    fn new(automatic: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let root = base.join("my wiki");
        let preferences = base.join("settings.toml");
        let providers = base.join("providers.toml");
        let mock = Mock::new();
        private_write(
            &providers,
            format!(
                "version=1\n[profiles.notes]\nembedding='embed'\n[services.embed]\nadapter='embeddings-v1'\nurl={}\nallow_loopback_http=true\nmodel='test-model'\nrevision='test-r1'\ndimensions=2\nmax_batch_items=32\n[services.embed.auth]\nkind='static'\nkey='disposable-fixture-key'\n[vault_bindings]\n",
                json!(mock.url)
            ),
        );
        let fixture = Self {
            _temp: temp,
            root,
            preferences,
            providers,
            mock,
        };
        let mut args = vec![
            "--offline",
            "--profile",
            "notes",
            "setup",
            fixture.root.to_str().unwrap(),
            "--non-interactive",
            "--providers-file",
            fixture.providers.to_str().unwrap(),
        ];
        args.push(if automatic {
            "--auto-embed"
        } else {
            "--grant-embed"
        });
        let setup = fixture.ok(&args);
        assert_eq!(setup["data"]["completion"], "complete");
        assert_eq!(fixture.mock.count(), 0);
        fixture
    }
    fn invoke(&self, arguments: &[&str]) -> (i32, Value) {
        let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .args(["--json", "--preferences"])
            .arg(&self.preferences)
            .args(["--wiki"])
            .arg(&self.root)
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{arguments:?}: {error}: {} / {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().unwrap(), value)
    }
    fn ok(&self, arguments: &[&str]) -> Value {
        let (code, output) = self.invoke(arguments);
        assert_eq!(code, 0, "{arguments:?}: {output}");
        assert_eq!(output["ok"], true);
        output
    }
    fn input(&self, name: &str, body: &str) -> PathBuf {
        let path = self._temp.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
    }
}
fn private_write(path: &Path, text: impl AsRef<[u8]>) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn fresh_setup_save_default_hybrid_and_repeated_query_reuse_actual_requests() {
    let fixture = Fixture::new(true);
    let input = fixture.input(
        "my note.md",
        "# Schedule\nThe cedar workshop starts at 14:30 in room Indigo.\n",
    );
    let save = fixture.ok(&[
        "source",
        "add",
        input.to_str().unwrap(),
        "--title",
        "Workshop",
    ]);
    assert_eq!(save["data"]["status"], "committed");
    assert_eq!(
        save["data"]["automatic_embeddings"]["state"], "ready",
        "{save}"
    );
    assert!(fixture.mock.count() > 0);
    let query = "When is the cedar workshop?";
    let found = fixture.ok(&["search", query]);
    assert!(
        found["data"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit.to_string().contains("14:30")),
        "{found}"
    );
    assert_ne!(
        found["data"]["automatic_lookup"]["returned_mode"], "lexical",
        "{found}"
    );
    assert_eq!(found["meta"]["network_used"], true);
    let sent = fixture.mock.count();
    let repeated = fixture.ok(&["search", query]);
    assert_eq!(fixture.mock.count(), sent);
    assert_eq!(repeated["meta"]["network_used"], false);
    assert!(!repeated["data"]["hits"].as_array().unwrap().is_empty());
    let context = fixture.ok(&["context", query]);
    assert!(context["data"].to_string().contains("14:30"), "{context}");
    assert_eq!(fixture.mock.count(), sent);
}

#[test]
fn old_grants_explicit_lexical_offline_no_sync_and_preview_never_dispatch() {
    for automatic in [false, true] {
        let fixture = Fixture::new(automatic);
        let input = fixture.input("note.md", "# Cedar\nCedar workshop at 14:30.\n");
        fixture.ok(&["--offline", "source", "add", input.to_str().unwrap()]);
        fixture.ok(&["search", "cedar", "--mode", "lexical"]);
        fixture.ok(&["search", "cedar", "--mode", "literal"]);
        fixture.ok(&["--offline", "search", "cedar"]);
        fixture.ok(&["search", "cedar", "--no-sync"]);
        fixture.ok(&["--dry-run", "search", "cedar"]);
        fixture.ok(&["--dry-run", "context", "cedar"]);
        if !automatic {
            fixture.ok(&["search", "cedar"]);
        }
        assert_eq!(fixture.mock.count(), 0);
    }
}

#[test]
fn optional_provider_failure_keeps_the_committed_save_and_local_lookup() {
    let fixture = Fixture::new(true);
    fixture.mock.fail.store(true, Ordering::SeqCst);
    let input = fixture.input("note.md", "Cedar workshop at 14:30.\n");
    let save = fixture.ok(&["source", "add", input.to_str().unwrap()]);
    assert_eq!(save["data"]["status"], "committed");
    assert_eq!(save["meta"]["network_used"], true);
    let sent = fixture.mock.count();
    let found = fixture.ok(&["search", "cedar"]);
    assert_eq!(
        found["data"]["automatic_lookup"]["returned_mode"],
        "lexical"
    );
    assert!(!found["data"]["hits"].as_array().unwrap().is_empty());
    assert_eq!(
        fixture.mock.count(),
        sent,
        "failed paid work must not be silently replaced"
    );
}

#[test]
fn staged_and_failed_saves_do_not_start_automatic_work() {
    let fixture = Fixture::new(true);
    let input = fixture.input("note.md", "Cedar workshop at 14:30.\n");
    let staged = fixture.ok(&["--stage", "source", "add", input.to_str().unwrap()]);
    assert_eq!(staged["data"]["status"], "prepared");
    assert_eq!(fixture.mock.count(), 0);
    let (code, _) = fixture.invoke(&["source", "add", "/missing/disposable-fixture-input"]);
    assert_ne!(code, 0);
    assert_eq!(fixture.mock.count(), 0);
}

#[test]
fn one_multi_page_command_shares_one_automatic_request_allowance() {
    let fixture = Fixture::new(true);
    let text = std::fs::read_to_string(&fixture.providers)
        .unwrap()
        .replace("max_batch_items=32", "max_batch_items=1");
    private_write(&fixture.providers, text);
    let pages = (0..8).map(|index| json!({
        "path":format!("knowledge/pages/page-{index}.md"),
        "markdown":format!("---\nwiki_schema: '1'\nwiki_id: Page.Batch{index}\nwiki_kind: page\ntitle: Batch {index}\nwiki_status: reviewed\n---\nWorkshop fact {index}.\n")
    })).collect::<Vec<_>>();
    let request = fixture.input(
        "page-batch.json",
        &json!({"title":"Save workshop notes","pages":pages}).to_string(),
    );
    let saved = fixture.ok(&["page", "batch", "--file", request.to_str().unwrap()]);
    assert_eq!(saved["data"]["status"], "committed");
    assert!(fixture.mock.count() > 0, "{saved}");
    assert!(
        fixture.mock.count() <= 2,
        "per-item preparation multiplied the allowance: {saved}"
    );
    assert!(saved["data"]["automatic_embeddings"].is_object());
}

#[test]
fn writer_denial_returns_useful_local_search_and_context_without_transport() {
    let fixture = Fixture::new(true);
    let input = fixture.input("note.md", "Cedar workshop at 14:30.\n");
    fixture.ok(&["--offline", "source", "add", input.to_str().unwrap()]);
    let root = lwiki::vault::VaultRoot::explicit(&fixture.root).unwrap();
    let writer = lwiki::vault::WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let search = fixture.ok(&["search", "cedar"]);
    assert!(!search["data"]["hits"].as_array().unwrap().is_empty());
    assert_eq!(
        search["data"]["automatic_lookup"]["returned_mode"],
        "lexical"
    );
    let context = fixture.ok(&["context", "cedar"]);
    assert!(context["data"].to_string().contains("14:30"));
    assert_eq!(fixture.mock.count(), 0);
    drop(writer);
}

#[cfg(unix)]
#[test]
fn skip_modes_do_not_execute_saved_credential_helpers() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(true);
    let helper = fixture._temp.path().join("credential-helper");
    let sentinel = fixture._temp.path().join("helper-ran");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\ntouch '{}'\nprintf fixture-key\n",
            sentinel.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let text = std::fs::read_to_string(&fixture.providers).unwrap()
        .replace("kind='static'\nkey='disposable-fixture-key'", &format!("kind='command'\ncommand=[{}]\noutput='text'\nttl_seconds=300\nrefresh_skew_seconds=60", json!(helper)));
    private_write(&fixture.providers, text);
    let input = fixture.input("note.md", "Cedar workshop at 14:30.\n");
    fixture.ok(&["--offline", "source", "add", input.to_str().unwrap()]);
    for args in [
        vec!["search", "cedar", "--mode", "lexical"],
        vec!["search", "cedar", "--mode", "literal"],
        vec!["--offline", "search", "cedar"],
        vec!["search", "cedar", "--no-sync"],
        vec!["--dry-run", "search", "cedar"],
        vec!["--dry-run", "embeddings", "sync"],
    ] {
        fixture.ok(&args);
    }
    assert!(!sentinel.exists());
    assert_eq!(fixture.mock.count(), 0);
}

#[test]
fn explicit_sync_preview_renders_exact_inputs_on_both_layouts_without_dispatch() {
    for normalized in [false, true] {
        let fixture = Fixture::new(true);
        let input = fixture.input("note.md", "# Cedar\nCedar workshop at 14:30.\n");
        fixture.ok(&["--offline", "source", "add", input.to_str().unwrap()]);
        if normalized {
            fixture.ok(&["--offline", "index", "rebuild", "--normalized"]);
        }
        let preview = fixture.ok(&["--dry-run", "embeddings", "sync"]);
        let payload = &preview["data"]["payload_preview"];
        assert_eq!(payload["state"], "complete", "{preview}");
        assert_eq!(
            payload["layout"],
            if normalized { "normalized" } else { "legacy" }
        );
        assert!(payload["eligible_rendered_inputs"].as_u64().unwrap() > 0);
        assert!(payload["rendered_utf8_bytes"].as_u64().unwrap() > 0);
        assert!(payload["batches"][0]["request_bytes"].as_u64().unwrap() > 0);
        assert!(payload["actual_pending_inputs"].is_null());
        assert_eq!(preview["meta"]["network_used"], false);
        assert_eq!(fixture.mock.count(), 0);
        assert!(!payload.to_string().contains("14:30"));
    }
}

#[test]
fn changed_endpoint_requires_new_automatic_permission() {
    let fixture = Fixture::new(true);
    let input = fixture.input("note.md", "Cedar workshop at 14:30.\n");
    fixture.ok(&["--offline", "source", "add", input.to_str().unwrap()]);
    let changed = format!("{}?deployment=changed", fixture.mock.url);
    fixture.ok(&[
        "--offline",
        "config",
        "set",
        "embedding",
        "--url",
        &changed,
        "--grant-provider",
        "--non-interactive",
    ]);
    let show = fixture.ok(&["config", "show"]);
    assert!(
        show["data"]["provider"]["embedding"]["automatic_embedding_consent"]
            .get("binding_changed")
            .is_some(),
        "{show}"
    );
    let found = fixture.ok(&["search", "cedar"]);
    assert_eq!(
        found["data"]["automatic_lookup"]["returned_mode"],
        "lexical"
    );
    assert_eq!(fixture.mock.count(), 0);
}

#[test]
fn invalid_default_lookup_is_rejected_before_preparation() {
    let fixture = Fixture::new(true);
    let input = fixture.input("note.md", "Cedar workshop at 14:30.\n");
    fixture.ok(&["--offline", "source", "add", input.to_str().unwrap()]);
    for command in ["search", "context"] {
        let (code, _) = fixture.invoke(&[command, ""]);
        assert_ne!(code, 0);
    }
    assert_eq!(fixture.mock.count(), 0);
}

//! One vault crosses the maintained host recipe and all local M0–M4 CLI phases.
//! Every paid request targets an explicit trusted loopback mock with a fake key.
#![cfg(unix)]
#[path = "../test_support/paths.rs"]
mod test_paths;
use lwiki as library;
use lwiki::{domain::*, jobs::*, vault::*};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod provider;
struct Vault {
    temp: tempfile::TempDir,
    fs: VaultFs,
    options: JobOptions,
}
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
impl Vault {
    fn new() -> Self {
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        std::fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: \"1\"\nwiki_id: Vault.Portable\nwiki_kind: vault\ntitle: Integrated local acceptance\n---\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        Self {
            temp,
            fs,
            options: lwiki::app::remote::native_job_options(ExecutionPolicy::default(), 5000),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
struct TreeEntry {
    kind: &'static str,
    mode: u32,
    hash: Blake3Hash,
}
fn tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };
        #[cfg(not(unix))]
        let mode = u32::from(metadata.permissions().readonly());
        let (kind, hash) = if metadata.is_dir() {
            pending.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            ("directory", Blake3Hash::digest([]))
        } else if metadata.file_type().is_symlink() {
            (
                "symlink",
                Blake3Hash::digest(
                    std::fs::read_link(&path)
                        .unwrap()
                        .to_string_lossy()
                        .as_bytes(),
                ),
            )
        } else {
            assert!(metadata.is_file(), "unexpected fixture node {path:?}");
            ("file", Blake3Hash::digest(std::fs::read(&path).unwrap()))
        };
        result.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            TreeEntry { kind, mode, hash },
        );
    }
    result
}

fn decode_output(format: &str, output: &Output) -> Value {
    assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-cli-secret"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-cli-secret"));
    let envelope = if format == "--jsonl" {
        let values: Vec<Value> = String::from_utf8(output.stdout.clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(values.len() >= 2, "{output:?}");
        assert_eq!(values[0]["event"], "started");
        assert_eq!(values[0]["sequence"], 0);
        let terminal = values.last().unwrap();
        assert_eq!(terminal["event"], "completed");
        assert_eq!(
            values.iter().filter(|v| v["event"] == "completed").count(),
            1
        );
        for (sequence, value) in values.iter().enumerate() {
            assert_eq!(value["sequence"], sequence as u64);
            assert_eq!(value["invocation_id"], values[0]["invocation_id"]);
            assert_eq!(value["schema_version"], "1");
        }
        terminal["data"].clone()
    } else {
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("invalid CLI output: {error}; {output:?}"))
    };
    assert_eq!(envelope["schema_version"], "1");
    assert_eq!(
        envelope["ok"],
        output.status.success(),
        "{envelope}; {output:?}"
    );
    envelope
}

fn private_config(f: &Vault, address: std::net::SocketAddr) -> PathBuf {
    let path = f.temp.path().join("providers.toml");
    provider::private(
        &path,
        format!(
            "version=1\n[profiles.primary]\nembedding=\"embedding\"\ngeneration=\"generation\"\n\
        [services.embedding]\nadapter=\"embeddings-v1\"\nurl=\"http://{address}/embed?retained=yes\"\nallow_loopback_http=true\nmodel=\"test-model\"\n\
        [services.embedding.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\n\
        [services.generation]\nadapter=\"chat-completions-v1\"\nurl=\"http://{address}/generate?retained=yes\"\nallow_loopback_http=true\nmodel=\"test-model\"\nrevision=\"r1\"\n\
        [services.generation.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\n\
        [vault_bindings.test]\nroot={}\nwiki_id=\"Vault.Portable\"\nallowed_profiles=[\"primary\"]\n",
            provider::quote(f.temp.path().to_str().unwrap())
        ),
    );
    path
}

#[derive(Debug, Clone)]
struct Request {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Value,
}
fn request(socket: &mut TcpStream) -> Request {
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut bytes = vec![];
    let (end, length) = loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).unwrap();
        assert!(count > 0, "native request ended before headers");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 512 * 1024, "native fixture request ceiling");
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            break (end + 4, length);
        }
    };
    assert!(end + length < 512 * 1024);
    while bytes.len() < end + length {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).unwrap();
        assert!(count > 0, "native request ended before body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    let header = std::str::from_utf8(&bytes[..end - 4]).unwrap();
    let mut lines = header.lines();
    let mut first = lines.next().unwrap().split_whitespace();
    let method = first.next().unwrap().into();
    let target = first.next().unwrap().into();
    assert_eq!(first.next(), Some("HTTP/1.1"));
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_lowercase(), value.trim().into())
        })
        .collect();
    let body = if length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[end..end + length]).unwrap()
    };
    Request {
        method,
        target,
        headers,
        body,
    }
}
struct Server {
    address: std::net::SocketAddr,
    seen: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&Request) -> Vec<u8> + Send + 'static) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(vec![]));
        let worker_stop = stop.clone();
        let worker_seen = seen.clone();
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let request = request(&mut socket);
                        let response = handler(&request);
                        worker_seen.lock().unwrap().push(request);
                        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
                        socket.write_all(&response).unwrap();
                        socket.flush().unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("native fixture accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            seen,
            stop,
            worker: Some(worker),
        }
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    fn finish(mut self) -> Vec<Request> {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
        self.seen.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}
fn generation(value: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":serde_json::to_string(&value).unwrap()}}],
        "usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0}}})).unwrap()
}
fn probe_reply(request: &Request) -> Vec<u8> {
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.headers["authorization"],
        "Bearer synthetic-cli-secret"
    );
    assert_eq!(request.body["model"], "test-model");
    if request.target == "/embed?retained=yes" {
        assert_eq!(request.body["encoding_format"], "float");
        assert_eq!(request.body["input"].as_array().unwrap().len(), 1);
        serde_json::to_vec(&json!({"model":"test-model","data":[{"index":0,"embedding":[1.0,0.0]}],"usage":{"prompt_tokens":1,"total_tokens":1}})).unwrap()
    } else {
        assert_eq!(request.target, "/generate?retained=yes");
        assert_eq!(request.body["messages"][0]["role"], "system");
        assert_eq!(request.body["messages"][1]["role"], "user");
        generation(json!({"ok":true}))
    }
}
fn ledger(f: &Vault, run: &str) -> JobLedger {
    JobLedger::new(
        f.fs.clone(),
        id("Vault.Portable"),
        RecordId::new(run).unwrap(),
        f.options.clone(),
    )
    .unwrap()
}
fn receipt(f: &Vault, reference: &DurableOutputRef) -> UsageReceipt {
    let bytes = std::fs::read(f.temp.path().join(reference.path.as_str())).unwrap();
    assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
    let note = lwiki::records::parse_note(&bytes);
    assert_eq!(
        note.canonical.as_ref().unwrap().kind(),
        RecordKind::RunEvent
    );
    assert_eq!(
        note.canonical.as_ref().unwrap().id(),
        &reference.record.record_id
    );
    let text = std::str::from_utf8(&bytes).unwrap();
    let fenced = text
        .split_once("```lwiki.run-event.v1\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    serde_json::from_value(serde_json::from_str::<Value>(fenced).unwrap()["receipt"].clone())
        .unwrap()
}

fn substitute(v: &Value, bindings: &BTreeMap<String, String>) -> Value {
    match v {
        Value::String(s) => {
            let mut s = s.clone();
            for (k, val) in bindings {
                s = s.replace(&format!("${{{k}}}"), val);
            }
            assert!(!s.contains("${"), "unbound example: {s}");
            Value::String(s)
        }
        Value::Array(a) => Value::Array(a.iter().map(|v| substitute(v, bindings)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), substitute(v, bindings)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

fn invoke(
    f: &Vault,
    format: &str,
    globals: &[&str],
    command: &[&str],
    input: Option<&Value>,
) -> (Output, Value) {
    let mut child = std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .current_dir(f.temp.path())
        .args(["--wiki", f.temp.path().to_str().unwrap(), format])
        .args(globals)
        .args(command)
        .env("EXPLICIT_FIXTURE_TOKEN", "synthetic-cli-secret")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        let bytes = match input {
            Value::String(s) => s.as_bytes().to_vec(),
            v => serde_json::to_vec(v).unwrap(),
        };
        child.stdin.as_mut().unwrap().write_all(&bytes).unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let envelope = decode_output(format, &output);
    assert!(output.stderr.is_empty(), "{command:?}: {output:?}");
    (output, envelope)
}
fn success(f: &Vault, globals: &[&str], command: &[&str], input: Option<&Value>) -> Value {
    let (output, envelope) = invoke(f, "--json", globals, command, input);
    assert!(output.status.success(), "{command:?}: {envelope}");
    envelope
}
fn offline(f: &Vault, command: &[&str], input: Option<&Value>) -> Value {
    let envelope = success(f, &["--offline"], command, input);
    assert_eq!(envelope["meta"]["network_used"], false);
    envelope
}
fn current_relationship(f: &Vault, assertion: &str) -> Value {
    offline(
        f,
        &["graph", "query", assertion, "--strategy", "relationship"],
        None,
    )
}
fn host_recipe(f: &Vault) -> BTreeMap<String, String> {
    let exported = f.temp.path().join("exported");
    offline(
        f,
        &[
            "skill",
            "export",
            "--target",
            "codex",
            "--output",
            exported.to_str().unwrap(),
        ],
        None,
    );
    let package = exported.join(".agents/skills/llm-wiki");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(package.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest,
        serde_json::from_str::<Value>(include_str!("fixtures/p14/package-manifest.json")).unwrap()
    );
    for (path, hash) in manifest["files"].as_object().unwrap() {
        assert_eq!(
            blake3::hash(&std::fs::read(package.join(path)).unwrap())
                .to_hex()
                .as_str(),
            hash.as_str().unwrap()
        );
    }
    let examples: Value =
        serde_json::from_slice(&std::fs::read(package.join("references/examples.json")).unwrap())
            .unwrap();
    assert_eq!(
        examples,
        serde_json::from_str::<Value>(include_str!("../skills/llm-wiki/references/examples.json"))
            .unwrap()
    );
    let mut bindings = BTreeMap::new();
    let mut imported = Value::Null;
    for step in examples["steps"].as_array().unwrap() {
        let name = step["name"].as_str().unwrap();
        if let Some(edit) = step.get("fixture_edit") {
            let path = f.temp.path().join(edit["path"].as_str().unwrap());
            assert_eq!(path, f.temp.path().join("pages/portable.md"));
            std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .unwrap()
                .write_all(edit["append"].as_str().unwrap().as_bytes())
                .unwrap();
        }
        let args = substitute(&step["args"], &bindings)
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let input = step.get("stdin").map(|v| substitute(v, &bindings));
        let (output, v) = invoke(f, "--json", &["--offline"], &refs, input.as_ref());
        if let Some(error) = step["expect_error"].as_str() {
            assert!(!output.status.success(), "{name}: {v}");
            assert_eq!(v["error"]["code"], error, "{name}: {v}");
        } else {
            assert!(output.status.success(), "{name}: {v}");
        }
        assert_eq!(v["meta"]["network_used"], false);
        if let Some(m) = step["bindings"].as_object() {
            for (key, pointer) in m {
                bindings.insert(
                    key.clone(),
                    v.pointer(pointer.as_str().unwrap())
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| panic!("{name} binding {key}: {v}"))
                        .to_owned(),
                );
            }
        }
        match step["check"].as_str().unwrap_or("") {
            "capabilities" => {
                assert_eq!(v["data"]["commands"], manifest["commands"]);
                assert_eq!(v["data"]["schemas"], manifest["schemas"]);
            }
            "research_collect" | "research_answer" => {
                assert_eq!(v["data"]["persisted"], true);
                assert_eq!(v["data"]["ready_to_import"], true);
                assert_eq!(v["data"]["external_tool_usage"], "unobserved");
                assert_eq!(v["data"]["packet"]["scope"]["offline"], true);
                let expected = if step["check"] == "research_collect" {
                    "collect_sources"
                } else {
                    "answer"
                };
                assert_eq!(v["data"]["packet"]["stage"], expected);
            }
            "research_complete" => {
                assert_eq!(v["data"]["status"], "completed");
                assert_eq!(v["data"]["ready_to_import"], false);
                assert_eq!(v["data"]["freshness"], "retained");
                assert_eq!(v["data"]["report"]["claims"][0]["assessment"], "unassessed");
            }
            "research_report" => {
                assert_eq!(
                    v["data"]["claims"][0]["citations"][0]["reference"]["source_revision"],
                    bindings["revision"]
                );
                assert_eq!(v["data"]["partial"], false);
            }
            "repeat_revision" => {
                assert_eq!(v["data"]["reused"], true);
                assert_eq!(v["data"]["allocated_ids"]["revision"], bindings["revision"]);
            }
            "literal_citation" => assert!(
                v["data"]["hits"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|hit| !hit["excerpt"]["citation"].is_null())
            ),
            "lexical_hits" => assert!(!v["data"]["hits"].as_array().unwrap().is_empty()),
            "homonyms" => assert_ne!(bindings["first_ada"], bindings["second_ada"]),
            "evidence_quote" | "counterevidence_quote" => {
                assert!(v["data"]["body"].as_str().unwrap().contains(
                    if step["check"] == "evidence_quote" {
                        "Ada works for Acme."
                    } else {
                        "The first Ada does not work for Acme."
                    }
                ));
                assert!(v.to_string().contains(&bindings["revision"]));
            }
            "relationship" => {
                let a = &v["data"]["assertions"];
                assert_eq!(a.as_array().unwrap().len(), 1);
                assert_eq!(a[0]["predicate"], "works_for");
                assert_eq!(a[0]["disputed"], true);
                assert!(!a[0]["support"][0]["citation"].is_null());
                assert_eq!(a[0]["contradictions"].as_array().unwrap().len(), 1);
            }
            "context_citation" => {
                assert!(!v["data"]["bundles"].as_array().unwrap().is_empty());
                assert!(v["data"]["usage"]["rendered_bytes"].as_u64().unwrap() <= 12000);
                assert!(v.to_string().contains(&bindings["evidence"]));
            }
            "budget_partial" => {
                assert_eq!(v["meta"]["partial"], true);
                assert_eq!(v["data"]["text"], "");
                assert!(!v["data"]["omissions"].as_array().unwrap().is_empty());
            }
            "semantic_unavailable" => assert!(!output.status.success()),
            "repeat_import" => {
                assert_eq!(v["data"]["reused"], true);
                assert_eq!(v["data"]["allocations"], imported["allocations"]);
            }
            "heading_identity" => {
                assert!(
                    v["data"]["body"]
                        .as_str()
                        .unwrap()
                        .contains("Second heading")
                );
                assert!(v.to_string().contains("Page.Portable"));
            }
            "author_preserved" => assert!(
                std::fs::read_to_string(f.temp.path().join("pages/portable.md"))
                    .unwrap()
                    .contains("Human-added observation.")
            ),
            "withdrawn_support" => assert!(v["data"]["assertions"].as_array().unwrap().is_empty()),
            "" => {}
            check => panic!("unimplemented maintained check {check}"),
        }
        if name == "import" {
            imported = v["data"].clone();
        }
        // Applying imports/resolutions cannot accept generated assertions.
        if name == "apply-import" || name == "apply-resolve" {
            assert!(
                current_relationship(f, &bindings["assertion"])["data"]["assertions"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
    assert_ne!(bindings["first_ada"], bindings["second_ada"]);
    bindings
}

fn wire_reply(request: &Request) -> Vec<u8> {
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.headers["authorization"],
        "Bearer synthetic-cli-secret"
    );
    assert_eq!(request.body["model"], "test-model");
    if request.target == "/embed?retained=yes" {
        assert_eq!(request.body["encoding_format"], "float");
        let inputs = request.body["input"].as_array().unwrap();
        assert!(!inputs.is_empty());
        // Return valid data in reverse index order through the native adapter.
        let data: Vec<_> = (0..inputs.len())
            .rev()
            .map(|index| json!({"index":index,"embedding":[1.0,0.0]}))
            .collect();
        return serde_json::to_vec(&json!({"model":"test-model","data":data,"usage":{"prompt_tokens":inputs.len(),"total_tokens":inputs.len()}})).unwrap();
    }
    assert_eq!(request.target, "/generate?retained=yes");
    assert_eq!(request.body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(request.body["messages"][0]["role"], "system");
    assert_eq!(request.body["messages"][1]["role"], "user");
    let data: Value =
        serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap()).unwrap();
    if data["schema"] == "lwiki.extraction-packet.v1" || data.get("packet_fingerprint").is_some() {
        assert_eq!(
            data["output_schema"]["properties"]["schema"]["const"],
            "lwiki.extraction.v1"
        );
        let window = &data["windows"][0];
        let text = window["text"].as_str().unwrap();
        assert!(text.contains("Ada maintains Relay."));
        let start = window["span"]["start"].as_u64().unwrap() as usize;
        let mention = |local: &str, label: &str| {
            let offset = start + text.find(label).unwrap();
            json!({"id":local,"window_id":window["id"],"label":label,"type":if label == "Ada" {"person"} else {"component"},"quote":label,"span":{"start":offset,"end":offset+label.len()}})
        };
        return generation(
            json!({"schema":"lwiki.extraction.v1","packet_id":data["packet_id"],"packet_fingerprint":data["packet_fingerprint"],"mentions":[mention("m1","Ada"),mention("m2","Relay")],"assertions":[{"id":"a1","subject":"m1","predicate":"maintains","object":{"kind":"mention","mention_id":"m2"},"negated":false,"modality":"asserted","evidence":[{"window_id":window["id"],"stance":"supports","quote":"Ada maintains Relay."}]}],"unresolved":[]}),
        );
    }
    panic!("unexpected generation request outside direct graph extraction");
}
fn run_ids(f: &Vault) -> Vec<RecordId> {
    let mut ids = f
        .fs
        .root()
        .scan_markdown()
        .unwrap()
        .into_iter()
        // Agent handoff heads use research.md and have no paid provider ledger.
        .filter(|p| p.as_str().starts_with("runs/") && p.as_str().ends_with("/run.md"))
        .filter_map(|p| {
            let note =
                lwiki::records::parse_note(&std::fs::read(f.temp.path().join(p.as_str())).unwrap());
            note.canonical
                .filter(|r| r.kind() == RecordKind::Run)
                .map(|r| r.id().clone())
        })
        .filter(|id| id.as_str() != "run_restored_p21")
        .collect::<Vec<_>>();
    ids.sort();
    ids
}
fn account_new_runs(f: &Vault, retained: &mut BTreeMap<RecordId, LedgerInspection>) {
    for (id, before) in retained.iter() {
        let now = ledger(f, id.as_str()).inspect().unwrap();
        assert_eq!(
            now.spec, before.spec,
            "unrelated operation changed prior genesis {id}"
        );
        assert_eq!(
            now.attempts, before.attempts,
            "unrelated operation changed prior receipts {id}"
        );
        assert_eq!(
            now.budget, before.budget,
            "unrelated operation changed prior budget {id}"
        );
    }
    for id in run_ids(f) {
        if retained.contains_key(&id) {
            continue;
        }
        let inspection = ledger(f, id.as_str()).inspect().unwrap();
        let PriorAccounting::Unknown {
            prior_run_ids,
            reason,
        } = &inspection.spec.prior_accounting
        else {
            panic!("new paid run omitted restored unknown accounting: {id}");
        };
        assert!(!reason.is_empty());
        assert!(prior_run_ids.contains(&self::id("run_restored_p21")));
        for prior in prior_run_ids {
            assert!(
                !f.temp
                    .path()
                    .join(format!("runs/{prior}/research.md"))
                    .exists(),
                "local agent handoff must not become unknown paid accounting: {prior}"
            );
        }
        for previous in retained.keys() {
            assert!(
                prior_run_ids.contains(previous),
                "{id} omitted prior {previous}"
            );
        }
        assert_eq!(inspection.state, RunState::Completed);
        assert!(!inspection.attempts.is_empty());
        assert_eq!(
            inspection.budget.dispatched_requests as usize,
            inspection.attempts.len()
        );
        for attempt in &inspection.attempts {
            assert_eq!(attempt.phase, AttemptPhase::Settled);
            let paid = receipt(f, attempt.receipt.as_ref().unwrap());
            assert_eq!(paid.attempt, attempt.attempt);
            assert_eq!(paid.output_disposition, OutputDisposition::Validated);
            assert_eq!(paid.outputs, attempt.outputs);
            assert_eq!(paid.cache_outputs, attempt.cache_outputs);
            for output in &paid.outputs {
                assert_eq!(
                    Blake3Hash::digest(
                        std::fs::read(f.temp.path().join(output.path.as_str())).unwrap()
                    ),
                    output.hash
                );
            }
        }
        retained.insert(id, inspection);
    }
}
fn agent_research_handoff(f: &Vault, source: &str, server: &Server) {
    let count = server.count();
    let started = success(
        f,
        &[],
        &[
            "research",
            "run",
            "Ada",
            "--source-id",
            source,
            "--run-id",
            "run_p21_handoff",
        ],
        None,
    );
    assert_eq!(started["meta"]["network_used"], false);
    assert_eq!(started["data"]["packet"]["stage"], "collect_sources");
    assert_eq!(started["data"]["ready_to_import"], true);
    let initial = &started["data"]["packet"];
    assert!(
        initial["passages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|passage| { passage["quote"].as_str().unwrap().contains("ACTIVE-P21") })
    );
    let collect = json!({
        "schema":"lwiki.research-submission.v1",
        "run_id":"run_p21_handoff",
        "packet_fingerprint":initial["packet_fingerprint"],
        "response":{"stage":"collect_sources","sources":[{
            "key":"host_source", "title":"Cedar host report",
            "origin":"https://example.invalid/cedar", "provenance":"Supplied by the host agent",
            "content":"Ada maintains Relay. Host report says Cedar backs up on Friday."
        }],"gaps":[]}
    });
    let imported = success(
        f,
        &[],
        &["research", "import", "--file", "-"],
        Some(&collect),
    );
    assert_eq!(imported["meta"]["network_used"], false);
    assert_eq!(imported["data"]["packet"]["stage"], "answer");
    assert_eq!(
        imported["data"]["imported_sources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let repeated = success(
        f,
        &[],
        &["research", "import", "--file", "-"],
        Some(&collect),
    );
    assert_eq!(repeated["data"]["reused"], true);
    assert_eq!(
        repeated["data"]["imported_sources"],
        imported["data"]["imported_sources"]
    );
    let answer_packet = &imported["data"]["packet"];
    let passage = &answer_packet["passages"][0];
    assert_eq!(passage["passage_id"], "p1");
    assert!(
        passage["quote"]
            .as_str()
            .unwrap()
            .contains("Cedar backs up on Friday")
    );
    let citation: CitationRef = serde_json::from_value(passage["citation"].clone()).unwrap();
    let view = lwiki::sources::SourceView::from_fs_bounded(&f.fs, 64 * 1024 * 1024, 4096).unwrap();
    assert_eq!(
        view.verify(&citation, lwiki::sources::CitationScope::Current)
            .unwrap()
            .quote,
        passage["quote"].as_str().unwrap().as_bytes()
    );
    let answer = json!({
        "schema":"lwiki.research-submission.v1",
        "run_id":"run_p21_handoff",
        "packet_fingerprint":answer_packet["packet_fingerprint"],
        "response":{"stage":"answer","claims":[{
            "text":"The host report says Cedar backs up on Friday.",
            "passage_ids":["p1"]
        }],"gaps":["Entailment remains unassessed."],"follow_up":null}
    });
    let completed = success(
        f,
        &[],
        &["research", "import", "--file", "-"],
        Some(&answer),
    );
    assert_eq!(completed["data"]["status"], "completed");
    assert_eq!(
        completed["data"]["report"]["claims"][0]["assessment"],
        "unassessed"
    );
    let report = offline(f, &["research", "report", "run_p21_handoff"], None);
    assert_eq!(report["data"], completed["data"]["report"]);
    let status = offline(f, &["research", "status", "run_p21_handoff"], None);
    assert_eq!(status["data"]["imports"], 2);
    assert_eq!(status["data"]["captured_sources"], 1);
    assert_eq!(
        server.count(),
        count,
        "research handoff dispatched no provider work"
    );
}

#[test]
#[cfg(unix)]
fn m0_m4_full_local_acceptance() {
    let f = Vault::new();
    let bindings = host_recipe(&f);
    // Markdown-only rebuild retains explicit decisions and cannot restore withdrawn support.
    let before = offline(&f, &["search", "KEY-731", "--mode", "literal"], None);
    std::fs::remove_dir_all(f.temp.path().join(".wiki/cache")).unwrap();
    offline(&f, &["index", "rebuild"], None);
    let after = offline(&f, &["search", "KEY-731", "--mode", "literal"], None);
    assert_eq!(after["data"]["hits"], before["data"]["hits"]);
    assert!(
        current_relationship(&f, &bindings["assertion"])["data"]["assertions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let capture = offline(
        &f,
        &["source", "add", "-", "--title", "P21 active local source"],
        Some(&json!("Ada maintains Relay. ACTIVE-P21.\n")),
    );
    let source = capture["data"]["allocated_ids"]["source"].as_str().unwrap();
    assert_ne!(source, bindings["source"]);
    // A restored Run without operational accounting is evidence of unknown past spend.
    let restored = f.temp.path().join("runs/restored/run.md");
    std::fs::create_dir_all(restored.parent().unwrap()).unwrap();
    let restored_bytes = b"---\nwiki_schema: \"1\"\nwiki_id: run_restored_p21\nwiki_kind: run\ntitle: Restored unknown spend\nwiki_status: planned\nwiki_created_at: \"2026-09-28T00:00:00Z\"\n---\nUnrecorded billing cannot confer paid authority.\n";
    assert!(
        lwiki::records::parse_note(restored_bytes)
            .canonical
            .is_some()
    );
    std::fs::write(&restored, restored_bytes).unwrap();
    let server = Server::new(wire_reply);
    let config = private_config(&f, server.address);
    let cfg = config.to_str().unwrap();
    let before = tree(f.temp.path());
    for command in [
        vec!["embeddings", "sync", "--providers-config", cfg],
        vec![
            "graph",
            "extract",
            "--source-id",
            source,
            "--executor",
            "api",
            "--providers-config",
            cfg,
        ],
        vec!["research", "run", "Ada", "--source-id", source],
    ] {
        let preview = success(
            &f,
            &["--dry-run", "--offline", "--profile", "primary"],
            &command,
            None,
        );
        assert_eq!(preview["meta"]["network_used"], false);
        assert_eq!(tree(f.temp.path()), before);
        assert_eq!(server.count(), 0);
    }
    let mut retained = BTreeMap::new();
    let sync = success(
        &f,
        &["--profile", "primary"],
        &["embeddings", "sync", "--providers-config", cfg],
        None,
    );
    assert_eq!(sync["meta"]["network_used"], true);
    assert_eq!(sync["data"]["published"], true);
    assert!(sync["data"]["generated_inputs"].as_u64().unwrap() > 0);
    account_new_runs(&f, &mut retained);
    let semantic = success(
        &f,
        &["--profile", "primary"],
        &[
            "search",
            "Ada",
            "--mode",
            "semantic",
            "--providers-config",
            cfg,
        ],
        None,
    );
    assert_eq!(semantic["meta"]["network_used"], true);
    assert!(!semantic["data"]["hits"].as_array().unwrap().is_empty());
    account_new_runs(&f, &mut retained);
    let count = server.count();
    let cached = offline(
        &f,
        &[
            "search",
            "Ada",
            "--mode",
            "semantic",
            "--providers-config",
            "unavailable.toml",
        ],
        None,
    );
    assert_eq!(cached["data"]["hits"], semantic["data"]["hits"]);
    offline(&f, &["graph", "query", "Ada", "--seed", "semantic"], None);
    let context = offline(
        &f,
        &[
            "context",
            "Ada",
            "--mode",
            "hybrid",
            "--target",
            "combined",
            "--seed",
            "semantic",
            "--max-bytes",
            "12000",
            "--max-tokens",
            "3000",
        ],
        None,
    );
    assert!(context["data"]["text"].as_str().unwrap().contains("Ada"));
    assert_eq!(server.count(), count);
    let api = success(
        &f,
        &["--profile", "primary"],
        &[
            "graph",
            "extract",
            "--source-id",
            source,
            "--executor",
            "api",
            "--run",
            "run_p21_api",
            "--providers-config",
            cfg,
        ],
        None,
    );
    assert_eq!(api["meta"]["network_used"], true);
    assert!(api["data"]["import"]["prepared"].is_object());
    assert_eq!(
        api["data"]["import"]["coverage"]["materialized_assertions"],
        0
    );
    let again = success(
        &f,
        &["--offline", "--profile", "primary"],
        &[
            "graph",
            "extract",
            "--source-id",
            source,
            "--executor",
            "api",
            "--run",
            "run_p21_api",
            "--providers-config",
            cfg,
        ],
        None,
    );
    assert_eq!(again["meta"]["network_used"], false);
    assert_eq!(again["data"]["reused"], true);
    assert_eq!(again["data"]["output"], api["data"]["output"]);
    assert_eq!(
        again["data"]["import"]["allocations"],
        api["data"]["import"]["allocations"]
    );
    account_new_runs(&f, &mut retained);
    agent_research_handoff(&f, source, &server);
    // A fresh probe still discloses and preserves earlier paid accounting.
    let probe_server = Server::new(probe_reply);
    let probe_config = private_config(&f, probe_server.address);
    let probe = success(
        &f,
        &["--profile", "primary"],
        &[
            "doctor",
            "--probe",
            "--role",
            "generate",
            "--max-requests",
            "1",
            "--providers-config",
            probe_config.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(probe["meta"]["network_used"], true);
    account_new_runs(&f, &mut retained);
    assert_eq!(probe_server.finish().len(), 1);
    assert_eq!(std::fs::read(&restored).unwrap(), restored_bytes);
    assert!(
        current_relationship(&f, &bindings["assertion"])["data"]["assertions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let requests = server.finish();
    let probe_run = probe["data"]["probe"]["run_id"].as_str().unwrap();
    let durable_requests: usize = retained
        .iter()
        .filter(|(run, _)| run.as_str() != probe_run)
        .map(|(_, run)| run.attempts.len())
        .sum();
    assert_eq!(
        requests.len(),
        durable_requests,
        "native wire entries must equal durable attempts"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.target.starts_with("/generate"))
            .count(),
        1,
        "only direct graph extraction used generation"
    );
}

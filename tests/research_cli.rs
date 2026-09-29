//! Native CLI exercises use disposable vaults and explicit loopback provider endpoints.
use lwiki as library;
#[path = "fixtures/p18/common.rs"]
mod common;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod provider;
use common::Fixture;
use lwiki::{domain::*, jobs::*};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Output,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

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

fn invoke(f: &Fixture, format: &str, globals: &[&str], command: &[&str]) -> (Output, Value) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lwiki"))
        .args(["--wiki", f.temp.path().to_str().unwrap(), format])
        .args(globals)
        .args(command)
        .env("EXPLICIT_FIXTURE_TOKEN", "synthetic-cli-secret")
        .output()
        .unwrap();
    let envelope = decode_output(format, &output);
    (output, envelope)
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

fn private_config(f: &Fixture, address: std::net::SocketAddr) -> PathBuf {
    let path = f.temp.path().join("providers.toml");
    provider::private(
        &path,
        format!(
            "version=1\n[profiles.primary]\nembedding=\"embedding\"\ngeneration=\"generation\"\nsearch=\"search\"\n\
        [services.embedding]\nadapter=\"embeddings-v1\"\nurl=\"http://{address}/embed?retained=yes\"\nallow_loopback_http=true\nmodel=\"test-model\"\n\
        [services.embedding.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\n\
        [services.generation]\nadapter=\"chat-completions-v1\"\nurl=\"http://{address}/generate?retained=yes\"\nallow_loopback_http=true\nmodel=\"test-model\"\nrevision=\"r1\"\n\
        [services.generation.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\n\
        [services.search]\nadapter=\"brave-web-v1\"\nurl=\"http://{address}/search?retained=yes\"\nallow_loopback_http=true\n\
        [services.search.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\nheader=\"X-Subscription-Token\"\nprefix=\"\"\n\
        [vault_bindings.test]\nroot={}\nwiki_id=\"vault_test\"\nallowed_profiles=[\"primary\"]\n",
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
    if request.target.starts_with("/search?") {
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.headers["x-subscription-token"],
            "synthetic-cli-secret"
        );
        let query: BTreeMap<_, _> = url::Url::parse(&format!("http://fixture{}", request.target))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(query["retained"], "yes");
        assert_eq!(query["q"], "lwiki provider probe");
        assert_eq!(query["count"], "1");
        assert_eq!(query["offset"], "0");
        assert!(request.body.is_null());
        serde_json::to_vec(&json!({"web":{"results":[]}})).unwrap()
    } else {
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
}
fn ledger(f: &Fixture, run: &str) -> JobLedger {
    JobLedger::new(
        f.fs.clone(),
        common::id("vault_test"),
        RecordId::new(run).unwrap(),
        f.options.clone(),
    )
    .unwrap()
}
fn receipt(f: &Fixture, reference: &DurableOutputRef) -> UsageReceipt {
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

#[test]
fn research_and_doctor_dry_runs_preserve_complete_tree_without_provider_access() {
    let f = Fixture::new();
    let config = private_config(&f, "127.0.0.1:9".parse().unwrap());
    let missing = f.temp.path().join("unavailable/private/providers.toml");
    #[cfg(unix)]
    {
        let helper = f.temp.path().join("credential-helper.sh");
        let marker = f.temp.path().join("helper-called");
        provider::private(
            &helper,
            b"#!/bin/sh\nprintf invoked > \"$1\"\nprintf 'synthetic-cli-secret\\n'\n",
        );
        let text = std::fs::read_to_string(&config).unwrap().replace(
            "kind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"",
            &format!(
                "kind=\"command\"\ncommand=[\"/bin/sh\",{},{}]\noutput=\"text\"",
                provider::quote(helper.to_str().unwrap()),
                provider::quote(marker.to_str().unwrap())
            ),
        );
        provider::private(&config, text);
    }
    let before = tree(f.temp.path());
    assert!(
        before
            .keys()
            .any(|p| p.to_string_lossy().contains("index.sqlite")),
        "fixture must include SQLite"
    );
    for provider_path in [&config, &missing] {
        for (format, globals, mut command) in [
            (
                "--json",
                vec!["--dry-run", "--profile", "primary"],
                vec!["research", "plan", "Ada"],
            ),
            (
                "--jsonl",
                vec!["--dry-run", "--offline", "--profile", "primary"],
                vec!["research", "run", "Ada"],
            ),
            (
                "--json",
                vec!["--dry-run", "--profile", "primary"],
                vec!["doctor", "--probe", "--role", "generate"],
            ),
            (
                "--json",
                vec!["--dry-run", "--offline", "--profile", "primary"],
                vec!["doctor", "--probe", "--role", "search"],
            ),
        ] {
            command.extend(["--providers-config", provider_path.to_str().unwrap()]);
            let (output, envelope) = invoke(&f, format, &globals, &command);
            assert!(output.status.success(), "{envelope}");
            assert_eq!(envelope["meta"]["network_used"], false);
            assert_eq!(
                tree(f.temp.path()),
                before,
                "{} mutated dry-run tree",
                envelope["command"]
            );
        }
    }
    let (output, envelope) = invoke(
        &f,
        "--json",
        &["--profile", "primary"],
        &[
            "research",
            "plan",
            "Ada",
            "--providers-config",
            missing.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{envelope}");
    assert_eq!(envelope["data"]["persisted"], false);
    assert_eq!(envelope["meta"]["network_used"], false);
    assert_eq!(tree(f.temp.path()), before);
}

#[test]
fn research_cli_rejects_excessive_scope_and_caller_exclusions_without_writes() {
    let f = Fixture::new();
    let missing = f.temp.path().join("unavailable/providers.toml");
    let before = tree(f.temp.path());
    let long_question = "a".repeat(4097);
    for (question, extra) in [
        ("Ada", vec!["--max-rounds", "17"]),
        ("Ada", vec!["--max-sources", "257"]),
        ("Ada", vec!["--stage-output-tokens", "32769"]),
        (long_question.as_str(), vec![]),
        ("Ada", vec!["--exclude", "  "]),
        ("Ada", vec!["--exclude", "ACME", "--exclude", " acme "]),
        (
            "Ada",
            vec![
                "--url",
                "https://public.example/blocked",
                "--exclude",
                "blocked",
            ],
        ),
        ("Ada", vec!["--url", "http://127.0.0.1/private"]),
    ] {
        let mut command = vec![
            "research",
            "run",
            question,
            "--providers-config",
            missing.to_str().unwrap(),
        ];
        command.extend(extra);
        let (output, envelope) = invoke(&f, "--jsonl", &["--dry-run"], &command);
        assert!(
            !output.status.success(),
            "accepted invalid scope: {envelope}"
        );
        assert_eq!(envelope["meta"]["network_used"], false);
        assert_ne!(
            envelope["error"]["code"], "CONFIG_INVALID",
            "scope must fail before config: {envelope}"
        );
        assert_eq!(tree(f.temp.path()), before);
    }
}

#[test]
fn doctor_without_probe_preserves_compatibility_and_explicit_role_requires_probe() {
    let f = Fixture::new();
    // Ordinary doctor may create rebuildable SQLite WAL/SHM files.
    // Canonical records, configuration and operational journals remain unchanged.
    let canonical_tree = || {
        tree(f.temp.path())
            .into_iter()
            .filter(|(path, _)| !path.starts_with(".wiki/cache"))
            .collect::<BTreeMap<_, _>>()
    };
    let before = canonical_tree();
    let (output, envelope) = invoke(&f, "--json", &[], &["doctor"]);
    assert!(output.status.success(), "{envelope}");
    assert!(envelope["data"]["check"].is_object());
    assert_eq!(envelope["data"]["provider_probe_performed"], false);
    assert_eq!(envelope["meta"]["network_used"], false);
    assert_eq!(canonical_tree(), before);
    let (output, envelope) = invoke(&f, "--jsonl", &[], &["doctor", "--role", "generate"]);
    assert_eq!(output.status.code(), Some(2), "{envelope}");
    assert_eq!(envelope["error"]["code"], "USAGE");
    assert_eq!(canonical_tree(), before);
}

#[test]
fn doctor_each_role_uses_native_sealed_wire_and_completed_probe_receipt() {
    let f = Fixture::new();
    let server = Server::new(probe_reply);
    let config = private_config(&f, server.address);
    // Restored Markdown can carry an identity different from its directory,
    // or live below an archival directory without an operational journal.
    // These records provide possible accounting history, never paid authority.
    let restored: Vec<_> = [
        ("runs/restored_namespace/run.md", "run_restored_history"),
        ("runs/archive/restored/run.md", "run_moved_history"),
    ]
    .into_iter()
    .map(|(relative, id)| {
        let path = f.temp.path().join(relative);
        let bytes = format!(
            "---\nwiki_schema: \"1\"\nwiki_id: {id}\nwiki_kind: run\ntitle: Restored unknown lifetime\nwiki_status: planned\nwiki_created_at: \"2026-09-28T00:00:00Z\"\n---\n\nPrivate restored history remains unavailable for resend.\n"
        )
        .into_bytes();
        assert!(lwiki::records::parse_note(&bytes).canonical.is_some());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        (path, bytes)
    })
    .collect();
    let before = tree(f.temp.path());
    for global in ["--dry-run", "--offline"] {
        let (output, envelope) = invoke(
            &f,
            "--jsonl",
            &[global, "--profile", "primary"],
            &[
                "doctor",
                "--probe",
                "--role",
                "generate",
                "--providers-config",
                config.to_str().unwrap(),
            ],
        );
        assert_eq!(output.status.success(), global == "--dry-run", "{envelope}");
        assert_eq!(envelope["meta"]["network_used"], false);
        assert_eq!(tree(f.temp.path()), before);
        assert_eq!(server.count(), 0);
    }
    let mut prior: Vec<LedgerInspection> = vec![];
    for (position, role) in ["generate", "search", "embed"].into_iter().enumerate() {
        let (output, envelope) = invoke(
            &f,
            "--jsonl",
            &["--profile", "primary"],
            &[
                "doctor",
                "--probe",
                "--role",
                role,
                "--max-requests",
                "1",
                "--providers-config",
                config.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{envelope}");
        assert_eq!(envelope["meta"]["network_used"], true);
        assert_eq!(envelope["data"]["doctor"]["provider_probe_performed"], true);
        assert_eq!(envelope["data"]["probe"]["role"], role);
        assert_eq!(envelope["data"]["probe"]["validated"], true);
        let run = envelope["data"]["probe"]["run_id"].as_str().unwrap();
        let inspection = ledger(&f, run).inspect().unwrap();
        assert_eq!(inspection.spec.scope.operation, "doctor_probe");
        assert_eq!(inspection.state, RunState::Completed);
        assert_eq!(inspection.effective_limits.requests, 1);
        assert_eq!(inspection.budget.dispatched_requests, 1);
        assert_eq!(inspection.attempts.len(), 1);
        let PriorAccounting::Unknown {
            prior_run_ids,
            reason,
        } = &inspection.spec.prior_accounting
        else {
            panic!("probes must disclose restored and previous unknown billing");
        };
        assert!(!reason.is_empty());
        for id in [
            "restored_namespace",
            "run_restored_history",
            "run_moved_history",
        ] {
            assert!(prior_run_ids.contains(&common::id(id)), "missing {id}");
        }
        for (path, bytes) in &restored {
            assert_eq!(std::fs::read(path).unwrap(), *bytes);
        }
        if !prior.is_empty() {
            for previous in &prior {
                assert!(prior_run_ids.contains(&previous.spec.run_id));
                let still_retained = ledger(&f, previous.spec.run_id.as_str()).inspect().unwrap();
                assert_eq!(still_retained.attempts, previous.attempts);
                assert_eq!(still_retained.budget.dispatched_requests, 1);
                assert_eq!(
                    still_retained.budget.unknown_attempts,
                    previous.budget.unknown_attempts
                );
            }
        }
        let attempt = &inspection.attempts[0];
        assert_eq!(attempt.phase, AttemptPhase::Settled);
        assert_eq!(attempt.bound.capability, Capability::Probe);
        let task = &inspection.tasks[&attempt.attempt.task_key];
        assert_eq!(task.spec.stage, TaskStage::Probe);
        assert_eq!(task.spec.capability, Some(Capability::Probe));
        assert_eq!(task.state, TaskState::Completed);
        let paid = receipt(&f, attempt.receipt.as_ref().unwrap());
        assert_eq!(paid.capability, Capability::Probe);
        assert_eq!(paid.attempt, attempt.attempt);
        assert_eq!(paid.output_disposition, OutputDisposition::Validated);
        // Legacy one-off probes bind their role/purpose through the sealed wire,
        // descriptor and actual retained decoder; research epochs additionally
        // retain historical codec snapshots for changed-model recovery.
        let requested_role = match role {
            "generate" => lwiki::providers::types::ServiceRole::Generate,
            "search" => lwiki::providers::types::ServiceRole::Search,
            "embed" => lwiki::providers::types::ServiceRole::Embed,
            _ => unreachable!(),
        };
        let trusted = lwiki::config::providers::ProviderConfig::load(&config)
            .unwrap()
            .authorize(
                &f.fs,
                &common::id("vault_test"),
                "primary",
                requested_role.capability(),
            )
            .unwrap();
        let retained_mock = common::Mock::response(json!({}));
        let decoder = f.dispatcher(retained_mock.clone());
        let current_ledger = ledger(&f, run);
        let decoded = decoder
            .decode_retained(
                &current_ledger,
                &trusted,
                &task.spec.key,
                lwiki::providers::types::DispatchPurpose::Probe {
                    role: requested_role,
                },
                &attempt.attempt,
            )
            .unwrap();
        assert!(
            matches!(decoded, lwiki::providers::types::ValidatedOutput::Probe { role: actual } if actual == requested_role)
        );
        assert!(
            decoder
                .decode_retained(
                    &current_ledger,
                    &trusted,
                    &task.spec.key,
                    lwiki::providers::types::DispatchPurpose::Task,
                    &attempt.attempt
                )
                .is_err()
        );
        assert_eq!(retained_mock.calls.load(Ordering::SeqCst), 0);
        assert_eq!(server.count(), position + 1);
        prior.push(inspection);
    }
    assert_eq!(server.finish().len(), 3);
}

#[test]
fn doctor_offline_refuses_without_native_requests_or_tree_changes() {
    let f = Fixture::new();
    let server = Server::new(probe_reply);
    let config = private_config(&f, server.address);
    let before = tree(f.temp.path());
    for role in ["generate", "search", "embed"] {
        let (output, envelope) = invoke(
            &f,
            "--json",
            &["--offline", "--profile", "primary"],
            &[
                "doctor",
                "--probe",
                "--role",
                role,
                "--providers-config",
                config.to_str().unwrap(),
            ],
        );
        assert!(!output.status.success(), "{envelope}");
        assert_eq!(envelope["error"]["code"], "OFFLINE_UNAVAILABLE");
        assert_eq!(envelope["meta"]["network_used"], false);
        assert_eq!(tree(f.temp.path()), before);
    }
    assert!(server.finish().is_empty());
}

#[test]
fn doctor_malformed_paid_response_reports_network_and_retains_unknown_hold() {
    let f = Fixture::new();
    let server = Server::new(|request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/generate?retained=yes");
        assert_eq!(request.body["model"], "test-model");
        b"{}".to_vec()
    });
    let config = private_config(&f, server.address);
    let (output, envelope) = invoke(
        &f,
        "--jsonl",
        &["--profile", "primary"],
        &[
            "doctor",
            "--probe",
            "--role",
            "generate",
            "--max-requests",
            "1",
            "--providers-config",
            config.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success(), "{envelope}");
    assert_eq!(envelope["meta"]["network_used"], true);
    assert_eq!(envelope["error"]["code"], "PROVIDER_RESPONSE");
    assert!(envelope["error"].get("network_used").is_none());
    let inspection = ledger(&f, envelope["error"]["details"]["run_id"].as_str().unwrap())
        .inspect()
        .unwrap();
    assert_eq!(inspection.budget.dispatched_requests, 1);
    assert_eq!(inspection.attempts.len(), 1);
    assert_eq!(inspection.attempts[0].phase, AttemptPhase::Settled);
    assert_eq!(
        inspection.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    assert!(
        inspection
            .budget
            .unknown_attempts
            .contains(&inspection.attempts[0].attempt.attempt_id)
    );
    let paid = receipt(&f, inspection.attempts[0].receipt.as_ref().unwrap());
    assert_eq!(paid.capability, Capability::Probe);
    assert_eq!(paid.output_disposition, OutputDisposition::Rejected);
    assert!(paid.outputs.is_empty());
    assert_eq!(server.finish().len(), 1);
}

fn research_reply(request: &Request) -> Vec<u8> {
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/generate?retained=yes");
    assert_eq!(
        request.headers["authorization"],
        "Bearer synthetic-cli-secret"
    );
    assert_eq!(request.body["model"], "test-model");
    assert_eq!(request.body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(request.body["messages"][0]["role"], "system");
    assert_eq!(request.body["messages"][1]["role"], "user");
    let data: Value =
        serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(data["binding"]["version"], 1);
    assert_eq!(data["binding"]["round"], 1);
    assert_eq!(data["question"], "Ada");
    assert_eq!(data["explicit_urls"], json!([]));
    let passages = data["passages"].as_array().unwrap();
    assert!(
        !passages.is_empty(),
        "native research must inspect actual captured local source"
    );
    let citation = passages[0]["citation"].clone();
    assert!(
        data["binding"]["citations"]
            .as_array()
            .unwrap()
            .contains(&citation)
    );
    generation(match data["binding"]["stage"].as_str().unwrap() {
        "plan_frontier" => {
            json!({"queries":[],"urls":[],"reason":"Inspect captured local evidence; no public leads required."})
        }
        "assess_gaps" => {
            json!({"covered_evidence_ids":[],"gaps":["Captured text establishes provenance, while entailment remains unassessed."],"next_queries":[],"next_urls":[],"stop":true})
        }
        "synthesize" => {
            json!({"sections":[{"heading":"Captured local evidence","claims":[{"text":"Ada is mentioned in a captured local passage.","citations":[citation.clone()]}]}],
            "unanswered_questions":["No independent identity or entailment assessment was performed."],
            "proposed_changes":[{"kind":"create_page","title":"Research CLI proposal","body":"This proposed page references the captured local passage; its claims remain unassessed.","citations":[citation]}]})
        }
        stage => panic!("unexpected native research stage {stage}"),
    })
}
fn delete_cache(f: &Fixture) {
    let path = f.temp.path().join(".wiki/cache");
    if path.exists() {
        std::fs::remove_dir_all(path).unwrap();
    }
}
fn count_pages(f: &Fixture) -> usize {
    f.fs.root()
        .scan_markdown()
        .unwrap()
        .iter()
        .filter(|path| {
            lwiki::records::parse_note(&std::fs::read(f.temp.path().join(path.as_str())).unwrap())
                .canonical
                .is_some_and(|record| record.kind() == RecordKind::Page)
        })
        .count()
}

#[test]
fn native_research_local_sources_complete_receipts_unassessed_report_and_staged_proposals() {
    let f = Fixture::new();
    let server = Server::new(research_reply);
    let config = private_config(&f, server.address);
    let (output, envelope) = invoke(
        &f,
        "--jsonl",
        &["--profile", "primary"],
        &[
            "research",
            "run",
            "Ada",
            "--run-id",
            "run_native_research",
            "--max-requests",
            "3",
            "--providers-config",
            config.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{envelope}");
    assert_eq!(envelope["command"], "research run");
    assert_eq!(envelope["meta"]["network_used"], true);
    assert_eq!(envelope["meta"]["partial"], false);
    let report = &envelope["data"]["report"];
    assert_eq!(report["partial"], false);
    assert!(!report["passages"].as_array().unwrap().is_empty());
    assert_eq!(report["claim_assessments"][0]["status"], "unassessed");
    assert_eq!(report["claim_assessments"][0]["provenance_verified"], true);
    assert_eq!(report["proposed_changes"].as_array().unwrap().len(), 1);
    assert_eq!(
        count_pages(&f),
        0,
        "generated page must remain staged without --apply"
    );
    let run = ledger(&f, "run_native_research");
    let inspection = run.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Completed);
    assert_eq!(inspection.budget.dispatched_requests, 3);
    assert_eq!(inspection.attempts.len(), 3);
    assert_eq!(inspection.research.as_ref().unwrap().rounds_started, 1);
    assert!(inspection.research.as_ref().unwrap().origins.is_empty());
    let mut paid_stages = vec![];
    for attempt in &inspection.attempts {
        assert_eq!(attempt.phase, AttemptPhase::Settled);
        // Settlement and retained raw-response cleanup are separate operations.
        assert!(attempt.spool.is_some());
        let task = &inspection.tasks[&attempt.attempt.task_key];
        assert_eq!(task.state, TaskState::Completed);
        let paid = receipt(&f, attempt.receipt.as_ref().unwrap());
        assert_eq!(paid.capability, Capability::Generate);
        assert_eq!(paid.output_disposition, OutputDisposition::Validated);
        assert_eq!(paid.outputs, attempt.outputs);
        assert_eq!(paid.outputs.len(), 1);
        let stored = &paid.outputs[0];
        assert_eq!(
            Blake3Hash::digest(std::fs::read(f.temp.path().join(stored.path.as_str())).unwrap()),
            stored.hash
        );
        paid_stages.push(task.spec.stage);
    }
    assert_eq!(
        paid_stages,
        [
            TaskStage::PlanFrontier,
            TaskStage::AssessGaps,
            TaskStage::Synthesize
        ]
    );
    assert_eq!(server.count(), 3);
    let (output, status) = invoke(
        &f,
        "--json",
        &["--offline"],
        &["research", "status", "run_native_research"],
    );
    assert!(output.status.success(), "{status}");
    assert_eq!(status["data"]["inspection"]["state"], "completed");
    assert_eq!(status["meta"]["network_used"], false);
    delete_cache(&f);
    std::fs::remove_file(&config).unwrap();
    let before = tree(f.temp.path());
    for command in [
        vec!["research", "status", "run_native_research"],
        vec!["research", "report", "run_native_research"],
        vec![
            "research",
            "resume",
            "run_native_research",
            "--providers-config",
            config.to_str().unwrap(),
        ],
    ] {
        let format = if command[1] == "resume" {
            "--jsonl"
        } else {
            "--json"
        };
        let (output, retained) = invoke(&f, format, &["--offline"], &command);
        assert!(output.status.success(), "{retained}");
        assert_eq!(retained["meta"]["network_used"], false);
        if command[1] == "report" {
            assert_eq!(&retained["data"], report);
        }
        if command[1] == "resume" {
            assert_eq!(&retained["data"]["report"], report);
        }
        assert_eq!(
            tree(f.temp.path()),
            before,
            "retained command changed tree: {retained}"
        );
    }
    assert_eq!(run.inspect().unwrap().budget.dispatched_requests, 3);
    assert_eq!(server.finish().len(), 3);
    // Explicit caller application is a concrete CLI operation against the staged proposal.
    let change = report["proposed_changes"][0]["change_id"].as_str().unwrap();
    let (output, applied) = invoke(&f, "--jsonl", &["--offline"], &["changes", "apply", change]);
    assert!(output.status.success(), "{applied}");
    assert_eq!(applied["meta"]["network_used"], false);
    assert_eq!(count_pages(&f), 1);
}

#[test]
fn native_research_budget_partial_resume_and_cache_loss_never_trigger_final_generation() {
    let f = Fixture::new();
    let server = Server::new(|request| {
        let data: Value =
            serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            data["binding"]["stage"], "plan_frontier",
            "request limit must prohibit further stages"
        );
        research_reply(request)
    });
    let config = private_config(&f, server.address);
    let (output, envelope) = invoke(
        &f,
        "--json",
        &["--profile", "primary"],
        &[
            "research",
            "run",
            "Ada",
            "--run-id",
            "run_native_partial",
            "--max-requests",
            "1",
            "--providers-config",
            config.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(7), "{envelope}");
    assert_eq!(envelope["error"]["code"], "BUDGET_EXCEEDED");
    assert_eq!(envelope["meta"]["network_used"], true);
    assert_eq!(envelope["meta"]["partial"], true);
    let report = &envelope["data"]["report"];
    assert_eq!(report["partial"], true);
    assert!(report["synthesis"].is_null());
    assert!(!report["gaps"].as_array().unwrap().is_empty());
    let run = ledger(&f, "run_native_partial");
    let before_resume = run.inspect().unwrap();
    assert_eq!(before_resume.state, RunState::Paused);
    assert_eq!(before_resume.effective_limits.requests, 1);
    assert_eq!(before_resume.budget.dispatched_requests, 1);
    let (output, resumed) = invoke(
        &f,
        "--jsonl",
        &["--profile", "primary"],
        &[
            "research",
            "resume",
            "run_native_partial",
            "--providers-config",
            config.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(7), "{resumed}");
    assert_eq!(resumed["error"]["code"], "BUDGET_EXCEEDED");
    assert_eq!(resumed["meta"]["network_used"], false);
    assert_eq!(resumed["meta"]["partial"], true);
    assert_eq!(
        resumed["data"]["report"], *report,
        "budget partial report must be deterministic"
    );
    let after_resume = run.inspect().unwrap();
    assert_eq!(after_resume.spec, before_resume.spec);
    assert_eq!(after_resume.effective_limits.requests, 1);
    assert_eq!(
        after_resume.effective_deadline_utc_ms,
        before_resume.effective_deadline_utc_ms
    );
    assert_eq!(after_resume.budget.dispatched_requests, 1);
    assert_eq!(after_resume.attempts.len(), 1);
    assert_eq!(server.count(), 1);
    delete_cache(&f);
    std::fs::remove_file(&config).unwrap();
    let before = tree(f.temp.path());
    let (output, retained) = invoke(
        &f,
        "--jsonl",
        &["--offline"],
        &[
            "research",
            "resume",
            "run_native_partial",
            "--providers-config",
            config.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{retained}");
    assert_eq!(retained["meta"]["network_used"], false);
    assert_eq!(retained["data"]["report"], *report);
    let (output, read) = invoke(
        &f,
        "--json",
        &["--offline"],
        &["research", "report", "run_native_partial"],
    );
    assert!(output.status.success(), "{read}");
    assert_eq!(read["data"], *report);
    assert_eq!(tree(f.temp.path()), before);
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn research_commands_and_four_exact_schemas_are_concrete_capabilities() {
    let f = Fixture::new();
    let before = tree(f.temp.path());
    let (output, envelope) = invoke(&f, "--json", &[], &["capabilities"]);
    assert!(output.status.success(), "{envelope}");
    let commands = envelope["data"]["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 37);
    for name in [
        "research plan",
        "research run",
        "research resume",
        "research status",
        "research report",
    ] {
        assert!(commands.contains(&json!(name)));
    }
    assert_eq!(envelope["data"]["schemas"].as_array().unwrap().len(), 19);
    for (name, actual) in [
        (
            "research-frontier",
            include_str!("../schemas/research-frontier-v1.json"),
        ),
        (
            "research-gaps",
            include_str!("../schemas/research-gaps-v1.json"),
        ),
        (
            "research-synthesis",
            include_str!("../schemas/research-synthesis-v1.json"),
        ),
        (
            "research-run-plan",
            include_str!("../schemas/research-run-plan-v1.json"),
        ),
    ] {
        let (output, schema) = invoke(&f, "--json", &[], &["schema", name]);
        assert!(output.status.success(), "{schema}");
        assert_eq!(
            schema["data"],
            serde_json::from_str::<Value>(actual).unwrap()
        );
        jsonschema::validator_for(&schema["data"]).unwrap();
    }
    assert_eq!(tree(f.temp.path()), before);
}

#[cfg(unix)]
#[test]
fn native_sigint_after_paid_entry_emits_terminal_130_and_retains_unknown_partial_report() {
    use std::{process::Stdio, sync::mpsc};
    let f = Fixture::new();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let config = private_config(&f, listener.local_addr().unwrap());
    let (entered, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let sent = request(&mut socket);
        assert_eq!(sent.method, "POST");
        assert_eq!(sent.target, "/generate?retained=yes");
        let data: Value =
            serde_json::from_str(sent.body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(data["binding"]["stage"], "plan_frontier");
        // Reading the entire native request proves entry before SIGINT. The server
        // provides no response, so cancellation cannot claim a known paid outcome.
        entered.send(sent).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(30));
        drop(socket);
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "cancelled research sent follow-up work"
        );
    });
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_lwiki"))
        .args([
            "--wiki",
            f.temp.path().to_str().unwrap(),
            "--jsonl",
            "--profile",
            "primary",
            "research",
            "run",
            "Ada",
            "--run-id",
            "run_native_interrupt",
            "--providers-config",
            config.to_str().unwrap(),
        ])
        .env("EXPLICIT_FIXTURE_TOKEN", "synthetic-cli-secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _sent = observed
        .recv_timeout(Duration::from_secs(20))
        .unwrap_or_else(|error| {
            child.kill().unwrap();
            panic!("native child never entered paid request: {error}")
        });
    // SAFETY: this PID identifies the directly owned, still-running disposable
    // child whose complete paid request was received at the channel barrier.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let output = child.wait_with_output().unwrap();
    let envelope = decode_output("--jsonl", &output);
    assert_eq!(output.status.code(), Some(130), "{envelope}");
    assert_eq!(envelope["error"]["code"], "CANCELLED");
    assert_eq!(envelope["meta"]["network_used"], true);
    assert_eq!(envelope["meta"]["partial"], true);
    let report = envelope["data"]["report"].clone();
    assert_eq!(report["partial"], true);
    assert!(report["synthesis"].is_null());
    assert!(report["proposed_changes"].as_array().unwrap().is_empty());
    assert_eq!(count_pages(&f), 0);
    let run = ledger(&f, "run_native_interrupt");
    let inspection = run.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Stopped);
    assert_eq!(inspection.budget.dispatched_requests, 1);
    assert_eq!(inspection.attempts.len(), 1);
    let unknown = &inspection.attempts[0];
    assert_eq!(unknown.phase, AttemptPhase::DispatchIntent);
    assert_eq!(unknown.billing, BillingDisposition::UnknownReserved);
    assert!(unknown.receipt.is_none());
    assert!(unknown.spool.is_none());
    assert!(
        inspection
            .budget
            .unknown_attempts
            .contains(&unknown.attempt.attempt_id)
    );
    let (output, refused) = invoke(
        &f,
        "--jsonl",
        &["--profile", "primary"],
        &[
            "research",
            "resume",
            "run_native_interrupt",
            "--providers-config",
            config.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success(), "{refused}");
    assert_eq!(refused["error"]["code"], "RECOVERY_REQUIRED");
    assert_eq!(refused["meta"]["network_used"], false);
    let retained = run.inspect().unwrap();
    assert_eq!(retained.budget.dispatched_requests, 1);
    assert_eq!(retained.attempts, inspection.attempts);
    std::fs::remove_file(&config).unwrap();
    let before = tree(f.temp.path());
    for command in [
        vec![
            "research",
            "resume",
            "run_native_interrupt",
            "--providers-config",
            config.to_str().unwrap(),
        ],
        vec!["research", "report", "run_native_interrupt"],
    ] {
        let format = if command[1] == "resume" {
            "--jsonl"
        } else {
            "--json"
        };
        let (output, cached) = invoke(&f, format, &["--offline"], &command);
        assert!(output.status.success(), "{cached}");
        assert_eq!(cached["meta"]["network_used"], false);
        assert_eq!(
            if command[1] == "resume" {
                &cached["data"]["report"]
            } else {
                &cached["data"]
            },
            &report
        );
        assert_eq!(tree(f.temp.path()), before);
    }
    release.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn explicit_native_limit_amendment_only_pays_synthesis_and_preserves_lifetime_history() {
    use lwiki::research::ResearchResumeAmendment;
    let f = Fixture::new();
    let server = Server::new(research_reply);
    let config = private_config(&f, server.address);
    let (output, partial) = invoke(
        &f,
        "--jsonl",
        &["--profile", "primary"],
        &[
            "research",
            "run",
            "Ada",
            "--run-id",
            "run_native_amend",
            "--max-requests",
            "2",
            "--providers-config",
            config.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(7), "{partial}");
    assert_eq!(partial["error"]["code"], "BUDGET_EXCEEDED");
    assert_eq!(partial["meta"]["network_used"], true);
    let run = ledger(&f, "run_native_amend");
    let original = run.inspect().unwrap();
    assert_eq!(original.state, RunState::Paused);
    assert_eq!(original.budget.dispatched_requests, 2);
    assert_eq!(original.research.as_ref().unwrap().rounds_started, 1);
    assert_eq!(
        original.research.as_ref().unwrap().rounds.len(),
        1,
        "first round assessment must already be closed"
    );
    assert_eq!(
        original.spec.scope.research.as_ref().unwrap().limits.rounds,
        3
    );
    let mut limits = original.effective_limits.clone();
    limits.requests = 3;
    let amendment = ResearchResumeAmendment {
        limits,
        deadline_utc_ms: original.effective_deadline_utc_ms,
        reason: "Caller permits exactly one remaining synthesis request.".into(),
    };
    let amendment_path = f.temp.path().join("requested-amendment.json");
    std::fs::write(&amendment_path, serde_json::to_vec(&amendment).unwrap()).unwrap();
    let missing_config = f.temp.path().join("unavailable-providers.toml");
    let before = tree(f.temp.path());
    for globals in [vec!["--dry-run"], vec!["--offline"]] {
        let (output, preview) = invoke(
            &f,
            "--jsonl",
            &globals,
            &[
                "research",
                "resume",
                "run_native_amend",
                "--amend-limits",
                amendment_path.to_str().unwrap(),
                "--providers-config",
                missing_config.to_str().unwrap(),
            ],
        );
        assert!(output.status.success(), "{preview}");
        assert_eq!(
            preview["data"]["amendment"],
            serde_json::to_value(&amendment).unwrap()
        );
        assert_eq!(preview["meta"]["network_used"], false);
        assert_eq!(
            tree(f.temp.path()),
            before,
            "amendment preview must not journal or dispatch"
        );
        assert_eq!(
            run.inspect().unwrap().effective_limits,
            original.effective_limits
        );
    }
    let (output, refused) = invoke(
        &f,
        "--jsonl",
        &[],
        &[
            "research",
            "resume",
            "run_native_amend",
            "--max-requests",
            "3",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{refused}");
    assert_eq!(refused["error"]["code"], "USAGE");
    assert_eq!(tree(f.temp.path()), before);
    let (output, resumed) = invoke(
        &f,
        "--jsonl",
        &["--profile", "primary"],
        &[
            "research",
            "resume",
            "run_native_amend",
            "--amend-limits",
            amendment_path.to_str().unwrap(),
            "--providers-config",
            config.to_str().unwrap(),
        ],
    );
    assert!(output.status.success(), "{resumed}");
    assert_eq!(resumed["meta"]["network_used"], true);
    assert_eq!(resumed["meta"]["partial"], false);
    let completed = run.inspect().unwrap();
    assert_eq!(completed.state, RunState::Completed);
    assert_eq!(completed.spec, original.spec, "genesis remains immutable");
    assert_eq!(completed.spec.limits.requests, 2);
    assert_eq!(completed.effective_limits, amendment.limits);
    assert_eq!(
        completed.effective_deadline_utc_ms,
        original.effective_deadline_utc_ms
    );
    assert_eq!(completed.budget.dispatched_requests, 3);
    assert_eq!(completed.attempts.len(), 3);
    assert_eq!(&completed.attempts[..2], original.attempts.as_slice());
    assert_eq!(
        completed.research.as_ref().unwrap().rounds,
        original.research.as_ref().unwrap().rounds
    );
    assert_eq!(completed.research.as_ref().unwrap().rounds_started, 1);
    assert_eq!(
        completed.research.as_ref().unwrap().origins,
        original.research.as_ref().unwrap().origins
    );
    for held in &original.budget.unknown_attempts {
        assert!(completed.budget.unknown_attempts.contains(held));
    }
    assert_eq!(
        completed.tasks[&completed.attempts[2].attempt.task_key]
            .spec
            .stage,
        TaskStage::Synthesize
    );
    assert_eq!(count_pages(&f), 0, "amendment confers no apply authority");
    let requests = server.finish();
    assert_eq!(requests.len(), 3);
    let stages: Vec<_> = requests
        .iter()
        .map(|request| {
            let data: Value =
                serde_json::from_str(request.body["messages"][1]["content"].as_str().unwrap())
                    .unwrap();
            data["binding"]["stage"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(stages, ["plan_frontier", "assess_gaps", "synthesize"]);
}

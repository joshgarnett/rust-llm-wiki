//! Follow both public graph routes with authored proposals and a loopback provider.
#[path = "../test_support/paths.rs"]
mod test_paths;
use lwiki::{
    domain::{Blake3Hash, CitationRef, VaultRelativePath},
    jobs::{
        AttemptPhase, BillableClass, BillingDisposition, LedgerInspection, RemoteExposure, RunState,
    },
    vault::VaultRoot,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const NOTE: &str = "# Café meeting\n\nAda works for Acme.\nAda Chen does not work for Acme.\n";

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new(normalized: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Ada's wiki with spaces");
        let f = Self { temp, root };
        f.ok(&["init", f.root.to_str().unwrap()], None);
        if normalized {
            f.ok(&["index", "rebuild", "--normalized"], None);
        }
        f
    }
    fn invoke(&self, args: &[&str], stdin: Option<&Value>) -> (i32, Value) {
        self.invoke_mode(args, stdin, true)
    }
    fn invoke_mode(&self, args: &[&str], stdin: Option<&Value>, offline: bool) -> (i32, Value) {
        let mut command = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
        if offline {
            command.arg("--offline");
        }
        let mut child = command
            .current_dir(self.temp.path())
            .arg("--wiki")
            .arg(&self.root)
            .args(["--json", "--no-user-config"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(value) = stdin {
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(&serde_json::to_vec(value).unwrap())
                .unwrap();
        }
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?}: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
        if offline {
            assert_eq!(value["meta"]["network_used"], false, "{args:?}");
        }
        (output.status.code().unwrap(), value)
    }
    fn ok(&self, args: &[&str], stdin: Option<&Value>) -> Value {
        let (code, value) = self.invoke(args, stdin);
        assert_eq!(code, 0, "{args:?}: {value}");
        value["data"].clone()
    }
    fn apply(&self, outcome: &Value) {
        let change = outcome["prepared"]["change_id"].as_str().unwrap();
        self.ok(&["changes", "show", change], None);
        self.ok(&["changes", "apply", change], None);
    }
    fn read(&self, id: &str) -> Value {
        self.ok(&["read", "--id", id, "--max-bytes", "12000"], None)
    }
    fn physical_path(&self, logical: &str) -> PathBuf {
        VaultRoot::explicit(&self.root)
            .unwrap()
            .resolve(&VaultRelativePath::new(logical).unwrap())
            .unwrap()
    }
    fn capture(&self) -> (String, String) {
        let path = self.temp.path().join("Ada's meeting note.md");
        fs::write(&path, NOTE).unwrap();
        let captured = self.ok(&["source", "capture", path.to_str().unwrap()], None);
        let source = captured["items"][0]["source_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let revision = captured["items"][0]["revision_id"]
            .as_str()
            .unwrap()
            .to_owned();
        (source, revision)
    }
    fn imported(&self) -> (Value, String, String) {
        let (source, revision) = self.capture();
        let exported = self.ok(
            &[
                "graph",
                "extract",
                "--executor",
                "agent",
                "--source-id",
                &source,
            ],
            None,
        );
        assert_eq!(exported["ready_to_import"], true);
        let packet = &exported["packet"];
        assert_eq!(packet["windows"][0]["text"], NOTE);
        let response = extraction_response(packet);
        let imported = self.ok(&["graph", "import", "--file", "-"], Some(&response));
        assert_eq!(imported["status"], "prepared");
        assert_eq!(imported["coverage"]["pending_mentions"], 3);
        let extraction = self.physical_path(imported["extraction"]["path"].as_str().unwrap());
        assert!(!extraction.exists());
        self.apply(&imported);
        assert!(extraction.is_file());
        (imported, source, revision)
    }
    fn resolve(&self, imported: &Value) -> Value {
        let extraction = imported["extraction"]["record"]["record_id"]
            .as_str()
            .unwrap();
        let current = self.read(extraction);
        let mappings = [("m1", "Ada", "person"),("m2", "Ada Chen", "person"),("m3", "Acme", "organization")].into_iter().map(|(mention,title,kind)| json!({"operation":"CreateEntity","mention_id":mention,"reason":"The quoted note identifies this distinct participant.","title":title,"entity_type":kind})).collect::<Vec<_>>();
        let request = json!({"schema":"lwiki.graph-resolution.v1","extraction_id":extraction,"expected_hash":current["hash"],"mappings":mappings});
        let resolved = self.ok(&["graph", "resolve", "--file", "-"], Some(&request));
        self.apply(&resolved);
        assert_ne!(
            resolved["allocations"]["entities"]["m1"],
            resolved["allocations"]["entities"]["m2"]
        );
        resolved
    }
    fn review(&self, imported: &Value, source: &str, revision: &str) -> Value {
        let mut decisions = Vec::new();
        for local in ["a1", "a2"] {
            let assertion_id = imported["allocations"]["assertions"][local]
                .as_str()
                .unwrap();
            let assertion = self.read(assertion_id);
            assert_eq!(assertion["record"]["wiki_status"], "proposed");
            let mut checks = Vec::new();
            for id in imported["allocations"]["evidence"][local]
                .as_array()
                .unwrap()
            {
                let evidence_id = id.as_str().unwrap();
                let evidence = self.read(evidence_id);
                let record = &evidence["record"];
                assert_eq!(record["wiki_source_id"], source);
                assert_eq!(record["wiki_source_revision"], revision);
                let start = record["wiki_span_start"].as_u64().unwrap().to_string();
                let end = record["wiki_span_end"].as_u64().unwrap().to_string();
                let passage = self.ok(
                    &[
                        "source",
                        "read",
                        source,
                        "--revision",
                        revision,
                        "--start",
                        &start,
                        "--end",
                        &end,
                        "--max-bytes",
                        "12000",
                    ],
                    None,
                );
                let quote = passage["read"]["body"].as_str().unwrap();
                let expected = if local == "a1" {
                    "Ada works for Acme."
                } else {
                    "Ada Chen does not work for Acme."
                };
                assert_eq!(quote, expected);
                assert_eq!(
                    record["wiki_quote_hash"],
                    Blake3Hash::digest(quote.as_bytes()).as_str()
                );
                checks.push(json!({"evidence_id":evidence_id,"expected_hash":evidence["hash"],"assessment":"supports"}));
            }
            decisions.push(json!({"assertion_id":assertion_id,"expected_hash":assertion["hash"],"decision":"accept","reason":"The exact current passage explicitly supports the qualified claim.","evidence_checks":checks}));
        }
        self.ok(
            &["graph", "review", "--file", "-"],
            Some(&json!({"schema":"lwiki.graph-review.v1","decisions":decisions})),
        )
    }
    fn assert_neighbors(&self, imported: &Value, resolved: &Value, source: &str, revision: &str) {
        for (mention, local, negated) in [("m1", "a1", false), ("m2", "a2", true)] {
            let entity = resolved["allocations"]["entities"][mention]
                .as_str()
                .unwrap();
            let graph = self.ok(&["graph", "neighbors", entity, "--depth", "1"], None);
            let assertions = graph["assertions"].as_array().unwrap();
            assert_eq!(assertions.len(), 1, "{graph}");
            let assertion = &assertions[0];
            assert_eq!(
                assertion["record_ref"]["record_id"],
                imported["allocations"]["assertions"][local]
            );
            assert_eq!(assertion["authored_status"], "accepted");
            assert_eq!(assertion["qualifiers"]["negated"], negated);
            assert_eq!(assertion["predicate"], "works_for");
            for evidence in assertion["support"].as_array().unwrap() {
                let citation: CitationRef =
                    serde_json::from_value(evidence["citation"].clone()).unwrap();
                let (sid, rev, span, hash) = match citation {
                    CitationRef::Assertion(r) => {
                        (r.source_id, r.source_revision, r.span, r.quote_hash)
                    }
                    CitationRef::Source(r) => {
                        (r.source_id, r.source_revision, r.span, r.quote_hash)
                    }
                };
                assert_eq!(sid.as_str(), source);
                assert_eq!(rev.as_str(), revision);
                let quote = &NOTE.as_bytes()[span.start() as usize..span.end() as usize];
                assert_eq!(hash, Blake3Hash::digest(quote));
                assert_eq!(
                    quote,
                    if negated {
                        b"Ada Chen does not work for Acme.".as_slice()
                    } else {
                        b"Ada works for Acme.".as_slice()
                    }
                );
            }
        }
    }
}

fn extraction_response(packet: &Value) -> Value {
    let mention = |id: &str, label: &str, kind: &str, start: usize| {
        json!({
            "id":id,"window_id":"w1","label":label,"type":kind,"quote":label,
            "span":{"start":start,"end":start + label.len()}
        })
    };
    let assertion = |id: &str, subject: &str, negated: bool, quote: &str| {
        json!({
            "id":id,"subject":subject,"predicate":"works_for","object":{"kind":"mention","mention_id":"m3"},
            "negated":negated,"modality":"asserted","evidence":[{"window_id":"w1","stance":"supports","quote":quote}]
        })
    };
    json!({"schema":"lwiki.extraction.v1","packet_id":packet["packet_id"],"packet_fingerprint":packet["packet_fingerprint"],
            "mentions":[mention("m1","Ada","person",NOTE.find("Ada works").unwrap()),mention("m2","Ada Chen","person",NOTE.find("Ada Chen").unwrap()),mention("m3","Acme","organization",NOTE.find("Acme").unwrap())],
            "assertions":[assertion("a1","m1",false,"Ada works for Acme."),assertion("a2","m2",true,"Ada Chen does not work for Acme.")],"unresolved":[]})
}

#[test]
fn both_layouts_capture_propose_resolve_review_and_return_exact_cited_neighbors() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (imported, source, revision) = f.imported();
        let resolved = f.resolve(&imported);
        let review = f.review(&imported, &source, &revision);
        f.apply(&review);
        f.assert_neighbors(&imported, &resolved, &source, &revision);
    }
}

#[test]
fn staged_review_refuses_external_edit_and_preserves_proposals() {
    let f = Fixture::new(false);
    let (imported, source, revision) = f.imported();
    f.resolve(&imported);
    let review = f.review(&imported, &source, &revision);
    let id = imported["allocations"]["assertions"]["a1"]
        .as_str()
        .unwrap();
    let current = f.read(id);
    let path = f.root.join(current["path"].as_str().unwrap());
    let original = fs::read(&path).unwrap();
    let edited = [
        original.as_slice(),
        b"\nOwner clarification: leave this proposal pending.\n",
    ]
    .concat();
    fs::write(&path, &edited).unwrap();
    let change = review["prepared"]["change_id"].as_str().unwrap();
    let (exit, result) = f.invoke(&["changes", "apply", change], None);
    assert_ne!(exit, 0, "{result}");
    assert_eq!(fs::read(path).unwrap(), edited);
    assert!(
        !String::from_utf8(edited)
            .unwrap()
            .contains("wiki_status: accepted")
    );
}

// The only network destination is this disposable loopback fixture. Its output
// is authored from the actual packet, including that packet's identity.
struct GenerationMock {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl GenerationMock {
    fn new() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/responses", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (captured, stopping) = (requests.clone(), stop.clone());
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
                let packet: Value =
                    serde_json::from_str(request["input"][0]["content"].as_str().unwrap()).unwrap();
                assert_eq!(packet["windows"][0]["text"], NOTE);
                let text = serde_json::to_string(&extraction_response(&packet)).unwrap();
                captured.lock().unwrap().push(request);
                let response = json!({"object":"response","status":"completed","model":"test-model","error":null,"incomplete_details":null,
                    "output":[{"type":"reasoning"},{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}],
                    "usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":0},"output_tokens":200,"output_tokens_details":{"reasoning_tokens":20},"total_tokens":300}});
                let body = serde_json::to_vec(&response).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for GenerationMock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn configured_api_effort_uses_one_request_then_shared_apply_resolve_review_and_neighbors() {
    let f = Fixture::new(true);
    let mock = GenerationMock::new();
    let providers = f.temp.path().join("providers.toml");
    let preferences = f.temp.path().join("preferences.toml");
    fs::write(&providers, format!("version=1\n[profiles.notes]\ngeneration='generate'\n[services.generate]\nadapter='responses-v1'\nurl={}\nallow_loopback_http=true\nmodel='test-model'\ntimeout_seconds=5\nmax_output_tokens=4096\n[services.generate.auth]\nkind='static'\nkey='disposable-fixture-key'\n[vault_bindings]\n",json!(mock.url))).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&providers, fs::Permissions::from_mode(0o600)).unwrap();
    }
    f.ok(
        &[
            "--preferences",
            preferences.to_str().unwrap(),
            "--profile",
            "notes",
            "config",
            "set",
            "generation",
            "--providers-config",
            providers.to_str().unwrap(),
            "--reasoning-effort",
            "low",
            "--grant-provider",
            "--non-interactive",
        ],
        None,
    );
    assert!(mock.requests.lock().unwrap().is_empty());
    let (source, revision) = f.capture();
    // This positive compatible-gateway fixture uses the wire output limit.
    // It does not request a hard billed-unit guarantee for unknown bounds.
    let extraction_args = [
        "--preferences",
        preferences.to_str().unwrap(),
        "graph",
        "extract",
        "--executor",
        "api",
        "--source-id",
        &source,
        "--providers-config",
        providers.to_str().unwrap(),
        "--max-requests",
        "1",
        "--max-output-tokens",
        "4096",
        "--deadline-ms",
        "60000",
    ];
    let (code, envelope) = f.invoke_mode(&extraction_args, None, false);
    assert_eq!(code, 0, "{envelope}");
    assert_eq!(envelope["meta"]["network_used"], true);
    let imported = &envelope["data"]["import"];
    assert_eq!(imported["status"], "prepared", "{envelope}");
    let extraction = f.physical_path(imported["extraction"]["path"].as_str().unwrap());
    assert!(!extraction.exists());
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    assert_eq!(envelope["data"]["reused"], false);
    let retained_outputs: Vec<_> = ["output", "receipt"]
        .into_iter()
        .map(|field| {
            let reference = &envelope["data"][field];
            let path = f.physical_path(reference["path"].as_str().unwrap());
            let bytes = fs::read(&path).unwrap();
            assert_eq!(reference["hash"], Blake3Hash::digest(&bytes).as_str());
            (path, bytes)
        })
        .collect();
    // Repeat before apply/resolution/review can change the candidate context.
    let (repeat_code, repeated) = f.invoke_mode(&extraction_args, None, false);
    assert_eq!(repeat_code, 0, "{repeated}");
    assert_eq!(repeated["meta"]["network_used"], false);
    assert_eq!(repeated["data"]["reused"], true);
    for field in ["packet", "run_id", "task_key", "output", "receipt"] {
        assert_eq!(repeated["data"][field], envelope["data"][field], "{field}");
    }
    let repeated_import = &repeated["data"]["import"];
    assert_eq!(repeated_import["status"], "prepared", "{repeated}");
    assert_eq!(repeated_import["reused"], true);
    for field in ["prepared", "extraction", "allocations"] {
        assert_eq!(repeated_import[field], imported[field], "{field}");
    }
    for (path, bytes) in retained_outputs {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert!(!extraction.exists());
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    // API already prepares the import. There is no second graph import.
    f.apply(imported);
    assert!(extraction.is_file());
    let resolved = f.resolve(imported);
    let review = f.review(imported, &source, &revision);
    f.apply(&review);
    f.assert_neighbors(imported, &resolved, &source, &revision);
    let status: LedgerInspection = serde_json::from_value(f.ok(
        &[
            "jobs",
            "status",
            "--run",
            envelope["data"]["run_id"].as_str().unwrap(),
        ],
        None,
    ))
    .unwrap();
    assert_eq!(status.state, RunState::Completed);
    assert_eq!(status.attempts.len(), 1);
    let attempt = &status.attempts[0];
    // Completed graph work and a durable settled-phase receipt do not settle
    // unknown monetary charges. Keep the original billing hold visible.
    assert_eq!(attempt.phase, AttemptPhase::Settled);
    assert_eq!(attempt.billing, BillingDisposition::UnknownReserved);
    assert_eq!(attempt.remote_exposure, RemoteExposure::TerminalConfirmed);
    assert!(attempt.receipt.is_some());
    assert_eq!(status.budget.dispatched_requests, 1);
    assert_eq!(status.budget.outstanding_requests, 0);
    assert_eq!(status.budget.remote_inflight, 0);
    assert_eq!(status.budget.concurrency_admitted, 0);
    assert_eq!(
        status.budget.unknown_attempts,
        vec![attempt.attempt.attempt_id.clone()]
    );
    // Provider output_tokens includes its reasoning_tokens subset. Retained
    // accounting splits 200 generated tokens into 180 visible and 20 reasoning.
    let usage = &status.budget.outstanding.billable_units;
    assert_eq!(usage[&BillableClass::Output], 180);
    assert_eq!(usage[&BillableClass::Reasoning], 20);
    let generated = usage[&BillableClass::Output] + usage[&BillableClass::Reasoning];
    assert_eq!(generated, 200);
    assert!(generated <= 4096);
    assert!(attempt.bound.rate_card.is_none());
    assert!(status.budget.outstanding.cost.is_none());
    assert!(status.budget.known_costs.is_empty());
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["reasoning"]["effort"], "low");
    assert_eq!(requests[0]["max_output_tokens"], 4096);
}

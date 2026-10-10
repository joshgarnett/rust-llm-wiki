//! One opt-in native-default diagnostic. No production representation or source reads in hooks.
use super::context_types::ContextPassage;
use crate::{catalog::DocumentRow, domain::Blake3Hash};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const ROOT: &str = "/Users/jgarnett/Devleopment/rust-llm-wiki/.artifacts/workflow-priority-resume-20261007/representative-default-quality-next-20261010-001";
const TEST: &str = "retrieval::context_stage_trace::frozen_native_default_stage_trace";
const TOTAL_BYTES: usize = 64 * 1024 * 1024;
// Reserve room for all native responses, errors, comparisons and the supervisor ledger.
const RESPONSE_RESERVE: usize = 256 * 1024;
// Distinct ledger reserve covers forty worst-case JSON-escaped bounded stderr rows.
const SUMMARY_RESERVE: usize = 2 * 1024 * 1024;
struct Observer {
    owners: BTreeMap<usize, Value>,
    events: Vec<Value>,
    counts: BTreeMap<String, usize>,
    bytes: usize,
    cap: usize,
    overflow: bool,
}
thread_local! { static OBSERVER: RefCell<Option<Observer>> = const { RefCell::new(None) }; }

pub(super) fn if_active<T>(run: impl FnOnce() -> T) -> Option<T> {
    let active = OBSERVER.with(|observer| {
        observer
            .borrow()
            .as_ref()
            .is_some_and(|observer| !observer.overflow)
    });
    if active { Some(run()) } else { None }
}
pub(super) fn event(stage: &str, row: impl FnOnce() -> Value) {
    OBSERVER.with(|observer| {
        if let Some(observer) = observer.borrow_mut().as_mut() {
            *observer.counts.entry(stage.to_owned()).or_default() += 1;
            if observer.overflow {
                return;
            }
            let row = json!({"stage": stage, "row": row()});
            let bytes = serde_json::to_vec(&row).unwrap().len() + 1;
            if bytes > observer.cap.saturating_sub(observer.bytes) {
                observer.overflow = true;
                return;
            }
            observer.bytes += bytes;
            observer.events.push(row);
        }
    });
}
pub(super) fn admit_owner(index: usize, document: &DocumentRow) {
    OBSERVER.with(|observer| {
        if let Some(observer) = observer.borrow_mut().as_mut() {
            observer.owners.insert(index, json!({"owner_index":index,"path":document.path,
                "hash":document.hash,"source_id":document.source_id,"revision":document.owner_revision}));
        }
    });
    event("authenticated_owner", || {
        json!({"owner_index":index,"path":document.path,
        "hash":document.hash,"source_id":document.source_id,"revision":document.owner_revision})
    });
}
fn identity(path: &Value, hash: &Value, start: u64, end: u64) -> Blake3Hash {
    Blake3Hash::digest(serde_json::to_vec(&(path, hash, start, end)).unwrap())
}
pub(super) fn candidate_event(stage: &str, owner: usize, start: usize, end: usize, reason: &str) {
    // Construct coordinates only while the session is active and still admits rows.
    let coordinates = OBSERVER.with(|observer| {
        let observer = observer.borrow();
        let observer = observer.as_ref()?;
        if observer.overflow {
            return None;
        }
        Some(
            observer
                .owners
                .get(&owner)
                .cloned()
                .unwrap_or(json!({"owner_index":owner,"owner_unregistered":true})),
        )
    });
    event(stage, || {
        let mut row = coordinates.unwrap_or(Value::Null);
        if row.is_object() {
            row["candidate_id"] = json!(identity(
                &row["path"],
                &row["hash"],
                start as u64,
                end as u64
            ));
            row["start"] = json!(start);
            row["end"] = json!(end);
            row["reason"] = json!(reason);
        }
        row
    });
}
pub(super) fn passage_identity(passage: &ContextPassage) -> Value {
    let path = json!(passage.locator.path);
    let hash = json!(passage.locator.observed_hash);
    json!({"candidate_id":identity(&path,&hash,passage.span.start(),passage.span.end()),
        "path":path,"hash":hash,"start":passage.span.start(),"end":passage.span.end(),
        "record":passage.locator.record})
}
fn observe<T>(cap: usize, run: impl FnOnce() -> T) -> (T, Value) {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            OBSERVER.with(|observer| {
                observer.borrow_mut().take();
            });
        }
    }
    OBSERVER.with(|observer| {
        assert!(observer.borrow().is_none());
        *observer.borrow_mut() = Some(Observer {
            owners: BTreeMap::new(),
            events: vec![],
            counts: BTreeMap::new(),
            bytes: 0,
            cap,
            overflow: false,
        });
    });
    let _clear = Clear;
    let result = run();
    let observer = OBSERVER.with(|observer| observer.borrow_mut().take().unwrap());
    (
        result,
        json!({"trace_complete":!observer.overflow,"overflow":observer.overflow,
        "absence_interpretation":if observer.overflow {"unknown"} else {"observed pipeline only; scan/discovery workcaps still apply"},
        "terminal_counts":observer.counts,"recorded_bytes":observer.bytes,"events":observer.events}),
    )
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: u32,
    recorded_utc: String,
    base_commit: String,
    vault: PathBuf,
    output: PathBuf,
    limits: Limits,
    request: String,
    tasks: Vec<Task>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    queries: usize,
    seconds_each: u64,
    aggregate_seconds: u64,
    trace_bytes: usize,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    id: String,
    query: String,
    baseline_output: PathBuf,
    baseline_sha256: String,
}
fn bounded_read(path: &Path, cap: usize) -> Vec<u8> {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file() && metadata.len() <= cap as u64);
    fs::read(path).unwrap()
}
fn difference_paths(expected: &Value, actual: &Value, prefix: &str, paths: &mut Vec<String>) {
    if expected == actual {
        return;
    }
    match (expected, actual) {
        (Value::Object(a), Value::Object(b)) => {
            let keys = a
                .keys()
                .chain(b.keys())
                .collect::<std::collections::BTreeSet<_>>();
            for key in keys {
                let key_path = format!("{prefix}/{key}");
                match (a.get(key), b.get(key)) {
                    (Some(expected), Some(actual)) => {
                        difference_paths(expected, actual, &key_path, paths)
                    }
                    (Some(_), None) => paths.push(format!("{key_path} (missing key)")),
                    (None, Some(_)) => paths.push(format!("{key_path} (added key)")),
                    (None, None) => unreachable!("key came from union"),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (a, b)) in a.iter().zip(b).enumerate() {
                difference_paths(a, b, &format!("{prefix}/{index}"), paths);
            }
        }
        _ => paths.push(prefix.to_owned()),
    }
}
fn write_new(path: &Path, value: &Value, cap: usize) -> usize {
    use std::io::Write;
    let bytes = serde_json::to_vec(value).unwrap();
    assert!(bytes.len() <= cap);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    bytes.len()
}
#[test]
#[ignore = "explicit neutral frozen development config; root serializes one native diagnostic"]
fn frozen_native_default_stage_trace() {
    let root = Path::new(ROOT).canonicalize().unwrap();
    let config_path =
        PathBuf::from(std::env::var("LWIKI_STAGE_TRACE_CONFIG").expect("explicit neutral config"));
    assert_eq!(
        config_path.canonicalize().unwrap(),
        root.join("STAGE-TRACE-NEUTRAL-CONFIG001.json")
    );
    let config_pin = Command::new("/usr/bin/shasum")
        .args(["-a", "256", "--"])
        .arg(&config_path)
        .output()
        .unwrap();
    assert!(config_pin.status.success());
    assert_eq!(
        std::str::from_utf8(&config_pin.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap(),
        "bf8526067d35f4657e3d181a4e47bee1ec7736a61c86825cbb521414ac73b315"
    );
    let config: Config = serde_json::from_slice(&bounded_read(&config_path, 128 * 1024)).unwrap();
    assert_eq!(config.schema_version, 1);
    assert!(!config.recorded_utc.is_empty());
    assert_eq!(
        config.base_commit,
        "3e2d2b4cf3b3b4319189062231f6ef9f5968bf2b"
    );
    assert_eq!(config.tasks.len(), 40);
    assert_eq!(
        (
            config.limits.queries,
            config.limits.seconds_each,
            config.limits.aggregate_seconds,
            config.limits.trace_bytes
        ),
        (40, 15, 600, TOTAL_BYTES)
    );
    assert_eq!(
        config.vault.canonicalize().unwrap(),
        root.join("runtime-account001/import002/vault")
    );
    assert_eq!(config.output, root.join("stage-trace001"));
    assert_eq!(
        config.request,
        "offline lexical indexed-documents automatic; native defaults unchanged (10owners80candidates1024excerpt12000bytes3000estimatedtokens; proof2s)"
    );
    let mut ids = std::collections::BTreeSet::new();
    for (index, task) in config.tasks.iter().enumerate() {
        assert!(ids.insert(&task.id) && task.query.len() <= 4096);
        assert_eq!(
            task.baseline_output.canonicalize().unwrap(),
            root.join(format!(
                "runtime-account001/baseline-dev001/question-{index:04}-context.stdout"
            ))
        );
        assert!(
            task.baseline_sha256.len() == 64
                && task
                    .baseline_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
        );
    }
    if let Ok(index) = std::env::var("LWIKI_STAGE_TRACE_CHILD") {
        child(&config, index.parse().unwrap());
        return;
    }
    fs::create_dir(&config.output).expect("new output directory only");
    let started = Instant::now();
    let mut bytes = 0usize;
    let mut ledger = Vec::new();
    let executable = std::env::current_exe();
    let mut supervisor_halt = executable
        .as_ref()
        .err()
        .map(|error| format!("current_exe: {error}"));
    for index in 0..40 {
        if let Some(reason) = &supervisor_halt {
            ledger.push(json!({"index":index,"id":config.tasks[index].id,
                "status":"UNRUN_SUPERVISOR_ERROR", "reason":reason, "trace_complete":false}));
            continue;
        }
        if started.elapsed() >= Duration::from_secs(600) {
            ledger.push(
                json!({"index":index,"id":config.tasks[index].id,"status":"UNRUN_AGGREGATE_BOUND"}),
            );
            continue;
        }
        let remaining = TOTAL_BYTES.saturating_sub(bytes);
        // Every remaining query reserves a response envelope even if tracing overflows.
        let reserve = (40 - index) * RESPONSE_RESERVE + SUMMARY_RESERVE;
        let cap = remaining.saturating_sub(reserve).min(8 * 1024 * 1024);
        let attempt = Instant::now();
        let process = Command::new(executable.as_ref().expect("checked executable"))
            .args([
                "--ignored",
                "--exact",
                TEST,
                "--nocapture",
                "--test-threads=1",
            ])
            .env("LWIKI_STAGE_TRACE_CHILD", index.to_string())
            .env("LWIKI_STAGE_TRACE_CAP", cap.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env("RUST_BACKTRACE", "0")
            .spawn();
        let mut process = match process {
            Ok(process) => process,
            Err(error) => {
                let reason = format!("spawn: {error}");
                ledger.push(json!({"index":index,"id":config.tasks[index].id,"status":"SPAWN_ERROR",
                    "reason":reason,"elapsed_ns":attempt.elapsed().as_nanos(),"trace_complete":false}));
                supervisor_halt = Some(reason);
                continue;
            }
        };
        let mut timed_out = false;
        let mut process_errors = Vec::new();
        let mut status = loop {
            match process.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(error) => {
                    process_errors.push(format!("try_wait: {error}"));
                    break None;
                }
            }
            if attempt.elapsed() >= Duration::from_secs(15)
                || started.elapsed() >= Duration::from_secs(600)
            {
                timed_out = true;
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // No further child starts unless this one has been confirmed reaped.
        // A failed kill may be an exit race; one observation records that state.
        if status.is_none() {
            match process.kill() {
                Ok(()) => match process.wait() {
                    Ok(reaped) => status = Some(reaped),
                    Err(error) => {
                        process_errors.push(format!("wait after successful kill: {error}"))
                    }
                },
                Err(error) => {
                    process_errors.push(format!("kill: {error}"));
                    match process.try_wait() {
                        Ok(reaped) => status = reaped,
                        Err(error) => {
                            process_errors.push(format!("observe after failed kill: {error}"))
                        }
                    }
                }
            }
        }
        let confirmed_reaped = status.is_some();
        use std::io::Read;
        let mut stderr_bytes = Vec::new();
        let mut stderr_error = None;
        if confirmed_reaped {
            if let Some(stderr) = process.stderr.take() {
                if let Err(error) = stderr.take(4097).read_to_end(&mut stderr_bytes) {
                    let reason = format!("read_stderr: {error}");
                    stderr_error = Some(reason.clone());
                    process_errors.push(reason);
                }
            }
        }
        let stderr_truncated = stderr_bytes.len() > 4096;
        stderr_bytes.truncate(4096);
        let process_stderr = String::from_utf8_lossy(&stderr_bytes);
        let path = config.output.join(format!("question-{index:04}.json"));
        let (artifact_exists, artifact_bytes) = match fs::metadata(&path) {
            Ok(metadata) => (true, metadata.len() as usize),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (false, 0),
            Err(error) => {
                process_errors.push(format!("artifact metadata: {error}"));
                (false, 0)
            }
        };
        bytes = bytes.saturating_add(artifact_bytes);
        if bytes.saturating_add(SUMMARY_RESERVE) > TOTAL_BYTES {
            process_errors.push("aggregate artifact bound exceeded; no output removed".to_owned());
        }
        if !confirmed_reaped || !process_errors.is_empty() {
            supervisor_halt = Some(format!(
                "case {index}: child_reaped={confirmed_reaped}; {}",
                process_errors.join("; ")
            ));
        }
        ledger.push(json!({"index":index,"id":config.tasks[index].id,
            "status":if !process_errors.is_empty() || !confirmed_reaped {"SUPERVISOR_ERROR"} else if timed_out {"TIMEOUT"} else if status.as_ref().is_some_and(|status|status.success()){ "RETURNED" } else {"PROCESS_ERROR"},
            "exit_code":status.as_ref().and_then(|status|status.code()),"child_pid":process.id(),
            "child_confirmed_reaped":confirmed_reaped,"process_control_errors":process_errors,
            "stderr":process_stderr,"stderr_error":stderr_error,"stderr_may_be_truncated":stderr_truncated,
            "stderr_unknown_live_child":!confirmed_reaped,"elapsed_ns":attempt.elapsed().as_nanos(),
            "artifact_bytes":artifact_bytes,"artifact_exists":artifact_exists}));
    }
    let summary = json!({"attempts":ledger,"elapsed_ns":started.elapsed().as_nanos(),"artifact_bytes":bytes,
        "limits":{"attempts":40,"seconds_each":15,"aggregate_seconds":600,"artifact_bytes":TOTAL_BYTES},
        "timing":"owning Rust Instant; child records native call interval separately; supervisor interval includes process launch and I/O",
        "supervisor_halt":supervisor_halt,
        "termination":"kill initiated at owning 15s/600s deadline; cancellation/reap interval is included in recorded supervisor elapsed, no native retry",
        "trace_absence":"unknown after overflow, timeout, process error, or an unrun bounded attempt; discovery membership before each leg and the 80-candidate merge cap is unknown"});
    write_new(
        &config.output.join("summary.json"),
        &summary,
        SUMMARY_RESERVE,
    );
}
fn child(config: &Config, index: usize) {
    assert!(index < 40);
    let task = &config.tasks[index];
    let cap: usize = std::env::var("LWIKI_STAGE_TRACE_CAP")
        .unwrap()
        .parse()
        .unwrap();
    assert!(cap <= 8 * 1024 * 1024);
    let baseline_bytes = bounded_read(&task.baseline_output, RESPONSE_RESERVE);
    let sha = Command::new("/usr/bin/shasum")
        .args(["-a", "256", "--"])
        .arg(&task.baseline_output)
        .output()
        .unwrap();
    assert!(sha.status.success());
    assert_eq!(
        std::str::from_utf8(&sha.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap(),
        task.baseline_sha256
    );
    let baseline: Value = serde_json::from_slice(&baseline_bytes).unwrap();
    let fs_handle =
        crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&config.vault).unwrap());
    let marker = crate::records::parse_note(&bounded_read(
        &config.vault.join("WIKI.md"),
        RESPONSE_RESERVE,
    ));
    let catalog = crate::catalog::Catalog::new(fs_handle, marker.canonical.unwrap().id().clone());
    assert!(
        catalog.operation_state().unwrap().is_some(),
        "no implicit reconstruction or sync"
    );
    let mut request = super::context_types::ContextRequest {
        scope: super::context_types::ContextScope::IndexedDocuments,
        ..Default::default()
    };
    request.documents.limits.excerpt_bytes = 1024;
    assert_eq!(
        (
            request.documents.limits.hits,
            request.documents.limits.candidates,
            request.budget.max_bytes,
            request.budget.max_tokens,
            request.verification_budget.max_elapsed_ms
        ),
        (10, 80, 12000, 3000, 2000)
    );
    let started = Instant::now();
    let (result, trace) = observe(cap, || {
        super::indexed_documents::context(&catalog, &task.query, &request, &Default::default())
    });
    let elapsed_ns = started.elapsed().as_nanos();
    let (native, error) = match result {
        // CLI dispatch's value(result) uses this exact serde conversion.
        Ok(value) => (serde_json::to_value(value).unwrap(), Value::Null),
        Err(error) => (Value::Null, json!(error)),
    };
    let mut actual = native.clone();
    let mut expected = baseline["data"].clone();
    for value in [&mut actual, &mut expected] {
        if let Some(verification) = value.get_mut("verification").and_then(Value::as_object_mut) {
            verification.remove("verified_at");
        }
    }
    let mut differences = vec![];
    difference_paths(&expected, &actual, "/data", &mut differences);
    let value = json!({"id":task.id,"query":task.query,"request":request,"elapsed_ns":elapsed_ns,
        "baseline_sha256":task.baseline_sha256,"native_response":native,"error":error,"trace":trace,
        "comparison":{"equal":error.is_null() && actual == expected,"difference_paths":differences,"excluded_fields":["/data/verification/verified_at"]},
        "trace_complete":error.is_null() && trace["trace_complete"]==true});
    write_new(
        &config.output.join(format!("question-{index:04}.json")),
        &value,
        cap + RESPONSE_RESERVE,
    );
}

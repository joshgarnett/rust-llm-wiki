//! Opt-in six-request Automatic observation; no selector, provider or grading.
use super::*;
use crate::{
    records::parse_note,
    retrieval::{SearchFilters, SearchLimits, SearchMode},
    vault::{VaultFs, VaultRoot},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

const IDS: [&str; 6] = ["C1", "H1", "H2", "D01", "D02", "D08"];
const TRACE_CAP: usize = 2 * 1024 * 1024;
const OUTPUT_CAP: usize = 16 * 1024 * 1024;
const P: &str =
    ".artifacts/workflow-priority-resume-20261007/representative-25k-readiness-20261009-001";
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: u32,
    report: PathBuf,
    vault: PathBuf,
    tasks: PathBuf,
    output: PathBuf,
    native_outputs: Vec<PathBuf>,
    expected_snapshot: Value,
    expected_vault_id: RecordId,
}
fn bounded_read(path: &Path, cap: usize) -> Vec<u8> {
    assert!(
        fs::symlink_metadata(path).unwrap().is_file(),
        "explicit regular input"
    );
    assert!(fs::metadata(path).unwrap().len() <= cap as u64);
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= cap, "bounded input");
    bytes
}
struct CappedBytes {
    bytes: Vec<u8>,
    cap: usize,
}
impl Write for CappedBytes {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        if input.len() > self.cap.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("diagnostic serialization cap"));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn compact(value: &impl serde::Serialize, cap: usize) -> Vec<u8> {
    try_compact(value, cap).unwrap()
}
fn try_compact(value: &impl serde::Serialize, cap: usize) -> serde_json::Result<Vec<u8>> {
    let mut writer = CappedBytes {
        bytes: Vec::new(),
        cap,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}
fn panic_reason(payload: Box<dyn std::any::Any + Send>) -> String {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied());
    match message {
        Some(message) if message.len() <= 1024 => message.into(),
        Some(_) => "panic message exceeds 1024 bytes; full stderr retained externally".into(),
        None => "non-string panic payload; stderr retained externally".into(),
    }
}
fn request(argv: &[String], ordinal: usize) -> ContextRequest {
    assert_eq!(
        &argv[..6],
        [
            "{candidate_binary}",
            "--json",
            "--offline",
            "--wiki",
            "{vault}",
            "context"
        ]
    );
    assert!(argv[6].len() <= 4096);
    let mut i = 7;
    let mut filters = SearchFilters::default();
    while argv[i] == "--source-id" {
        filters
            .source_ids
            .push(RecordId::new(&argv[i + 1]).unwrap());
        i += 2;
    }
    assert_eq!(argv[i], "--path-prefix");
    filters.path_prefix = Some(argv[i + 1].clone());
    i += 2;
    if argv[i] == "--include-historical" {
        filters.include_historical = true;
        i += 1;
    }
    let config = "002165859543557196174196584787713861598";
    let features = "002165859543557196174196584927123023090";
    let expected_sources = match ordinal {
        1 => vec![features],
        2 | 5 => vec![features, config],
        _ => vec![config],
    };
    assert_eq!(
        filters
            .source_ids
            .iter()
            .map(RecordId::as_str)
            .collect::<Vec<_>>(),
        expected_sources
    );
    let expected_path = match ordinal {
        1 => format!(
            "sources/{features}/revisions/revision_01a1211e-b8c7-7663-81a4-c16875996882/content.md"
        ),
        2 | 5 => "sources/".into(),
        _ => format!(
            "sources/{config}/revisions/revision_01a1211e-b8c7-7663-81a4-c14a00754f44/content.md"
        ),
    };
    assert_eq!(filters.path_prefix.as_deref(), Some(expected_path.as_str()));
    assert_eq!(filters.include_historical, matches!(ordinal, 1 | 2 | 5));
    assert_eq!(
        &argv[i..],
        [
            "--mode",
            "lexical",
            "--limit",
            "10",
            "--candidates",
            "80",
            "--excerpt-bytes",
            "1024",
            "--scope",
            "indexed-documents",
            "--target",
            "documents",
            "--max-bytes",
            "12000",
            "--max-tokens",
            "3000",
            "--instruction-bytes",
            "0",
            "--instruction-tokens",
            "0",
            "--output-bytes",
            "0",
            "--output-tokens",
            "0",
            "--verification-max-bytes",
            "67108864",
            "--verification-max-files",
            "4096",
            "--verification-max-entries",
            "16384",
            "--verification-max-elapsed-ms",
            "2000",
        ]
    );
    ContextRequest {
        scope: ContextScope::IndexedDocuments,
        target: ContextTarget::Documents,
        documents: crate::retrieval::QueryPlan {
            mode: SearchMode::Lexical,
            filters,
            limits: SearchLimits {
                hits: 10,
                candidates: 80,
                excerpt_bytes: 1024,
            },
            cursor: None,
        },
        graph: None,
        budget: ContextBudget::default(),
        verification_budget: VerificationBudget::default(),
    }
}
fn report(config: &Config, ok: bool, completed: &Value, seconds: f64) {
    fs::write(
        &config.report,
        compact(
            &json!({"ok": ok, "meta": {"network_used": false},
        "data": {"diagnostic": "Automatic lineage only", "completed": completed,
            "owner_monotonic_seconds": seconds, "phase_deadline_failed": seconds > 180.0, "quality_claim": false}}),
            65536,
        ),
    )
    .unwrap();
}

#[test]
#[ignore = "root-pinned same3131 config and monitored six-request execution only"]
fn normalized_mixed_context_six_automatic_lineage_probe() {
    let started = Instant::now();
    let config_path =
        PathBuf::from(std::env::var("LWIKI_SCALE_ATTRIBUTION_TASK").expect("root config"));
    let config: Config = serde_json::from_slice(&bounded_read(&config_path, 65536)).unwrap();
    let worktree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    let repo = worktree.ancestors().nth(3).unwrap();
    let account = repo.join(P).join("runtime-account-25k-001");
    assert_eq!(config.schema, 1);
    assert_eq!(config.vault, account.join("Imported Wiki with spaces"));
    assert_eq!(
        config.tasks,
        repo.join(P)
            .join("twentyfivek-workflow-001/INDEXED-HISTORICAL-CONTEXT-TASKS001.json")
    );
    assert_eq!(
        config.output,
        account.join("mixed-context-lineage001/replay")
    );
    assert_eq!(config.report, config.output.join("DRIVER-REPORT.json"));
    assert_eq!(
        config.native_outputs,
        IDS.iter()
            .map(|id| account.join(format!("indexed-historical-context001/{id}-context.json")))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        config.expected_vault_id.as_str(),
        "vault_01a1211e-7ca3-7600-b63f-e6b5b7e1085b"
    );
    assert_eq!(config.expected_snapshot["generation"], 3131);
    let tasks: Value = serde_json::from_slice(&bounded_read(&config.tasks, 65536)).unwrap();
    let tasks = tasks["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 6);
    let mut requests = Vec::new();
    let mut originals = Vec::new();
    for (ordinal, task) in tasks.iter().enumerate() {
        assert_eq!(task["id"], IDS[ordinal]);
        let argv: Vec<String> = serde_json::from_value(task["candidate_argv"].clone()).unwrap();
        assert!((7..=64).contains(&argv.len()));
        assert_eq!(task["question"], argv[6]);
        requests.push(request(&argv, ordinal));
        let original: Value =
            serde_json::from_slice(&bounded_read(&config.native_outputs[ordinal], 262144)).unwrap();
        assert_eq!(original["snapshot"], config.expected_snapshot);
        assert!(original["verification"]["verified_at"].is_string());
        originals.push(original);
    }
    let marker = parse_note(&bounded_read(&config.vault.join("WIKI.md"), 65536));
    assert_eq!(
        marker.canonical.as_ref().unwrap().id(),
        &config.expected_vault_id
    );
    let catalog = Catalog::new(
        VaultFs::new(VaultRoot::explicit(&config.vault).unwrap()),
        config.expected_vault_id.clone(),
    );
    assert!(
        catalog.operation_state().unwrap().is_some(),
        "no normalized rebuild"
    );
    fs::create_dir(&config.output).expect("fresh replay directory");
    let mut written = 0usize;
    let mut completed = Vec::new();
    for (ordinal, task) in tasks.iter().enumerate() {
        assert!(
            started.elapsed().as_secs_f64() < 180.0,
            "finite phase deadline"
        );
        let query = task["question"].as_str().unwrap();
        let (outcome, trace) = context::with_candidate_ordering_trace(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                context(
                    &catalog,
                    query,
                    &requests[ordinal],
                    &ContextOptions::default(),
                )
            }))
        });
        let (result, panic) = match outcome {
            Ok(result) => (Some(result), None),
            Err(payload) => (None, Some(panic_reason(payload))),
        };
        let trace_bytes = try_compact(&trace, TRACE_CAP);
        let trace_fits = trace_bytes.is_ok();
        let rows = trace.iter().map(context::lineage_row_units).sum::<usize>();
        let mut observed = result
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|r| serde_json::to_value(r).unwrap())
            .unwrap_or(Value::Null);
        let observed_timestamp = observed["verification"]["verified_at"].clone();
        let original_timestamp = originals[ordinal]["verification"]["verified_at"].clone();
        let canonical = observed.clone();
        if !observed.is_null() {
            observed["verification"]["verified_at"] = Value::Null;
        }
        let mut original = originals[ordinal].clone();
        original["verification"]["verified_at"] = Value::Null;
        let complete = panic.is_none()
            && trace_fits
            && trace.len() <= 16
            && rows <= 4096
            && [
                "owner_inputs",
                "structural_blocks",
                "teaching_blocks",
                "eligible_pre_cap",
                "retained_post32",
                "candidate_pool",
                "sorted_packets",
                "packing_trials",
                "final_accepted",
            ]
            .iter()
            .all(|stage| trace.iter().any(|value| value["stage"] == *stage));
        let equivalent = result.as_ref().is_some_and(|result| result.is_ok())
            && observed_timestamp.is_string()
            && observed == original;
        let within_deadline = started.elapsed().as_secs_f64() <= 180.0;
        let stage_counts = trace
            .iter()
            .map(|stage| {
                json!({"stage": stage["stage"],
            "top_level_rows": stage["rows"].as_array().map_or(1, Vec::len),
            "nested_row_units": context::lineage_row_units(stage)})
            })
            .collect::<Vec<_>>();
        let payload = json!({"task_id": IDS[ordinal], "argv": task["candidate_argv"], "request": requests[ordinal],
            "query": query, "context_result": canonical, "error": result.as_ref().and_then(|result| result.as_ref().err()),
            "panic": panic, "trace": if trace_fits { json!(trace) } else { Value::Null }, "trace_unavailable": !trace_fits,
            "counts": {"stages": trace.len(), "rows": rows, "compact_trace_bytes": trace_bytes.as_ref().ok().map(Vec::len), "trace_byte_cap_exceeded": !trace_fits, "per_stage": stage_counts},
            "complete": complete, "native_equivalent": equivalent, "within_phase_deadline_before_writes": within_deadline,
            "equality_exemptions": {"/verification/verified_at": {"original": original_timestamp, "observed": observed_timestamp}}});
        let bytes = compact(&payload, TRACE_CAP + 512 * 1024);
        assert!(written + bytes.len() + 65536 <= OUTPUT_CAP);
        written += bytes.len();
        fs::write(
            config.output.join(format!("{}-lineage.json", IDS[ordinal])),
            bytes,
        )
        .unwrap();
        completed.push(
            json!({"task_id": IDS[ordinal], "complete": complete, "native_equivalent": equivalent}),
        );
        report(
            &config,
            complete && equivalent && within_deadline,
            &json!(completed),
            started.elapsed().as_secs_f64(),
        );
        let terminal_seconds = started.elapsed().as_secs_f64();
        let within_deadline_after_writes = terminal_seconds <= 180.0;
        if !within_deadline_after_writes {
            completed.push(json!({"terminal_stop": "phase_deadline_after_writes", "owner_monotonic_seconds": terminal_seconds}));
            report(&config, false, &json!(completed), terminal_seconds);
        }
        assert!(
            complete && equivalent && within_deadline_after_writes,
            "preserved inadmissible trace; no retry or next request"
        );
    }
}

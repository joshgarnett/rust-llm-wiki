//! One fixed, experimental 32-cell native app replay; no full lineage trace.
//! Root owns wiring, release binary/fixture/source pins and hard supervision.
use crate::{
    app::{OfflineApp, OperationOptions},
    retrieval::{
        context, context_evidence,
        context_types::{
            ContextBudget, ContextRequest, ContextScope, ContextTarget, VerificationBudget,
        },
        types::{QueryPlan, SearchFilters, SearchLimits, SearchMode},
        vectors::VectorStore,
    },
    vault::{VaultFs, VaultRoot},
};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

const REPO: &str = "/Users/jgarnett/Devleopment/rust-llm-wiki";
const ROOT: &str = ".artifacts/normalized-semantic-lifecycle-001";
const READ_LIMIT: u64 = 1024 * 1024 * 1024;
const CELL_LIMIT: usize = 512 * 1024;
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const TASKS: [(&str, &str); 4] = [
    (
        "h02",
        "I want a smaller Rust release binary and can tolerate terminating immediately on a panic. How does this differ from normal cleanup, and what Cargo configuration selects it?",
    ),
    (
        "q02",
        "A failed write produces a giant traceback. How should I help the user fix the problem while still retaining information for a bug report?",
    ),
    (
        "q03",
        "How can the same command support shell pipelines and readable terminal messages, and how can I test its output without spawning it every time?",
    ),
    (
        "q23",
        "What median startup latency did the authors measure for lwiki on an Intel Mac in 2026?",
    ),
];

fn request(mode: SearchMode) -> ContextRequest {
    ContextRequest {
        scope: ContextScope::IndexedDocuments,
        target: ContextTarget::Documents,
        documents: QueryPlan {
            mode,
            filters: SearchFilters::default(),
            limits: SearchLimits {
                hits: 5,
                candidates: 80,
                excerpt_bytes: 1024,
            },
            cursor: None,
        },
        graph: None,
        budget: ContextBudget {
            max_bytes: 6000,
            max_tokens: 1500,
            ..Default::default()
        },
        verification_budget: VerificationBudget {
            max_bytes: 64 * 1024 * 1024,
            max_files: 4096,
            max_entries: 16384,
            max_elapsed_ms: 2000,
        },
    }
}

fn exact_path(path: &Path) {
    assert!(path.is_absolute());
    for ancestor in path.ancestors() {
        if let Ok(metadata) = fs::symlink_metadata(ancestor) {
            assert!(
                !metadata.file_type().is_symlink(),
                "owned/protocol path symlink rejected"
            );
        }
    }
}
fn read_json(path: &Path, maximum: usize, reads: &mut u64) -> Value {
    exact_path(path);
    let metadata = fs::metadata(path).expect("root-pinned public input must exist");
    assert!(metadata.is_file() && metadata.len() <= maximum as u64);
    *reads = reads.checked_add(metadata.len()).unwrap();
    assert!(*reads <= READ_LIMIT);
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes.len() as u64, metadata.len());
    serde_json::from_slice(&bytes).expect("bounded public JSON")
}
fn write_json(path: &Path, value: &Value, written: &mut usize) {
    exact_path(path);
    let bytes = serde_json::to_vec(value).unwrap();
    assert!(
        bytes.len() <= CELL_LIMIT,
        "complete compact result/summary 512KiB ceiling"
    );
    *written = written.checked_add(bytes.len()).unwrap();
    assert!(*written <= OUTPUT_LIMIT);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}
fn identity(value: &Value) -> Value {
    let mut identity = value.clone();
    identity["verification"]
        .as_object_mut()
        .unwrap()
        .remove("verified_at");
    identity
}
fn known_work(summary: Option<&Value>) -> Option<u64> {
    let work = summary?.get("normalized_read_work")?;
    [
        "canonical_proof_bytes",
        "cache_source_bytes",
        "vector_bytes",
        "metadata_bytes",
    ]
    .into_iter()
    .try_fold(0u64, |total, key| total.checked_add(work[key].as_u64()?))
}
fn selection_within_limits(summary: Option<&Value>, arm: &str) -> bool {
    let Some(selection) = summary.and_then(|s| s.get("selection")) else {
        return false;
    };
    if selection.is_null() {
        return true; // Record unavailable selection; never infer an objective win.
    }
    if selection["arm"] != arm
        || !selection["candidate_count"]
            .as_u64()
            .is_some_and(|n| n <= 80)
    {
        return false;
    }
    if selection.get("fallback").is_some() {
        return true; // Frozen zero-signal fallback has no renderer trials.
    }
    selection["render_trials"]
        .as_u64()
        .is_some_and(|n| n <= if arm == "F0" { 160 } else { 240 })
        && selection["pair_cosines"]
            .as_u64()
            .is_some_and(|n| n <= 3160)
        && selection["matrix_bytes"]
            .as_u64()
            .is_some_and(|n| n <= 51200)
        && selection["selected_origins"]
            .as_array()
            .is_some_and(|origins| origins.len() <= 80)
}

#[test]
#[ignore = "root-frozen 32-cell evidence-set protocol, reviewed native binary and real owned fixture required"]
fn normalized_real_context_evidence_set_thirty_two_development_requests() {
    let started = Instant::now();
    assert!(
        !cfg!(debug_assertions),
        "root must separately pin actual opt3 compiler action"
    );
    assert!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "native ARM64 only"
    );
    let base = PathBuf::from(REPO).join(ROOT);
    let output = base.join("evidence-set-001");
    let vault = base.join("lineage-001/vaults/real");
    exact_path(&vault);
    assert_eq!(vault.canonicalize().unwrap(), vault);
    let mut explicit_reads = 0u64;
    let protocol_path = base.join("critic/public/evidence-set-protocol-001.json");
    let protocol = read_json(&protocol_path, 2 * 1024 * 1024, &mut explicit_reads);
    let controls = read_json(
        &output.join("controls.json"),
        2 * 1024 * 1024,
        &mut explicit_reads,
    );
    assert_eq!(
        protocol["schema"],
        "normalized-semantic-evidence-set-development-protocol-v1"
    );
    let cases = protocol["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 8);
    let matrix = protocol["matrix"].as_array().unwrap();
    assert_eq!(matrix.len(), 32);
    assert_eq!(controls["eligible_units"], 223);
    assert_eq!(controls["available_units"], 223);
    assert_eq!(controls["cached_query_vectors"], 4);
    assert_eq!(controls["dimensions"], 1536);
    assert_eq!(controls["space_id"], protocol["fixture"]["space_id"]);
    assert_eq!(controls["spec"], protocol["fixture"]["space_spec"]);
    let app = OfflineApp::new(
        VaultFs::new(VaultRoot::explicit(&vault).unwrap()),
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        app.vault_id().as_str(),
        "vault_01a0ff30-4eb0-7251-bca5-96d1405b9cef"
    );
    {
        // Read only the retained owned cache. No repair or provider runtime.
        let store = VectorStore::open(app.fs(), None).unwrap();
        let state = store.active().unwrap().expect("real active Space required");
        assert!(state.active);
        assert_eq!(state.actual_dimensions, Some(1536));
        assert_eq!(json!(state.id), controls["space_id"]);
        assert_eq!(serde_json::to_value(&state.spec).unwrap(), controls["spec"]);
    }
    // Original actual baseline packets are public, immutable controls. Read
    // all eight before launch so a missing control cannot create partial arms.
    let mut baselines = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let (task, query) = TASKS[index / 2];
        let mode = ["semantic", "hybrid"][index % 2];
        let slot = format!("M03.{:02}", index + 1);
        assert_eq!(case["task_id"], task);
        assert_eq!(case["query"], query);
        assert_eq!(case["mode"], mode);
        assert_eq!(case["id"], format!("{task}-{mode}"));
        assert_eq!(case["baseline_slot"], slot);
        let declared = format!("{ROOT}/runtime-001/commands/{slot}/stdout");
        assert_eq!(case["baseline_stdout_path"], declared);
        let baseline = read_json(
            &PathBuf::from(REPO).join(declared),
            2 * 1024 * 1024,
            &mut explicit_reads,
        );
        assert!(baseline["data"].is_object());
        baselines.push(baseline);
    }
    let mut ledger = Vec::new();
    let mut calls = 0usize;
    let mut renders = 0usize;
    let mut written = 0usize;
    let mut logical_reads = 0u64;
    let mut baseline_failed = false;
    let mut guard_failed = false;
    let mut errors = 0usize;
    for (index, row) in matrix.iter().enumerate() {
        let arm = ["B", "L", "F0", "F1"][index / 8];
        let case = &cases[index % 8];
        let task = case["task_id"].as_str().unwrap();
        let mode_name = case["mode"].as_str().unwrap();
        let case_id = case["id"].as_str().unwrap();
        let query = case["query"].as_str().unwrap();
        let expected_path = format!("evidence-set-001/results/{arm}/{case_id}.json");
        assert_eq!(row["ordinal"], index + 1);
        assert_eq!(row["arm"], arm);
        assert_eq!(row["case_id"], case_id);
        assert_eq!(row["app_calls"], 1);
        assert_eq!(row["result_path"], expected_path);
        let path = base.join(expected_path);
        if started.elapsed().as_secs() >= 1170 {
            guard_failed = true;
        }
        if baseline_failed || guard_failed {
            let reason = if baseline_failed {
                "baseline equivalence/error blocks comparison"
            } else {
                "owning/read/render/instrumentation guard blocks remaining calls"
            };
            write_json(
                &path,
                &json!({"ordinal":index+1,"arm":arm,"task_id":task,"mode":mode_name,
                "status":"UNRUN","reason":reason,"app_calls":0}),
                &mut written,
            );
            ledger.push(json!({"ordinal":index+1,"arm":arm,"case_id":case_id,"status":"UNRUN","reason":reason}));
            continue;
        }
        let mode = if mode_name == "semantic" {
            SearchMode::Semantic
        } else {
            SearchMode::Hybrid
        };
        let request = request(mode);
        calls += 1;
        assert!(calls <= 32);
        let call_started = Instant::now();
        let ((result, summary), render_counts) =
            context::with_context_render_counts_for_test(256.min(8192 - renders), || {
                context_evidence::with_policy_for_test(arm, || {
                    context_evidence::with_summary_for_test(|| {
                        app.semantic_context(query, &request, None, true, false)
                    })
                })
            });
        let elapsed_ms = call_started.elapsed().as_secs_f64() * 1000.0;
        renders += render_counts.calls;
        let successful = result.is_ok();
        let data = result
            .as_ref()
            .ok()
            .map(|value| serde_json::to_value(value).unwrap());
        let network_used = result
            .as_ref()
            .map_or_else(|error| error.network_used, |context| context.network_used);
        let caps_ok = result.as_ref().is_ok_and(|context| {
            context.usage().rendered_bytes <= 6000 && context.usage().estimated_tokens <= 1500
        });
        let bytes = known_work(summary.as_ref());
        if let Some(bytes) = bytes {
            logical_reads = logical_reads.checked_add(bytes).unwrap();
        }
        let accounting_ok = !successful || bytes.is_some();
        let selection_limits_ok = !successful || selection_within_limits(summary.as_ref(), arm);
        let budget_ok =
            renders <= 8192 && explicit_reads.checked_add(logical_reads).unwrap() <= READ_LIMIT;
        let matches = arm == "B"
            && data
                .as_ref()
                .is_some_and(|data| identity(data) == identity(&baselines[index % 8]["data"]));
        let valid = successful
            && !network_used
            && caps_ok
            && accounting_ok
            && selection_limits_ok
            && budget_ok
            && (arm != "B" || matches);
        if !valid {
            errors += 1;
        }
        if arm == "B" && (!matches || !valid) {
            baseline_failed = true;
        }
        if network_used || !accounting_ok || !selection_limits_ok || !budget_ok {
            guard_failed = true;
        }
        let status = if valid { "OK" } else { "ERROR" };
        write_json(
            &path,
            &json!({
                "ordinal":index+1,"arm":arm,"task_id":task,"mode":mode_name,"status":status,"app_calls":1,
                "query":query,"request":request,"result":result,"data":data,"summary":summary,
                "network_used":network_used,"provider_calls":if network_used {Value::String("unexpected; count unavailable".into())} else {json!(0)},
                "requested_lexical_fallback":false,"offline":true,
                "objective_fallback":"retained verbatim in selection summary; not provider or lexical fallback",
                "baseline_identity_matches":if arm=="B" {Some(matches)} else {None},
                "controls":controls,"controls_authority":"root-attested fixture inventory, not fresh bridge coverage scan",
                "query_input_hash":case["query_input_hash"],"query_vector_hash":case["query_vector_hash"],
                "baseline_stdout_sha256":case["baseline_stdout_sha256"],"baseline_path":case["baseline_stdout_path"],
                "render_counts":render_counts,"render_calls_total_so_far":renders,
                "call_elapsed_ms":elapsed_ms,"clock":"process-owned Instant; nested within parent interval",
                "known_logical_read_bytes":bytes,"explicit_json_read_bytes_so_far":explicit_reads,
                "known_logical_read_bytes_total_so_far":logical_reads,"read_work_available":bytes.is_some(),
                "selection_summary_limits_ok":selection_limits_ok,
                "physical_io_and_uninstrumented_reads":"unavailable; root conservative reservation remains charged",
                "selection_summary_available":summary.as_ref().is_some_and(|s| s["selection"].is_object()),
                "quality_acceptance":"OPEN; private independent grading required, no objective-score promotion"
            }),
            &mut written,
        );
        ledger.push(json!({"ordinal":index+1,"arm":arm,"case_id":case_id,"status":status,
            "successful":successful,"baseline_identity_matches":if arm=="B" {Some(matches)} else {None},"call_elapsed_ms":elapsed_ms,"render_calls":render_counts.calls}));
    }
    write_json(
        &output.join("runtime/summary.json"),
        &json!({
            "status":if baseline_failed {"BASELINE_BLOCKER_CANDIDATES_UNRUN"} else if guard_failed || errors>0 {"INCOMPLETE_NO_WINNER"} else {"THIRTY_TWO_OBSERVATIONS_READY_FOR_INDEPENDENT_GRADING"},
            "matrix":ledger,"app_context_calls":calls,"actual_render_calls":renders,"errors":errors,
            "baseline_failed":baseline_failed,"guard_failed":guard_failed,"explicit_json_read_bytes":explicit_reads,
            "known_logical_read_bytes":logical_reads,"counted_read_bytes":explicit_reads+logical_reads,
            "elapsed_seconds":started.elapsed().as_secs_f64(),"new_result_summary_bytes_before_final":written,
            "provider_calls":if guard_failed {Value::String("see per-cell network telemetry".into())} else {json!(0)},
            "trace_enabled":false,"scope":"32 internal app-dispatch DEVELOPMENT cells, no public CLI/lifecycle/quality qualification",
            "read_coverage":"logical decoded work plus explicit JSON reads; active-Space bootstrap/pager/physical/uninstrumented safety unavailable and root-reserved",
            "root_owned_enforcement":"1200s cumulative owner with 30s reserve,120s hard child,1GiB reads,256MiB whole artifacts,16MiB diagnostics,ALL 100GiB/free32GiB+8GiB stricter safeguards"
        }),
        &mut written,
    );
    assert!(
        !baseline_failed && !guard_failed && errors == 0 && calls == 32,
        "preserved failed/error/unrun cells; no candidate winner"
    );
}

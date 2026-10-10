//! One ignored control campaign under the existing native app test facility.
use super::retrieval_lineage022::{Pin, json_pin, pinned, write_new};
use super::*;
use crate::retrieval::{
    ContextBudget, ContextRequest, ContextScope, ContextTarget, VerificationBudget,
    context::representation025 as control,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

const IDS: [&str; 6] = [
    "dev-support-01",
    "dev-support-08",
    "dev-support-20",
    "dev-long-train-26",
    "dev-long-train-1108",
    "dev-long-train-35",
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    version: u32,
    vault: PathBuf,
    tasks: Pin,
    protocol: Pin,
    pins: Vec<Pin>,
    output: PathBuf,
}
fn request() -> ContextRequest {
    ContextRequest {
        scope: ContextScope::IndexedDocuments,
        target: ContextTarget::Documents,
        documents: QueryPlan {
            mode: SearchMode::Hybrid,
            limits: SearchLimits {
                hits: 10,
                candidates: 80,
                excerpt_bytes: 1024,
            },
            ..Default::default()
        },
        budget: ContextBudget {
            max_bytes: 12000,
            max_tokens: 3000,
            ..Default::default()
        },
        verification_budget: VerificationBudget {
            max_bytes: 67108864,
            max_files: 4096,
            max_entries: 16384,
            max_elapsed_ms: 2000,
        },
        ..Default::default()
    }
}
fn source_only_config_pins(task: &Task) -> Result<()> {
    pinned(&task.tasks)?;
    pinned(&task.protocol)?;
    for pin in &task.pins {
        pinned(pin)?;
    }
    Ok(())
}
fn outcome(error: &WikiError) -> String {
    format!("{:?}: {}", error.code, error)
}

#[test]
#[ignore = "root-admitted structural025; six discovery, one preparation, six candidates; no reference replay"]
fn replay_six_frozen_structural_contexts() {
    let config = PathBuf::from(
        std::env::var_os("LWIKI_REPRESENTATION025_TASK").expect("explicit025 config path"),
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap();
    let fresh = root.join(".artifacts/workflow-priority-resume-20261010-fresh");
    assert_eq!(config, fresh.join("readiness/REPRESENTATION025-TASK.json"));
    let mut bytes = Vec::new();
    fs::File::open(&config)
        .unwrap()
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 64 * 1024);
    let task: Task = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(task.version, 1);
    assert_eq!(task.vault,root.join(".artifacts/workflow-priority-resume-20261007/representative-default-quality-next-20261010-001/runtime-account001/import002/vault"));
    assert_eq!(task.tasks.path, fresh.join("root/TASKS001.json"));
    assert_eq!(
        task.protocol.path,
        fresh.join("critic/RETRIEVAL-REPRESENTATION-PROTOCOL025.json")
    );
    assert_eq!(
        task.output,
        fresh.join("representative011/structural_representation025")
    );
    assert!(!task.output.exists() && task.pins.len() <= 24);
    assert!(task.pins.iter().map(|pin| pin.bytes).sum::<u64>() <= 512 * 1024 * 1024);
    let selected = task.vault.join(".wiki/cache/catalog-current.json");
    let vectors = task.vault.join(".wiki/cache/embeddings.sqlite3");
    for required in [
        &selected,
        &vectors,
        &fresh.join("root/providers003.toml"),
        &fresh.join("root/SOURCE-REVISIONS001.json"),
        &fresh.join("build010/terminal029/bin/lwiki-binary"),
    ] {
        assert!(task.pins.iter().any(|pin| &pin.path == required));
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for name in [
        "retrieval/context.rs",
        "retrieval/context_representation025.rs",
        "retrieval/indexed_semantic.rs",
        "app/indexed_embedding_workflow_tests.rs",
        "app/representation_control025.rs",
    ] {
        assert!(task.pins.iter().any(|pin| pin.path == source.join(name)));
    }
    fs::create_dir(&task.output).unwrap();
    let owner_start = Instant::now();
    let mut discoveries = Vec::<Value>::new();
    let mut attempts = Vec::<Value>::new();
    let mut prep = Value::Null;
    let mut pin_passes = 0usize;
    let run = (|| -> Result<()> {
        let mut check = || -> Result<()> {
            source_only_config_pins(&task)?;
            pin_passes += 1;
            if fs::read(&config).map_err(|e| WikiError::invalid(e.to_string()))? != bytes {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "025 config changed",
                ));
            }
            if owner_start.elapsed().as_secs() >= 3900 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "025 finite3900s campaign cap",
                ));
            }
            Ok(())
        };
        check()?;
        let protocol = json_pin(&task.protocol)?;
        if protocol["status"] != "GO_DETERMINISTIC_CONTROL" || protocol["tasks"] != json!(IDS) {
            return Err(WikiError::invalid("025 frozen protocol/tasks drift"));
        }
        let selector = json_pin(task.pins.iter().find(|pin| pin.path == selected).unwrap())?;
        let catalog = task.vault.join(format!(
            ".wiki/cache/catalogs/{}.sqlite",
            selector["file_id"]
                .as_str()
                .ok_or_else(|| WikiError::invalid("025 publication file id"))?
        ));
        if !task.pins.iter().any(|pin| pin.path == catalog) {
            return Err(WikiError::invalid("025 missing actual database pin"));
        }
        for database in [&catalog, &vectors] {
            let wal = PathBuf::from(format!("{}-wal", database.display()));
            if wal.exists()
                && (!task.pins.iter().any(|pin| pin.path == wal)
                    || fs::metadata(wal).unwrap().len() != 0)
            {
                return Err(WikiError::invalid("025 unfrozen/nonempty WAL"));
            }
        }
        let questions = json_pin(&task.tasks)?;
        let rows = questions["tasks"]
            .as_array()
            .ok_or_else(|| WikiError::invalid("025 original task rows"))?;
        if rows.len() != 40 {
            return Err(WikiError::invalid("025 original forty rows required"));
        }
        let app = OfflineApp::new(
            crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&task.vault)?),
            OperationOptions {
                offline: true,
                ..Default::default()
            },
        )?;
        let request = request();
        let mut captures = Vec::new();
        for id in IDS {
            check()?;
            discoveries.push(json!({"id":id,"status":"STARTED"}));
            let question = rows
                .iter()
                .find(|row| row["id"] == id)
                .and_then(|row| row["question"].as_str())
                .ok_or_else(|| WikiError::invalid("025 frozen question missing"))?;
            let start = Instant::now();
            let capture = match control::discover_only(|| {
                app.semantic_context_with_options(
                    question,
                    &request,
                    None,
                    true,
                    false,
                    &Default::default(),
                )
            }) {
                Ok(value) => value,
                Err(error) => {
                    *discoveries.last_mut().unwrap() = json!({"id":id,"status":"FAILED","seconds":start.elapsed().as_secs_f64(),"error":outcome(&error)});
                    return Err(error);
                }
            };
            *discoveries.last_mut().unwrap() = json!({"id":id,"status":"COMPLETE","seconds":start.elapsed().as_secs_f64(),"metadata":capture.metadata()});
            captures.push(capture);
            check()?;
        }
        write_new(
            &task.output.join("DISCOVERY025.json"),
            &serde_json::to_vec_pretty(&discoveries).unwrap(),
        )?;
        if captures.len() != 6 {
            return Err(WikiError::invalid(
                "025 campaign requires all six complete discoveries",
            ));
        }
        check()?;
        let (prepared, usage) = control::prepare(&captures, &task.output.join("sidecars"));
        prep = json!({"usage":usage,"error":prepared.as_ref().err().map(outcome),"compiler_input":"complete source identity/title/bytes and fixed recipe only; no task/rank/reference/labels", "provider_calls":0,"model_calls":0});
        write_new(
            &task.output.join("PREPARATION025.json"),
            &serde_json::to_vec_pretty(&prep).unwrap(),
        )?;
        let prepared = prepared?;
        check()?;
        for (index, id) in IDS.into_iter().enumerate() {
            check()?;
            attempts.push(json!({"id":id,"status":"STARTED"}));
            let question = rows
                .iter()
                .find(|row| row["id"] == id)
                .and_then(|row| row["question"].as_str())
                .unwrap();
            let start = Instant::now();
            let result = control::query(&prepared, &captures[index].discovery, || {
                app.semantic_context_with_options(
                    question,
                    &request,
                    None,
                    true,
                    false,
                    &Default::default(),
                )
            });
            let seconds = start.elapsed().as_secs_f64();
            let expanded_fts_bytes = control::expanded_fts_bytes();
            match result {
                Ok(result) => {
                    let response = serde_json::to_vec(
                        &json!({"ok":true,"workflow":"structural_representation025","data":result}),
                    )
                    .unwrap();
                    if response.len() > 1024 * 1024 {
                        *attempts.last_mut().unwrap() = json!({"id":id,"status":"FAILED","seconds":seconds,"expanded_fts_bytes":expanded_fts_bytes,"error":"existing1MiB capture cap"});
                        return Err(WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "025 response capture cap",
                        ));
                    }
                    write_new(
                        &task
                            .output
                            .join(format!("{:02}-candidate.stdout", index + 1)),
                        &response,
                    )?;
                    *attempts.last_mut().unwrap() = json!({"id":id,"status":"COMPLETE","seconds":seconds,"expanded_fts_bytes":expanded_fts_bytes,"response_bytes":response.len(),"provider_calls":0,"model_calls":0});
                    if seconds > 15.0 {
                        *attempts.last_mut().unwrap() = json!({"id":id,"status":"FAILED","seconds":seconds,"expanded_fts_bytes":expanded_fts_bytes,"response_bytes":response.len(),"error":"query15s cap; no retry"});
                        return Err(WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "025 query15s cap; no retry",
                        ));
                    }
                }
                Err(error) => {
                    *attempts.last_mut().unwrap() = json!({"id":id,"status":"FAILED","seconds":seconds,"expanded_fts_bytes":expanded_fts_bytes,"error":outcome(&error)});
                    return Err(error);
                }
            }
            check()?;
        }
        Ok(())
    })();
    let max = attempts
        .iter()
        .filter_map(|row| row["seconds"].as_f64())
        .fold(0.0_f64, f64::max);
    let report = json!({"status":if run.is_ok() {"COMPLETE_FOR_INDEPENDENT_SIX_TASK_ASSESSMENT"} else {"FAILED_PRESERVED"},
        "workflow":"structural_representation025","recipe":control::RECIPE,"failure":run.as_ref().err().map(outcome),
        "discovery_attempts":discoveries.len(),"unrun_discovery":6-discoveries.len(),"discoveries":discoveries,
        "preparation_invocations":if prep.is_null(){0}else{1},"preparation":prep,
        "expanded_fts_bytes":attempts.iter().filter_map(|row|row["expanded_fts_bytes"].as_u64()).sum::<u64>(),
        "expanded_fts_bytes_scope":"actual four-field UTF8 bytes materialized across attempted queries; separate from serialized preparation record_bytes; failed work retained",
        "candidate_attempts":attempts.len(),"unrun_candidates":6-attempts.len(),"attempts":attempts,
        "query_p95_nearest_rank_max_of_six":if run.is_ok(){json!(max)}else{Value::Null},"p95_gate_passed":run.is_ok() && max<=5.0,
        "owner_seconds":owner_start.elapsed().as_secs_f64(),"pin_authentication_passes":pin_passes,
        "pinned_bytes_per_pass":task.pins.iter().map(|pin| pin.bytes).sum::<u64>()+task.tasks.bytes+task.protocol.bytes,
        "query_interval":"owning process monotonic; discovery, sidecar lookup, original proof, FTS/closure selection, rendering, final recheck; pin campaign checks measured separately",
        "provider_calls":0,"model_calls":0,"reference_replays":0,"default_native_score_claim":false,"release_latency_claim":false,
        "source_spans_only":true,"errors_and_unrun_preserved":true});
    write_new(
        &task.output.join("RESULTS025.json"),
        &serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    assert!(
        run.is_ok(),
        "025 diagnostic failed; retain all outputs and stop"
    );
}

fn prepared_fixture(
    raw: &str,
) -> (
    Fixture,
    Arc<Responses>,
    tempfile::TempDir,
    control::Prepared,
    control::Capture,
) {
    let fixture = normalized();
    add(&fixture, "Signalneedle policy", "control025.md", raw);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    fixture
        .app
        .embeddings_sync(
            &EmbeddingSettings::default(),
            &runtime(&fixture.service, &dispatch),
        )
        .unwrap();
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1.0, 0.0]);
    let capture = control::discover_only(|| {
        fixture.offline().semantic_context_with_options(
            "Signalneedle",
            &request(),
            None,
            true,
            false,
            &Default::default(),
        )
    })
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let captures = [capture];
    let (prepared, _) = control::prepare(&captures, &tmp.path().join("sidecars"));
    let [capture] = captures;
    (fixture, responses, tmp, prepared.unwrap(), capture)
}
#[test]
fn mandatory_closure_counts_citation_headers_tokens_and_four_exact_passages_without_merging() {
    let raw = "# Signalneedle scope\n\nSignalneedle rule.\n\nSignalneedle qualification.\n\nSignalneedle exception.\n\nSignalneedle further condition.\n";
    let (fixture, responses, _tmp, prepared, capture) = prepared_fixture(raw);
    let result = control::query(&prepared, &capture.discovery, || {
        fixture.offline().semantic_context_with_options(
            "Signalneedle",
            &request(),
            None,
            true,
            false,
            &Default::default(),
        )
    })
    .unwrap();
    assert!(!result.network_used && !result.passages().is_empty());
    assert!(control::expanded_fts_bytes() > raw.len());
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let reader = catalog.cached_query_snapshot(Default::default()).unwrap();
    let mut passages = Vec::new();
    for wanted in [
        "# Signalneedle scope\n",
        "Signalneedle rule.\n",
        "Signalneedle qualification.\n",
        "Signalneedle exception.\n",
        "Signalneedle further condition.\n",
    ] {
        let start = raw.find(wanted).unwrap();
        let span = ByteSpan::new(start as u64, (start + wanted.len()) as u64).unwrap();
        let mut passage = result.passages()[0].clone();
        passage.span = span;
        passage.text = wanted.into();
        for citation in &mut passage.citations {
            if let CitationRef::Source(reference) = citation {
                reference.span = span;
                reference.quote_hash = Blake3Hash::digest(wanted.as_bytes());
            }
        }
        passages.push(passage);
    }
    let trial = control::exact_trial(&reader, &request(), &[], &passages[..4]).unwrap();
    assert!(trial.reason.is_none());
    assert_eq!(trial.passages.len(), 4);
    let mut exact = request();
    exact.budget.max_bytes = trial.text.len();
    exact.budget.max_tokens = trial.text.len().div_ceil(4);
    assert!(
        control::exact_trial(&reader, &exact, &[], &passages[..4])
            .unwrap()
            .reason
            .is_none()
    );
    exact.budget.max_bytes -= 1;
    assert!(
        control::exact_trial(&reader, &exact, &[], &passages[..4])
            .unwrap()
            .reason
            .is_some()
    );
    exact = request();
    exact.budget.max_tokens = trial.text.len().div_ceil(4) - 1;
    assert!(
        control::exact_trial(&reader, &exact, &[], &passages[..4])
            .unwrap()
            .reason
            .is_some()
    );
    assert_eq!(
        control::exact_trial(&reader, &request(), &passages[..4], &passages[4..])
            .unwrap()
            .reason,
        Some("support_closure_owner_passage_cap")
    );
    assert_eq!(
        control::exact_trial(&reader, &request(), &passages[..4], &passages[..4])
            .unwrap()
            .passages
            .len(),
        4
    );
    // Exercise the active025 early-return branch's existing final proof, after
    // structural selection but before seal/emission, in this same disposable fixture.
    struct ChangeBeforeEmission {
        path: PathBuf,
        reached: AtomicBool,
    }
    impl crate::retrieval::ContextFault for ChangeBeforeEmission {
        fn check(&self, checkpoint: crate::retrieval::ContextCheckpoint) -> Result<()> {
            assert_eq!(
                checkpoint,
                crate::retrieval::ContextCheckpoint::BeforeFinalVerification { attempt: 0 }
            );
            assert!(!self.reached.swap(true, Ordering::SeqCst));
            fs::write(&self.path, "Changed selected source only at final proof.\n").unwrap();
            Ok(())
        }
    }
    let captured_path = capture.metadata()["discovery"]["owners"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let fault = Arc::new(ChangeBeforeEmission {
        path: fixture.fs.root().path().join(captured_path),
        reached: AtomicBool::new(false),
    });
    let before = responses.calls.load(Ordering::SeqCst);
    let options = crate::retrieval::ContextOptions {
        fault: Some(fault.clone()),
        ..Default::default()
    };
    let result = control::query(&prepared, &capture.discovery, || {
        fixture.offline().semantic_context_with_options(
            "Signalneedle",
            &request(),
            None,
            true,
            false,
            &options,
        )
    });
    assert_eq!(result.unwrap_err().code, ErrorCode::FreshnessConflict);
    assert!(fault.reached.load(Ordering::SeqCst));
    assert_eq!(responses.calls.load(Ordering::SeqCst), before);
}
#[test]
fn missing_sidecars_and_uncached_offline_queries_refuse_without_provider_work() {
    let (fixture, responses, tmp, prepared, capture) =
        prepared_fixture("# Signalneedle\n\nA complete offline rule.\n");
    let before = responses.calls.load(Ordering::SeqCst);
    fs::rename(
        tmp.path().join("sidecars"),
        tmp.path().join("retained-sidecars"),
    )
    .unwrap();
    let error = control::query(&prepared, &capture.discovery, || {
        fixture.offline().semantic_context_with_options(
            "Signalneedle",
            &request(),
            None,
            true,
            false,
            &Default::default(),
        )
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::OfflineUnavailable);
    let error = control::query(&prepared, &capture.discovery, || {
        fixture.offline().semantic_context_with_options(
            "Never cached025",
            &request(),
            None,
            true,
            false,
            &Default::default(),
        )
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::OfflineUnavailable);
    assert_eq!(responses.calls.load(Ordering::SeqCst), before);
}

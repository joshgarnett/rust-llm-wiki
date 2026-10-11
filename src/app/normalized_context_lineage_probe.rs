//! Test-only, fixed eight-request replay of the already exposed development tasks.
//! Root owns fixture copying, protocol/binary/source/resource pins and execution.
//! No provider runtime, reconstruction, labels, nominations or alternate allocator.
use crate::{
    app::{OfflineApp, OperationOptions},
    domain::*,
    retrieval::{
        context,
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
const READ_LIMIT: u64 = 256 * 1024 * 1024;
const TRACE_LIMIT: usize = 16 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 128 * 1024 * 1024;
const SPACE: &str = "blake3:9561688aec5f990e3006dea33767f99b4b83145613896f71cd111adef7ebd93d";
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

fn exact_path(path: &Path) {
    assert!(path.is_absolute(), "fixed absolute owned path required");
    for ancestor in path.ancestors() {
        if let Ok(metadata) = fs::symlink_metadata(ancestor) {
            assert!(!metadata.file_type().is_symlink(), "symlink rejected");
        }
    }
}

fn read_json(path: &Path, maximum: usize, read_bytes: &mut u64) -> Value {
    exact_path(path);
    let metadata = fs::metadata(path).expect("root-pinned input must exist");
    assert!(metadata.is_file() && metadata.len() <= maximum as u64);
    *read_bytes = read_bytes.checked_add(metadata.len()).unwrap();
    assert!(*read_bytes <= READ_LIMIT, "counted read allowance");
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes.len() as u64, metadata.len());
    serde_json::from_slice(&bytes).expect("bounded public JSON")
}

fn write_json(
    path: &Path,
    value: &Value,
    written: &mut usize,
    trace_written: &mut usize,
    trace: bool,
) {
    exact_path(path);
    let bytes = if trace {
        serde_json::to_vec(value).unwrap()
    } else {
        serde_json::to_vec_pretty(value).unwrap()
    };
    *written = written.checked_add(bytes.len()).unwrap();
    assert!(
        *written <= OUTPUT_LIMIT,
        "whole diagnostic output allowance"
    );
    if trace {
        assert!(
            bytes.len() <= 2 * 1024 * 1024,
            "per-case complete trace bytes: {}",
            bytes.len()
        );
        *trace_written = trace_written.checked_add(bytes.len()).unwrap();
        assert!(
            *trace_written <= TRACE_LIMIT,
            "whole eight-request trace allowance"
        );
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    file.write_all(&bytes).unwrap();
}

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

fn packet_identity(value: &Value) -> Value {
    // Everything matches exactly except the prospectively declared proof
    // issuance timestamp. Optional-field shapes and every other field match.
    let mut identity = value.clone();
    identity["verification"]
        .as_object_mut()
        .unwrap()
        .remove("verified_at");
    identity
}

fn known_read_work(trace: &[Value]) -> u64 {
    // Records existing logical decoded cache/vector and canonical-proof work.
    // SQLite pager/physical IO and uninstrumented safety reads are unavailable;
    // this sum must never be reported as a complete physical read measurement.
    let stages = trace
        .iter()
        .filter(|stage| stage["stage"] == "normalized_read_work")
        .collect::<Vec<_>>();
    assert_eq!(
        stages.len(),
        1,
        "the coordinator must supply one read-work record"
    );
    let rows = stages[0]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    [
        "canonical_proof_bytes",
        "cache_source_bytes",
        "vector_bytes",
        "metadata_bytes",
    ]
    .into_iter()
    .map(|key| row[key].as_u64().expect("existing logical work count"))
    .try_fold(0u64, |total, count| total.checked_add(count))
    .unwrap()
}

#[test]
#[ignore = "root-frozen zero-provider lineage protocol and explicitly owned real-cache copy required"]
fn normalized_real_context_lineage_eight_development_requests() {
    let started = Instant::now();
    assert!(
        !cfg!(debug_assertions),
        "release opt3 native probe only; root pins compiler evidence"
    );
    assert!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "native ARM64 qualification"
    );
    let base = PathBuf::from(REPO).join(ROOT);
    let owned = base.join("lineage-001");
    let output_root = owned.join("replacement-001");
    let vault = owned.join("vaults/real");
    exact_path(&vault);
    assert_eq!(
        vault.canonicalize().unwrap(),
        vault,
        "exact allowlisted disposable root"
    );
    let mut counted_reads = 0u64;
    let manifest = read_json(
        &base.join("critic/public/runtime-invocations-001.json"),
        256 * 1024,
        &mut counted_reads,
    );
    let commands = manifest["matrix"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row["commands"].as_array().unwrap())
        .filter(|command| command["id"].as_str().unwrap().starts_with("M03."))
        .collect::<Vec<_>>();
    assert_eq!(commands.len(), 8);
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
        // Only the root-owned SQLite copy is opened. Missing real cache blocks;
        // no probe, mock corpus, active-space repair or provider setup is used.
        let store = VectorStore::open(app.fs(), None).expect("retained real cache required");
        let state = store
            .active()
            .unwrap()
            .expect("retained real active space required");
        assert!(state.active);
        assert_eq!(state.id.as_str(), SPACE);
        assert_eq!(state.actual_dimensions, Some(1536));
        assert_eq!(state.spec.dimensions, Some(1536));
        assert_eq!(state.spec.settings.max_input_bytes, 8000);
        assert_eq!(state.spec.settings.quality_target_bytes, Some(1000));
        assert_eq!(state.spec.model, "text-embedding-3-small");
    }
    let mut written = 0usize;
    let mut trace_written = 0usize;
    let mut summaries = Vec::new();
    let mut mismatches = 0usize;
    let mut render_calls_total = 0usize;
    'cases: for (task_index, (task, query)) in TASKS.iter().enumerate() {
        for (mode_index, (mode_name, mode)) in [
            ("semantic", SearchMode::Semantic),
            ("hybrid", SearchMode::Hybrid),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                started.elapsed().as_secs() < 1170,
                "single owning interval/shutdown reserve"
            );
            let ordinal = task_index * 2 + mode_index;
            let slot = format!("M03.{:02}", ordinal + 1);
            let declared = commands[ordinal];
            assert_eq!(declared["id"], slot);
            let argv = declared["argv"].as_array().unwrap();
            let position = argv.iter().position(|word| word == "context").unwrap();
            assert_eq!(argv[position + 1], *query, "exact public task string");
            let position = argv.iter().position(|word| word == "--mode").unwrap();
            assert_eq!(argv[position + 1], mode_name);
            let baseline = read_json(
                &base.join(format!("runtime-001/commands/{slot}/stdout")),
                2 * 1024 * 1024,
                &mut counted_reads,
            );
            assert!(
                baseline["data"].is_object(),
                "all eight existing actual baseline contexts required"
            );
            let request = request(mode);
            let call_started = Instant::now();
            let ((result, trace), render_counts) = context::with_context_render_counts_for_test(
                256.min(2048 - 183 - render_calls_total),
                || {
                    context::with_candidate_ordering_trace(|| {
                        app.semantic_context(query, &request, None, true, false)
                    })
                },
            );
            render_calls_total += render_counts.calls;
            assert!(
                render_calls_total + 183 <= 2048,
                "whole automatic render allowance"
            );
            let elapsed_ms = call_started.elapsed().as_secs_f64() * 1000.0;
            assert!(elapsed_ms <= 30_000.0, "per automatic request 30s ceiling");
            assert!(trace.len() <= 16, "per-case trace stage envelopes");
            let trace_rows = trace
                .iter()
                .map(|stage| {
                    stage["rows"]
                        .as_array()
                        .map(Vec::len)
                        .or_else(|| stage["rows"]["proposals"].as_array().map(Vec::len))
                        .unwrap_or(1)
                })
                .sum::<usize>();
            assert!(trace_rows <= 4096, "per-case complete trace rows");
            let successful = result.is_ok();
            let actual = serde_json::to_value(&result).unwrap();
            // Result serializes as Ok/Err; keep it intact on every path.
            let identity_matches = result.as_ref().is_ok_and(|context| {
                assert!(!context.network_used);
                assert!(
                    context.usage().rendered_bytes <= 6000
                        && context.usage().estimated_tokens <= 1500
                );
                packet_identity(&serde_json::to_value(context).unwrap())
                    == packet_identity(&baseline["data"])
            });
            if !identity_matches {
                mismatches += 1;
            }
            if successful {
                counted_reads = counted_reads.checked_add(known_read_work(&trace)).unwrap();
                assert!(
                    counted_reads <= READ_LIMIT,
                    "finite counted logical-read allowance"
                );
            }
            write_json(
                &output_root.join(format!("automatic/{task}/{mode_name}.json")),
                &json!({
                    "task": task, "mode": mode_name, "slot": slot, "request": request,
                    "result": actual, "data": result.as_ref().ok().map(|value| serde_json::to_value(value).unwrap()), "elapsed_ms": elapsed_ms,
                    "render_counts": render_counts, "clock": "process-owned Instant",
                    "counted_logical_read_bytes_so_far": counted_reads,
                    "actual_render_calls_total_so_far": render_calls_total,
                    "baseline_path": format!("runtime-001/commands/{slot}/stdout"),
                    "identity_matches": identity_matches,
                    "baseline_provenance": {"snapshot": baseline["data"]["snapshot"], "fingerprint": baseline["data"]["dependency_fingerprint"], "verification": baseline["data"]["verification"]},
                    "interpretation": "observational equivalence only; no private labels, relevance or completeness grade"
                }),
                &mut written,
                &mut trace_written,
                false,
            );
            write_json(
                &output_root.join(format!("trace/{task}/{mode_name}.json")),
                &json!({
                    "task": task, "mode": mode_name, "query": query, "request": request,
                    "stages": trace, "scope": "already performed unit mapping and packing; no extra source reads or ranking/allocator changes",
                    "physical_io_and_complete_owned_read_bytes": "unavailable; logical decoded work is not physical IO"
                }),
                &mut written,
                &mut trace_written,
                true,
            );
            summaries.push(json!({"task": task, "mode": mode_name, "slot": slot, "identity_matches": identity_matches, "successful": successful}));
            if !identity_matches {
                break 'cases;
            }
        }
    }
    write_json(
        &output_root.join("logs/eight-request-summary.json"),
        &json!({
            "status": if mismatches == 0 {"OBSERVATIONAL_IDENTITY_EIGHT_CONTEXTS"} else {"EQUIVALENCE_BLOCKER_NO_ORACLE"},
        "requests": summaries, "request_count": summaries.len(), "mismatches": mismatches, "actual_render_calls_total": render_calls_total,
            "elapsed_seconds": started.elapsed().as_secs_f64(), "trace_bytes": trace_written,
            "new_output_bytes": written, "counted_logical_read_bytes": counted_reads,
            "owned_read_coverage": "canonical proof + decoded source/vector/metadata + explicit public input reads; initial VectorStore active-space metadata, query cache bootstrap, pager/physical/uninstrumented safety reads unavailable; root must reserve/account before execution",
            "provider_calls": 0, "oracle_requests": 0,
            "quality_acceptance": "OPEN; observed identity is not task completion or useful evidence qualification"
        }),
        &mut written,
        &mut trace_written,
        false,
    );
    assert_eq!(
        mismatches, 0,
        "every baseline case must reproduce; any mismatch blocks oracle"
    );
}

// Decode only the product's already authenticated proposal. This is a local
// test representation, not a new product serializer or proposal constructor.
fn frozen_passage(value: &Value) -> crate::retrieval::context_types::ContextPassage {
    use crate::retrieval::context_types::ContextPassage;
    assert_eq!(value["contributors"], json!([]));
    ContextPassage {
        locator: serde_json::from_value(value["locator"].clone()).unwrap(),
        text: value["text"].as_str().unwrap().to_owned(),
        span: serde_json::from_value(value["span"].clone()).unwrap(),
        label: serde_json::from_value(value["label"].clone()).unwrap(),
        eligibility: serde_json::from_value(value["eligibility"].clone()).unwrap(),
        citations: serde_json::from_value(value["citations"].clone()).unwrap(),
        contributors: vec![],
        rank_contributions: serde_json::from_value(value["rank_contributions"].clone()).unwrap(),
        support_group: serde_json::from_value(value["support_group"].clone()).unwrap(),
    }
}

#[test]
#[ignore = "independent critic's ID-only nomination, baseline equality and root runtime reservation required"]
fn normalized_real_context_lineage_existing_proposal_witness() {
    use crate::{
        catalog::{Catalog, query_types::QueryCatalog},
        retrieval::indexed_semantic,
    };
    use std::collections::{BTreeMap, BTreeSet};
    let started = Instant::now();
    assert!(!cfg!(debug_assertions) && cfg!(all(target_os = "macos", target_arch = "aarch64")));
    let ordinal: usize = std::env::var("LWIKI_LINEAGE_NOMINATION")
        .unwrap()
        .parse()
        .unwrap();
    assert!(ordinal < 8, "frozen total nomination allowance");
    let remaining: usize = std::env::var("LWIKI_LINEAGE_REMAINING_RENDER_CALLS")
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=128).contains(&remaining));
    let base = PathBuf::from(REPO).join(ROOT);
    let owned = base.join("lineage-001");
    let output_root = owned.join("replacement-001");
    let mut reads = 0;
    let summary = read_json(
        &output_root.join("logs/eight-request-summary.json"),
        64 * 1024,
        &mut reads,
    );
    assert_eq!(
        summary["mismatches"], 0,
        "no witness after an automatic mismatch"
    );
    let nomination = read_json(
        &base.join(format!(
            "critic/private/lineage-witness-001/nomination-{ordinal:02}.json"
        )),
        64 * 1024,
        &mut reads,
    );
    assert_eq!(
        nomination
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        ["case_id", "proposal_ids"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let mode_name = match nomination["case_id"].as_str().unwrap() {
        "q03-semantic" => "semantic",
        "q03-hybrid" => "hybrid",
        _ => panic!("only the two exposed q03 modes are witness eligible"),
    };
    let ids = nomination["proposal_ids"].as_array().unwrap();
    assert!(!ids.is_empty() && ids.len() <= 20);
    let automatic = read_json(
        &output_root.join(format!("automatic/q03/{mode_name}.json")),
        2 * 1024 * 1024,
        &mut reads,
    );
    assert_eq!(automatic["identity_matches"], true);
    let trace = read_json(
        &output_root.join(format!("trace/q03/{mode_name}.json")),
        2 * 1024 * 1024,
        &mut reads,
    );
    let stages = trace["stages"].as_array().unwrap();
    let pool = &stages
        .iter()
        .find(|stage| stage["stage"] == "candidate_pool")
        .unwrap()["rows"]["proposals"];
    assert!(pool.as_array().unwrap().len() <= 80);
    let mut by_id = BTreeMap::new();
    for packet in pool.as_array().unwrap() {
        let key = packet["key"].as_str().unwrap();
        assert_eq!(packet["passages"].as_array().unwrap().len(), 1);
        let parent = &packet["passages"][0]["authenticated_passage"];
        let passage = frozen_passage(parent);
        let id = context::lineage_proposal_id(
            key,
            &passage.locator.path,
            &passage.locator.observed_hash,
            passage.span,
        );
        assert_eq!(json!(id), packet["identity"]["proposals"][0]["proposal_id"]);
        assert!(
            by_id
                .insert(id.to_string(), (key.to_owned(), passage))
                .is_none()
        );
        if packet["fallback"].is_object() {
            let passage = frozen_passage(&packet["fallback"]);
            let id = context::lineage_proposal_id(
                &format!("{key}:child"),
                &passage.locator.path,
                &passage.locator.observed_hash,
                passage.span,
            );
            assert_eq!(json!(id), packet["fallback_identity"]["proposal_id"]);
            assert!(
                by_id
                    .insert(id.to_string(), (key.to_owned(), passage))
                    .is_none()
            );
        }
    }
    let mut seen_owners = BTreeSet::new();
    let mut paths = Vec::new();
    let scored = &stages
        .iter()
        .find(|stage| stage["stage"] == "scored_units")
        .unwrap()["rows"];
    for row in scored.as_array().unwrap() {
        let path = VaultRelativePath::new(row["owner"].as_str().unwrap()).unwrap();
        if seen_owners.insert(path.clone()) {
            paths.push(path);
        }
    }
    let request: ContextRequest = serde_json::from_value(automatic["request"].clone()).unwrap();
    assert_eq!(
        request,
        self::request(if mode_name == "semantic" {
            SearchMode::Semantic
        } else {
            SearchMode::Hybrid
        })
    );
    let fs = VaultFs::new(VaultRoot::explicit(&owned.join("vaults/real")).unwrap());
    let app = OfflineApp::new(
        fs.clone(),
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    let state = VectorStore::open(&fs, None)
        .unwrap()
        .active()
        .unwrap()
        .unwrap();
    assert_eq!(state.id.as_str(), SPACE);
    let catalog = Catalog::new(fs, app.vault_id().clone());
    let fingerprint = Blake3Hash::new(
        automatic["data"]["dependency_fingerprint"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let (result, render_counts) = context::with_context_render_counts_for_test(remaining, || {
        indexed_semantic::with_lineage_witness_catalog_for_test(
            &catalog,
            &request,
            &automatic["data"]["snapshot"],
            &fingerprint,
            &paths,
            &state,
            |reader: &dyn QueryCatalog| {
                let mut groups = BTreeSet::new();
                let mut admitted = Vec::new();
                let mut trials = Vec::new();
                let mut text = context::render_documents_for_test(reader, &request, &[])?;
                let mut feasible = true;
                for id in ids {
                    let (group, passage) = by_id
                        .get(id.as_str().unwrap())
                        .expect("existing admitted proposal ID required");
                    assert!(
                        groups.insert(group),
                        "parent/child exclusion and no duplicate proposal"
                    );
                    let document = reader.document(&passage.locator.path)?.unwrap();
                    assert_eq!(document.hash, passage.locator.observed_hash);
                    assert_eq!(document.eligibility, passage.eligibility);
                    assert_eq!(passage.span.slice(&document.raw_text)?, passage.text);
                    match context::admit_document_for_test(reader, &request, &admitted, passage)? {
                        Ok((next, rendered)) => {
                            trials.push(json!({"proposal_id": id, "accepted": true, "rendered_bytes": rendered.len()}));
                            admitted = next;
                            text = rendered;
                        }
                        Err(reason) => {
                            trials.push(
                                json!({"proposal_id": id, "accepted": false, "reason": reason}),
                            );
                            feasible = false;
                            break;
                        }
                    }
                }
                Ok(
                    json!({"feasible_all_nominated_ids": feasible, "text": text, "passages": admitted,
                    "rendered_bytes": text.len(), "estimated_tokens": text.len().div_ceil(4), "trials": trials}),
                )
            },
        )
    });
    assert!(started.elapsed().as_secs_f64() <= 15.0);
    let mut written = 0;
    let mut trace_written = 0;
    write_json(
        &output_root.join(format!("witness/nomination-{ordinal:02}.json")),
        &json!({
            "case_id": nomination["case_id"], "nomination": ordinal,
            "nomination_commitment": Blake3Hash::digest(serde_json::to_vec(&nomination).unwrap()),
            "result": result, "render_counts": render_counts, "explicit_json_read_bytes": reads,
            "elapsed_seconds": started.elapsed().as_secs_f64(), "provider_calls": 0, "new_scored_units": 0,
            "interpretation": "ID-only same-renderer feasibility witness, not automatic selection or quality acceptance",
        }),
        &mut written,
        &mut trace_written,
        false,
    );
}

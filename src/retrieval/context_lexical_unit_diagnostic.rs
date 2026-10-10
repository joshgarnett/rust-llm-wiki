//! Finite, opt-in DEVELOPMENT replay of the failed frozen facility allocator.
//! This test-only observer never changes a production request or admission rule.
use super::{
    context::{DocumentTrial, Packet},
    context_set_packing::{self, Selection},
    context_types::{ContextRequest, ContextScope, ContextTarget},
};
use crate::{
    catalog::{Catalog, query_types::QueryCatalog},
    domain::*,
    vault::{VaultFs, VaultRoot},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

const CASES: [&str; 4] = [
    "dev-support-04",
    "dev-support-11",
    "dev-long-train-26",
    "dev-long-qasper-1908.11664",
];
const TOTAL_TRIALS: usize = 77_000;
const OWNER_SECONDS: u64 = 60;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePin {
    path: VaultRelativePath,
    hash: Blake3Hash,
    source_id: RecordId,
    revision_id: RecordId,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    query: String,
    query_hash: Blake3Hash,
    request: ContextRequest,
    expected_snapshot: ReadSnapshot,
    expected_dependency_fingerprint: Blake3Hash,
    source_pins: Vec<SourcePin>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: u32,
    vault_path: PathBuf,
    vault_id: RecordId,
    mapping_pin: String,
    output_dir: PathBuf,
    cases: Vec<Case>,
}
#[derive(Default, Serialize)]
struct Trace {
    captured_old_pool: bool,
    representation_complete: bool,
    observed_snapshot: Option<ReadSnapshot>,
    observed_dependency_fingerprint: Option<Blake3Hash>,
    pool: Vec<Value>,
    tokens: Vec<Vec<String>>,
    weights: Vec<f64>,
    similarities: Vec<f64>,
    trials: Vec<Value>,
    commits: Vec<Value>,
    errors: Vec<Value>,
    last_committed_members: Vec<usize>,
    last_committed_objective: f64,
    returned_statistics: Option<Value>,
    fallback: bool,
}
struct Session {
    case: Case,
    continuation: bool,
    start: Instant,
    deadline: Instant,
    shared_realizations: Rc<Cell<usize>>,
    trace: Trace,
}
thread_local! {static SESSION:RefCell<Option<Session>>=const {RefCell::new(None)};}
pub(super) fn active() -> bool {
    SESSION.with(|s| s.borrow().is_some())
}

fn error_row(stage: &str, error: &WikiError, elapsed: f64) -> Value {
    json!({"stage":stage,"elapsed_seconds":elapsed,"error":error})
}
pub(super) fn record_error(stage: &str, error: &WikiError) {
    SESSION.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.trace
                .errors
                .push(error_row(stage, error, s.start.elapsed().as_secs_f64()));
        }
    });
}
pub(super) fn check_limits() -> Result<()> {
    SESSION.with(|s| {
        let mut borrowed = s.borrow_mut();
        let Some(s) = borrowed.as_mut() else {
            return Ok(());
        };
        let reason = if Instant::now() >= s.deadline {
            Some("shared per-case 60-second diagnostic deadline")
        } else {
            None
        };
        if let Some(reason) = reason {
            let error = WikiError::new(ErrorCode::BudgetExceeded, reason);
            s.trace.errors.push(error_row(
                "diagnostic_limit",
                &error,
                s.start.elapsed().as_secs_f64(),
            ));
            Err(error)
        } else {
            Ok(())
        }
    })
}
pub(super) fn before_realization() -> Result<()> {
    check_limits()?;
    SESSION.with(|s| {
        let mut borrowed = s.borrow_mut();
        let Some(s) = borrowed.as_mut() else {
            return Ok(());
        };
        if s.shared_realizations.get() == TOTAL_TRIALS {
            let error = WikiError::new(
                ErrorCode::BudgetExceeded,
                "shared per-case 77000-realization diagnostic limit",
            );
            s.trace.errors.push(error_row(
                "diagnostic_limit",
                &error,
                s.start.elapsed().as_secs_f64(),
            ));
            return Err(error);
        }
        s.shared_realizations.set(s.shared_realizations.get() + 1);
        Ok(())
    })
}
pub(super) fn record_tokens(tokens: &[BTreeSet<String>]) {
    SESSION.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.trace.tokens = tokens.iter().map(|t| t.iter().cloned().collect()).collect();
            s.trace.representation_complete = false;
        }
    });
}
pub(super) fn record_representation(weights: &[f64], similarities: &[f64]) {
    SESSION.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.trace.weights = weights.to_vec();
            s.trace.similarities = similarities.to_vec();
            let n = s.trace.pool.len();
            s.trace.representation_complete = s.trace.captured_old_pool
                && n > 0
                && s.trace.tokens.len() == n
                && s.trace.weights.len() == n
                && n.checked_mul(n) == Some(s.trace.similarities.len());
        }
    });
}

fn paired_representation_identical(arms: &[Value]) -> Option<bool> {
    if arms.len() != 2
        || !arms.iter().all(|arm| {
            let trace = &arm["trace"];
            let n = trace["pool"].as_array().map_or(0, Vec::len);
            trace["captured_old_pool"] == true
                && trace["representation_complete"] == true
                && n > 0
                && trace["tokens"].as_array().map(Vec::len) == Some(n)
                && trace["weights"].as_array().map(Vec::len) == Some(n)
                && trace["similarities"].as_array().map(Vec::len) == n.checked_mul(n)
        })
    {
        return None;
    }
    Some(
        ["pool", "tokens", "weights", "similarities"]
            .into_iter()
            .all(|key| arms[0]["trace"][key] == arms[1]["trace"][key]),
    )
}

fn successful_paired_diagnostic(arms: &[Value], representation: Option<bool>) -> bool {
    representation == Some(true)
        && arms.len() == 2
        && arms.iter().all(|arm| arm["successful_response"] == true)
        && arms[1]["naturally_terminated"] == true
}
pub(super) fn record_trial(members: &[usize], trial: &DocumentTrial) {
    SESSION.with(|s|{if let Some(s)=s.borrow_mut().as_mut(){s.trace.trials.push(json!({
        "members":members,"rendered_bytes":trial.text.len(),"estimated_tokens":trial.text.len().div_ceil(4),
        "owner_counts":trial.counts,"reason":trial.reason,"elapsed_seconds":s.start.elapsed().as_secs_f64(),
    }));}});
}
pub(super) fn record_trial_error(members: &[usize], error: &WikiError) {
    SESSION.with(|s|{if let Some(s)=s.borrow_mut().as_mut(){
        s.trace.trials.push(json!({"members":members,"error":error,"elapsed_seconds":s.start.elapsed().as_secs_f64()}));
        s.trace.errors.push(error_row("document_trial",error,s.start.elapsed().as_secs_f64()));
    }});
}
pub(super) fn record_commit(members: &[usize], objective: f64) {
    SESSION.with(|s|{if let Some(s)=s.borrow_mut().as_mut(){
        s.trace.last_committed_members=members.to_vec();s.trace.last_committed_objective=objective;
        s.trace.commits.push(json!({"members":members,"objective":objective,"elapsed_seconds":s.start.elapsed().as_secs_f64()}));
    }});
}

pub(super) fn allocate_old(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    packets: &[Packet],
    initial_bytes: usize,
) -> Result<Selection<DocumentTrial>> {
    check_limits()?;
    let continuation=SESSION.with(|s| -> Result<bool> {
        let mut borrowed=s.borrow_mut();
        let s=borrowed.as_mut().ok_or_else(||WikiError::invalid("diagnostic session absent"))?;
        let fingerprint=reader.dependency_fingerprint()?;
        s.trace.observed_snapshot=Some(reader.snapshot().clone());
        s.trace.observed_dependency_fingerprint=Some(fingerprint.clone());
        if request!=&s.case.request || reader.snapshot()!=&s.case.expected_snapshot
            || fingerprint!=s.case.expected_dependency_fingerprint
            || packets.len()>320
        {return Err(WikiError::new(ErrorCode::FreshnessConflict,"diagnostic request/snapshot/dependency/pool pin mismatch"));}
        for (index,packet) in packets.iter().enumerate() {
            let candidate=packet.selection.as_ref().ok_or_else(||WikiError::invalid("old proposal metadata absent"))?;
            let passage=packet.passages.first().ok_or_else(||WikiError::invalid("old proposal passage absent"))?;
            if packet.passages.len()!=1 {return Err(WikiError::invalid("old proposal is not indivisible"));}
            let pin=s.case.source_pins.iter().find(|pin|pin.path==passage.locator.path).ok_or_else(||WikiError::invalid("old proposal source path is not pinned"))?;
            let pinned=passage.citations.iter().any(|c|matches!(c,CitationRef::Source(r) if r.source_id==pin.source_id && r.source_revision==pin.revision_id));
            if pin.hash!=passage.locator.observed_hash || !pinned {return Err(WikiError::new(ErrorCode::FreshnessConflict,"old proposal source pin mismatch"));}
            s.trace.pool.push(json!({"index":index,"original_id":packet.key,"passage":passage,
                "local_relevance":candidate.local_relevance,"owner_score":packet.score,"owner_index":candidate.owner_index,
                "covered_terms":candidate.covered_terms,"seed_overlap":candidate.seed_overlap,"clipped":candidate.clipped}));
        }
        s.trace.captured_old_pool=true;
        Ok(s.continuation)
    }).inspect_err(|e|record_error("pool_pins",e))?;
    let outcome = if continuation {
        context_set_packing::allocate_with_caps(
            reader,
            request,
            packets,
            initial_bytes,
            (usize::MAX, usize::MAX),
        )
    } else {
        context_set_packing::allocate(reader, request, packets, initial_bytes)
    }?;
    let Some(outcome) = outcome else {
        SESSION.with(|s| {
            s.borrow_mut().as_mut().unwrap().trace.fallback = true;
        });
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "frozen old representation unavailable; diagnostic refuses a different fallback algorithm",
        ));
    };
    SESSION.with(|s| {
        s.borrow_mut().as_mut().unwrap().trace.returned_statistics =
            Some(json!(outcome.statistics));
    });
    Ok(outcome)
}

/// Root supplies the four owned public development inputs only after source
/// freeze. Both arms share one finite owner clock and realization allowance.
#[test]
#[ignore = "root-owned four-case frozen DEVELOPMENT diagnostic; no native acceptance claim"]
fn replay_declared_development_old_pool() {
    let path = PathBuf::from(
        std::env::var("LWIKI_LEXICAL_UNIT_DIAGNOSTIC_CONFIG")
            .expect("explicit diagnostic config path"),
    );
    let bytes = std::fs::read(&path).expect("read pinned diagnostic config");
    assert!(bytes.len() <= 8 * 1024 * 1024, "finite config byte limit");
    let config: Config = serde_json::from_slice(&bytes).expect("strict diagnostic config");
    assert_eq!(config.schema, 1);
    assert!(!config.mapping_pin.trim().is_empty());
    assert!(config.vault_path.is_absolute() && config.output_dir.is_absolute());
    assert_eq!(config.cases.len(), 4);
    assert_eq!(
        config
            .cases
            .iter()
            .map(|c| c.id.as_str())
            .collect::<BTreeSet<_>>(),
        CASES.into_iter().collect()
    );
    assert!(
        !config.output_dir.exists(),
        "diagnostic refuses to overwrite prior evidence"
    );
    std::fs::create_dir(&config.output_dir).unwrap();
    let catalog = Catalog::new(
        VaultFs::new(VaultRoot::explicit(&config.vault_path).unwrap()),
        config.vault_id.clone(),
    );
    for case in config.cases {
        assert_eq!(Blake3Hash::digest(case.query.as_bytes()), case.query_hash);
        assert_eq!(case.request.scope, ContextScope::IndexedDocuments);
        assert_eq!(case.request.target, ContextTarget::Documents);
        assert!(case.request.graph.is_none());
        assert_eq!(
            case.request.documents.mode,
            super::types::SearchMode::Lexical
        );
        assert_eq!(case.request.documents.limits.hits, 10);
        assert_eq!(case.request.documents.limits.candidates, 80);
        assert_eq!(case.request.documents.limits.excerpt_bytes, 1024);
        assert_eq!(
            case.request.budget,
            super::context_types::ContextBudget::default()
        );
        assert_eq!(
            case.request.verification_budget,
            super::context_types::VerificationBudget::default()
        );
        assert!(!case.source_pins.is_empty());
        assert_eq!(
            case.source_pins
                .iter()
                .map(|p| &p.path)
                .collect::<BTreeSet<_>>()
                .len(),
            case.source_pins.len()
        );
        let start = Instant::now();
        let deadline = start + Duration::from_secs(OWNER_SECONDS);
        let shared = Rc::new(Cell::new(0));
        let mut arms = Vec::new();
        for continuation in [false, true] {
            SESSION.with(|s| {
                assert!(s.borrow().is_none());
                *s.borrow_mut() = Some(Session {
                    case: case.clone(),
                    continuation,
                    start,
                    deadline,
                    shared_realizations: shared.clone(),
                    trace: Trace::default(),
                });
            });
            let outcome = check_limits()
                .and_then(|_| {
                    super::verification::context(&catalog, None, &case.query, &case.request)
                })
                .and_then(|response| {
                    check_limits()?;
                    if response.snapshot() != &case.expected_snapshot
                        || response.dependency_fingerprint()
                            != &case.expected_dependency_fingerprint
                    {
                        return Err(WikiError::new(
                            ErrorCode::FreshnessConflict,
                            "diagnostic final context snapshot/dependency pin mismatch",
                        ));
                    }
                    if SESSION.with(|s| s.borrow().as_ref().unwrap().trace.captured_old_pool) {
                        Ok(response)
                    } else {
                        Err(WikiError::new(
                            ErrorCode::CapabilityUnavailable,
                            "diagnostic old pool route was not captured",
                        ))
                    }
                });
            if let Err(error) = &outcome {
                record_error("coordinator", error);
            }
            let session = SESSION.with(|s| s.borrow_mut().take().unwrap());
            let naturally_terminated = continuation
                && outcome.is_ok()
                && session.trace.captured_old_pool
                && session.trace.representation_complete
                && !session.trace.fallback
                && session.trace.returned_statistics.as_ref().is_some_and(|v| {
                    v["addition_exhausted"] == false && v["exchange_exhausted"] == false
                });
            arms.push(json!({"arm":if continuation {"natural_continuation"} else {"frozen_3072_1024"},
                "naturally_terminated":naturally_terminated,"successful_response":outcome.is_ok(),
                "response":outcome.as_ref().ok(),"error":outcome.as_ref().err(),"trace":session.trace,
                "shared_realizations_after_arm":shared.get(),"owner_elapsed_seconds":start.elapsed().as_secs_f64()}));
        }
        assert!(shared.get() <= TOTAL_TRIALS);
        let paired_old_pool_identical = if arms
            .iter()
            .all(|arm| arm["trace"]["captured_old_pool"] == true)
        {
            Some(arms[0]["trace"]["pool"] == arms[1]["trace"]["pool"])
        } else {
            None
        };
        let paired_representation_identical = paired_representation_identical(&arms);
        let successful_paired_diagnostic =
            successful_paired_diagnostic(&arms, paired_representation_identical);
        let pair_comparison_error = if paired_old_pool_identical == Some(false)
            || paired_representation_identical == Some(false)
        {
            Some(WikiError::new(
                ErrorCode::FreshnessConflict,
                "diagnostic arms reconstructed different old proposal representations",
            ))
        } else {
            None
        };
        let output = json!({"schema":1,"kind":"failed-facility-four-case-development-diagnostic","case_id":case.id,
            "experimental":true,"native_acceptance_claim":false,"query_hash":case.query_hash,"request":case.request,
            "config_blake3":Blake3Hash::digest(&bytes),"mapping_pin":config.mapping_pin,"vault_id":config.vault_id,
            "shared_realization_limit":TOTAL_TRIALS,"owner_seconds_limit":OWNER_SECONDS,
            "paired_old_pool_identical":paired_old_pool_identical,"pair_comparison_error":pair_comparison_error,
            "paired_representation_identical":paired_representation_identical,"successful_paired_diagnostic":successful_paired_diagnostic,
            "shared_realizations":shared.get(),"owner_elapsed_seconds":start.elapsed().as_secs_f64(),"arms":arms,
            "interpretation":"Same old builder, objective, traversal and exact authority. Diagnostic capture overhead is included; inherited query/proof/SQL budgets remain binding. Failed/interrupted traces are not successful responses or natural termination."});
        let target = config
            .output_dir
            .join(format!("{}.diagnostic.json", case.id));
        std::fs::write(target, serde_json::to_vec_pretty(&output).unwrap()).unwrap();
    }
}

#[test]
fn interrupted_observation_never_claims_a_complete_paired_representation() {
    let mut arm = json!({"successful_response":false,"naturally_terminated":false,
        "trace":{"captured_old_pool":true,"representation_complete":false,"pool":[{"index":0}],
            "tokens":[],"weights":[],"similarities":[]}});
    assert_eq!(
        paired_representation_identical(&[arm.clone(), arm.clone()]),
        None
    );
    arm["trace"]["tokens"] = json!([["alpha"]]);
    assert_eq!(
        paired_representation_identical(&[arm.clone(), arm.clone()]),
        None
    );
    arm["trace"]["representation_complete"] = json!(true);
    arm["trace"]["weights"] = json!([1.0]);
    // An asserted marker cannot excuse a partial matrix.
    assert_eq!(
        paired_representation_identical(&[arm.clone(), arm.clone()]),
        None
    );
    arm["trace"]["similarities"] = json!([1.0]);
    let representation = paired_representation_identical(&[arm.clone(), arm.clone()]);
    assert_eq!(representation, Some(true));
    assert!(!successful_paired_diagnostic(
        &[arm.clone(), arm.clone()],
        representation
    ));
    arm["successful_response"] = json!(true);
    assert!(!successful_paired_diagnostic(
        &[arm.clone(), arm.clone()],
        representation
    ));
    let mut continuation = arm.clone();
    continuation["naturally_terminated"] = json!(true);
    assert!(successful_paired_diagnostic(
        &[arm, continuation],
        representation
    ));
}

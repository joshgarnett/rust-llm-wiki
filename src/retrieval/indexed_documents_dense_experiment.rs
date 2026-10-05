//! Explicitly experimental cached discovery. The retained cache is not a
//! normalized preparation path, current-universe proof, or scalable index.
use super::*;
use crate::retrieval::{fusion, render, types::*, vectors};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, fs, io::Write, path::PathBuf, time::Instant};

fn fail(code: ErrorCode, message: &str) -> WikiError {
    WikiError::new(code, message)
}
fn conflict(message: &str) -> WikiError {
    fail(ErrorCode::FreshnessConflict, message)
}
fn pinned_request() -> ContextRequest {
    let mut request = ContextRequest {
        scope: ContextScope::IndexedDocuments,
        ..Default::default()
    };
    request.documents.limits.hits = 5;
    request.documents.limits.candidates = 80;
    request.documents.limits.excerpt_bytes = 1024;
    request.budget.max_bytes = 6000;
    request.budget.max_tokens = 1500;
    request.verification_budget = VerificationBudget {
        max_bytes: 64 * 1024 * 1024,
        max_files: 4096,
        max_entries: 65536,
        max_elapsed_ms: 5000,
    };
    request
}
#[derive(Default, serde::Serialize)]
struct Diagnostics {
    experimental: bool,
    normalized_publication: Option<String>,
    normalized_snapshot: Option<ReadSnapshot>,
    retained_snapshot_key: Option<Blake3Hash>,
    active_space: Option<Value>,
    query_input_hash: Option<Blake3Hash>,
    query_vector_hash: Option<Blake3Hash>,
    retained_work: vectors::RetainedUsage,
    coverage: Option<vectors::Coverage>,
    published_eligible_owners: usize,
    matched_retained_owners: usize,
    published_owners_without_retained_matching_units: usize,
    missing_retained_owners: BTreeSet<VaultRelativePath>,
    changed_retained_owners: BTreeSet<VaultRelativePath>,
    filtered_units: usize,
    selected_source_bytes_rendered: usize,
    selected_units_rendered: usize,
    selected_units_scored: usize,
    selected_owners_without_matching_units: BTreeSet<VaultRelativePath>,
    semantic_coverage: String,
    ranked_unit_selection_used: bool,
    catalog_rows_decoded: usize,
    catalog_bytes_decoded: usize,
    elapsed_ms: f64,
}
fn discovery_placeholder(
    reader: &QuerySnapshot,
    hit: &vectors::DenseHit,
    rank: usize,
) -> SearchHit {
    SearchHit {
        locator: DocumentLocator {
            record: hit.target_id.as_ref().map(|id| RecordRef {
                vault_id: reader.vault_id().clone(),
                record_id: id.clone(),
                expected_kind: RecordKind::Revision,
            }),
            path: hit.owner.clone(),
            observed_hash: Blake3Hash::digest(b"discovery placeholder"),
        },
        title: String::new(),
        kind: None,
        authored_status: None,
        eligibility: Eligibility::Current,
        identity_eligibility: None,
        excerpt: SearchExcerpt {
            text: String::new(),
            span: hit
                .source_span
                .unwrap_or(ByteSpan::new(0, 0).expect("empty span")),
            matched_spans: vec![],
            label: ExcerptLabel::NoteText,
            citation: None,
        },
        secondary_excerpts: vec![],
        reasons: vec![RetrievalReason::Semantic],
        rank_contributions: vec![RankContribution {
            channel: "dense".into(),
            rank,
            score: Some(hit.score),
        }],
        source_id: None,
        owner_revision: None,
    }
}

fn run(
    catalog: &Catalog,
    query: &str,
    request: &ContextRequest,
    arm: &str,
    preflight: bool,
    stats: &mut Diagnostics,
    before_final: impl FnOnce() -> Result<()>,
) -> Result<Value> {
    let started = Instant::now();
    stats.experimental = true;
    stats.semantic_coverage =
        "unknown: retained snapshot is not the complete current normalized unit universe".into();
    let meter = Meter::new(&request.verification_budget);
    let request = context::validate_request(query, request)?;
    if request.scope != ContextScope::IndexedDocuments
        || request.target != ContextTarget::Documents
        || request.graph.is_some()
        || request.documents.mode != SearchMode::Lexical
        || request.documents.cursor.is_some()
        || !matches!(arm, "L" | "S" | "H")
        || request.verification_budget.max_elapsed_ms > 5000
    {
        return Err(fail(
            ErrorCode::Usage,
            "experimental native dense arm requires bounded indexed document request",
        ));
    }
    catalog.guard_query()?;
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_rows: 4096,
        max_row_bytes: 1024 * 1024,
        max_bytes: 64 * 1024 * 1024,
        max_elapsed_ms: meter.remaining_ms(),
        ..Default::default()
    })?;
    if !reader.normalized_layout() {
        return Err(fail(
            ErrorCode::CapabilityUnavailable,
            "experimental dense arm requires normalized publication",
        ));
    }
    stats.normalized_publication = reader.publication_id().map(str::to_owned);
    stats.normalized_snapshot = Some(reader.snapshot().clone());
    let dense = arm != "L";
    let retained = if dense {
        Some(
            vectors::RetainedMembershipReader::open(catalog.fs(), None, meter.remaining_ms())
                .map_err(|error| {
                    if let Ok(usage) =
                        serde_json::from_value(error.details["retained_work"].clone())
                    {
                        stats.retained_work = usage;
                    }
                    error
                })?,
        )
    } else {
        None
    };
    let result = (|| -> Result<Value> {
        let mut dense_groups = Vec::new();
        let mut query_vector = None;
        if let Some(retained) = &retained {
            stats.retained_snapshot_key = Some(retained.snapshot_key.clone());
            stats.active_space = Some(json!(retained.state));
            let input = retained.state.spec.query(query)?;
            stats.query_input_hash = Some(input.input_hash.clone());
            let vector = retained.store().vector(&retained.state.id, &input.input_hash)?.ok_or_else(|| fail(ErrorCode::OfflineUnavailable,"cached query vector unavailable; experimental arm performs no provider calls"))?;
            stats.query_vector_hash = Some(Blake3Hash::digest(vectors::normalize(&vector)?));
            stats.published_eligible_owners =
                fusion::published_context_owner_count(&reader, &request.documents.filters)?;
            let matched = RefCell::new(BTreeSet::new());
            let missing = RefCell::new(BTreeSet::new());
            let changed = RefCell::new(BTreeSet::new());
            let filtered = std::cell::Cell::new(0);
            let scan = retained.store().exact_stream(
                &retained.state.id,
                &vector,
                || Ok(retained.units()),
                &[render::TargetKind::Document],
                request.documents.limits.candidates,
                |unit| {
                    meter.check()?;
                    match fusion::retained_owner_match(&reader, unit, &request.documents.filters)? {
                        fusion::RetainedOwnerMatch::Match => {
                            matched.borrow_mut().insert(unit.owner.clone());
                            Ok(true)
                        }
                        fusion::RetainedOwnerMatch::Missing => {
                            missing.borrow_mut().insert(unit.owner.clone());
                            Ok(false)
                        }
                        fusion::RetainedOwnerMatch::Changed => {
                            changed.borrow_mut().insert(unit.owner.clone());
                            Ok(false)
                        }
                        fusion::RetainedOwnerMatch::Filtered => {
                            filtered.set(filtered.get() + 1);
                            Ok(false)
                        }
                    }
                },
            )?;
            stats.coverage = Some(scan.coverage.clone());
            stats.retained_work = retained.usage();
            stats.matched_retained_owners = matched.borrow().len();
            stats.published_owners_without_retained_matching_units = stats
                .published_eligible_owners
                .saturating_sub(stats.matched_retained_owners);
            stats.missing_retained_owners = missing.into_inner();
            stats.changed_retained_owners = changed.into_inner();
            stats.filtered_units = filtered.get();
            if scan.coverage.missing_units != 0 {
                return Err(fail(
                    ErrorCode::OfflineUnavailable,
                    "retained eligible vectors missing/corrupt/pending; cached dense preflight incomplete",
                ));
            }
            dense_groups = fusion::collapse_dense(
                scan.hits
                    .get(&render::TargetKind::Document)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            );
            query_vector = Some(vector);
        }
        let mut lexical_hits = None;
        if arm != "S" {
            let mut plan = request.documents.clone();
            if arm == "H" {
                plan.limits.hits = 50.min(plan.limits.candidates);
            }
            let mut result = lexical::search_context_catalog(&reader, query, &plan, false)?;
            // Preserve the existing hybrid lexical channel's pagination/cap.
            if arm == "H" {
                while result.hits.len() < plan.limits.candidates && result.next_cursor.is_some() {
                    plan.cursor = result.next_cursor.clone();
                    let page = lexical::search_context_catalog(&reader, query, &plan, false)?;
                    result.hits.extend(page.hits);
                    result.next_cursor = page.next_cursor;
                    result.omitted_candidates =
                        result.omitted_candidates.max(page.omitted_candidates);
                }
            }
            lexical_hits = Some(result);
        }
        let placeholders = dense_groups
            .iter()
            .enumerate()
            .map(|(rank, (hit, _))| discovery_placeholder(&reader, hit, rank + 1))
            .collect::<Vec<_>>();
        let mut ranked = if arm == "L" {
            lexical_hits.as_ref().expect("lexical result").hits.clone()
        } else {
            fusion::fuse_hits(vec![
                placeholders,
                lexical_hits
                    .as_ref()
                    .map(|hits| hits.hits.clone())
                    .unwrap_or_default(),
            ])
        };
        let candidate_count = ranked.len();
        ranked.truncate(request.documents.limits.hits);
        let paths = ranked
            .iter()
            .map(|hit| hit.locator.path.clone())
            .collect::<Vec<_>>();
        let mut budget = request.verification_budget.clone();
        budget.max_elapsed_ms = meter.remaining_ms();
        let mut proof = selected_documents::authenticate(catalog, &reader, &paths, &budget)?;
        let selected = SelectedCatalog {
            reader: &reader,
            proof: &proof,
        };
        let mut signals = ContextSelectionSignals::default();
        let mut authenticated_dense = Vec::new();
        if let Some(retained) = &retained {
            for path in &paths {
                meter.check()?;
                let document = proof
                    .documents
                    .get(path)
                    .ok_or_else(|| conflict("selected dense owner absent from proof"))?;
                if document.raw_text.len() > 1024 * 1024
                    || stats
                        .selected_source_bytes_rendered
                        .saturating_add(document.raw_text.len())
                        > 4 * 1024 * 1024
                {
                    return Err(fail(
                        ErrorCode::BudgetExceeded,
                        "selected exact rerender source byte bound exceeded",
                    ));
                }
                stats.selected_source_bytes_rendered += document.raw_text.len();
                let units = render::render_selected_document_for_test(
                    document,
                    &retained.state.spec.settings,
                    proof.fingerprint.clone(),
                )?;
                let mut verified = BTreeMap::new();
                for unit in units {
                    meter.check()?;
                    let unit = unit?;
                    stats.selected_units_rendered += 1;
                    let old = retained.unit(&unit.unit_id, &unit.input_hash)?;
                    if let Some(old) = old {
                        render::require_selected_unit_agreement(&old, &unit)?;
                        let vector = retained
                            .store()
                            .vector(&retained.state.id, &unit.input_hash)?
                            .ok_or_else(|| {
                                fail(
                                    ErrorCode::OfflineUnavailable,
                                    "selected unit vector missing; no partial semantic output",
                                )
                            })?;
                        let score =
                            vectors::cosine(query_vector.as_ref().expect("cached query"), &vector)?;
                        signals.semantic.push(ContextSemanticCue {
                            owner: path.clone(),
                            observed_hash: document.hash.clone(),
                            span: unit.source_span.ok_or_else(|| {
                                conflict("selected document unit has no source span")
                            })?,
                            cosine: score,
                        });
                        stats.selected_units_scored += 1;
                        verified.insert(unit.unit_id.clone(), unit);
                    } else {
                        stats
                            .selected_owners_without_matching_units
                            .insert(path.clone());
                    }
                }
                if let Some((rank, (best, passages))) = dense_groups
                    .iter()
                    .enumerate()
                    .find(|(_, (best, _))| &best.owner == path)
                {
                    for winning in passages {
                        let current = verified.get(&winning.unit_id).ok_or_else(|| conflict("selected retained winner failed exact rerender; no stale winner discard"))?;
                        if current.input_hash != winning.input_hash
                            || current.source_span != winning.source_span
                            || current.source_hash != document.hash
                        {
                            return Err(conflict("selected winning identity/input/span changed"));
                        }
                    }
                    let primary_budget = if passages.len() > 1 {
                        request.documents.limits.excerpt_bytes / 2
                    } else {
                        request.documents.limits.excerpt_bytes
                    };
                    let mut hit = fusion::dense_hit_for_selected_query(
                        &selected,
                        document,
                        best,
                        rank + 1,
                        primary_budget.max(1),
                        query,
                    )?;
                    if let Some(second) = passages.get(1) {
                        let remaining = request
                            .documents
                            .limits
                            .excerpt_bytes
                            .saturating_sub(hit.excerpt.text.len());
                        if remaining > 0 {
                            let excerpt = fusion::dense_hit_for_selected_query(
                                &selected,
                                document,
                                second,
                                rank + 1,
                                remaining,
                                query,
                            )?
                            .excerpt;
                            if excerpt.span.end() <= hit.excerpt.span.start()
                                || hit.excerpt.span.end() <= excerpt.span.start()
                            {
                                hit.secondary_excerpts.push(excerpt);
                            }
                        }
                    }
                    authenticated_dense.push(hit);
                }
            }
            stats.retained_work = retained.usage();
            signals.semantic_complete = stats.selected_owners_without_matching_units.is_empty();
            stats.ranked_unit_selection_used =
                signals.semantic_complete && !signals.semantic.is_empty();
            if !signals.semantic_complete {
                signals.warnings.push("Selected lexical owners lack matching retained semantic units; ordinary lexical passage allocation is used for this hybrid result. Semantic current-universe coverage remains unknown.".into());
            }
        }
        let selected_lexical = lexical_hits
            .as_ref()
            .map(|hits| {
                hits.hits
                    .iter()
                    .filter(|hit| paths.contains(&hit.locator.path))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let final_hits = if arm == "L" {
            ranked
        } else {
            fusion::fuse_hits(vec![authenticated_dense, selected_lexical])
        };
        if final_hits
            .iter()
            .map(|hit| &hit.locator.path)
            .collect::<Vec<_>>()
            != paths.iter().collect::<Vec<_>>()
        {
            return Err(conflict(
                "authenticated fusion differs from selected discovery owners",
            ));
        }
        let hits = HitSet {
            network_used: false,
            graph: None,
            hits: final_hits,
            next_cursor: None,
            truncated: candidate_count > paths.len(),
            candidate_count,
            omitted_candidates: lexical_hits
                .as_ref()
                .map_or(0, |hits| hits.omitted_candidates),
            snapshot: reader.snapshot().clone(),
            verification: reader.verification().clone(),
            dependency_fingerprint: proof.fingerprint.clone(),
            warnings: vec![],
        };
        let draft = if preflight {
            None
        } else {
            Some(context::assemble_bounded_documents_with_signals_for_test(
                &selected,
                &request,
                &hits,
                query,
                &signals,
                &crate::retrieval::context_selection_packet::SelectionAction::Automatic,
            )?)
        };
        meter.check()?;
        before_final()?;
        proof.recheck(catalog, &reader)?;
        if let Some(retained) = &retained {
            retained.recheck_active(catalog.fs())?;
            stats.retained_work = retained.usage();
        }
        meter.check()?;
        stats.catalog_rows_decoded = reader.usage().rows;
        stats.catalog_bytes_decoded = reader.usage().bytes;
        stats.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let verified = verification(&reader)?;
        meter.check()?;
        if let Some(draft) = draft {
            Ok(json!(seal(draft, verified, proof.meter())))
        } else {
            Ok(
                json!({"complete":true,"selected_proof_fingerprint":proof.fingerprint,"selected_owner_count":paths.len(),"proof_work":proof.meter().work()}),
            )
        }
    })();
    if let Some(retained) = &retained {
        stats.retained_work = retained.usage();
    }
    stats.catalog_rows_decoded = reader.usage().rows;
    stats.catalog_bytes_decoded = reader.usage().bytes;
    stats.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    result
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryCase {
    id: String,
    query: String,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    vault: PathBuf,
    output: PathBuf,
    action: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    arm: String,
    #[serde(default)]
    queries: Vec<QueryCase>,
    #[serde(default)]
    preflight: Option<PathBuf>,
}
fn bounded_json(path: &std::path::Path, cap: usize) -> Result<(Value, Blake3Hash)> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| fail(ErrorCode::Internal, &e.to_string()))?
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| fail(ErrorCode::Internal, &e.to_string()))?;
    if bytes.len() > cap {
        return Err(fail(
            ErrorCode::BudgetExceeded,
            "experimental control/receipt exceeds bound",
        ));
    }
    let hash = Blake3Hash::digest(&bytes);
    Ok((
        serde_json::from_slice(&bytes).map_err(|_| {
            fail(
                ErrorCode::Usage,
                "experimental control/receipt JSON invalid",
            )
        })?,
        hash,
    ))
}
#[test]
#[ignore = "root-owned finite frozen diagnostic; no production dense mode"]
fn frozen_dense_action() {
    let started = Instant::now();
    let config_path =
        PathBuf::from(std::env::var("LWIKI_DENSE_CONFIG").expect("explicit dense config path"));
    let (config, config_hash) = bounded_json(&config_path, 64 * 1024).unwrap();
    let config: Control = serde_json::from_value(config).unwrap();
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&config.output)
        .unwrap();
    let mut diagnostics = Diagnostics::default();
    let mut preflight_hash = None;
    let result = (|| -> Result<Value> {
        let handle = crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&config.vault)?);
        let engine = crate::changes::ChangeEngine::new(handle.clone())?;
        let catalog = Catalog::new(handle, engine.vault_id().clone());
        if config.action == "preflight" {
            if config.queries.len() != 6
                || config
                    .queries
                    .iter()
                    .map(|case| &case.id)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != 6
                || config
                    .queries
                    .iter()
                    .map(|case| &case.query)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != 6
            {
                return Err(fail(
                    ErrorCode::Usage,
                    "exactly six distinct frozen exposed query IDs/texts required",
                ));
            }
            let mut cases = Vec::new();
            for case in &config.queries {
                let mut stats = Diagnostics::default();
                let receipt = run(
                    &catalog,
                    &case.query,
                    &pinned_request(),
                    "S",
                    true,
                    &mut stats,
                    || Ok(()),
                );
                cases.push(
                    json!({"id":case.id,"query":case.query,"receipt":receipt,"diagnostics":stats}),
                );
            }
            let complete = cases
                .iter()
                .all(|case| case["receipt"]["Ok"]["complete"] == true);
            return Ok(json!({"complete":complete,"cases":cases}));
        }
        if config.action != "outcome" || !config.queries.is_empty() {
            return Err(fail(
                ErrorCode::Usage,
                "unknown dense action or outcome query batch",
            ));
        }
        let (receipt, hash) = bounded_json(
            config
                .preflight
                .as_ref()
                .ok_or_else(|| fail(ErrorCode::Usage, "frozen preflight required"))?,
            4 * 1024 * 1024,
        )?;
        preflight_hash = Some(hash);
        if receipt["status"] != "OK"
            || receipt["action"] != "preflight"
            || receipt["payload"]["complete"] != true
        {
            return Err(conflict("unsuccessful frozen preflight"));
        }
        let cases = receipt["payload"]["cases"]
            .as_array()
            .ok_or_else(|| conflict("successful frozen preflight absent"))?;
        let frozen = cases
            .iter()
            .find(|case| case["query"] == config.query)
            .ok_or_else(|| conflict("query absent from frozen preflight"))?;
        let payload = run(
            &catalog,
            &config.query,
            &pinned_request(),
            &config.arm,
            false,
            &mut diagnostics,
            || Ok(()),
        )?;
        if diagnostics
            .normalized_publication
            .as_ref()
            .map(|v| json!(v))
            != Some(frozen["diagnostics"]["normalized_publication"].clone())
            || json!(diagnostics.normalized_snapshot)
                != frozen["diagnostics"]["normalized_snapshot"]
        {
            return Err(conflict(
                "normalized publication differs from frozen preflight",
            ));
        }
        if config.arm != "L"
            && (json!(diagnostics.retained_snapshot_key)
                != frozen["diagnostics"]["retained_snapshot_key"]
                || json!(diagnostics.active_space) != frozen["diagnostics"]["active_space"]
                || json!(diagnostics.query_input_hash) != frozen["diagnostics"]["query_input_hash"]
                || json!(diagnostics.query_vector_hash)
                    != frozen["diagnostics"]["query_vector_hash"])
        {
            return Err(conflict(
                "cached space/query/snapshot differs from frozen preflight",
            ));
        }
        Ok(payload)
    })();
    let status = if result.is_ok()
        && !(config.action == "preflight"
            && result
                .as_ref()
                .ok()
                .is_some_and(|payload| payload["complete"] != true))
    {
        "OK"
    } else {
        "FAILED"
    };
    let payload = result.as_ref().ok().cloned();
    let error = result.as_ref().err().cloned();
    let envelope = json!({"experimental":true,"action":config.action,"arm":config.arm,"query":config.query,"config_hash":config_hash,"preflight_hash":preflight_hash,"request":pinned_request(),"status":status,"payload":payload,"error":error,"diagnostics":diagnostics,"owner_elapsed_ms":started.elapsed().as_secs_f64()*1000.0});
    serde_json::to_writer(&mut output, &envelope).unwrap();
    output.write_all(b"\n").unwrap();
}

#[path = "indexed_documents_dense_tests.rs"]
mod tests;

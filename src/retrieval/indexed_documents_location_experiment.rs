//! Frozen development experiment; location allocation never supplies evidence authority.
use super::*;
use crate::retrieval::{context_selection, context_units, render, vectors};
use std::{cmp::Ordering, io::Write, time::Instant};

const CORE_CAP: usize = 160;
const SCAN_CAP: usize = 4 * 1024 * 1024;
fn invalid(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}

struct Proposal {
    packet: context::Packet,
    cost: usize,
    child: ByteSpan,
}
#[derive(Clone)]
struct Location {
    id: String,
    owner: usize,
    original: ByteSpan,
    effective: ByteSpan,
    parent: Option<ByteSpan>,
    supports: Vec<usize>,
    terms: Vec<usize>,
    clipped: bool,
    passage: ContextPassage,
    parent_passage: Option<ContextPassage>,
}
#[derive(Default, serde::Serialize)]
struct Diagnostics {
    original_proposals: usize,
    original_costs_reused: usize,
    scanned_bytes: usize,
    structural_starts: usize,
    tokenized_bytes: usize,
    term_parser_prefix_bytes: u64,
    core_trials: usize,
    parent_trials: usize,
    inventory: Vec<Value>,
    exclusions: Vec<Value>,
    choices: Vec<Value>,
    trace: Vec<Value>,
}

fn proposals(trace: &[Value]) -> Result<(Vec<Proposal>, Vec<u64>)> {
    let cards = trace_cards(trace)
        .into_iter()
        .map(|card| (card.id.clone(), card))
        .collect::<BTreeMap<_, _>>();
    let rows = trace_stage(trace, "sorted_packets")
        .as_array()
        .ok_or_else(|| invalid("proposal rows absent"))?;
    let mut output = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let card = cards
            .get(&format!("c{i:04}"))
            .ok_or_else(|| invalid("native full-pool card absent"))?;
        let selection = if row["lexical_candidate"].is_null() {
            None
        } else {
            Some(
                serde_json::from_value::<TraceCandidate>(row["lexical_candidate"].clone())
                    .map_err(|_| invalid("lexical trace invalid"))?
                    .into_candidate(),
            )
        };
        let fallback = if row["fallback"].is_null() {
            None
        } else {
            let value = json!({"id":"fallback","title":"","child_span":null,"rendered_bytes":0,"passage":row["fallback"]});
            Some(
                serde_json::from_value::<TraceCard>(value)
                    .map_err(|_| invalid("unit child trace invalid"))?
                    .into_card()
                    .passage,
            )
        };
        let parent = card.passage.span;
        let child = if let Some(score) = row["unit_score"].as_f64() {
            let lineage = trace
                .iter()
                .find(|stage| stage["stage"] == "unit_lineage")
                .and_then(|stage| stage["rows"].as_array())
                .ok_or_else(|| invalid("explicit unit lineage absent"))?;
            let matches = lineage
                .iter()
                .filter(|unit| {
                    unit["path"] == json!(card.passage.locator.path)
                        && unit["hash"] == json!(card.passage.locator.observed_hash)
                        && unit["parent_span"] == json!(parent)
                        && unit["score"].as_f64() == Some(score)
                })
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(invalid("explicit unit lineage ambiguous or absent"));
            }
            let child = serde_json::from_value::<ByteSpan>(matches[0]["child_span"].clone())
                .map_err(|_| invalid("explicit unit child invalid"))?;
            if fallback
                .as_ref()
                .map_or(child != parent, |p| p.span != child)
            {
                return Err(invalid("unit fallback and explicit child disagree"));
            }
            child
        } else {
            parent
        };
        output.push(Proposal {
            cost: card.rendered_bytes,
            child,
            packet: context::Packet {
                passages: vec![card.passage.clone()],
                bundle: None,
                navigation: None,
                key: row["key"]
                    .as_str()
                    .ok_or_else(|| invalid("proposal key absent"))?
                    .into(),
                score: row["score"]
                    .as_f64()
                    .ok_or_else(|| invalid("proposal score absent"))?,
                selection_ordinal: row["ordinal"].as_u64().map(|x| x as usize),
                selection,
                unit_score: row["unit_score"].as_f64(),
                fallback,
                unit_clipped: row["unit_clipped"].as_bool().unwrap_or(false),
            },
        });
    }
    let weights = trace_stage(trace, "candidate_pool")["term_weights"]
        .as_array()
        .ok_or_else(|| invalid("term weights absent"))?
        .iter()
        .map(|v| v.as_u64().ok_or_else(|| invalid("term weight invalid")))
        .collect::<Result<Vec<_>>>()?;
    Ok((output, weights))
}
fn priority(
    supports: &[usize],
    pool: &[Proposal],
    covered: &[bool],
    weights: &[u64],
) -> (f64, String) {
    let total = weights.iter().copied().sum::<u64>().max(1) as f64;
    supports
        .iter()
        .map(|&i| {
            (
                context::packet_utility(
                    &pool[i].packet,
                    covered,
                    weights,
                    total,
                    pool[i].cost,
                    0.0,
                ),
                pool[i].packet.key.clone(),
            )
        })
        .max_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)))
        .expect("nonempty supporting proposals")
}
fn rank(
    a: &Location,
    b: &Location,
    pool: &[Proposal],
    covered: &[bool],
    weights: &[u64],
) -> Ordering {
    let a_score = priority(&a.supports, pool, covered, weights);
    let b_score = priority(&b.supports, pool, covered, weights);
    b_score
        .0
        .total_cmp(&a_score.0)
        .then(a_score.1.cmp(&b_score.1))
        .then(a.id.cmp(&b.id))
}
fn contains(a: ByteSpan, b: ByteSpan) -> bool {
    a.start() <= b.start() && a.end() >= b.end()
}
fn intersects(a: ByteSpan, b: ByteSpan) -> bool {
    a.start() < b.end() && b.start() < a.end()
}
fn supported_span(proposal: &Proposal) -> ByteSpan {
    proposal.child
}
fn preserves(location: &Location, passages: &[ContextPassage]) -> bool {
    let wanted_text = location.passage.text.as_str();
    let quote_matches = |text: &str, reference: ByteSpan| {
        if !contains(reference, location.effective) {
            return false;
        }
        let start = (location.effective.start() - reference.start()) as usize;
        text.get(start..start + wanted_text.len()) == Some(wanted_text)
    };
    match location.passage.citations.first() {
        Some(CitationRef::Source(wanted)) => passages.iter().any(|passage|passage.citations.iter().any(|c|
            matches!(c,CitationRef::Source(actual) if actual.source_id==wanted.source_id
                && actual.source_revision==wanted.source_revision && quote_matches(&passage.text,actual.span)))),
        None => passages.iter().any(|passage|passage.locator==location.passage.locator
            && quote_matches(&passage.text,passage.span)),
        _ => false,
    }
}

fn locations(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &crate::retrieval::HitSet,
    query: &str,
    pool: &[Proposal],
    weights: &[u64],
    meter: &Meter,
    stats: &mut Diagnostics,
) -> Result<Vec<Location>> {
    let mut output = Vec::new();
    let mut identities = BTreeSet::new();
    let empty = vec![false; weights.len()];
    for (owner, hit) in hits.hits.iter().enumerate() {
        meter.check()?;
        let document = reader
            .document(&hit.locator.path)?
            .ok_or_else(|| invalid("location owner absent"))?;
        let allowance = (1024 * 1024).min(SCAN_CAP.saturating_sub(stats.scanned_bytes));
        let (blocks, starts, scanned, usable, limited) = context_units::location_blocks_for_test(
            &document,
            allowance,
            4096usize.saturating_sub(stats.structural_starts),
        )?;
        stats.scanned_bytes += scanned;
        stats.structural_starts += starts;
        if limited || usable < document.raw_text.len() {
            stats.exclusions.push(
                json!({"owner":owner,"reason":"location_structural_scan_cap","usable_end":usable}),
            );
        }
        let mut pending = blocks.into_iter().peekable();
        let mut atoms = Vec::new();
        while let Some(mut block) = pending.next() {
            if block.kind == "prose"
                && pending
                    .peek()
                    .is_some_and(|next| matches!(next.kind, "code" | "list"))
            {
                let next = pending.next().unwrap();
                block.span = ByteSpan::new(block.span.start(), next.span.end())?;
                block.kind = next.kind;
                block.complete &= next.complete;
            }
            atoms.push(block);
        }
        for atom in atoms {
            if atom.kind == "heading" {
                continue;
            }
            let id = serde_json::to_string(&(
                reader.vault_id(),
                &hit.locator,
                &hit.source_id,
                &hit.owner_revision,
                atom.span,
            ))
            .map_err(|_| invalid("location identity encoding"))?;
            let mut supports = (0..pool.len())
                .filter(|&i| {
                    pool[i].packet.passages[0].locator.path == document.path
                        && intersects(supported_span(&pool[i]), atom.span)
                })
                .collect::<Vec<_>>();
            if supports.is_empty() {
                continue;
            }
            supports.sort_by(|&a, &b| {
                let a_score = priority(&[a], pool, &empty, weights);
                let b_score = priority(&[b], pool, &empty, weights);
                b_score
                    .0
                    .total_cmp(&a_score.0)
                    .then(a_score.1.cmp(&b_score.1))
            });
            if !atom.complete {
                stats.exclusions.push(
                    json!({"id":id,"reason":"location_incomplete_structure","supports":supports}),
                );
                continue;
            }
            let mut clipped = false;
            let effective = if atom.span.len() as usize <= request.documents.limits.excerpt_bytes {
                atom.span
            } else {
                if atom.kind != "prose" {
                    stats.exclusions.push(json!({"id":id,"reason":"location_indivisible_core_exceeds_excerpt_bound","supports":supports}));
                    continue;
                }
                let window = supports
                    .iter()
                    .filter(|&&i| {
                        pool[i]
                            .packet
                            .selection
                            .as_ref()
                            .map_or(pool[i].packet.unit_clipped, |c| c.clipped)
                    })
                    .map(|&i| supported_span(&pool[i]))
                    .find(|&span| contains(atom.span, span));
                let Some(window) = window else {
                    stats.exclusions.push(json!({"id":id,"reason":"location_no_frozen_clipped_window","supports":supports}));
                    continue;
                };
                clipped = true;
                window
            };
            let parent = if clipped {
                None
            } else {
                supports
                    .iter()
                    .filter_map(|&i| {
                        let proposal = &pool[i];
                        let span = proposal.packet.passages[0].span;
                        let complete = proposal
                            .packet
                            .selection
                            .as_ref()
                            .map_or(!proposal.packet.unit_clipped, |x| !x.clipped);
                        (complete
                            && contains(span, effective)
                            && span != effective
                            && span.len() as usize <= request.documents.limits.excerpt_bytes)
                            .then_some(span)
                    })
                    .next()
            };
            let passage =
                context::passage_for_span_for_test(reader, request, hit, effective, owner + 1)?
                    .ok_or_else(|| invalid("core not eligible"))?;
            let parent_passage = parent
                .map(|span| {
                    context::passage_for_span_for_test(reader, request, hit, span, owner + 1)
                })
                .transpose()?
                .flatten();
            if stats
                .tokenized_bytes
                .saturating_add(effective.len() as usize)
                > SCAN_CAP
            {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "location tokenization cap",
                ));
            }
            meter.check()?;
            stats.term_parser_prefix_bytes += effective.end();
            let terms = context_selection::location_terms_for_test(
                reader,
                query,
                &document.raw_text,
                &[effective],
            )?
            .remove(0);
            stats.tokenized_bytes += effective.len() as usize;
            meter.check()?;
            let location = Location {
                id: id.clone(),
                owner,
                original: atom.span,
                effective,
                parent,
                supports,
                terms,
                clipped,
                passage,
                parent_passage,
            };
            stats.inventory.push(json!({"id":id,"owner":owner,"original":location.original,"effective":effective,"parent":parent,"supports":location.supports,"terms":location.terms,"clipped":clipped}));
            if !identities.insert(id) {
                return Err(invalid("duplicate canonical identity"));
            }
            output.push(location);
        }
        for proposal in pool
            .iter()
            .filter(|p| p.packet.passages[0].locator.path == document.path)
        {
            if supported_span(proposal).end() > usable as u64 {
                stats.exclusions.push(json!({"proposal":proposal.packet.key,"reason":"location_outside_structural_inventory"}));
            }
        }
        meter.check()?;
    }
    output.sort_by(|a, b| rank(a, b, pool, &empty, weights));
    let mut counts = BTreeMap::new();
    let mut retained = Vec::new();
    for location in output {
        let count = counts.entry(location.owner).or_insert(0usize);
        if *count == 32 || retained.len() == CORE_CAP {
            stats.exclusions.push(json!({"id":location.id,"supports":location.supports,"reason":"location_candidate_cap"}));
            continue;
        }
        *count += 1;
        retained.push(location);
    }
    Ok(retained)
}

fn allocate(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &crate::retrieval::HitSet,
    query: &str,
    prepared: context::ContextDraft,
    trace: &[Value],
    meter: &Meter,
    stats: &mut Diagnostics,
    oracle: Option<&Value>,
) -> Result<context::ContextDraft> {
    let (pool, weights) = proposals(trace)?;
    stats.original_proposals = pool.len();
    stats.original_costs_reused = pool.len();
    if pool.len() > request.documents.limits.hits * 32 {
        return Err(invalid("native proposal cap breached"));
    }
    let cores = locations(reader, request, hits, query, &pool, &weights, meter, stats)?;
    let binding = json!({"query":query,"request":request,"snapshot":reader.snapshot(),"dependency_fingerprint":hits.dependency_fingerprint,"inventory":stats.inventory});
    let inventory_hash = Blake3Hash::digest(
        serde_json::to_vec(&binding).map_err(|_| invalid("inventory serialization"))?,
    );
    stats
        .trace
        .push(json!({"stage":"inventory_binding","binding":binding,"hash":inventory_hash}));
    let chosen = if let Some(oracle) = oracle {
        if oracle["inventory_hash"] != json!(inventory_hash) {
            return Err(invalid("oracle inventory binding differs"));
        }
        let ids = oracle["ids"]
            .as_array()
            .ok_or_else(|| invalid("oracle IDs absent"))?;
        if ids.len() > 20 {
            return Err(invalid("oracle ID cap"));
        }
        let mut seen = BTreeSet::new();
        let mut selected = Vec::new();
        for id in ids {
            let id = id.as_str().ok_or_else(|| invalid("oracle ID invalid"))?;
            if !seen.insert(id) {
                return Err(invalid("duplicate oracle ID"));
            }
            selected.push(
                cores
                    .iter()
                    .position(|c| c.id == id)
                    .ok_or_else(|| invalid("unknown oracle ID"))?,
            );
        }
        Some(selected)
    } else {
        None
    };
    let mut pending = chosen.unwrap_or_else(|| (0..cores.len()).collect());
    let mut admitted: Vec<usize> = Vec::new();
    let mut passages = Vec::new();
    let mut covered = vec![false; weights.len()];
    let mut omissions = prepared.omissions.clone();
    let scope = context::render_documents_for_test(reader, request, &[])?;
    if scope.len()
        > request.budget.max_bytes - request.budget.instruction_bytes - request.budget.output_bytes
        || scope.len().div_ceil(4)
            > request.budget.max_tokens
                - request.budget.instruction_tokens
                - request.budget.output_tokens
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "scope header does not fit",
        ));
    }
    while !pending.is_empty() {
        meter.check()?;
        let best = if oracle.is_some() {
            0
        } else {
            (0..pending.len())
                .min_by(|&a, &b| {
                    rank(
                        &cores[pending[a]],
                        &cores[pending[b]],
                        &pool,
                        &covered,
                        &weights,
                    )
                })
                .unwrap()
        };
        let index = pending.remove(best);
        let core = &cores[index];
        stats.core_trials += 1;
        let score = priority(&core.supports, &pool, &covered, &weights);
        let trial = context::admit_document_for_test(reader, request, &passages, &core.passage)?;
        match trial {
            Ok((next, _)) => {
                if admitted.iter().any(|&i| !preserves(&cores[i], &next)) || !preserves(core, &next)
                {
                    return Err(invalid("core admission lost an identity"));
                }
                for &term in &core.terms {
                    if let Some(value) = covered.get_mut(term) {
                        *value = true;
                    }
                }
                passages = next;
                admitted.push(index);
                stats.choices.push(json!({"stage":"core","id":core.id,"status":"admitted","priority":score,"covered_terms":covered}));
            }
            Err(reason) => {
                omissions.push(ContextOmission {
                    record_id: core
                        .passage
                        .locator
                        .record
                        .as_ref()
                        .map(|r| r.record_id.clone()),
                    path: Some(core.passage.locator.path.clone()),
                    reason: reason.into(),
                    count: 1,
                });
                stats.choices.push(json!({"stage":"core","id":core.id,"status":"excluded","reason":reason,"priority":score}));
            }
        }
        meter.check()?;
    }
    // Successful core admission order is frozen. No re-ranking after expansion.
    for &index in &admitted {
        let core = &cores[index];
        let Some(parent) = &core.parent_passage else {
            continue;
        };
        meter.check()?;
        stats.parent_trials += 1;
        match context::admit_document_for_test(reader, request, &passages, parent)? {
            Ok((next, _)) => {
                if admitted.iter().any(|&i| !preserves(&cores[i], &next)) {
                    return Err(invalid("parent expansion lost an admitted identity"));
                }
                passages = next;
                stats.choices.push(
                    json!({"stage":"parent","id":core.id,"status":"expanded","span":core.parent}),
                );
            }
            Err(reason) => stats
                .choices
                .push(json!({"stage":"parent","id":core.id,"status":"rejected","reason":reason})),
        }
        meter.check()?;
    }
    if stats.core_trials > 160 || stats.parent_trials > 160 {
        return Err(invalid("incremental trial cap breached"));
    }
    let text = context::render_documents_for_test(reader, request, &passages)?;
    meter.check()?;
    let quote_bytes = passages.iter().map(|p| p.text.len()).sum::<usize>();
    stats.trace.push(json!({"stage":"final","admitted":admitted.iter().map(|&i|&cores[i].id).collect::<Vec<_>>(),"quote_bytes":quote_bytes,"header_reference_scope_bytes":text.len()-quote_bytes}));
    for excluded in &stats.exclusions {
        omissions.push(ContextOmission {
            record_id: None,
            path: None,
            reason: excluded["reason"]
                .as_str()
                .unwrap_or("location_excluded")
                .into(),
            count: 1,
        });
    }
    let truncated = hits.truncated || !omissions.is_empty();
    let mut warnings = prepared.warnings.clone();
    warnings.push("test-only core-first original-location allocation; selected proof is not a completeness claim".into());
    if admitted.iter().any(|&i| cores[i].clipped) {
        warnings.push("location allocation retained a frozen clipped prose window; complete atom coverage is not claimed".into());
    }
    Ok(context::ContextDraft {
        usage: ContextUsage {
            rendered_bytes: text.len(),
            estimated_tokens: text.len().div_ceil(4),
            token_accounting: TokenAccounting::EstimatedUtf8BytesDiv4Ceil,
            reserved_bytes: request.budget.instruction_bytes + request.budget.output_bytes,
            reserved_tokens: request.budget.instruction_tokens + request.budget.output_tokens,
            graph_bytes: 0,
            graph_estimated_tokens: 0,
            verification_bytes: 0,
            verification_files: 0,
            verification_entries: 0,
        },
        text,
        passages,
        bundles: vec![],
        omissions,
        snapshot: reader.snapshot().clone(),
        dependency_fingerprint: hits.dependency_fingerprint.clone(),
        truncated,
        warnings,
        selection_packet: None,
    })
}

fn vector_hash(vector: &[f32]) -> Blake3Hash {
    Blake3Hash::digest(
        vector
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )
}
fn cache_preflight(
    cache: &VaultFs,
    reader: &SelectedCatalog<'_>,
    request: &ContextRequest,
    hits: &crate::retrieval::HitSet,
    query: &str,
    meter: &Meter,
) -> Result<Value> {
    let store = vectors::VectorStore::open(cache, None)?;
    let state = store.active()?.ok_or_else(|| {
        WikiError::new(ErrorCode::OfflineUnavailable, "active cache space absent")
    })?;
    if state.spec.id()? != state.id || !state.active {
        return Err(invalid("active space identity differs"));
    }
    let dimensions = state
        .actual_dimensions
        .ok_or_else(|| invalid("active dimensions absent"))? as usize;
    let input = state.spec.query(query)?;
    let query_vector = store.vector(&state.id, &input.input_hash)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::OfflineUnavailable,
            "query vector missing or corrupt",
        )
    })?;
    if query_vector.len() != dimensions {
        return Err(invalid("query dimension differs"));
    }
    let mut bytes = 0usize;
    let mut units = 0usize;
    let mut vector_bytes = dimensions * 4;
    let mut missing = 0usize;
    let mut capped = false;
    let mut bindings = Vec::new();
    let mut owners = Vec::new();
    let mut covered_owners = BTreeSet::new();
    for hit in &hits.hits {
        meter.check()?;
        let document = reader
            .document(&hit.locator.path)?
            .ok_or_else(|| invalid("preflight owner absent"))?;
        owners.push(json!({"path":document.path,"hash":document.hash,"source":document.source_id,"revision":document.owner_revision}));
        if document.raw_text.len() > 1024 * 1024 || bytes + document.raw_text.len() > SCAN_CAP {
            capped = true;
            continue;
        }
        bytes += document.raw_text.len();
        for unit in render::render_selected_document_for_test(
            &document,
            &state.spec.settings,
            reader.proof.fingerprint.clone(),
        )? {
            meter.check()?;
            if units == 4096 || vector_bytes.saturating_add(dimensions * 4) > 64 * 1024 * 1024 {
                capped = true;
                break;
            }
            units += 1;
            vector_bytes += dimensions * 4;
            let unit = unit?;
            let Some(span) = unit.source_span.filter(|s| !s.is_empty()) else {
                continue;
            };
            let vector = store.vector(&state.id, &unit.input_hash)?;
            let (hash, cosine) = if let Some(vector) = vector {
                if vector.len() != dimensions {
                    return Err(invalid("unit dimension differs"));
                }
                (
                    Some(vector_hash(&vector)),
                    Some(vectors::cosine(&query_vector, &vector)?),
                )
            } else {
                missing += 1;
                (None, None)
            };
            if hash.is_some() && cosine.is_some() {
                covered_owners.insert(unit.owner.clone());
            }
            bindings.push(json!({"owner":unit.owner,"observed_hash":unit.source_hash,"span":span,"unit_id":unit.unit_id,"input_hash":unit.input_hash,"vector_hash":hash,"cosine":cosine}));
        }
    }
    meter.check()?;
    Ok(
        json!({"complete":!capped&&missing==0&&!hits.hits.is_empty()&&covered_owners.len()==hits.hits.len(),"capped":capped,"missing_or_corrupt":missing,
        "query":query,"request":request,"snapshot":reader.snapshot(),"owners":owners,"units":bindings,
        "source_bytes":bytes,"rendered_units":units,"reserved_vector_bytes":vector_bytes,
        "space_id":state.id,"space_spec":state.spec,"dimensions":dimensions,"query_input_hash":input.input_hash,
        "query_vector_hash":vector_hash(&query_vector),"scope":"selected-owner cached localization only; no membership authority"}),
    )
}
fn signals_from_frozen(
    value: &Value,
    reader: &SelectedCatalog<'_>,
    request: &ContextRequest,
    hits: &crate::retrieval::HitSet,
    query: &str,
) -> Result<ContextSelectionSignals> {
    if value["complete"] != true
        || value["query"] != query
        || value["request"] != json!(request)
        || value["snapshot"] != json!(reader.snapshot())
    {
        return Err(invalid("semantic preflight binding unavailable or changed"));
    }
    let mut signals = ContextSelectionSignals::default();
    let owners = value["owners"]
        .as_array()
        .ok_or_else(|| invalid("preflight owners absent"))?;
    if owners.len() != hits.hits.len() {
        return Err(invalid("preflight owner set differs"));
    }
    for (hit, owner) in hits.hits.iter().zip(owners) {
        if owner["path"] != json!(hit.locator.path)
            || owner["hash"] != json!(hit.locator.observed_hash)
            || owner["source"] != json!(hit.source_id)
            || owner["revision"] != json!(hit.owner_revision)
        {
            return Err(invalid("preflight source binding differs"));
        }
    }
    let units = value["units"]
        .as_array()
        .ok_or_else(|| invalid("preflight units absent"))?;
    if units.is_empty() || units.len() > 4096 {
        return Err(invalid("preflight unit cap"));
    }
    let mut covered_owners = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for unit in units {
        let owner = VaultRelativePath::new(
            unit["owner"]
                .as_str()
                .ok_or_else(|| invalid("unit owner invalid"))?,
        )?;
        let hash = Blake3Hash::new(
            unit["observed_hash"]
                .as_str()
                .ok_or_else(|| invalid("unit hash invalid"))?,
        )?;
        let span = serde_json::from_value::<ByteSpan>(unit["span"].clone())
            .map_err(|_| invalid("unit span invalid"))?;
        let cosine = unit["cosine"]
            .as_f64()
            .filter(|v| v.is_finite() && (-1.0..=1.0).contains(v))
            .ok_or_else(|| invalid("unit cosine invalid"))?;
        for key in ["vector_hash", "input_hash", "unit_id"] {
            Blake3Hash::new(
                unit[key]
                    .as_str()
                    .ok_or_else(|| invalid("unit payload pin absent"))?,
            )?;
        }
        if span.is_empty() || !seen.insert((owner.clone(), span.start(), span.end())) {
            return Err(invalid("empty or duplicate semantic unit"));
        }
        covered_owners.insert(owner.clone());
        let document = reader
            .document(&owner)?
            .ok_or_else(|| invalid("semantic cue escaped selected catalog"))?;
        if document.hash != hash {
            return Err(invalid("semantic cue source changed"));
        }
        span.slice(&document.raw_text)?;
        signals.semantic.push(ContextSemanticCue {
            owner,
            observed_hash: hash,
            span,
            cosine,
        });
    }
    let expected = hits
        .hits
        .iter()
        .map(|hit| hit.locator.path.clone())
        .collect::<BTreeSet<_>>();
    if covered_owners != expected {
        return Err(invalid(
            "semantic cue receipt does not cover every selected owner",
        ));
    }
    signals.semantic_complete = true;
    signals.warnings.push("test-only frozen selected-owner cached cues; no global semantic discovery or membership claim".into());
    Ok(signals)
}

fn run_case(
    catalog: &Catalog,
    query: &str,
    request: &ContextRequest,
    arm: &str,
    frozen: Option<&Value>,
    cache: Option<&VaultFs>,
    oracle: Option<&Value>,
    stats: &mut Diagnostics,
) -> Result<Value> {
    let meter = Meter::new(&request.verification_budget);
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &SelectionAction::Prepare)?;
    if request.scope != ContextScope::IndexedDocuments || catalog.operation_state()?.is_none() {
        return Err(invalid(
            "location experiment requires normalized indexed documents",
        ));
    }
    meter.check()?;
    catalog.guard_query()?;
    meter.check()?;
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: meter.remaining_ms(),
        ..Default::default()
    })?;
    let mut hits = lexical::search_context_catalog(&reader, query, &request.documents, false)?;
    meter.check()?;
    let paths = hits
        .hits
        .iter()
        .map(|hit| hit.locator.path.clone())
        .collect::<Vec<_>>();
    let budget = VerificationBudget {
        max_elapsed_ms: meter.remaining_ms(),
        ..request.verification_budget.clone()
    };
    let mut proof = selected_documents::authenticate(catalog, &reader, &paths, &budget)?;
    hits.dependency_fingerprint = proof.fingerprint.clone();
    let selected = SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let mut draft_to_seal = None;
    let payload = if arm == "preflight" {
        match cache_preflight(
            cache.ok_or_else(|| invalid("cache fixture absent"))?,
            &selected,
            &request,
            &hits,
            query,
            &meter,
        ) {
            Ok(value) => value,
            Err(error) => {
                json!({"complete":false,"error":error,"query":query,"request":request,"snapshot":reader.snapshot()})
            }
        }
    } else {
        let signals = if matches!(arm, "S" | "LS") {
            signals_from_frozen(
                frozen.ok_or_else(|| invalid("preflight receipt absent"))?,
                &selected,
                &request,
                &hits,
                query,
            )?
        } else {
            ContextSelectionSignals::default()
        };
        let draft = if matches!(arm, "B" | "S") {
            context::assemble_bounded_documents_with_signals_for_test(
                &selected,
                &request,
                &hits,
                query,
                &signals,
                &SelectionAction::Automatic,
            )?
        } else {
            let (prepared, trace) = context::with_candidate_ordering_trace(|| {
                context::assemble_bounded_documents_with_signals_for_test(
                    &selected,
                    &request,
                    &hits,
                    query,
                    &signals,
                    &SelectionAction::Prepare,
                )
            });
            stats.trace = trace.clone();
            allocate(
                &selected, &request, &hits, query, prepared?, &trace, &meter, stats, oracle,
            )?
        };
        meter.check()?;
        draft_to_seal = Some(draft);
        Value::Null
    };
    // Finish the real dependency recheck even for diagnostic preflight receipts.
    proof.recheck(catalog, &reader)?;
    let verification = verification(&reader)?;
    meter.check()?;
    let payload = if let Some(draft) = draft_to_seal {
        json!(seal(draft, verification.clone(), proof.meter()))
    } else {
        payload
    };
    Ok(
        json!({"payload":payload,"verification":verification,"proof_work":proof.meter().work(),"dependency_fingerprint":proof.fingerprint,"catalog_rows":reader.usage().rows,"catalog_bytes":reader.usage().bytes}),
    )
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    vault: PathBuf,
    cases: PathBuf,
    vector_vault: PathBuf,
    output: PathBuf,
    action: String,
    case_id: String,
    #[serde(default)]
    arm: String,
    #[serde(default)]
    profile: String,
    requests: BTreeMap<String, ContextRequest>,
    #[serde(default)]
    preflights: Vec<PathBuf>,
    #[serde(default)]
    oracle: Option<PathBuf>,
}
fn pinned_request(profile: &str) -> ContextRequest {
    let mut request = ContextRequest {
        scope: ContextScope::IndexedDocuments,
        ..Default::default()
    };
    request.documents.limits.candidates = 80;
    request.documents.limits.excerpt_bytes = 1024;
    if profile == "high" {
        request.documents.limits.hits = 5;
        request.budget.max_bytes = 6000;
        request.budget.max_tokens = 1500;
        request.verification_budget.max_entries = 65536;
        request.verification_budget.max_elapsed_ms = 5000;
    } else {
        request.documents.limits.hits = 10;
    }
    request
}
fn check_requests(config: &Control) -> Result<()> {
    if config.requests.len() != 2 {
        return Err(invalid("two pinned request profiles required"));
    }
    for profile in ["high", "default"] {
        let actual = context::validate_request(
            QUERY,
            config
                .requests
                .get(profile)
                .ok_or_else(|| invalid("profile absent"))?,
        )?;
        let expected = context::validate_request(QUERY, &pinned_request(profile))?;
        if actual != expected {
            return Err(invalid(
                "full normalized request differs from frozen profile",
            ));
        }
    }
    Ok(())
}

fn frozen_profiles(config: &Control, cases: &[PilotCase]) -> Result<Value> {
    if config.preflights.len() != 6 {
        return Err(invalid("all six preflights required before outcomes"));
    }
    let mut seen = BTreeSet::new();
    let mut selected = None;
    for path in &config.preflights {
        let value = bounded_json(path, 16 * 1024 * 1024);
        let case = value["case"]
            .as_str()
            .ok_or_else(|| invalid("preflight case absent"))?;
        if value["action"] != "preflight"
            || !seen.insert(case.to_owned())
            || !cases
                .iter()
                .any(|c| c.id == case && value["query"] == c.query)
        {
            return Err(invalid("preflight case/query set differs"));
        }
        for profile in ["high", "default"] {
            if !value["result"]["Ok"]["profiles"][profile]["result"].is_object() {
                return Err(invalid("preflight must retain both profile results"));
            }
        }
        if case == config.case_id {
            selected = Some(value["result"]["Ok"].clone());
        }
    }
    selected
        .filter(|v| v.is_object())
        .ok_or_else(|| invalid("selected case preflight action failed"))
}

#[test]
#[ignore = "one explicit frozen action only; root serializes preflights before outcomes"]
fn frozen_location_action() {
    let started = Instant::now();
    let config: Control = serde_json::from_value(bounded_json(
        &PathBuf::from(std::env::var("LWIKI_LOCATION_CONFIG").expect("explicit config path")),
        64 * 1024,
    ))
    .unwrap();
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config.output)
        .unwrap();
    let mut query_value = Value::Null;
    let result = (|| -> Result<Value> {
        check_requests(&config)?;
        if config.action == "operational" {
            return Err(invalid(
                "six grouped operational fixtures are checkpoint tests; do not replay them as extra development calls",
            ));
        }
        let cases: Vec<PilotCase> = serde_json::from_value(bounded_json(&config.cases, 32 * 1024))
            .map_err(|_| invalid("six cases invalid"))?;
        if cases.len() != 6 || cases.iter().map(|c| &c.id).collect::<BTreeSet<_>>().len() != 6 {
            return Err(invalid("six unique fixed cases required"));
        }
        let repo = crate::test_paths::fixture(
            env!("CARGO_MANIFEST_DIR"),
            ".artifacts/context-development-first-loss-001/manifest.json",
        );
        let allowed = bounded_json(&repo, 128 * 1024);
        if cases.iter().any(|case| {
            !allowed["cases"].as_array().is_some_and(|values| {
                values
                    .iter()
                    .any(|v| v["id"] == case.id && v["query"] == case.query)
            })
        }) {
            return Err(invalid("query differs from exposed development manifest"));
        }
        let case = cases
            .iter()
            .find(|case| case.id == config.case_id)
            .ok_or_else(|| invalid("unknown case"))?;
        query_value = json!(case.query);
        let handle = VaultFs::new(VaultRoot::explicit(&config.vault)?);
        let engine = crate::changes::ChangeEngine::new(handle.clone())?;
        let catalog = Catalog::new(handle, engine.vault_id().clone());
        if config.action == "preflight" {
            let cache = VaultFs::new(VaultRoot::explicit(&config.vector_vault)?);
            let mut profiles = BTreeMap::new();
            for profile in ["high", "default"] {
                let mut stats = Diagnostics::default();
                let profile_started = Instant::now();
                let result = run_case(
                    &catalog,
                    &case.query,
                    &config.requests[profile],
                    "preflight",
                    None,
                    Some(&cache),
                    None,
                    &mut stats,
                );
                let elapsed_ms = profile_started.elapsed().as_secs_f64() * 1000.0;
                profiles.insert(
                    profile,
                    json!({"result":result,"diagnostics":stats,"elapsed_ms":elapsed_ms}),
                );
            }
            return Ok(
                json!({"profiles":profiles,"cache_path":config.vector_vault.join(".wiki/cache/embeddings.sqlite3")}),
            );
        }
        if !matches!(config.action.as_str(), "outcome" | "inventory" | "oracle")
            || !matches!(config.arm.as_str(), "B" | "L" | "S" | "LS")
        {
            return Err(invalid("unknown finite action/arm"));
        }
        if config.action != "oracle" && config.oracle.is_some() {
            return Err(invalid("oracle cannot enter automatic allocation"));
        }
        let profiles = frozen_profiles(&config, &cases)?;
        let request = config
            .requests
            .get(&config.profile)
            .ok_or_else(|| invalid("unknown profile"))?;
        let frozen = &profiles["profiles"][&config.profile]["result"]["Ok"]["payload"];
        if matches!(config.arm.as_str(), "S" | "LS") && frozen["complete"] != true {
            return Ok(
                json!({"status":"UNRUN","reason":"exact selected-unit profile coverage unavailable","preflight":frozen}),
            );
        }
        if config.action == "inventory" {
            return Ok(
                json!({"status":"NO_OUTCOME","preflight":profiles,"note":"core inventory is emitted by the existing L/LS outcome; no hidden extra context call"}),
            );
        }
        let oracle = config
            .oracle
            .as_ref()
            .map(|path| bounded_json(path, 128 * 1024));
        if config.action == "oracle"
            && (!matches!(config.arm.as_str(), "L" | "LS")
                || oracle.is_none()
                || config.case_id == "q23")
        {
            return Err(invalid("oracle action requires one positive L/LS ID reply"));
        }
        let cap = if config.profile == "high" { 643 } else { 963 };
        let mut stats = Diagnostics::default();
        let semantic = matches!(config.arm.as_str(), "S" | "LS");
        let preflight_elapsed_ms = if semantic {
            Some(
                profiles["profiles"][&config.profile]["elapsed_ms"]
                    .as_f64()
                    .filter(|ms| ms.is_finite() && *ms >= 0.0)
                    .ok_or_else(|| invalid("preflight profile coordinator time absent"))?,
            )
        } else {
            None
        };
        let mut arm_elapsed_ms = 0.0;
        let (result, counts) = context::with_context_render_counts_for_test(cap, || {
            let arm_started = Instant::now();
            let result = run_case(
                &catalog,
                &case.query,
                request,
                &config.arm,
                Some(frozen),
                None,
                oracle.as_ref(),
                &mut stats,
            );
            arm_elapsed_ms = arm_started.elapsed().as_secs_f64() * 1000.0;
            result
        });
        let composed_elapsed_ms = preflight_elapsed_ms.map(|ms| ms + arm_elapsed_ms);
        let composed_budget_ok =
            composed_elapsed_ms.map(|ms| ms <= request.verification_budget.max_elapsed_ms as f64);
        Ok(
            json!({"result":result,"diagnostics":stats,"native_context_renders":counts.calls,"native_context_rendered_bytes":counts.bytes,"render_cap":cap,"cached_cues_used":semantic,"arm_elapsed_ms":arm_elapsed_ms,"preflight_profile_elapsed_ms":preflight_elapsed_ms,"composed_elapsed_ms":composed_elapsed_ms,"composed_budget_ok":composed_budget_ok,"timing_scope":"S/LS composed time is a conservative noncontiguous diagnostic with duplicate proof overhead; no shipping latency or semantic speedup claim"}),
        )
    })();
    let value = json!({"action":config.action,"case":config.case_id,"query":query_value,"profile":config.profile,"arm":config.arm,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"result":result});
    serde_json::to_writer_pretty(&mut output, &value).unwrap();
    writeln!(output).unwrap();
    output.sync_all().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "action output retained before 30-second assertion"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selected<T>(
        fixture: &Fixture,
        request: &ContextRequest,
        f: impl FnOnce(&SelectedCatalog<'_>, &crate::retrieval::HitSet) -> Result<T>,
    ) -> Result<T> {
        let reader = fixture.catalog.cached_query_snapshot(Default::default())?;
        let mut hits = lexical::search_context_catalog(&reader, QUERY, &request.documents, false)?;
        let paths = hits
            .hits
            .iter()
            .map(|h| h.locator.path.clone())
            .collect::<Vec<_>>();
        let mut proof = selected_documents::authenticate(
            &fixture.catalog,
            &reader,
            &paths,
            &request.verification_budget,
        )?;
        hits.dependency_fingerprint = proof.fingerprint.clone();
        let result = f(
            &SelectedCatalog {
                reader: &reader,
                proof: &proof,
            },
            &hits,
        )?;
        proof.recheck(&fixture.catalog, &reader)?;
        Ok(result)
    }
    fn location(passage: ContextPassage) -> Location {
        Location {
            id: "fixture".into(),
            owner: 0,
            original: passage.span,
            effective: passage.span,
            parent: None,
            supports: vec![0],
            terms: vec![],
            clipped: false,
            passage,
            parent_passage: None,
        }
    }
    fn authored_body(fixture: &Fixture, body: &str) {
        fs::write(
            fixture.catalog.fs().root().path().join("page.md"),
            note(
                "page",
                "page_host",
                json!({"wiki_status":"reviewed","wiki_depends_on_ids":["assertion_host"]}),
                body.as_bytes(),
            ),
        )
        .unwrap();
        fixture.republish();
    }
    fn compare_native(
        reader: &dyn QueryCatalog,
        request: &ContextRequest,
        hits: &crate::retrieval::HitSet,
        current: &[ContextPassage],
        candidate: &ContextPassage,
    ) -> Result<bool> {
        let trial = context::admit_document_for_test(reader, request, current, candidate)?;
        let packets = current
            .iter()
            .chain(std::iter::once(candidate))
            .enumerate()
            .map(|(i, p)| context::Packet {
                passages: vec![p.clone()],
                bundle: None,
                navigation: None,
                key: format!("p{i:04}"),
                score: 0.0,
                selection_ordinal: Some(i),
                selection: None,
                unit_score: Some(100.0 - i as f64),
                fallback: None,
                unit_clipped: false,
            })
            .collect();
        let native = context::pack(
            reader,
            request,
            context::PackingInput {
                packets,
                omissions: vec![],
                term_weights: vec![],
                selection_warnings: vec![],
                source_aware: true,
                query: Some(QUERY),
                signals: &ContextSelectionSignals::default(),
                selection_action: &SelectionAction::Automatic,
                hits,
                graph: None,
                dependency_fingerprint: hits.dependency_fingerprint.clone(),
            },
        )?;
        match trial {
            Ok((passages, text)) => {
                assert!(native.omissions().is_empty());
                assert_eq!(native.passages(), passages);
                assert_eq!(native.text(), text);
                Ok(true)
            }
            Err(reason) => {
                assert_eq!(native.passages(), current);
                assert_eq!(native.omissions().last().unwrap().reason, reason);
                assert_eq!(
                    native.text(),
                    context::render_documents_for_test(reader, request, current)?
                );
                Ok(false)
            }
        }
    }
    #[test]
    fn location_incremental_matches_native_and_exact_budget() {
        let fixture = Fixture::new();
        let request = request();
        selected(&fixture, &request, |reader, hits| {
            let passages = hits
                .hits
                .iter()
                .take(2)
                .map(|hit| {
                    context::passage_for_span_for_test(reader, &request, hit, hit.excerpt.span, 1)?
                        .ok_or_else(|| invalid("fixture passage absent"))
                })
                .collect::<Result<Vec<_>>>()?;
            assert_eq!(passages.len(), 2);
            let mut admitted = Vec::new();
            let mut rendered = String::new();
            for passage in &passages {
                let trial = context::admit_document_for_test(reader, &request, &admitted, passage)?
                    .unwrap();
                admitted = trial.0;
                rendered = trial.1;
            }
            let packets = passages
                .iter()
                .enumerate()
                .map(|(i, p)| context::Packet {
                    passages: vec![p.clone()],
                    bundle: None,
                    navigation: None,
                    key: format!("{i}"),
                    score: 0.0,
                    selection_ordinal: Some(i),
                    selection: None,
                    unit_score: Some(10.0 - i as f64),
                    fallback: None,
                    unit_clipped: false,
                })
                .collect();
            let native = context::pack(
                reader,
                &request,
                context::PackingInput {
                    packets,
                    omissions: vec![],
                    term_weights: vec![],
                    selection_warnings: vec![],
                    source_aware: true,
                    query: Some(QUERY),
                    signals: &ContextSelectionSignals::default(),
                    selection_action: &SelectionAction::Automatic,
                    hits,
                    graph: None,
                    dependency_fingerprint: hits.dependency_fingerprint.clone(),
                },
            )?;
            assert_eq!(native.text(), rendered);
            assert_eq!(native.passages(), admitted);
            let mut exact = request.clone();
            exact.budget.max_bytes = rendered.len() + 11;
            exact.budget.instruction_bytes = 7;
            exact.budget.output_bytes = 4;
            assert!(
                context::admit_document_for_test(reader, &exact, &admitted[..1], &passages[1])?
                    .is_ok()
            );
            assert!(compare_native(
                reader,
                &exact,
                hits,
                &admitted[..1],
                &passages[1]
            )?);
            exact.budget.max_bytes -= 1;
            assert!(
                context::admit_document_for_test(reader, &exact, &admitted[..1], &passages[1])?
                    .is_err()
            );
            assert!(!compare_native(
                reader,
                &exact,
                hits,
                &admitted[..1],
                &passages[1]
            )?);
            let mut tokens = request.clone();
            tokens.budget.max_tokens = rendered.len().div_ceil(4) + 5;
            tokens.budget.instruction_tokens = 3;
            tokens.budget.output_tokens = 2;
            assert!(compare_native(
                reader,
                &tokens,
                hits,
                &admitted[..1],
                &passages[1]
            )?);
            tokens.budget.max_tokens -= 1;
            assert!(!compare_native(
                reader,
                &tokens,
                hits,
                &admitted[..1],
                &passages[1]
            )?);
            exact.budget.instruction_bytes = exact.budget.max_bytes + 1;
            assert!(context::admit_document_for_test(reader, &exact, &[], &passages[0]).is_err());
            let (result, counts) = context::with_context_render_counts_for_test(0, || {
                context::render_documents_for_test(reader, &request, &[])
            });
            assert_eq!(result.unwrap_err().code, ErrorCode::BudgetExceeded);
            assert_eq!(counts.calls, 0);
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn location_equal_content_coordinates_preserve_both_revisions() {
        let fixture = Fixture::new();
        let body =
            "selectionprobe Café 東京 violet permit must be kept with its original source.\n";
        let mut ids = Vec::new();
        for _ in 0..2 {
            let plan = SourceStore::new(fixture.catalog.fs().clone())
                .plan_capture(capture(body.as_bytes()))
                .unwrap();
            ids.push(plan.source_id.clone());
            Fixture::seed(fixture.catalog.fs(), plan.draft.unwrap());
        }
        fixture.republish();
        let request = request();
        selected(&fixture, &request, |reader, hits| {
            let mut cores = Vec::new();
            for (i, id) in ids.iter().enumerate() {
                let hit = hits
                    .hits
                    .iter()
                    .find(|hit| hit.source_id.as_ref() == Some(id))
                    .unwrap();
                let start = if i == 0 {
                    0
                } else {
                    body.find("violet").unwrap() as u64
                };
                let end = if i == 0 {
                    body.find("must").unwrap() as u64
                } else {
                    body.find("source").unwrap() as u64
                };
                cores.push(location(
                    context::passage_for_span_for_test(
                        reader,
                        &request,
                        hit,
                        ByteSpan::new(start, end)?,
                        1,
                    )?
                    .unwrap(),
                ));
            }
            let (first, _) =
                context::admit_document_for_test(reader, &request, &[], &cores[0].passage)?
                    .unwrap();
            let (merged, _) =
                context::admit_document_for_test(reader, &request, &first, &cores[1].passage)?
                    .unwrap();
            assert_eq!(merged.len(), 1);
            assert_ne!(cores[0].effective, cores[1].effective);
            assert_ne!(cores[0].passage.locator, cores[1].passage.locator);
            assert!(cores.iter().all(|core| preserves(core, &merged)));
            let mut excerpt = request.clone();
            excerpt.documents.limits.excerpt_bytes = merged[0].text.len();
            assert!(compare_native(
                reader,
                &excerpt,
                hits,
                &first,
                &cores[1].passage
            )?);
            excerpt.documents.limits.excerpt_bytes -= 1;
            assert!(!compare_native(
                reader,
                &excerpt,
                hits,
                &first,
                &cores[1].passage
            )?);
            assert!(cores.iter().all(|core| preserves(core, &merged)));
            // A rejected broader parent never changes the accepted core set.
            let mut too_small = request.clone();
            too_small.budget.max_bytes =
                context::render_documents_for_test(reader, &request, &merged)?.len();
            let parent = context::passage_for_span_for_test(
                reader,
                &request,
                hits.hits
                    .iter()
                    .find(|h| h.source_id.as_ref() == Some(&ids[0]))
                    .unwrap(),
                ByteSpan::new(0, body.len() as u64)?,
                1,
            )?
            .unwrap();
            assert!(
                context::admit_document_for_test(reader, &too_small, &merged, &parent)?.is_err()
            );
            assert!(cores.iter().all(|core| preserves(core, &merged)));

            let mut changed = merged.clone();
            changed[0].text = changed[0].text.replace("violet", "orange");
            assert!(!preserves(&cores[1], &changed));
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn location_structures_deduplicate_and_respect_owner_cap() {
        let fixture = Fixture::new();
        authored_body(
            &fixture,
            "# Intro\n\nselectionprobe Café 東京 command introduction.\n\n```sh\nselectionprobe --violet\n```\n\nselectionprobe list introduction.\n\n- selectionprobe first\n- second exact item\n\nselectionprobe final paragraph.\n",
        );
        let request = request();
        let mut stats = Diagnostics::default();
        let output = run_case(
            &fixture.catalog,
            QUERY,
            &request,
            "L",
            None,
            None,
            None,
            &mut stats,
        )
        .unwrap();
        assert!(output["payload"]["network_used"] == false);
        assert!(stats.inventory.iter().all(|c| c["id"].is_string()));
        assert_eq!(
            stats
                .inventory
                .iter()
                .map(|c| c["id"].as_str().unwrap())
                .collect::<BTreeSet<_>>()
                .len(),
            stats.inventory.len()
        );
        let raw = fs::read_to_string(fixture.catalog.fs().root().path().join("page.md")).unwrap();
        let atoms = stats
            .inventory
            .iter()
            .filter_map(|row| {
                let identity: Value = serde_json::from_str(row["id"].as_str()?).ok()?;
                if identity[1]["path"] != "page.md" {
                    return None;
                }
                serde_json::from_value::<ByteSpan>(row["original"].clone())
                    .ok()?
                    .slice(&raw)
                    .ok()
            })
            .collect::<Vec<_>>();
        assert!(
            atoms
                .iter()
                .any(|text| text.contains("command introduction") && text.contains("```sh"))
        );
        assert!(
            atoms.iter().any(
                |text| text.contains("list introduction") && text.contains("second exact item")
            )
        );

        assert!(stats.core_trials <= 160 && stats.parent_trials <= 160);
        assert!(stats.term_parser_prefix_bytes >= stats.tokenized_bytes as u64);
        let oversize = format!(
            "selectionprobe code introduction.\n\n```sh\n{}\n```\n",
            "selectionprobe --violet\n".repeat(100)
        );
        authored_body(&fixture, &oversize);
        let mut oversized = Diagnostics::default();
        run_case(
            &fixture.catalog,
            QUERY,
            &request,
            "L",
            None,
            None,
            None,
            &mut oversized,
        )
        .unwrap();
        assert!(
            oversized
                .exclusions
                .iter()
                .any(|e| e["reason"] == "location_indivisible_core_exceeds_excerpt_bound")
        );
        selected(&fixture, &request, |reader, hits| {
            let (prepared, trace) = context::with_candidate_ordering_trace(|| {
                context::assemble_bounded_documents_with_signals_for_test(
                    reader,
                    &request,
                    hits,
                    QUERY,
                    &ContextSelectionSignals::default(),
                    &SelectionAction::Prepare,
                )
            });
            prepared?;
            let (pool, weights) = proposals(&trace)?;
            let mut only_page = hits.clone();
            only_page.hits.retain(|h| h.locator.path == path("page.md"));
            let mut incomplete = Diagnostics::default();
            incomplete.scanned_bytes = SCAN_CAP - 100;
            locations(
                reader,
                &request,
                &only_page,
                QUERY,
                &pool,
                &weights,
                &Meter::new(&request.verification_budget),
                &mut incomplete,
            )?;
            assert!(
                incomplete
                    .exclusions
                    .iter()
                    .any(|e| e["reason"] == "location_incomplete_structure")
            );
            Ok(())
        })
        .unwrap();
        authored_body(
            &fixture,
            &format!("{}\n", "selectionprobe Café violet prose ".repeat(100)),
        );
        let mut clipped = Diagnostics::default();
        run_case(
            &fixture.catalog,
            QUERY,
            &request,
            "L",
            None,
            None,
            None,
            &mut clipped,
        )
        .unwrap();
        assert!(clipped.inventory.iter().any(|row| row["clipped"] == true));
        authored_body(
            &fixture,
            &(0..6)
                .map(|i| format!("selectionprobe paragraph {i} requires a violet permit.\n\n"))
                .collect::<String>(),
        );
        selected(&fixture, &request, |reader, hits| {
            let hit = hits
                .hits
                .iter()
                .find(|h| h.locator.path == path("page.md"))
                .unwrap();
            let doc = reader.document(&hit.locator.path)?.unwrap();
            let (blocks, _, _, _, _) =
                context_units::location_blocks_for_test(&doc, 1024 * 1024, 4096)?;
            let mut admitted = Vec::new();
            let mut refused = false;
            for block in blocks.into_iter().filter(|b| b.kind == "prose") {
                let p = context::passage_for_span_for_test(reader, &request, hit, block.span, 1)?
                    .unwrap();
                if admitted.len() == 4 {
                    assert!(!compare_native(reader, &request, hits, &admitted, &p)?);
                }
                match context::admit_document_for_test(reader, &request, &admitted, &p)? {
                    Ok((next, _)) => admitted = next,
                    Err(reason) => {
                        assert_eq!(reason, "document_passage_cap");
                        refused = true;
                    }
                }
            }
            assert_eq!(admitted.len(), 4);
            assert!(refused);
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn location_final_recheck_refuses_refresh_withdraw_and_page_edit() {
        for mutation in ["refresh", "withdraw", "page"] {
            let fixture = Fixture::new();
            let request = request();
            let result = selected(&fixture, &request, |reader, hits| {
                let draft = context::assemble_bounded_documents_with_signals_for_test(
                    reader,
                    &request,
                    hits,
                    QUERY,
                    &ContextSelectionSignals::default(),
                    &SelectionAction::Automatic,
                )?;
                assert!(!draft.passages().is_empty());
                let store = SourceStore::new(fixture.catalog.fs().clone());
                match mutation {
                    "refresh" => Fixture::seed(
                        fixture.catalog.fs(),
                        store
                            .plan_refresh(
                                &fixture.source,
                                capture(b"selectionprobe changed original.\n"),
                            )?
                            .draft
                            .unwrap(),
                    ),
                    "withdraw" => Fixture::seed(
                        fixture.catalog.fs(),
                        store
                            .plan_withdraw(&fixture.source, "Superseded")?
                            .draft
                            .unwrap(),
                    ),
                    _ => fixture.edit("page.md", "title", json!("Changed selected page")),
                }
                Ok(())
            });
            assert_eq!(
                result.unwrap_err().code,
                ErrorCode::FreshnessConflict,
                "{mutation}"
            );
        }
    }
    #[test]
    fn location_closed_catalog_and_unavailable_cues_refuse_without_writes() {
        let fixture = Fixture::new();
        let request = request();
        let before = fs::read(
            fixture
                .catalog
                .fs()
                .root()
                .path()
                .join(fixture.content.as_str()),
        )
        .unwrap();
        selected(&fixture,&request,|reader,hits| {
            assert!(reader.document(&path("unselected.md"))?.is_none());
            for malformed in [json!({"complete":false}),json!({"complete":true,"query":"different"}),json!({"complete":true,"query":QUERY,"request":request,"snapshot":reader.snapshot(),"owners":[]})] {assert!(signals_from_frozen(&malformed,reader,&request,hits,QUERY).is_err());}
            let owners=hits.hits.iter().map(|h|json!({"path":h.locator.path,"hash":h.locator.observed_hash,"source":h.source_id,"revision":h.owner_revision})).collect::<Vec<_>>();
            let units=hits.hits.iter().map(|h|json!({"owner":h.locator.path,"observed_hash":h.locator.observed_hash,"span":h.excerpt.span,"unit_id":Blake3Hash::digest(h.locator.path.as_str()),"input_hash":Blake3Hash::digest("input"),"vector_hash":Blake3Hash::digest("vector"),"cosine":0.8})).collect::<Vec<_>>();
            let receipt=json!({"complete":true,"query":QUERY,"request":request,"snapshot":reader.snapshot(),"owners":owners,"units":units});
            assert!(signals_from_frozen(&receipt,reader,&request,hits,QUERY).is_ok());
            let mut empty=receipt.clone();empty["units"]=json!([]);assert!(signals_from_frozen(&empty,reader,&request,hits,QUERY).is_err());
            let mut partial=receipt.clone();partial["units"].as_array_mut().unwrap().pop();assert!(signals_from_frozen(&partial,reader,&request,hits,QUERY).is_err());
            let mut duplicate=receipt.clone();duplicate["units"].as_array_mut().unwrap().push(receipt["units"][0].clone());assert!(signals_from_frozen(&duplicate,reader,&request,hits,QUERY).is_err());
            // Run the actual legacy entry on the exact authenticated DocumentRow.
            // Its independent dependency binding is not used as membership authority.
            let legacy_temp=tempfile::tempdir().unwrap();fs::write(legacy_temp.path().join("WIKI.md"),note("vault","vault_selected_host",json!({}),b"")).unwrap();
            let legacy_catalog=Catalog::new(VaultFs::new(VaultRoot::explicit(legacy_temp.path())?),id("vault_selected_host"));let legacy=legacy_catalog.canonical_snapshot()?;
            let settings=crate::retrieval::spaces::EmbeddingSettings{max_input_bytes:1024,quality_target_bytes:Some(512),..Default::default()};
            let mut segmented_types=BTreeSet::new();
            for hit in &hits.hits {
                let document=reader.document(&hit.locator.path)?.unwrap();
                // Diagnostic clones exercise segmentation; they grant no evidence authority.
                let mut long=document.clone();
                let body=format!("# Operations Café 東京\n\n## Exception ancestry\n\n{}","selectionprobe: Café 東京 requires the violet permit before restoring the archived service.\n\n".repeat(40));
                long.raw_text=if long.owner_revision.is_some() {body.clone()}else{String::from_utf8(note("page","page_host",json!({"wiki_status":"reviewed"}),body.as_bytes())).unwrap()};
                long.hash=Blake3Hash::digest(long.raw_text.as_bytes());
                for (segmented,row) in [(false,&document),(true,&long)] {
                    let old=render::render_document(&legacy,row,&settings)?;
                    let mut selected=render::render_selected_document_for_test(row,&settings,reader.proof.fingerprint.clone())?.collect::<Result<Vec<_>>>()?;
                    assert_eq!(old.len(),selected.len());
                    if segmented {assert!(old.len()>1,"long renderer diagnostic must use multiple units");segmented_types.insert(row.owner_revision.is_some());}
                    for (old,new) in old.iter().zip(&mut selected) {new.dependency_fingerprint=old.dependency_fingerprint.clone();assert_eq!(old,new);}
                }
            }
            assert_eq!(segmented_types,BTreeSet::from([false,true]));
            let source=hits.hits.iter().find(|hit|hit.source_id.is_some()).unwrap();
            let mut signals=ContextSelectionSignals::default();signals.semantic_complete=true;
            signals.semantic.push(ContextSemanticCue{owner:source.locator.path.clone(),observed_hash:source.locator.observed_hash.clone(),span:source.excerpt.span,cosine:0.8});
            let (prepared,mut trace)=context::with_candidate_ordering_trace(||context::assemble_bounded_documents_with_signals_for_test(reader,&request,hits,QUERY,&signals,&SelectionAction::Prepare));
            prepared?;let (pool,_)=proposals(&trace)?;assert!(pool.iter().any(|p|p.packet.unit_score.is_some()));
            let lineage=trace.iter_mut().find(|v|v["stage"]=="unit_lineage").unwrap();lineage["rows"][0]["child_span"]=json!(ByteSpan::new(0,0)?);
            assert!(proposals(&trace).is_err());
            let missing=tempfile::tempdir().unwrap();
            let marker=note("vault","vault_selected_host",json!({}),b"");
            fs::write(missing.path().join("WIKI.md"),&marker).unwrap();
            let cache=VaultFs::new(VaultRoot::explicit(missing.path())?);
            assert!(cache_preflight(&cache,reader,&request,hits,QUERY,&Meter::new(&request.verification_budget)).is_err());
            assert_eq!(fs::read(missing.path().join("WIKI.md")).unwrap(),marker);
            let entries=fs::read_dir(missing.path()).unwrap().map(|entry|entry.unwrap().file_name()).collect::<Vec<_>>();
            assert_eq!(entries,vec![std::ffi::OsString::from("WIKI.md")]);
            Ok(())
        }).unwrap();
        assert_eq!(
            before,
            fs::read(
                fixture
                    .catalog
                    .fs()
                    .root()
                    .path()
                    .join(fixture.content.as_str())
            )
            .unwrap()
        );
    }
    #[test]
    fn location_invalid_requests_and_oracle_binding_fail_closed() {
        let base = json!({"vault":".","cases":".","vector_vault":".","output":".","action":"preflight","case_id":"fixture","requests":{"high":pinned_request("high"),"default":pinned_request("default")}});
        let config: Control = serde_json::from_value(base.clone()).unwrap();
        check_requests(&config).unwrap();
        for field in [
            "instruction_bytes",
            "instruction_tokens",
            "output_bytes",
            "output_tokens",
        ] {
            let mut value = base.clone();
            value["requests"]["high"]["budget"][field] = json!(1);
            assert!(check_requests(&serde_json::from_value(value).unwrap()).is_err());
        }
        for variant in ["tags", "cursor", "graph"] {
            let mut value = base.clone();
            match variant {
                "tags" => {
                    value["requests"]["default"]["documents"]["filters"]["tags"] = json!(["tag"])
                }
                "cursor" => value["requests"]["default"]["documents"]["cursor"] = json!("cursor"),
                _ => value["requests"]["default"]["graph"] = json!({}),
            }
            assert!(check_requests(&serde_json::from_value(value).unwrap()).is_err());
        }
        let fixture = Fixture::new();
        let mut stats = Diagnostics::default();
        let mut unsupported = request();
        unsupported.target = ContextTarget::Graph;
        assert!(
            run_case(
                &fixture.catalog,
                QUERY,
                &unsupported,
                "L",
                None,
                None,
                None,
                &mut stats
            )
            .is_err()
        );
        let request = request();
        assert!(
            run_case(
                &fixture.catalog,
                QUERY,
                &request,
                "S",
                Some(&json!({"complete":false})),
                None,
                None,
                &mut stats
            )
            .is_err()
        );
        let oracle = json!({"inventory_hash":"wrong","ids":[]});
        assert_eq!(
            run_case(
                &fixture.catalog,
                QUERY,
                &request,
                "L",
                None,
                None,
                Some(&oracle),
                &mut stats
            )
            .unwrap_err()
            .code,
            ErrorCode::FreshnessConflict
        );
        assert!(
            stats
                .trace
                .iter()
                .any(|stage| stage["stage"] == "inventory_binding")
        );
    }
}

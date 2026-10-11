//! Test-only native scope and label-informed witness diagnostic; no shipping selector.
use super::{context, context_types::*, indexed_documents, selected_documents, types::*};
use crate::{
    catalog::{
        Catalog,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    changes::ChangeEngine,
    domain::*,
    vault::{VaultFs, VaultRoot},
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

// Frozen before witness exposure: only enclosing original Markdown headings.
// Unknown/multiple labels retain, requested status/procedure labels form unions.
// Values, condition/fact phrases, task IDs and witness spans never enter decision.
#[derive(Clone)]
struct Heading {
    start: usize,
    end: usize,
    level: usize,
    text: String,
}
fn headings(raw: &str) -> Vec<Heading> {
    let mut end = raw.len().min(1024 * 1024);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = Vec::new();
    let mut active = None;
    for (event, range) in Parser::new(&raw[..end]).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                active = Some(Heading {
                    start: range.start,
                    end: range.end,
                    level: level as usize,
                    text: String::new(),
                })
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(h) = &mut active {
                    h.text.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(mut h) = active.take() {
                    h.end = range.end;
                    out.push(h);
                }
            }
            _ => {}
        }
        if out.len() == 4096 {
            break;
        }
    }
    out
}
fn ancestry(all: &[Heading], start: usize) -> Vec<Heading> {
    let mut out: Vec<Heading> = Vec::new();
    for h in all.iter().take_while(|h| h.start <= start) {
        while out.last().is_some_and(|old| old.level >= h.level) {
            out.pop();
        }
        out.push(h.clone());
    }
    out
}
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}
fn phrase(labels: &[String], requested: &str) -> bool {
    let needle = words(requested);
    !needle.is_empty()
        && labels.windows(needle.len()).any(|window| {
            window
                .iter()
                .zip(&needle)
                .all(|(a, b)| a.trim_end_matches('s') == b.trim_end_matches('s'))
        })
}
fn scope_decision(predicate: &Value, anchors: &[Heading]) -> (bool, String) {
    if predicate["kind"] == "unchanged_all" {
        return (true, "unchanged_all_control".into());
    }
    if predicate["kind"] == "guide_identity" {
        return (true, "guide_namespace_authentication_unavailable".into());
    }
    if predicate["kind"] != "original_scope_union" {
        return (true, "unknown_predicate".into());
    }
    let labels = words(
        &anchors
            .iter()
            .map(|h| h.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    );
    let projects = labels
        .iter()
        .filter(|s| {
            s.len() == 4 && s.starts_with('p') && s[1..].bytes().all(|b| b.is_ascii_digit())
        })
        .collect::<std::collections::BTreeSet<_>>();
    let known_procedures = ["deployment", "restore", "transfer", "audit"]
        .into_iter()
        .filter(|name| phrase(&labels, name))
        .collect::<Vec<_>>();
    let approved = phrase(&labels, "approved");
    let archived = phrase(&labels, "archived") || phrase(&labels, "draft");
    let mut mismatch = false;
    let mut unknown = Vec::new();
    if let Some(project) = predicate["project"].as_str() {
        if projects.len() != 1 {
            unknown.push("project_unknown_or_contradictory");
        } else if !projects.iter().any(|p| **p == project.to_lowercase()) {
            mismatch = true;
        }
    }
    if let Some(requested) = predicate["procedures"].as_array() {
        if known_procedures.len() != 1 {
            unknown.push("procedure_unknown_or_contradictory");
        } else if !requested
            .iter()
            .any(|v| v.as_str() == Some(known_procedures[0]))
        {
            mismatch = true;
        }
    }
    if let Some(requested) = predicate["status_scopes"].as_array() {
        if approved
            && requested
                .iter()
                .filter_map(Value::as_str)
                .any(|s| phrase(&words(s), "current"))
            && !phrase(&labels, "current")
        {
            unknown.push("current_status_unknown");
        }
        if approved == archived {
            unknown.push("status_unknown_or_contradictory");
        } else if !requested.iter().filter_map(Value::as_str).any(|s| {
            if approved {
                phrase(&words(s), "approved")
            } else {
                phrase(&words(s), "archived") || phrase(&words(s), "draft")
            }
        }) {
            mismatch = true;
        }
    }
    if !unknown.is_empty() {
        return (true, unknown.join(";"));
    }
    if mismatch {
        (false, "explicit_enclosing_scope_mismatch".into())
    } else {
        (true, "explicit_enclosing_scope_union".into())
    }
}
fn range_scope_decision(
    predicate: &Value,
    all: &[Heading],
    inspected_end: usize,
    span: ByteSpan,
) -> (bool, String, Vec<Heading>) {
    let enclosing = ancestry(all, span.start() as usize);
    let decision = if span.end() > inspected_end as u64 {
        (true, "scope_unknown_beyond_parsed_prefix".into())
    } else if all
        .iter()
        .any(|h| h.start as u64 > span.start() && (h.start as u64) < span.end())
    {
        (true, "scope_unknown_crosses_heading".into())
    } else {
        scope_decision(predicate, &enclosing)
    };
    (decision.0, decision.1, enclosing)
}
#[test]
fn scope_retains_unexamined_or_crossing_ranges_and_unqualified_current() {
    let raw = "# Project P003\n## Approved current transfer\nOwner.\n# Project P006\n## Approved current deployment\nLead.\n";
    let all = headings(raw);
    let predicate = json!({"kind":"original_scope_union","project":"P000","procedures":["transfer"],"status_scopes":["approved current"]});
    let crossing = ByteSpan::new(0, raw.len() as u64).unwrap();
    assert_eq!(
        range_scope_decision(&predicate, &all, raw.len(), crossing).1,
        "scope_unknown_crosses_heading"
    );
    assert!(range_scope_decision(&predicate, &all, 3, crossing).0);
    let missing_current = headings("# Project P003\n## Approved transfer\nOwner.\n");
    let requested = json!({"kind":"original_scope_union","project":"P003","procedures":["transfer"],"status_scopes":["approved current"]});
    assert_eq!(
        scope_decision(&requested, &missing_current).1,
        "current_status_unknown"
    );
}
#[test]
fn scope_uses_enclosing_headings_not_body_values() {
    let raw = "# Project P003
## Approved current transfer
Owner Project P006 deployment.
";
    let h = ancestry(&headings(raw), raw.len());
    let p = json!({"kind":"original_scope_union","project":"P003","procedures":["transfer"],"status_scopes":["approved current"]});
    assert!(scope_decision(&p, &h).0);
    let mut other = p.clone();
    other["project"] = json!("P006");
    assert!(!scope_decision(&other, &h).0);
}
#[test]
fn scope_preserves_comparators_and_unknown_utf8_scope() {
    let p = json!({"kind":"original_scope_union","project":"P000","procedures":["deployment"],"status_scopes":["approved current","archived draft"]});
    assert!(
        scope_decision(
            &p,
            &headings(
                "# Project P000
## Archived drafts deployment
🦀
"
            )
        )
        .0
    );
    assert!(
        scope_decision(
            &p,
            &headings(
                "# Café unknown scope
"
            )
        )
        .0
    );
    assert!(scope_decision(&json!({"kind":"unchanged_all"}), &[]).0);
}

const MIB: usize = 1024 * 1024;
const READ_LIMIT: usize = 512 * MIB;
const OUTPUT_LIMIT: usize = 32 * MIB;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    vault: PathBuf,
    tasks: PathBuf,
    scope: PathBuf,
    witnesses: PathBuf,
    frozen_packets: PathBuf,
    outputdir: PathBuf,
}
#[derive(Deserialize)]
struct Task {
    id: String,
    query: String,
}
#[derive(Deserialize)]
struct Tasks {
    tasks: Vec<Task>,
}
#[derive(Default)]
struct Work {
    total: usize,
    categories: BTreeMap<String, usize>,
}
fn checked<T, E: std::fmt::Display>(result: std::result::Result<T, E>) -> Result<T> {
    result.map_err(|error| WikiError::invalid(error.to_string()))
}
fn charge(work: &mut Work, category: &str, bytes: usize) -> Result<()> {
    work.total = work
        .total
        .checked_add(bytes)
        .ok_or_else(|| WikiError::invalid("read count overflow"))?;
    *work.categories.entry(category.into()).or_default() += bytes;
    if work.total > READ_LIMIT {
        return Err(WikiError::invalid("512MiB combined logical read bound"));
    }
    Ok(())
}
fn read(path: &Path, work: &mut Work) -> Result<Value> {
    if path.is_symlink() {
        return Err(WikiError::invalid("experiment input symlink"));
    }
    let size = checked(fs::metadata(path))?.len() as usize;
    charge(work, "metadata_and_frozen_reads", size)?;
    checked(serde_json::from_slice(&checked(fs::read(path))?))
}
fn save(path: &Path, value: &Value, output: &mut usize) -> Result<()> {
    let bytes = checked(serde_json::to_vec(value))?;
    if bytes.len() > 2 * MIB || *output + bytes.len() > OUTPUT_LIMIT {
        return Err(WikiError::invalid("compact case/whole output bound"));
    }
    let mut file = checked(OpenOptions::new().write(true).create_new(true).open(path))?;
    checked(file.write_all(&bytes))?;
    *output += bytes.len();
    Ok(())
}
fn deadline_check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(WikiError::invalid("reference owning-process deadline"))
    } else {
        Ok(())
    }
}
fn strip_timestamps(value: &mut Value) {
    if let Some(fields) = value.get_mut("verification").and_then(Value::as_object_mut) {
        fields.remove("verified_at");
    }
}
fn rows<'a>(trace: &'a [Value], stage: &str) -> Result<&'a Vec<Value>> {
    trace
        .iter()
        .find(|entry| entry["stage"] == stage)
        .and_then(|entry| entry["rows"].as_array())
        .ok_or_else(|| WikiError::invalid(format!("missing trace stage {stage}")))
}
fn hit_field<T: serde::de::DeserializeOwned>(row: &Value, name: &str) -> Result<T> {
    checked(serde_json::from_value(row[name].clone()))
}
fn decode_hit(row: &Value) -> Result<SearchHit> {
    let hit = SearchHit {
        locator: hit_field(row, "locator")?,
        title: hit_field(row, "title")?,
        kind: hit_field(row, "kind")?,
        authored_status: hit_field(row, "authored_status")?,
        eligibility: hit_field(row, "eligibility")?,
        identity_eligibility: hit_field(row, "identity_eligibility")?,
        excerpt: hit_field(row, "excerpt")?,
        secondary_excerpts: hit_field(row, "secondary_excerpts")?,
        reasons: hit_field(row, "reasons")?,
        rank_contributions: hit_field(row, "rank_contributions")?,
        source_id: hit_field(row, "source_id")?,
        owner_revision: hit_field(row, "owner_revision")?,
    };
    if checked(serde_json::to_value(&hit))? != *row {
        return Err(WikiError::invalid("exact traced hit roundtrip differs"));
    }
    Ok(hit)
}

#[derive(Deserialize)]
struct Witnesses {
    tasks: Vec<Witness>,
}
#[derive(Deserialize)]
struct Witness {
    task_id: String,
    groups: Vec<Group>,
    status: String,
    reason: String,
}
#[derive(Deserialize)]
struct Group {
    owner_index: usize,
    path: VaultRelativePath,
    source_blake3: Blake3Hash,
    core_span: ByteSpan,
    exact_spans: Vec<ExactSpan>,
}
#[derive(Deserialize)]
struct ExactSpan {
    start: u64,
    end: u64,
    blake3: Blake3Hash,
}
fn request() -> ContextRequest {
    ContextRequest {
        scope: ContextScope::IndexedDocuments,
        documents: QueryPlan {
            filters: SearchFilters {
                path_prefix: Some("sources/".into()),
                ..Default::default()
            },
            limits: SearchLimits {
                hits: 5,
                candidates: 80,
                excerpt_bytes: 1024,
            },
            ..Default::default()
        },
        budget: ContextBudget {
            max_bytes: 6000,
            max_tokens: 1500,
            ..Default::default()
        },
        verification_budget: VerificationBudget {
            max_entries: 65536,
            ..Default::default()
        },
        ..Default::default()
    }
}
fn native_work(native: &ContextResult, work: &mut Work, arm: &str) -> Result<()> {
    let crate::catalog::SnapshotVerification::IndexedEvidence {
        catalog_bytes_decoded,
        ..
    } = native.verification()
    else {
        return Err(WikiError::invalid("selected native verification required"));
    };
    charge(
        work,
        &format!("{arm}_catalog_decoded"),
        *catalog_bytes_decoded,
    )?;
    charge(
        work,
        &format!("{arm}_canonical_verification"),
        native.usage().verification_bytes,
    )
}
fn pool_row(
    packet: &context::Packet,
    weights: &[u64],
    keep: bool,
    reason: &str,
    anchors: &[Heading],
) -> Value {
    json!({"key":packet.key,"keep":keep,"reason":reason,"selection":packet.selection.as_ref().map(|s|json!({"owner_index":s.owner_index,"span":s.span,"covered_terms":s.covered_terms,"local_relevance":s.local_relevance,"clipped":s.clipped,"seed_overlap":s.seed_overlap,"semantic_affinity":s.semantic_affinity})),
        "passages":packet.passages.iter().map(|p|json!({"path":p.locator.path,"span":p.span})).collect::<Vec<_>>(),
        "fallback":packet.fallback.as_ref().map(|p|json!({"path":p.locator.path,"span":p.span})),"origin":packet.unit_origin,"score":packet.score,"unit_score":packet.unit_score,"selection_ordinal":packet.selection_ordinal,"unit_clipped":packet.unit_clipped,
        "term_weights":weights,"term_weight_count":weights.len(),"term_weight_fingerprint":Blake3Hash::digest(serde_json::to_vec(weights).expect("u64 weights serialize")),"anchors":anchors.iter().map(|h|json!({"start":h.start,"end":h.end,"text":h.text})).collect::<Vec<_>>()})
}
fn source_spans(trace: &[Value]) -> Result<Value> {
    Ok(
        json!({"generated":rows(trace,"source_candidates_generated")?,"retained":rows(trace,"source_candidates_retained")?}),
    )
}
fn witness(
    selected: &indexed_documents::SelectedCatalog<'_>,
    request: &ContextRequest,
    hits: &[SearchHit],
    trace: &[Value],
    nomination: &Witness,
    work: &mut Work,
    deadline: Instant,
    report: &mut Value,
) -> Result<()> {
    if nomination.groups.is_empty() {
        report["witness"] =
            json!({"admitted":false,"status":nomination.status,"reason":nomination.reason});
        return Ok(());
    }
    if nomination.groups.len() > 6 {
        return Err(WikiError::invalid("witness group bound"));
    }
    let mut additions = Vec::new();
    let mut owners = BTreeMap::new();
    for group in &nomination.groups {
        deadline_check(deadline)?;
        let hit = hits
            .get(group.owner_index)
            .ok_or_else(|| WikiError::invalid("witness owner outside original five"))?;
        if hit.locator.path != group.path || hit.locator.observed_hash != group.source_blake3 {
            return Err(WikiError::invalid("witness owner path mismatch"));
        }
        let retained = rows(trace, "source_candidates_retained")?
            .iter()
            .find(|r| r["owner_index"].as_u64() == Some(group.owner_index as u64))
            .ok_or_else(|| WikiError::invalid("witness owner missing retained cores"))?;
        let spans: Vec<ByteSpan> = checked(serde_json::from_value(retained["spans"].clone()))?;
        if !spans.contains(&group.core_span) {
            return Err(WikiError::invalid("witness core not native retained32"));
        }
        if group.exact_spans.is_empty() {
            return Err(WikiError::invalid("empty witness group"));
        }
        for nominated in &group.exact_spans {
            let span = ByteSpan::new(nominated.start, nominated.end)?;
            if span.is_empty()
                || span.start() < group.core_span.start()
                || span.end() > group.core_span.end()
            {
                return Err(WikiError::invalid("witness outside retained core"));
            }
            *owners.entry(group.owner_index).or_insert(0usize) += 1;
            if owners.len() > 2 || owners.values().any(|n| *n > 3) {
                return Err(WikiError::invalid("witness two owners/three spans bound"));
            }
            let passage = context::passage_for_span_for_test(
                selected,
                request,
                hit,
                span,
                group.owner_index + 1,
            )?
            .ok_or_else(|| WikiError::invalid("witness ineligible"))?;
            if Blake3Hash::digest(passage.text.as_bytes()) != nominated.blake3 {
                return Err(WikiError::invalid("witness exact byte hash mismatch"));
            }
            charge(work, "witness_exact_passage_reads", passage.text.len())?;
            additions.push(passage);
        }
    }
    let trial = context::document_trial(selected, request, &[], &additions)?;
    report["witness"] = json!({"verified":false,"admitted":trial.reason.is_none(),"reason":trial.reason,"text":trial.text,"passages":trial.passages,
        "rendered_bytes":trial.text.len(),"estimated_tokens":trial.text.len().div_ceil(4),"qualification":"label-informed documentary upper bound; never native automatic quality"});
    deadline_check(deadline)?;
    Ok(())
}
fn case(
    catalog: &Catalog,
    task: &Task,
    scope: &Value,
    nomination: &Witness,
    frozen: &Value,
    request: &ContextRequest,
    work: &mut Work,
    deadline: Instant,
    report: &mut Value,
) -> Result<()> {
    let baseline_pool = Rc::new(RefCell::new(Vec::new()));
    let pool = baseline_pool.clone();
    let (native, trace) = context::with_source_selection_trace(|| {
        context::with_packet_scope_for_test(
            move |p, w| {
                pool.borrow_mut()
                    .push(pool_row(p, w, true, "baseline_unchanged", &[]));
                true
            },
            || {
                indexed_documents::context(
                    catalog,
                    &task.query,
                    request,
                    &ContextOptions::default(),
                )
            },
        )
    });
    report["baseline_work_available"] = json!(native.is_ok());
    let native = native?;
    native_work(&native, work, "baseline")?;
    report["baseline"] = checked(serde_json::to_value(&native))?;
    report["baseline_pool"] = json!(*baseline_pool.borrow());
    report["source_trace"] = json!(trace);
    let mut actual = report["baseline"].clone();
    let mut expected = frozen["native"].clone();
    strip_timestamps(&mut actual);
    strip_timestamps(&mut expected);
    report["baseline_packet_matches_frozen"] = json!(actual == expected);
    let frozen_trace: Vec<Value> = checked(serde_json::from_value(frozen["trace"].clone()))?;
    report["baseline_spans_match_frozen"] =
        json!(source_spans(&trace)? == source_spans(&frozen_trace)?);
    if actual != expected || report["baseline_spans_match_frozen"] != true {
        return Err(WikiError::invalid(
            "baseline packet or generated/retained spans differ; stop interpretation",
        ));
    }
    deadline_check(deadline)?;
    let hits = rows(&trace, "document_hits")?
        .iter()
        .map(decode_hit)
        .collect::<Result<Vec<_>>>()?;
    if hits.len() > 5 {
        return Err(WikiError::invalid("original owner ceiling"));
    }
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: 2000,
        ..Default::default()
    })?;
    if checked(serde_json::to_value(reader.snapshot()))? != report["baseline"]["snapshot"] {
        return Err(WikiError::invalid("scope proof generation changed"));
    }
    let paths = hits
        .iter()
        .map(|h| h.locator.path.clone())
        .collect::<Vec<_>>();
    let mut proof =
        selected_documents::authenticate(catalog, &reader, &paths, &request.verification_budget)?;
    let decoder_before = reader.usage().bytes;
    charge(work, "diagnostic_catalog_decoded", decoder_before)?;
    let auth_bytes = proof.meter().work().0;
    charge(work, "diagnostic_authentication", auth_bytes)?;
    let selected = indexed_documents::SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let mut owner_headings = BTreeMap::new();
    for hit in &hits {
        let owner = selected
            .document(&hit.locator.path)?
            .ok_or_else(|| WikiError::invalid("selected owner unavailable"))?;
        if owner.hash != hit.locator.observed_hash {
            return Err(WikiError::invalid("selected owner hash changed"));
        }
        charge(work, "scope_heading_prefix", owner.raw_text.len().min(MIB))?;
        let all = headings(&owner.raw_text);
        let mut inspected_end = owner.raw_text.len().min(MIB);
        while !owner.raw_text.is_char_boundary(inspected_end) {
            inspected_end -= 1;
        }
        if all.len() >= 4096 {
            inspected_end = all.last().map_or(0, |h| h.end);
        }
        owner_headings.insert(owner.path.clone(), (all, inspected_end));
    }
    let scope_pool = Rc::new(RefCell::new(Vec::new()));
    let pool = scope_pool.clone();
    let predicate = scope["predicate"].clone();
    let (scoped, scoped_trace) = context::with_source_selection_trace(|| {
        context::with_packet_scope_for_test(
            move |p, w| {
                // Every origin/fallback/actual passage must agree to exclude a packet.
                // Mixed or unavailable enclosing scope retains the entire packet.
                let mut ranges = p
                    .passages
                    .iter()
                    .map(|p| (&p.locator.path, p.span))
                    .collect::<Vec<_>>();
                if let Some(fallback) = &p.fallback {
                    ranges.push((&fallback.locator.path, fallback.span));
                }
                if let (Some((span, _)), Some(passage)) = (p.unit_origin, p.passages.first()) {
                    ranges.push((&passage.locator.path, span));
                }
                let mut anchors = Vec::new();
                let mut decisions = Vec::new();
                for (path, span) in ranges {
                    let (keep, reason, enclosing) = owner_headings
                        .get(path)
                        .map(|(all, end)| range_scope_decision(&predicate, all, *end, span))
                        .unwrap_or_else(|| (true, "owner_scope_unknown".into(), Vec::new()));
                    decisions.push((keep, reason));
                    anchors.extend(enclosing);
                }
                let keep = decisions.is_empty() || decisions.iter().any(|(keep, _)| *keep);
                let reason = if decisions.is_empty() {
                    "packet_scope_unavailable".into()
                } else {
                    decisions
                        .iter()
                        .map(|(_, why)| why.as_str())
                        .collect::<Vec<_>>()
                        .join("|")
                };
                pool.borrow_mut()
                    .push(pool_row(p, w, keep, &reason, &anchors));
                keep
            },
            || {
                indexed_documents::context(
                    catalog,
                    &task.query,
                    request,
                    &ContextOptions::default(),
                )
            },
        )
    });
    report["scoped_work_available"] = json!(scoped.is_ok());
    let scoped = scoped?;
    native_work(&scoped, work, "scoped")?;
    report["scoped"] = checked(serde_json::to_value(&scoped))?;
    report["scope_pool"] = json!(*scope_pool.borrow());
    let normalize_pool = |pool: &[Value]| {
        pool.iter()
            .map(|r| {
                let mut r = r.clone();
                if let Some(fields) = r.as_object_mut() {
                    for key in ["keep", "reason", "anchors"] {
                        fields.remove(key);
                    }
                }
                r
            })
            .collect::<Vec<_>>()
    };
    let baseline_metadata = normalize_pool(&baseline_pool.borrow());
    let scoped_metadata = normalize_pool(&scope_pool.borrow());
    if baseline_metadata != scoped_metadata || source_spans(&trace)? != source_spans(&scoped_trace)?
    {
        return Err(WikiError::invalid(
            "scope full prefilter metadata/weights or original source spans changed",
        ));
    }
    let scope_ids = scoped_metadata
        .iter()
        .map(|r| r["key"].clone())
        .collect::<Vec<_>>();
    report["scope_effect"] = json!({"total":scope_ids.len(),"retained":scope_pool.borrow().iter().filter(|r|r["keep"]==true).count(),"excluded":scope_pool.borrow().iter().filter(|r|r["keep"]==false).count(),
        "unknown":scope_pool.borrow().iter().filter(|r|r["reason"].as_str().is_some_and(|s|s.contains("unknown")||s.contains("unavailable")||s.contains("contradictory"))).count()});
    deadline_check(deadline)?;
    witness(
        &selected, request, &hits, &trace, nomination, work, deadline, report,
    )?;
    // Actual passage/fallback coverage is distinct from parent-core ancestry.
    let covering_packets = |pool: &[Value], group: &Group, span: &ExactSpan| -> Vec<Value> {
        pool.iter()
            .filter(|row| row["keep"] == true)
            .filter(|row| {
                row["passages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(std::iter::once(&row["fallback"]))
                    .any(|p| {
                        p["path"] == json!(group.path)
                            && p["span"]["start"].as_u64().is_some_and(|s| s <= span.start)
                            && p["span"]["end"].as_u64().is_some_and(|e| e >= span.end)
                    })
            })
            .map(|row| row["key"].clone())
            .collect()
    };
    report["witness_scope_retention"] = json!(nomination.groups.iter().map(|group| json!({
        "owner_index":group.owner_index,"path":group.path,"core_span":group.core_span,
        "support_spans":group.exact_spans.iter().map(|span| json!({"start":span.start,"end":span.end,"hash":span.blake3,
            "baseline_covering_packets":covering_packets(&baseline_pool.borrow(),group,span),
            "scoped_covering_packets":covering_packets(&scope_pool.borrow(),group,span)})).collect::<Vec<_>>()
    })).collect::<Vec<_>>());
    proof.recheck(catalog, &reader)?;
    charge(
        work,
        "diagnostic_final_recheck",
        proof.meter().work().0 - auth_bytes,
    )?;
    charge(
        work,
        "diagnostic_catalog_decoded_final_delta",
        reader.usage().bytes.saturating_sub(decoder_before),
    )?;
    report["scope_anchor_proof_verified"] = json!(true);
    report["witness"]["verified"] = json!(true);
    reader.check_query_budget()?;
    deadline_check(deadline)?;
    Ok(())
}
#[test]
#[ignore = "root-frozen 12-case public scope/witness diagnostic; no automatic advancement"]
fn frozen_native_scope_and_witness_diagnostic() {
    run().unwrap();
}
fn run() -> Result<()> {
    let began = Instant::now();
    let mut work = Work::default();
    let mut output = 0;
    let config_path = checked(std::env::var("LWIKI_NATIVE_GROUP_CONFIG"))?;
    let config: Config = checked(serde_json::from_value(read(
        Path::new(&config_path),
        &mut work,
    )?))?;
    checked(fs::create_dir(&config.outputdir))?;
    let mut cases = Vec::new();
    let planned = (1..=12).map(|n| format!("D{n:02}")).collect::<Vec<_>>();
    let execution = (|| -> Result<()> {
        let tasks: Tasks = checked(serde_json::from_value(read(&config.tasks, &mut work)?))?;
        let scopes = read(&config.scope, &mut work)?;
        let nominations: Witnesses =
            checked(serde_json::from_value(read(&config.witnesses, &mut work)?))?;
        if tasks.tasks.iter().map(|t| t.id.clone()).collect::<Vec<_>>() != planned
            || nominations.tasks.len() != 12
        {
            return Err(WikiError::invalid(
                "original twelve tasks and nominations required",
            ));
        }
        let handle = VaultFs::new(VaultRoot::explicit(&config.vault)?);
        let engine = ChangeEngine::new(handle.clone())?;
        charge(
            &mut work,
            "vault_identity",
            checked(fs::metadata(config.vault.join("WIKI.md")))?.len() as usize,
        )?;
        let catalog = Catalog::new(handle, engine.vault_id().clone());
        let request = request();
        for task in tasks.tasks {
            if began.elapsed() >= Duration::from_secs(60) {
                return Err(WikiError::invalid("60s active bound"));
            }
            let started = Instant::now();
            let deadline = (started + Duration::from_secs(5)).min(began + Duration::from_secs(60));
            let before = work.total;
            let mut report = json!({"task_id":task.id,"query":task.query,"classification":"PUBLIC manual diagnostic; W label-informed, S query-scope only"});
            let (outcome, counts) = context::with_context_render_counts_for_test(1024, || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                    let scope = scopes["tasks"]
                        .as_array()
                        .and_then(|a| a.iter().find(|s| s["task_id"] == task.id))
                        .ok_or_else(|| WikiError::invalid("scope task missing"))?;
                    if scope["query"] != task.query {
                        return Err(WikiError::invalid("scope query changed"));
                    }
                    let nomination = nominations
                        .tasks
                        .iter()
                        .find(|w| w.task_id == task.id)
                        .ok_or_else(|| WikiError::invalid("witness task missing"))?;
                    let frozen = read(
                        &config.frozen_packets.join(format!("{}.json", task.id)),
                        &mut work,
                    )?;
                    if frozen["task_id"] != task.id || frozen["query"] != task.query {
                        return Err(WikiError::invalid("frozen task/query mismatch"));
                    }
                    case(
                        &catalog,
                        &task,
                        scope,
                        nomination,
                        &frozen,
                        &request,
                        &mut work,
                        deadline,
                        &mut report,
                    )
                }))
            });
            let error = match outcome {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(_) => Some("diagnostic panic retained".into()),
            };
            report["error"] = json!(error);
            report["logical_accounting_complete"] = json!(error.is_none());
            report["error_path_work"] = if error.is_some() {
                json!("unavailable; recorded work is a known minimum, never zero inferred")
            } else {
                Value::Null
            };
            report["render_counts"] = json!(counts);
            report["logical_read_bytes"] = json!(work.total - before);
            report["elapsed_before_persist_seconds"] = json!(started.elapsed().as_secs_f64());
            let receipt_path = config.outputdir.join(format!("{}.json", task.id));
            let persisted = save(&receipt_path, &report, &mut output);
            let overrun = Instant::now() >= deadline;
            let persist_error = persisted.as_ref().err().map(ToString::to_string);
            cases.push(json!({"task_id":task.id,"status":if error.is_none() && !overrun && persisted.is_ok() {"COMPLETE"} else {"ERROR"},"error":error,"persist_error":persist_error,"deadline_overrun":overrun,"owning_seconds":started.elapsed().as_secs_f64()}));
            persisted?;
            if error.is_some() || overrun {
                return Err(WikiError::invalid("case failed; remaining UNRUN"));
            }
        }
        Ok(())
    })();
    for id in &planned {
        if !cases.iter().any(|row| row["task_id"] == *id) {
            cases.push(json!({"task_id":id,"status":"UNRUN"}));
        }
    }
    let summary = json!({"planned":12,"cases":cases,"error":execution.as_ref().err().map(ToString::to_string),"logical_read_categories":work.categories,"combined_logical_reads_known":work.total,"logical_accounting_complete":execution.is_ok(),"error_path_work":if execution.is_err(){Some("unavailable; known counters retained, no hard read-cap completion claim")}else{None},
        "output_bytes_before_summary":output,"owning_seconds_before_summary":began.elapsed().as_secs_f64(),"hard65_overrun":began.elapsed()>=Duration::from_secs(65),"physical_io":"unavailable","quality_credit":false});
    save(
        &config.outputdir.join("summary.json"),
        &summary,
        &mut output,
    )?;
    let finish = json!({"owning_seconds_after_summary_persist":began.elapsed().as_secs_f64(),"output_bytes_after_summary":output,"planned":12,"hard65_overrun":began.elapsed()>=Duration::from_secs(65),"final_persist_included_in_process_supervisor":true});
    save(&config.outputdir.join("finish.json"), &finish, &mut output)?;
    if began.elapsed() >= Duration::from_secs(60) {
        return Err(WikiError::invalid(
            "60s finish-inclusive owning deadline overrun",
        ));
    }
    execution
}

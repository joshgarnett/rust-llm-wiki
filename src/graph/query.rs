//! Pinned, offline graph lookup with safe lexical seeds and stateless pagination.
use super::{rank, traverse, types::*};
use crate::{
    catalog::ReaderSnapshot,
    domain::*,
    retrieval::{RankContribution, excerpts::Tokenizer, filters, lexical::lexical_expression},
};
use rusqlite::{params_from_iter, types::Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub fn validate_plan(plan: &GraphPlan) -> Result<GraphPlan> {
    let limits = &plan.limits;
    if limits.candidates == 0
        || limits.candidates > 80
        || limits.seeds == 0
        || limits.seeds > 12
        || limits.depth > 2
        || limits.incident_per_seed == 0
        || limits.incident_per_seed > 16
        || limits.assertions == 0
        || limits.assertions > 128
        || limits.hits == 0
        || limits.hits > 50
        || limits.excerpt_bytes == 0
        || limits.excerpt_bytes > 2048
        || limits.support_per_assertion > 2
        || limits.contradictions_per_assertion > 1
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "graph limits exceed bounded candidates80/seeds12/depth2/incident16/assertions128/hits50/excerpt2048/support2/contradiction1",
        ));
    }
    let mut normalized = plan.clone();
    normalized.filters = filters::normalize(&plan.filters)?;
    Ok(normalized)
}
pub fn query(reader: &ReaderSnapshot, text: &str, plan: &GraphPlan) -> Result<GraphResult> {
    if plan.seed_mode != GraphSeedMode::Lexical {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "semantic seeds require the embedding application",
        ));
    }
    let plan = validate_plan(plan)?;
    let (mut seeds, mut coverage) = lexical_seed_lists(reader, text, &plan)?;
    seeds.sort_by(rank::seed_order);
    coverage.omitted_seeds = seeds.len().saturating_sub(plan.limits.seeds);
    seeds.truncate(plan.limits.seeds);
    result(reader, seeds, coverage, &plan, &format!("query:{text}"))
}
/// Return bounded lexical candidates before the shared cross-channel seed cap.
pub fn lexical_seed_lists(
    reader: &ReaderSnapshot,
    text: &str,
    plan: &GraphPlan,
) -> Result<(Vec<GraphSeed>, GraphCoverage)> {
    let plan = validate_plan(plan)?;
    let expression = lexical_expression(text)?;
    if Tokenizer::new(reader.connection())?
        .tokens(text)?
        .is_empty()
    {
        let mut e = WikiError::new(
            ErrorCode::Usage,
            "graph lexical query has no searchable tokens",
        );
        e.hint = Some("Use literal search for punctuation-only queries".into());
        return Err(e);
    }
    let mut seeds = Vec::new();
    let mut coverage = GraphCoverage::default();
    for kind in match plan.strategy {
        GraphStrategy::Entity => vec![RecordKind::Entity],
        GraphStrategy::Relationship => vec![RecordKind::Assertion],
        GraphStrategy::Combined => vec![RecordKind::Entity, RecordKind::Assertion],
    } {
        let (mut found, overflow) = seed_candidates(reader, text, &expression, &plan, kind)?;
        if kind == RecordKind::Entity {
            coverage.entity_candidates = found.len();
        } else {
            coverage.assertion_candidates = found.len();
        }
        coverage.omitted_candidates += overflow;
        seeds.append(&mut found);
    }
    Ok((seeds, coverage))
}
/// Traversal uses canonical rows and the ordinary eligibility/filter policy.
/// Candidate ranks carry no identity, evidence, or currentness authority.
pub fn from_seeds(
    reader: &ReaderSnapshot,
    seeds: Vec<GraphSeed>,
    mut coverage: GraphCoverage,
    plan: &GraphPlan,
    key: &str,
) -> Result<GraphResult> {
    let plan = validate_plan(plan)?;
    if seeds.len() > 160 || key.len() > 8192 {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "graph seed input exceeds bound",
        ));
    }
    let allowed = traverse::filtered_ids(reader, &plan, None)?;
    let mut canonical = BTreeMap::new();
    for candidate in seeds {
        let id = &candidate.record_ref.record_id;
        let row = reader.projection().records.get(id).ok_or_else(|| {
            WikiError::new(
                ErrorCode::FreshnessConflict,
                "seed absent from pinned projection",
            )
        })?;
        if candidate.record_ref != traverse::reference(reader, row)
            || candidate.locator != traverse::locator(reader, row)
            || candidate.rank_contributions.len() > 8
            || candidate.rank_contributions.iter().any(|r| {
                r.rank == 0
                    || r.rank > 80
                    || r.channel.len() > 128
                    || r.score.is_some_and(|s| !s.is_finite())
            })
        {
            return Err(WikiError::invalid("graph seed identity or rank differs"));
        }
        let selected_kind = match plan.strategy {
            GraphStrategy::Entity => row.record.kind() == RecordKind::Entity,
            GraphStrategy::Relationship => row.record.kind() == RecordKind::Assertion,
            GraphStrategy::Combined => matches!(
                row.record.kind(),
                RecordKind::Entity | RecordKind::Assertion
            ),
        };
        if !selected_kind
            || !allowed.contains(id)
            || row.record.kind() == RecordKind::Assertion
                && traverse::proposition(reader, row).is_none()
        {
            coverage.omitted_candidates += 1;
            continue;
        }
        if canonical
            .insert(id.clone(), seed(reader, row, candidate.rank_contributions))
            .is_some()
        {
            return Err(WikiError::invalid("duplicate graph seed"));
        }
    }
    let mut seeds: Vec<_> = canonical.into_values().collect();
    seeds.sort_by(rank::seed_order);
    coverage.omitted_seeds += seeds.len().saturating_sub(plan.limits.seeds);
    seeds.truncate(plan.limits.seeds);
    result(reader, seeds, coverage, &plan, key)
}
pub fn neighbors(reader: &ReaderSnapshot, id: &RecordId, plan: &GraphPlan) -> Result<GraphResult> {
    let plan = validate_plan(plan)?;
    if reader
        .projection()
        .diagnostics
        .iter()
        .any(|d| d.record_id.as_ref() == Some(id) && d.code == ErrorCode::ReferenceAmbiguous)
    {
        return Err(WikiError::new(
            ErrorCode::ReferenceAmbiguous,
            "neighbor ID has multiple canonical claims",
        ));
    }
    let row = reader
        .projection()
        .records
        .get(id)
        .ok_or_else(|| WikiError::new(ErrorCode::RecordNotFound, "neighbor ID not found"))?;
    let allowed = traverse::filtered_ids(reader, &plan, Some(row.record.kind()))?;
    let seeds = if allowed.contains(id) {
        vec![seed(
            reader,
            row,
            vec![RankContribution {
                channel: "exact_id".into(),
                rank: 1,
                score: None,
            }],
        )]
    } else {
        vec![]
    };
    result(
        reader,
        seeds,
        GraphCoverage::default(),
        &plan,
        &format!("neighbors:{id}"),
    )
}
fn seed(
    reader: &ReaderSnapshot,
    row: &crate::catalog::RecordRow,
    contributions: Vec<RankContribution>,
) -> GraphSeed {
    GraphSeed {
        record_ref: traverse::reference(reader, row),
        locator: traverse::locator(reader, row),
        title: row.record.title().into(),
        kind: row.record.kind(),
        eligibility: row.eligibility,
        identity_eligibility: row.identity_eligibility,
        rrf_score: rank::rrf(&contributions),
        rank_contributions: contributions,
    }
}
fn seed_candidates(
    reader: &ReaderSnapshot,
    text: &str,
    expression: &str,
    plan: &GraphPlan,
    kind: RecordKind,
) -> Result<(Vec<GraphSeed>, usize)> {
    let mut candidates: BTreeMap<RecordId, GraphSeed> = BTreeMap::new();
    let mut overflow = 0;
    let mut legs = vec![
        ("r.id=?1", "exact_id", false, false),
        ("d.title=?1", "exact_title", false, false),
        (
            "EXISTS(SELECT 1 FROM aliases a WHERE a.gen=r.gen AND a.id=r.id AND a.alias=?1)",
            "exact_alias",
            false,
            false,
        ),
        ("graph_fts MATCH ?1", "graph_lexical", true, false),
    ];
    if kind == RecordKind::Entity && plan.filters.include_historical {
        legs.push(("documents_fts MATCH ?1 AND coalesce(json_extract(r.row_json,'$.description_eligibility'),'')<>'current'", "historical_description", true, true));
    }
    for (condition, channel, fts, historical) in legs {
        let mut values = vec![
            Value::Text(if fts { expression.into() } else { text.into() }),
            Value::Integer(
                i64::try_from(reader.snapshot().generation)
                    .map_err(|_| WikiError::invalid("generation exceeds SQL range"))?,
            ),
        ];
        let common = filters::sql(&plan.filters, &mut values);
        let policy = traverse::policy(plan, kind);
        let bound = filters::bind(
            &mut values,
            Value::Integer((plan.limits.candidates + 1) as i64),
        );
        let weights = if kind == RecordKind::Entity {
            "8,6,0,0,0,1,0,0,0"
        } else {
            "0,0,4,6,2,1,0,0,0"
        };
        let score = if historical {
            "bm25(documents_fts,8,6,3,2,1,0,0)".into()
        } else if fts {
            format!("bm25(graph_fts,{weights})")
        } else {
            "NULL".into()
        };
        let join = if historical {
            "JOIN documents_fts ON documents_fts.gen=d.gen AND documents_fts.doc_row=d.doc_row"
        } else if fts {
            "JOIN graph_fts ON graph_fts.gen=r.gen AND graph_fts.target_id=r.id"
        } else {
            ""
        };
        let sql = format!(
            "SELECT r.id,{score} AS score FROM records r JOIN documents d ON d.gen=r.gen AND d.record_id=r.id {join} WHERE r.gen=?2 AND r.kind='{kind}' AND ({condition}) AND ({common}) AND ({policy}) ORDER BY score,r.id LIMIT {bound}"
        );
        let mut statement = reader
            .connection()
            .prepare(&sql)
            .map_err(traverse::sql_error)?;
        let rows = statement
            .query_map(params_from_iter(values), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<f64>>(1)?))
            })
            .map_err(traverse::sql_error)?;
        for (index, row) in rows.enumerate() {
            if index == plan.limits.candidates {
                overflow += 1;
                break;
            }
            let (id, score) = row.map_err(traverse::sql_error)?;
            if score.is_some_and(|s| !s.is_finite()) {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "graph rank nonfinite",
                ));
            }
            let id = RecordId::new(id)?;
            let row = reader.projection().records.get(&id).ok_or_else(|| {
                WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "graph seed missing projection record",
                )
            })?;
            if kind == RecordKind::Assertion && traverse::proposition(reader, row).is_none() {
                overflow += 1;
                continue;
            }
            let contribution = RankContribution {
                channel: format!("{}_{}", kind, channel),
                rank: index + 1,
                score,
            };
            if let Some(found) = candidates.get_mut(&id) {
                found.rank_contributions.push(contribution);
                found.rrf_score = rank::rrf(&found.rank_contributions);
            } else {
                candidates.insert(id, seed(reader, row, vec![contribution]));
            }
        }
    }
    let mut found: Vec<_> = candidates.into_values().collect();
    found.sort_by(rank::seed_order);
    overflow += found.len().saturating_sub(plan.limits.candidates);
    found.truncate(plan.limits.candidates);
    Ok((found, overflow))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u32,
    snapshot: ReadSnapshot,
    fingerprint: Blake3Hash,
    offset: usize,
}
fn fingerprint(key: &str, plan: &GraphPlan) -> Result<Blake3Hash> {
    let mut plan = plan.clone();
    plan.cursor = None;
    serde_json::to_vec(&("lwiki-graph-query-v1", key, plan))
        .map(Blake3Hash::digest)
        .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))
}
fn offset(
    reader: &ReaderSnapshot,
    hash: &Blake3Hash,
    value: Option<&str>,
    length: usize,
) -> Result<usize> {
    let Some(value) = value else { return Ok(0) };
    let stale = || {
        WikiError::new(
            ErrorCode::CursorStale,
            "graph cursor snapshot, query, filters or limits changed",
        )
    };
    if value.len() > 4096 || value.len() % 2 != 0 {
        return Err(stale());
    }
    let bytes = value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |b| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                _ => None,
            };
            Ok(digit(pair[0]).ok_or_else(stale)? * 16 + digit(pair[1]).ok_or_else(stale)?)
        })
        .collect::<Result<Vec<_>>>()?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| stale())?;
    if cursor.version != 1
        || cursor.snapshot != *reader.snapshot()
        || cursor.fingerprint != *hash
        || cursor.offset >= length
    {
        return Err(stale());
    }
    Ok(cursor.offset)
}
fn result(
    reader: &ReaderSnapshot,
    seeds: Vec<GraphSeed>,
    coverage: GraphCoverage,
    plan: &GraphPlan,
    key: &str,
) -> Result<GraphResult> {
    let mut walked = traverse::walk(reader, &seeds, plan, coverage)?;
    // One primary ordered stream, assertions followed by navigation. Navigation-only
    // neighbors page correctly; both categories share display and visited budgets.
    let total = walked.assertions.len() + walked.navigation.len();
    let hash = fingerprint(key, plan)?;
    let offset = offset(reader, &hash, plan.cursor.as_deref(), total)?;
    let end = (offset + plan.limits.hits).min(total);
    let assertion_count = walked.assertions.len();
    let assertions = walked
        .assertions
        .drain(offset.min(assertion_count)..end.min(assertion_count))
        .collect::<Vec<_>>();
    let navigation = walked
        .navigation
        .drain(
            offset
                .saturating_sub(assertion_count)
                .min(walked.navigation.len())
                ..end
                    .saturating_sub(assertion_count)
                    .min(walked.navigation.len()),
        )
        .collect::<Vec<_>>();
    let mut entity_ids: BTreeSet<_> = seeds
        .iter()
        .filter(|s| s.kind == RecordKind::Entity)
        .map(|s| s.record_ref.record_id.clone())
        .collect();
    for edge in &assertions {
        entity_ids.insert(edge.subject.record_id.clone());
        if let GraphObject::Entity { record_ref } = &edge.object {
            entity_ids.insert(record_ref.record_id.clone());
        }
    }
    let entities = entity_ids
        .into_iter()
        .filter_map(|id| reader.projection().records.get(&id))
        .map(|row| traverse::entity(reader, row, plan))
        .collect();
    let next_cursor = if end < total {
        let bytes = serde_json::to_vec(&Cursor {
            version: 1,
            snapshot: reader.snapshot().clone(),
            fingerprint: hash,
            offset: end,
        })
        .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
        Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
    } else {
        None
    };
    let mut warnings = Vec::new();
    if walked.coverage.omitted_candidates > 0 {
        warnings.push("candidate omissions are a lower bound after per-list caps".into());
    }
    if walked.coverage.omitted_incident_assertions > 0 {
        warnings.push("incident/total assertion budget omitted known recorded assertions".into());
    }
    if walked.coverage.omitted_navigation > 0 {
        warnings.push(
            "navigation shares visited/output budgets; omitted navigation count is a lower bound"
                .into(),
        );
    }
    if walked.coverage.depth_limited {
        warnings.push("depth limit reached; traversal is not exhaustive".into());
    }
    if assertions
        .iter()
        .any(|e| e.omitted_support + e.omitted_contradictions > 0)
    {
        warnings.push(
            "evidence omission counts include all capped or ineligible canonical associations"
                .into(),
        );
    }
    let truncated = end < total
        || walked.coverage.omitted_candidates > 0
        || walked.coverage.omitted_seeds > 0
        || walked.coverage.omitted_assertions > 0
        || walked.coverage.omitted_navigation > 0
        || walked.coverage.depth_limited;
    let dependency_fingerprint = Blake3Hash::digest(
        serde_json::to_vec(&reader.projection().dependencies)
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
    );
    Ok(GraphResult {
        network_used: false,
        seeds,
        entities,
        assertions,
        navigation,
        next_cursor,
        truncated,
        coverage: walked.coverage,
        snapshot: reader.snapshot().clone(),
        verification: reader.verification().clone(),
        dependency_fingerprint,
        warnings,
    })
}

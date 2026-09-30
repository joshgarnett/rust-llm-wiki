//! Named rank fusion and owner collapse before voting.
use super::{types::*, vectors::DenseHit};
use crate::{
    catalog::{DocumentRow, ReaderSnapshot},
    domain::*,
    graph::*,
};
use std::collections::BTreeMap;
pub fn rrf(contributions: &[RankContribution]) -> f64 {
    contributions
        .iter()
        .map(|c| 1.0 / (60.0 + c.rank as f64))
        .sum()
}
pub fn collapse_dense(hits: &[DenseHit]) -> Vec<(DenseHit, Vec<DenseHit>)> {
    let mut owners: BTreeMap<VaultRelativePath, Vec<DenseHit>> = BTreeMap::new();
    for hit in hits {
        let passages = owners.entry(hit.owner.clone()).or_default();
        passages.push(hit.clone());
        passages.sort();
        passages.truncate(2);
    }
    let mut out = owners
        .into_values()
        .map(|passages| (passages[0].clone(), passages))
        .collect::<Vec<_>>();
    out.sort_by(|(a, _), (b, _)| {
        b.score
            .total_cmp(&a.score)
            .then(a.target_id.cmp(&b.target_id))
            .then(a.owner.cmp(&b.owner))
            .then(a.unit_id.cmp(&b.unit_id))
    });
    out
}
pub(crate) fn unit_allowed(
    reader: &ReaderSnapshot,
    unit: &super::render::RenderedUnit,
    filters: &SearchFilters,
    context: Option<bool>,
    graph: Option<&GraphPlan>,
) -> Result<bool> {
    let mut values = vec![
        rusqlite::types::Value::Integer(reader.snapshot().generation as i64),
        rusqlite::types::Value::Text(unit.owner.as_str().into()),
    ];
    let common = super::filters::sql(filters, &mut values);
    let policy = if let Some(graph) = graph {
        crate::graph::traverse::policy(
            graph,
            match unit.target {
                super::render::TargetKind::Entity => RecordKind::Entity,
                super::render::TargetKind::Assertion => RecordKind::Assertion,
                _ => return Ok(false),
            },
        )
    } else if let Some(historical) = context {
        super::filters::context_policy(historical)
    } else {
        format!(
            "({}) OR ({})",
            super::filters::normal_policy(filters),
            super::filters::identity_policy()
        )
    };
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM documents d LEFT JOIN records r ON r.gen=d.gen AND r.id=d.record_id WHERE d.gen=?1 AND d.path=?2 AND ({common}) AND ({policy}))"
    );
    reader
        .connection()
        .query_row(&sql, rusqlite::params_from_iter(values), |r| r.get(0))
        .map_err(crate::catalog::sql::sql_error)
}
pub fn dense_hit(
    reader: &ReaderSnapshot,
    document: &DocumentRow,
    dense: &DenseHit,
    rank: usize,
    bytes: usize,
) -> Result<SearchHit> {
    let record = super::filters::row(reader, document);
    let identity = record.is_some_and(|r| {
        r.record.kind() == RecordKind::Entity
            && r.description_eligibility != Some(Eligibility::Current)
    });
    let matches = dense
        .source_span
        .into_iter()
        .map(|s| s.start() as usize..s.end() as usize)
        .collect::<Vec<_>>();
    let excerpt = super::lexical::excerpt(
        reader,
        document,
        &matches,
        bytes,
        SearchMode::Lexical,
        identity,
    )?;
    let reference = if let (Some(id), Some(kind)) = (&document.record_id, document.kind) {
        Some(RecordRef {
            vault_id: reader.projection().vault_id.clone(),
            record_id: id.clone(),
            expected_kind: kind,
        })
    } else {
        document.owner_revision.as_ref().map(|id| RecordRef {
            vault_id: reader.projection().vault_id.clone(),
            record_id: id.clone(),
            expected_kind: RecordKind::Revision,
        })
    };
    Ok(SearchHit {
        locator: DocumentLocator {
            record: reference,
            path: document.path.clone(),
            observed_hash: document.hash.clone(),
        },
        title: document.title.clone(),
        kind: document.kind,
        authored_status: record.and_then(|r| r.authored_status.clone()),
        eligibility: document.eligibility,
        identity_eligibility: record.and_then(|r| r.identity_eligibility),
        excerpt,
        secondary_excerpts: vec![],
        reasons: vec![RetrievalReason::Semantic],
        rank_contributions: vec![RankContribution {
            channel: "dense".into(),
            rank,
            score: Some(dense.score),
        }],
        source_id: document.source_id.clone(),
        owner_revision: document.owner_revision.clone(),
    })
}
pub fn fuse_hits(lists: Vec<Vec<SearchHit>>) -> Vec<SearchHit> {
    let mut found: BTreeMap<VaultRelativePath, SearchHit> = BTreeMap::new();
    for list in lists {
        for hit in list {
            if let Some(old) = found.get_mut(&hit.locator.path) {
                for excerpt in std::iter::once(&hit.excerpt).chain(&hit.secondary_excerpts) {
                    let distinct = |other: &SearchExcerpt| {
                        excerpt.span.end() <= other.span.start()
                            || other.span.end() <= excerpt.span.start()
                    };
                    if old.secondary_excerpts.is_empty()
                        && !excerpt.span.is_empty()
                        && distinct(&old.excerpt)
                        && old.secondary_excerpts.iter().all(distinct)
                    {
                        old.secondary_excerpts.push(excerpt.clone());
                    }
                }
                for contribution in hit.rank_contributions {
                    if !old
                        .rank_contributions
                        .iter()
                        .any(|c| c.channel == contribution.channel)
                    {
                        old.rank_contributions.push(contribution);
                    }
                }
                for reason in hit.reasons {
                    if !old.reasons.contains(&reason) {
                        old.reasons.push(reason);
                    }
                }
            } else {
                found.insert(hit.locator.path.clone(), hit);
            }
        }
    }
    let mut hits = found.into_values().collect::<Vec<_>>();
    hits.sort_by(|a, b| {
        rrf(&b.rank_contributions)
            .total_cmp(&rrf(&a.rank_contributions))
            .then(
                a.locator
                    .record
                    .as_ref()
                    .map(|r| &r.record_id)
                    .cmp(&b.locator.record.as_ref().map(|r| &r.record_id)),
            )
            .then(a.locator.path.cmp(&b.locator.path))
    });
    hits
}
pub fn graph_seeds(
    reader: &ReaderSnapshot,
    dense: &[DenseHit],
    kind: RecordKind,
) -> Result<Vec<GraphSeed>> {
    collapse_dense(dense)
        .into_iter()
        .enumerate()
        .map(|(index, (hit, _))| {
            let id = hit
                .target_id
                .ok_or_else(|| WikiError::invalid("graph vector target missing"))?;
            let row = reader
                .projection()
                .records
                .get(&id)
                .filter(|r| r.record.kind() == kind)
                .ok_or_else(|| {
                    WikiError::new(ErrorCode::FreshnessConflict, "graph vector target changed")
                })?;
            let contribution = RankContribution {
                channel: format!("{kind}_dense"),
                rank: index + 1,
                score: Some(hit.score),
            };
            Ok(GraphSeed {
                record_ref: RecordRef {
                    vault_id: reader.projection().vault_id.clone(),
                    record_id: id,
                    expected_kind: kind,
                },
                locator: DocumentLocator {
                    record: Some(RecordRef {
                        vault_id: reader.projection().vault_id.clone(),
                        record_id: row.record.id().clone(),
                        expected_kind: kind,
                    }),
                    path: row.path.clone(),
                    observed_hash: row.hash.clone(),
                },
                title: row.record.title().into(),
                kind,
                eligibility: row.eligibility,
                identity_eligibility: row.identity_eligibility,
                rrf_score: rrf(std::slice::from_ref(&contribution)),
                rank_contributions: vec![contribution],
            })
        })
        .collect()
}

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
    dense_hit_inner(reader, document, dense, rank, bytes, None)
}

pub(crate) fn dense_hit_for_query(
    reader: &ReaderSnapshot,
    document: &DocumentRow,
    dense: &DenseHit,
    rank: usize,
    bytes: usize,
    query: &str,
) -> Result<SearchHit> {
    dense_hit_inner(reader, document, dense, rank, bytes, Some(query))
}

fn dense_hit_inner(
    reader: &ReaderSnapshot,
    document: &DocumentRow,
    dense: &DenseHit,
    rank: usize,
    bytes: usize,
    query: Option<&str>,
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
    let excerpt = if let (false, Some(query), Some(span)) = (identity, query, dense.source_span) {
        super::lexical::focused_excerpt(reader, document, query, span, bytes)?
    } else {
        super::lexical::excerpt(
            reader,
            document,
            &matches,
            bytes,
            SearchMode::Lexical,
            identity,
        )?
    };
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

#[cfg(test)]
mod focused_excerpt_tests {
    use super::*;
    use crate::{
        app::{OfflineApp, OperationOptions, offline},
        catalog::Catalog,
        retrieval::render::TargetKind,
        sources::{CaptureRequest, CitationScope, ExtractionInput, SourceOrigin, SourceView},
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use std::time::Duration;

    fn captured(raw: &str) -> (tempfile::TempDir, OfflineApp, Catalog) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        offline::init(
            &root,
            "Focused excerpt fixture",
            OperationOptions::default(),
        )
        .unwrap();
        let app = OfflineApp::new(
            VaultFs::new(VaultRoot::explicit(&root).unwrap()),
            OperationOptions::default(),
        )
        .unwrap();
        app.source_add(CaptureRequest {
            title: "Captured Unicode source".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.txt".into(),
            original: raw.as_bytes().to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        (temp, app, catalog)
    }

    fn dense(document: &DocumentRow, span: ByteSpan) -> DenseHit {
        DenseHit {
            unit_id: Blake3Hash::digest(b"selected unit"),
            target: TargetKind::Document,
            owner: document.path.clone(),
            target_id: document.record_id.clone(),
            source_span: Some(span),
            input_hash: Blake3Hash::digest(span.slice(&document.raw_text).unwrap()),
            score: 1.0,
        }
    }

    #[test]
    fn query_focus_stays_inside_selected_unit_and_verifies_exact_unicode_citation() {
        let prefix = "OUTSIDE BEFORE: quasar orbit lambda boundary\n".repeat(20);
        let middle = format!(
            "{}\n## Navigation\n\nquasar and orbit align with lambda near naïve 中文 🦀.\n",
            "ordinary surrounding notes café 中文 🦀\n".repeat(100)
        );
        let raw = format!("{prefix}{middle}OUTSIDE AFTER: quasar orbit lambda boundary\n");
        let (_temp, app, catalog) = captured(&raw);
        let reader = catalog.verified_snapshot(None).unwrap();
        let document = reader
            .projection()
            .documents
            .iter()
            .find(|document| document.owner_revision.is_some())
            .unwrap();
        let selected =
            ByteSpan::new(prefix.len() as u64, (prefix.len() + middle.len()) as u64).unwrap();
        let winning = dense(document, selected);
        let view = SourceView::from_fs(app.fs()).unwrap();
        for bytes in [96, 128] {
            let hit = dense_hit_for_query(
                &reader,
                document,
                &winning,
                1,
                bytes,
                "quasar orbit lambda boundary",
            )
            .unwrap();
            let excerpt = &hit.excerpt;
            assert!(
                excerpt.text.contains("quasar and orbit align with lambda"),
                "{}",
                excerpt.text
            );
            assert!(!excerpt.text.contains("OUTSIDE"));
            assert!(excerpt.span.start() >= selected.start());
            assert!(excerpt.span.end() <= selected.end());
            assert!(excerpt.text.len() <= bytes);
            assert_eq!(excerpt.span.slice(&raw).unwrap(), excerpt.text);
            assert!(excerpt.matched_spans.len() <= 64);
            for span in &excerpt.matched_spans {
                assert!(span.start() >= excerpt.span.start());
                assert!(span.end() <= excerpt.span.end());
                span.slice(&raw).unwrap();
            }
            let CitationRef::Source(reference) = excerpt.citation.as_ref().unwrap() else {
                panic!("captured source must have a direct source citation");
            };
            assert_eq!(reference.span, excerpt.span);
            assert_eq!(
                reference.quote_hash,
                Blake3Hash::digest(excerpt.text.as_bytes())
            );
            assert_eq!(
                view.verify(excerpt.citation.as_ref().unwrap(), CitationScope::Current)
                    .unwrap()
                    .quote,
                excerpt.text.as_bytes()
            );
        }
        let fallback =
            dense_hit_for_query(&reader, document, &winning, 1, 97, "unmatchedidentifier").unwrap();
        assert_eq!(fallback.excerpt.span.start(), selected.start());
        assert!(fallback.excerpt.span.end() <= selected.end());
        assert!(fallback.excerpt.text.len() <= 97);
        assert_eq!(
            fallback.excerpt.span.slice(&raw).unwrap(),
            fallback.excerpt.text
        );
        let mut invalid = winning.clone();
        let inside_emoji = raw.find('🦀').unwrap() + 1;
        invalid.source_span = Some(ByteSpan::new(inside_emoji as u64, selected.end()).unwrap());
        assert!(dense_hit_for_query(&reader, document, &invalid, 1, 128, "quasar").is_err());
    }

    #[test]
    fn query_focus_does_not_promote_unverified_or_stale_source_to_citation() {
        let raw = "naïve café 中文 quasar orbit lambda\n";
        let (_temp, _app, catalog) = captured(raw);
        let reader = catalog.verified_snapshot(None).unwrap();
        let mut document = reader
            .projection()
            .documents
            .iter()
            .find(|document| document.owner_revision.is_some())
            .unwrap()
            .clone();
        let winning = dense(&document, ByteSpan::new(0, raw.len() as u64).unwrap());
        let unverified = catalog.index_snapshot().unwrap();
        let hit = dense_hit_for_query(&unverified, &document, &winning, 1, 128, "quasar").unwrap();
        assert_eq!(hit.excerpt.text, raw);
        assert!(hit.excerpt.citation.is_none());
        assert_eq!(hit.excerpt.label, ExcerptLabel::NoteText);
        document.eligibility = Eligibility::Stale;
        let hit = dense_hit_for_query(&reader, &document, &winning, 1, 128, "quasar").unwrap();
        assert!(hit.excerpt.citation.is_none());
        assert_eq!(hit.excerpt.label, ExcerptLabel::NoteText);
    }

    #[test]
    fn query_focus_keeps_identity_only_entity_description_empty() {
        let (_temp, app, catalog) = captured("An unrelated source.\n");
        std::fs::write(
            app.fs().root().path().join("knowledge/entities/identity.md"),
            "---\nwiki_schema: \"1\"\nwiki_id: entity_identity\nwiki_kind: entity\ntitle: Named identity\nwiki_status: active\nwiki_entity_type: concept\n---\nUnsupported quasar orbit lambda description.\n",
        ).unwrap();
        let writer = WriterPermit::acquire(app.fs().root(), Duration::from_secs(5)).unwrap();
        catalog.sync(&writer).unwrap();
        let reader = catalog.verified_snapshot(Some(&writer)).unwrap();
        let document = reader
            .projection()
            .documents
            .iter()
            .find(|document| {
                document
                    .record_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "entity_identity")
            })
            .unwrap();
        let start = document.raw_text.find("Unsupported").unwrap();
        let winning = dense(
            document,
            ByteSpan::new(start as u64, document.raw_text.len() as u64).unwrap(),
        );
        let hit = dense_hit_for_query(&reader, document, &winning, 1, 128, "quasar orbit lambda")
            .unwrap();
        assert_eq!(hit.identity_eligibility, Some(Eligibility::Current));
        assert_eq!(hit.eligibility, Eligibility::Unsupported);
        assert!(hit.excerpt.text.is_empty());
        assert!(hit.excerpt.span.is_empty());
        assert!(hit.excerpt.matched_spans.is_empty());
        assert!(hit.excerpt.citation.is_none());
    }
}

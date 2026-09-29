//! Current exact-source inspection with a disposable in-memory lexical index.
use super::{ResearchPassage, ResearchScope};
use crate::{
    catalog::Catalog,
    changes::ReadDependency,
    domain::*,
    sources::{CitationScope, SourceView},
    vault::{ExpectedState, VaultFs},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchInspection {
    pub passages: Vec<ResearchPassage>,
    pub records: Vec<RecordRef>,
    pub snapshot: ReadSnapshot,
    pub dependencies: Vec<ReadDependency>,
    pub warnings: Vec<String>,
}
pub fn inspect(
    fs: &VaultFs,
    vault: &RecordId,
    scope: &ResearchScope,
) -> Result<ResearchInspection> {
    super::plan::validate_scope(scope)?;
    let reader = Catalog::new(fs.clone(), vault.clone()).canonical_snapshot()?;
    let hits = crate::retrieval::search(&reader, &scope.question, &scope.limits.retrieval)?;
    let view = SourceView::from_fs_bounded(fs, 64 * 1024 * 1024, 4096)?;
    let mut passages = Vec::new();
    let mut dependencies = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut bytes = 0usize;
    let mut records = BTreeMap::new();
    let mut warnings = hits.warnings;
    for hit in hits.hits {
        if hit.eligibility != Eligibility::Current {
            continue;
        }
        if let Some(reference) = &hit.locator.record
            && reference.expected_kind == RecordKind::Page
            && records.len() < scope.limits.stage.max_proposals
        {
            dependencies.insert(
                hit.locator.path.clone(),
                ExpectedState::Hash(hit.locator.observed_hash.clone()),
            );
            records.insert(reference.record_id.clone(), reference.clone());
        }
        let Some(citation) = hit.excerpt.citation else {
            continue;
        };
        if !seen.insert(crate::graph::packet::canonical_json(&citation)?) {
            continue;
        }
        let verified = view.verify(&citation, CitationScope::Current)?;
        bytes = bytes
            .checked_add(verified.quote.len())
            .ok_or_else(|| WikiError::invalid("research passage bytes overflow"))?;
        if bytes > scope.limits.stage.max_text_bytes
            || passages.len() >= scope.limits.stage.max_citations
        {
            warnings.push("Existing source passages exceeded the research context limit".into());
            break;
        }
        for dep in &verified.dependencies {
            if dependencies
                .insert(dep.path.clone(), dep.expected.clone())
                .is_some_and(|old| old != dep.expected)
            {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "research inspection saw conflicting source states",
                ));
            }
        }
        passages.push(ResearchPassage {
            citation,
            quote: String::from_utf8(verified.quote)
                .map_err(|_| WikiError::invalid("research quotation UTF-8"))?,
            dependencies: verified.dependencies,
        });
    }
    let source_refs: Vec<_> = passages
        .iter()
        .flat_map(|p| match &p.citation {
            CitationRef::Source(s) => vec![
                (s.source_id.clone(), RecordKind::Source),
                (s.source_revision.clone(), RecordKind::Revision),
            ],
            CitationRef::Assertion(e) => vec![
                (e.source_id.clone(), RecordKind::Source),
                (e.source_revision.clone(), RecordKind::Revision),
                (e.evidence_id.clone(), RecordKind::Evidence),
                (e.assertion_id.clone(), RecordKind::Assertion),
            ],
        })
        .collect();
    for (id, kind) in source_refs {
        records.insert(
            id.clone(),
            RecordRef {
                vault_id: vault.clone(),
                record_id: id,
                expected_kind: kind,
            },
        );
    }
    Ok(ResearchInspection {
        passages,
        records: records.into_values().collect(),
        snapshot: reader.snapshot().clone(),
        dependencies: dependencies
            .into_iter()
            .map(|(path, expected)| ReadDependency { path, expected })
            .collect(),
        warnings,
    })
}

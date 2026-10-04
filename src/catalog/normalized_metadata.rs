//! Shared derivation of final normalized metadata for construction and explicit checking.
use super::{
    eligibility_facts::{EligibilityBaseline, EligibilityEdge, NormalizedEligibilityFacts},
    eligibility_rules::OppositionKey,
    link_facts::MatchKey,
    structural_rules::StructuralFact,
    types::ValidationProjection,
};
use crate::{
    domain::{Eligibility, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    vault::ExpectedState,
};

pub(crate) enum MetadataRow<'a> {
    AssertionNavigation {
        assertion: &'a RecordId,
        key: &'a MatchKey,
    },
    Opposition {
        assertion: &'a RecordId,
        key: &'a OppositionKey,
        negated: bool,
    },
    Baseline {
        id: &'a RecordId,
        baseline: &'a EligibilityBaseline,
        structural: &'a StructuralFact,
    },
    DirectPath {
        owner: &'a RecordId,
        path: &'a VaultRelativePath,
    },
    SemanticEdge(&'a EligibilityEdge),
    SourceRevision {
        source: &'a RecordId,
        revision: &'a RecordId,
        ordinal: usize,
        original: &'a str,
        content: Option<&'a str>,
        fingerprint: &'a str,
        status: &'a str,
    },
    SourceEvidence {
        source: &'a str,
        evidence: &'a RecordId,
        assertion: &'a str,
    },
}

pub(crate) trait MetadataSink {
    fn progress(&mut self) -> Result<()>;
    fn row(&mut self, row: MetadataRow<'_>) -> Result<()>;
}

// Keep derivation separate from persistence; callbacks borrow one row at a time.
pub(crate) fn visit_eligibility_rows(
    projection: &ValidationProjection,
    facts: &NormalizedEligibilityFacts,
    sink: &mut dyn MetadataSink,
) -> Result<()> {
    sink.progress()?;
    if facts.version != 2
        || facts.records.keys().ne(projection.records.keys())
        || projection
            .records
            .values()
            .any(|row| !row.dependencies.is_empty())
    {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "normalized proof layout or record scope is incomplete",
        ));
    }
    let mut expected = std::collections::BTreeMap::new();
    for dependency in &projection.dependencies {
        sink.progress()?;
        expected.insert(&dependency.path, &dependency.expected);
    }
    for (path, observed) in &facts.observed {
        sink.progress()?;
        if expected.get(path).copied() != Some(observed) {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "normalized observation differs from complete dependency inventory",
            ));
        }
    }
    for (id, fact) in &facts.records {
        sink.progress()?;
        let record = &projection.records[id];
        let own = ExpectedState::Hash(record.hash.clone());
        if !fact.direct_paths.contains(&record.path)
            || expected.get(&record.path).copied() != Some(&own)
        {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "normalized record lacks its exact own canonical state",
            ));
        }
        if record.record.kind() == RecordKind::Assertion {
            let mut seen = std::collections::BTreeSet::new();
            for destination in super::scan::list(&record.record, "wiki_evidence") {
                sink.progress()?;
                let resolution = match crate::records::links::untyped_lookup(&destination) {
                    crate::records::links::UntypedLookup::External => {
                        crate::records::LinkResolution::External
                    }
                    _ => crate::records::LinkResolution::Missing,
                };
                let fact =
                    super::link_facts::untyped_fact(&record.path, 0, &destination, &resolution)?;
                for key in fact.keys {
                    sink.progress()?;
                    if !seen.insert(key.clone()) {
                        continue;
                    }
                    sink.row(MetadataRow::AssertionNavigation {
                        assertion: id,
                        key: &key,
                    })?;
                }
            }
        }
        if record.record.kind() == RecordKind::Assertion
            && record.record.string("wiki_status") == Some("accepted")
            && let Some((key, negated)) = super::eligibility::opposition_key(&record.record)
        {
            sink.row(MetadataRow::Opposition {
                assertion: id,
                key: &key,
                negated,
            })?;
        }
        sink.row(MetadataRow::Baseline {
            id,
            baseline: &fact.baseline,
            structural: &fact.structural,
        })?;
        for path in &fact.direct_paths {
            sink.progress()?;
            if !expected.contains_key(path)
                || (path != &record.path && crate::sources::revision::canonical_path(path))
            {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "normalized direct path lacks state or captures another canonical note",
                ));
            }
            sink.row(MetadataRow::DirectPath { owner: id, path })?;
        }
    }
    for edge in &facts.edges {
        sink.progress()?;
        if !projection.records.contains_key(&edge.owner_id) {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "normalized semantic edge has unknown owner",
            ));
        }
        // Missing targets remain explicit facts for structurally invalid records.
        sink.row(MetadataRow::SemanticEdge(edge))?;
    }
    Ok(())
}

pub(crate) fn visit_refresh_rows(
    projection: &ValidationProjection,
    sink: &mut dyn MetadataSink,
) -> Result<()> {
    // Rebuild work may inspect complete metadata once; refresh keeps source-local indexes.
    for source in projection.records.values() {
        sink.progress()?;
        if source.record.kind() != RecordKind::Source || source.eligibility == Eligibility::Invalid
        {
            continue;
        }
        let revisions = source
            .record
            .field("wiki_revisions")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "source revision list missing from validated projection",
                )
            })?;
        for (ordinal, value) in revisions.iter().enumerate() {
            sink.progress()?;
            let revision_id = RecordId::new(value.as_str().ok_or_else(|| {
                WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "source revision identity is not text",
                )
            })?)?;
            let row = projection
                .records
                .get(&revision_id)
                .filter(|row| {
                    row.record.kind() == RecordKind::Revision
                        && row.record.string("wiki_source_id") == Some(source.record.id().as_str())
                })
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "validated retained revision identity is missing or foreign",
                    )
                })?;
            let required = |field| {
                row.record.string(field).ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::IndexCorrupt,
                        format!("validated revision lacks {field}"),
                    )
                })
            };
            let original = required("wiki_original_hash")?;
            let content = row.record.string("wiki_content_hash");
            let fingerprint = required("wiki_extractor_fingerprint")?;
            let status = required("wiki_extraction_status")?;
            sink.row(MetadataRow::SourceRevision {
                source: source.record.id(),
                revision: &revision_id,
                ordinal,
                original,
                content,
                fingerprint,
                status,
            })?;
        }
    }
    for row in projection.records.values() {
        sink.progress()?;
        if row.record.kind() != RecordKind::Evidence {
            continue;
        }
        let source = row.record.string("wiki_source_id").ok_or_else(|| {
            WikiError::new(ErrorCode::IndexCorrupt, "evidence lacks source identity")
        })?;
        let assertion = row.record.string("wiki_assertion_id").ok_or_else(|| {
            WikiError::new(ErrorCode::IndexCorrupt, "evidence lacks assertion identity")
        })?;
        sink.row(MetadataRow::SourceEvidence {
            source,
            evidence: row.record.id(),
            assertion,
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "newmetadata_tests.rs"]
mod tests;

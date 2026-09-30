use super::{codec::invalid, types::*};
use crate::{
    catalog::Catalog,
    changes::ReadDependency,
    domain::*,
    sources::{CitationScope, CitationState, SourceView},
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn inspect(
    fs: &VaultFs,
    vault: &RecordId,
    scope: &ResearchScope,
) -> Result<Vec<ResearchPassage>> {
    let view = SourceView::from_fs_bounded(fs, 64 * 1024 * 1024, 4096)?;
    let mut citations = vec![];
    for source in &scope.source_ids {
        citations.extend(source_citations(
            &view,
            source,
            &scope.question,
            &scope.source_ranges,
        )?);
    }
    let reader = Catalog::new(fs.clone(), vault.clone()).canonical_snapshot()?;
    let hits = crate::retrieval::search(
        &reader,
        &scope.question,
        &crate::retrieval::QueryPlan::default(),
    )?;
    citations.extend(
        hits.hits
            .into_iter()
            .filter(|h| h.eligibility == Eligibility::Current)
            .filter_map(|h| h.excerpt.citation),
    );
    let mut seen = BTreeSet::new();
    let mut passages = vec![];
    for citation in citations {
        if !seen.insert(crate::graph::packet::canonical_json(&citation)?) {
            continue;
        }
        let verified = view.verify(&citation, CitationScope::Current)?;
        passages.push(ResearchPassage {
            passage_id: format!("p{}", passages.len() + 1),
            citation,
            quote: String::from_utf8(verified.quote)
                .map_err(|_| invalid("research source UTF-8"))?,
            dependencies: verified.dependencies,
        });
    }
    Ok(passages)
}
fn source_citations(
    view: &SourceView<'_>,
    source: &RecordId,
    question: &str,
    ranges: &[ResearchSourceRange],
) -> Result<Vec<CitationRef>> {
    let (_, note) = view.resolve(source, RecordKind::Source, None)?;
    let revision = RecordId::new(
        note.canonical
            .as_ref()
            .and_then(|r| r.string("wiki_current_revision"))
            .ok_or_else(|| invalid("source head missing"))?,
    )?;
    let content = view.revision_content_bounded(
        source,
        &revision,
        &mut BTreeMap::new(),
        1024 * 1024,
        1024 * 1024,
    )?;
    let text = std::str::from_utf8(&content).map_err(|_| invalid("source is not UTF-8"))?;
    let selected: Vec<_> = ranges
        .iter()
        .filter(|r| &r.source_id == source)
        .map(|r| r.span)
        .collect();
    let spans = if selected.is_empty() {
        relevant_spans(text, question)?
    } else {
        selected
    };
    spans
        .into_iter()
        .map(|span| {
            let quote = span
                .slice(text)
                .map_err(|_| invalid("research source range is outside current UTF-8 revision"))?;
            if quote.is_empty() || quote.len() > 4096 {
                return Err(invalid("research source range exceeds passage ceiling"));
            }
            Ok(CitationRef::Source(SourceSpanRef {
                source_id: source.clone(),
                source_revision: revision.clone(),
                span,
                quote_hash: Blake3Hash::digest(quote.as_bytes()),
            }))
        })
        .collect()
}

/// Deterministic bounded local selection; every output remains an exact byte slice.
pub(crate) fn relevant_spans(text: &str, question: &str) -> Result<Vec<ByteSpan>> {
    if text.is_empty() {
        return Ok(vec![]);
    }
    let terms: BTreeSet<String> = question
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| term.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    let mut candidates = Vec::new();
    let mut start = 0usize;
    while start < text.len() {
        let mut end = (start + 4096).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            return Err(invalid("research UTF-8 selection cannot advance"));
        }
        let folded = text[start..end].to_lowercase();
        let score = terms
            .iter()
            .filter(|term| folded.contains(term.as_str()))
            .count();
        candidates.push((score, start, end));
        start = end;
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let best = candidates.first().map(|c| c.0).unwrap_or(0);
    let take = if best == 0 { 1 } else { 2 };
    candidates
        .into_iter()
        .filter(|c| best == 0 || c.0 > 0)
        .take(take)
        .map(|(_, start, end)| ByteSpan::new(start as u64, end as u64))
        .collect()
}
pub(crate) fn verify(fs: &VaultFs, packet: &ResearchPacket) -> Result<Vec<ReadDependency>> {
    let view = SourceView::from_fs_bounded(fs, 64 * 1024 * 1024, 4096)?;
    let mut dependencies = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut total = 0;
    for passage in &packet.passages {
        total += passage.quote.len();
        if total > MAX_PASSAGE_BYTES || !ids.insert(&passage.passage_id) {
            return Err(invalid("packet passage bounds or identity differs"));
        }
        let proof = view
            .verify(&passage.citation, CitationScope::Current)
            .map_err(stale)?;
        if proof.quote != passage.quote.as_bytes() {
            return Err(stale(invalid("packet quotation changed")));
        }
        for dependency in &passage.dependencies {
            if !matches!(&dependency.expected, ExpectedState::Hash(hash) if crate::changes::prepare::read_bounded(fs, &dependency.path, 64 * 1024 * 1024)?.is_some_and(|b| &Blake3Hash::digest(b) == hash))
            {
                return Err(stale(invalid("packet dependency changed")));
            }
        }
        for dependency in proof.dependencies {
            if dependencies
                .insert(dependency.path, dependency.expected.clone())
                .is_some_and(|old| old != dependency.expected)
            {
                return Err(stale(invalid("inconsistent packet dependencies")));
            }
        }
    }
    Ok(dependencies
        .into_iter()
        .map(|(path, expected)| ReadDependency { path, expected })
        .collect())
}
/// Read-time status of retained citations. The report itself always remains history.
pub(crate) fn report_citations(fs: &VaultFs, report: &ResearchReport) -> serde_json::Value {
    let view = match SourceView::from_fs_bounded(fs, 64 * 1024 * 1024, 4096) {
        Ok(view) => view,
        Err(_) => {
            return serde_json::json!({"state":"unavailable","current":0,"historical":0,"withdrawn":0,"invalid":0,"citations":[]});
        }
    };
    let mut current = 0usize;
    let mut historical = 0usize;
    let mut withdrawn = 0usize;
    let mut invalid_count = 0usize;
    let mut citations = Vec::new();
    for (claim_index, claim) in report.claims.iter().enumerate() {
        for citation in &claim.citations {
            let (source_id, revision_id) = match citation {
                CitationRef::Source(value) => (&value.source_id, &value.source_revision),
                CitationRef::Assertion(value) => (&value.source_id, &value.source_revision),
            };
            let state = match view.verify(citation, CitationScope::Historical) {
                Ok(proof) => match proof.state {
                    CitationState::Current => {
                        current += 1;
                        "current"
                    }
                    CitationState::Historical => {
                        historical += 1;
                        "historical"
                    }
                    CitationState::Withdrawn => {
                        withdrawn += 1;
                        "withdrawn"
                    }
                },
                Err(_) => {
                    invalid_count += 1;
                    "invalid"
                }
            };
            citations.push(serde_json::json!({
                "claim_index": claim_index,
                "source_id": source_id,
                "source_revision": revision_id,
                "state": state,
            }));
        }
    }
    serde_json::json!({
        "state": if invalid_count > 0 { "invalid" } else if withdrawn > 0 || historical > 0 { "stale" } else { "current" },
        "current": current,
        "historical": historical,
        "withdrawn": withdrawn,
        "invalid": invalid_count,
        "citations": citations,
    })
}
fn stale(error: WikiError) -> WikiError {
    WikiError::new(
        ErrorCode::FreshnessConflict,
        format!(
            "Research packet is stale; use research resume RUN --refresh to replace it: {}",
            error.message
        ),
    )
}

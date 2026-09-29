use super::{codec::invalid, types::*};
use crate::{
    catalog::Catalog,
    changes::ReadDependency,
    domain::*,
    sources::{CitationScope, SourceView},
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
        citations.push(source_citation(&view, source)?);
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
    let mut total = 0;
    for citation in citations {
        if !seen.insert(crate::graph::packet::canonical_json(&citation)?) {
            continue;
        }
        let verified = view.verify(&citation, CitationScope::Current)?;
        if passages.len() == MAX_PASSAGES || total + verified.quote.len() > MAX_PASSAGE_BYTES {
            break;
        }
        total += verified.quote.len();
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
fn source_citation(view: &SourceView<'_>, source: &RecordId) -> Result<CitationRef> {
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
    let mut end = text.len().min(4096);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(CitationRef::Source(SourceSpanRef {
        source_id: source.clone(),
        source_revision: revision,
        span: ByteSpan::new(0, end as u64)?,
        quote_hash: Blake3Hash::digest(&content[..end]),
    }))
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
fn stale(error: WikiError) -> WikiError {
    WikiError::new(
        ErrorCode::FreshnessConflict,
        format!(
            "Research packet is stale; use research resume RUN --refresh to replace it: {}",
            error.message
        ),
    )
}

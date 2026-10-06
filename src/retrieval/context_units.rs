//! Retained exact embedding units, ranked before canonical parent expansion.
//! These scores guide selection; assembly alone authenticates emitted evidence.
use super::{
    context_types::ContextSemanticCue,
    excerpts::{SourceMap, Tokenizer},
    types::{MAX_CONTEXT_QUERY_TERMS, RankContribution},
};
use crate::{
    catalog::{DocumentRow, query_types::QueryCatalog},
    domain::*,
    records::parse_note,
};
use pulldown_cmark::{Event, Parser, Tag};
use std::{collections::BTreeMap, ops::Range};

const OWNER_SCAN_BYTES: usize = 1024 * 1024;
const TOTAL_SCAN_BYTES: usize = 4 * OWNER_SCAN_BYTES;
const MAX_STARTS: usize = 4096;
const MAX_UNITS: usize = 4096;
const MAX_CANDIDATES: usize = 160;

pub(crate) struct UnitDocument<'a> {
    pub owner_index: usize,
    pub document: &'a DocumentRow,
}
#[derive(Clone, Debug)]
pub(crate) struct UnitCandidate {
    pub owner_index: usize,
    /// Exact retained unit before focusing or structural-parent deduplication.
    pub origin_span: ByteSpan,
    pub origin_cosine: f64,
    pub parent_span: ByteSpan,
    pub child_span: ByteSpan,
    pub score: f64,
    pub rank_contributions: Vec<RankContribution>,
    pub clipped: bool,
}
pub(crate) struct UnitOmission {
    pub owner_index: usize,
    pub reason: &'static str,
}
pub(crate) struct UnitSelection {
    pub candidates: Vec<UnitCandidate>,
    pub omissions: Vec<UnitOmission>,
    /// Bounded original owner bytes supplied to structural parsing.
    pub scanned_bytes: usize,
    pub scanned_blocks: usize,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Prose,
    Code,
    List,
    Heading,
}
struct Block {
    range: Range<usize>,
    kind: Kind,
    complete: bool,
}
struct Owner<'a> {
    input: &'a UnitDocument<'a>,
    end: usize,
    blocks: Vec<Block>,
}
struct RankedUnit {
    owner: usize,
    span: ByteSpan,
    cosine: f64,
    terms: Option<BTreeMap<String, usize>>,
    length: usize,
    contributions: Vec<RankContribution>,
    score: f64,
}

fn boundary_before(raw: &str, mut end: usize) -> usize {
    end = end.min(raw.len());
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    end
}
fn body_start(document: &DocumentRow) -> usize {
    if document.owner_revision.is_some() {
        return 0;
    }
    let raw = &document.raw_text;
    let end = boundary_before(
        raw,
        raw.len()
            .min(crate::records::ParseLimits::default().max_envelope_bytes + 16),
    );
    parse_note(&raw.as_bytes()[..end]).body_start
}
fn omission(result: &mut UnitSelection, owner_index: usize, reason: &'static str) {
    if !result
        .omissions
        .iter()
        .any(|item| item.owner_index == owner_index && item.reason == reason)
    {
        result.omissions.push(UnitOmission {
            owner_index,
            reason,
        });
    }
}

/// Parse one bounded owner prefix. Count every structural start, including
/// nested inline starts, so deeply nested input cannot bypass the work cap.
fn blocks(raw: &str, body: usize, end: usize, limit: usize) -> (Vec<Block>, usize, usize, bool) {
    let mut found = Vec::new();
    let mut starts = 0;
    let mut usable_end = end;
    let mut limited = false;
    for (event, range) in Parser::new(&raw[body..end]).into_offset_iter() {
        let Event::Start(tag) = event else {
            continue;
        };
        if starts == limit {
            limited = true;
            usable_end = body + range.start;
            break;
        }
        starts += 1;
        let kind = match tag {
            Tag::Paragraph => Kind::Prose,
            Tag::CodeBlock(_) => Kind::Code,
            Tag::List(_) => Kind::List,
            Tag::Heading { .. } => Kind::Heading,
            _ => continue,
        };
        let range = body + range.start..body + range.end;
        if range.is_empty()
            || found
                .last()
                .is_some_and(|parent: &Block| parent.range.end >= range.end)
        {
            continue;
        }
        let complete = range.end < end || end == raw.len();
        found.push(Block {
            range,
            kind,
            complete,
        });
    }
    // A containing block may have been discovered before a nested start hit
    // the cap. Do not mistake its unparsed suffix for a complete parent.
    found.retain(|block| block.range.end <= usable_end);
    (found, starts, usable_end, limited)
}

#[cfg(test)]
pub(super) struct LocationBlockForTest {
    pub span: ByteSpan,
    pub kind: &'static str,
    pub complete: bool,
}

#[cfg(test)]
pub(super) fn location_blocks_for_test(
    document: &DocumentRow,
    max_bytes: usize,
    max_starts: usize,
) -> Result<(Vec<LocationBlockForTest>, usize, usize, usize, bool)> {
    if max_bytes > OWNER_SCAN_BYTES || max_starts > MAX_STARTS {
        return Err(WikiError::invalid(
            "location structural scan exceeds native limits",
        ));
    }
    let raw = &document.raw_text;
    let body = body_start(document);
    let end = boundary_before(raw, body.saturating_add(max_bytes));
    let (parsed, starts, usable_end, limited) = blocks(raw, body, end, max_starts);
    let parsed = parsed
        .into_iter()
        .map(|block| {
            Ok(LocationBlockForTest {
                span: ByteSpan::new(block.range.start as u64, block.range.end as u64)?,
                kind: match block.kind {
                    Kind::Prose => "prose",
                    Kind::Code => "code",
                    Kind::List => "list",
                    Kind::Heading => "heading",
                },
                complete: block.complete,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((
        parsed,
        starts,
        end.saturating_sub(body),
        usable_end,
        limited,
    ))
}

/// Canonical grouping is structural and independent of query/unit scores.
/// A fitting introductory paragraph, command and terminal explanation form
/// one parent; headings always end the group. Lists keep their introduction.
fn group_blocks(blocks: Vec<Block>, bytes: usize) -> Vec<Block> {
    let mut grouped = Vec::new();
    let mut pending = blocks.into_iter().peekable();
    while let Some(mut block) = pending.next() {
        if block.kind == Kind::Prose
            && block.complete
            && pending.peek().is_some_and(|next| {
                matches!(next.kind, Kind::Code | Kind::List)
                    && next.complete
                    && next.range.end - block.range.start <= bytes
            })
        {
            let next = pending.next().expect("peeked block");
            block.range.end = next.range.end;
            block.kind = next.kind;
        }
        if block.kind == Kind::Code && block.complete {
            while pending.peek().is_some_and(|next| {
                matches!(next.kind, Kind::Prose | Kind::Code)
                    && next.complete
                    && next.range.end - block.range.start <= bytes
            }) {
                let next = pending.next().expect("peeked block");
                block.range.end = next.range.end;
                if next.kind == Kind::Prose
                    && !pending.peek().is_some_and(|after| after.kind == Kind::Code)
                {
                    break;
                }
            }
        }
        grouped.push(block);
    }
    grouped
}

fn validated_units(
    document: &DocumentRow,
    body: usize,
    cues: &[ContextSemanticCue],
) -> Result<Vec<(ByteSpan, f64)>> {
    let mut unique = BTreeMap::new();
    for cue in cues.iter().filter(|cue| cue.owner == document.path) {
        if cue.observed_hash != document.hash {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "context unit owner hash differs from pinned document",
            ));
        }
        if !cue.cosine.is_finite() || !(-1.0..=1.0).contains(&cue.cosine) {
            return Err(WikiError::invalid("context unit cosine invalid"));
        }
        cue.span.slice(&document.raw_text)?;
        if cue.span.is_empty() || cue.span.start() < body as u64 {
            return Err(WikiError::invalid(
                "context unit span outside document body",
            ));
        }
        if let Some(previous) = unique.insert((cue.span.start(), cue.span.end()), cue.cosine)
            && previous != cue.cosine
        {
            return Err(WikiError::invalid("conflicting duplicate context unit"));
        }
    }
    unique
        .into_iter()
        .map(|((start, end), cosine)| Ok((ByteSpan::new(start, end)?, cosine)))
        .collect()
}

fn unit_tie(a: &RankedUnit, b: &RankedUnit, owners: &[Owner<'_>]) -> std::cmp::Ordering {
    owners[a.owner]
        .input
        .document
        .path
        .cmp(&owners[b.owner].input.document.path)
        .then(a.span.start().cmp(&b.span.start()))
        .then(a.span.end().cmp(&b.span.end()))
}

/// Conventional single-field BM25: Robertson's positive IDF, k1=1.2,b=.75.
/// Dense/BM25 ranks are diagnostics. Selection preserves cosine order, using
/// positive BM25 only for exact cosine ties, then stable original-byte ties.
fn rank_units(units: &mut [RankedUnit], owners: &[Owner<'_>], query_terms: &[String]) {
    let mut dense = (0..units.len()).collect::<Vec<_>>();
    dense.sort_by(|&a, &b| {
        units[b]
            .cosine
            .partial_cmp(&units[a].cosine)
            .expect("validated finite cosine")
            .then_with(|| unit_tie(&units[a], &units[b], owners))
    });
    for (rank, index) in dense.into_iter().enumerate() {
        let cosine = units[index].cosine;
        units[index].contributions.push(RankContribution {
            channel: "context_unit_dense".into(),
            rank: rank + 1,
            score: Some(cosine),
        });
    }
    let measured = units.iter().filter(|unit| unit.terms.is_some()).count();
    let mut lexical_scores = vec![0.0; units.len()];
    if measured > 0 {
        let average = units.iter().map(|unit| unit.length).sum::<usize>() as f64 / measured as f64;
        let frequencies = query_terms
            .iter()
            .map(|term| {
                units
                    .iter()
                    .filter(|unit| {
                        unit.terms
                            .as_ref()
                            .is_some_and(|terms| terms.contains_key(term))
                    })
                    .count()
            })
            .collect::<Vec<_>>();
        let mut lexical = Vec::new();
        for (index, unit) in units.iter().enumerate() {
            let Some(terms) = &unit.terms else { continue };
            let mut score = 0.0;
            for (term, &df) in query_terms.iter().zip(&frequencies) {
                let Some(&tf) = terms.get(term) else { continue };
                let tf = tf as f64;
                let idf = (1.0 + (measured as f64 - df as f64 + 0.5) / (df as f64 + 0.5)).ln();
                let norm = if average > 0.0 {
                    unit.length as f64 / average
                } else {
                    0.0
                };
                score += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * norm));
            }
            if score > 0.0 {
                lexical_scores[index] = score;
                lexical.push((index, score));
            }
        }
        lexical.sort_by(|&(a, score_a), &(b, score_b)| {
            score_b
                .total_cmp(&score_a)
                .then_with(|| unit_tie(&units[a], &units[b], owners))
        });
        for (rank, (index, score)) in lexical.into_iter().enumerate() {
            units[index].contributions.push(RankContribution {
                channel: "context_unit_bm25".into(),
                rank: rank + 1,
                score: Some(score),
            });
        }
    }
    let mut selection = (0..units.len()).collect::<Vec<_>>();
    selection.sort_by(|&a, &b| {
        units[b]
            .cosine
            .partial_cmp(&units[a].cosine)
            .expect("validated finite cosine")
            .then_with(|| lexical_scores[b].total_cmp(&lexical_scores[a]))
            .then_with(|| unit_tie(&units[a], &units[b], owners))
    });
    for (rank, index) in selection.into_iter().enumerate() {
        let selection_rank = rank + 1;
        // One ranked evidence-selection channel on the existing reciprocal
        // scale; diagnostic channels must not manufacture selection votes.
        let score = 1.0 / (60.0 + selection_rank as f64);
        units[index].score = score;
        units[index].contributions.push(RankContribution {
            channel: "context_unit_selection".into(),
            rank: selection_rank,
            score: Some(score),
        });
    }
}

fn parent_span(owner: &Owner<'_>, child: ByteSpan, bytes: usize) -> Result<(ByteSpan, bool)> {
    let start = child.start() as usize;
    let end = child.end() as usize;
    let intersecting = owner
        .blocks
        .iter()
        .filter(|block| block.range.start < end && block.range.end > start)
        .collect::<Vec<_>>();
    let Some(first) = intersecting.first() else {
        return Ok((child, true));
    };
    let last = intersecting.last().expect("nonempty intersection");
    let parent_start = first.range.start.min(start);
    let parent_end = last.range.end.max(end);
    if parent_end - parent_start > bytes || intersecting.iter().any(|block| !block.complete) {
        return Ok((child, true));
    }
    Ok((
        ByteSpan::new(parent_start as u64, parent_end as u64)?,
        false,
    ))
}

pub(crate) fn select_units(
    reader: &dyn QueryCatalog,
    query: &str,
    documents: &[UnitDocument<'_>],
    cues: &[ContextSemanticCue],
    max_excerpt_bytes: usize,
    candidate_limit: usize,
) -> Result<UnitSelection> {
    if max_excerpt_bytes == 0 || max_excerpt_bytes > 2048 {
        return Err(WikiError::invalid(
            "context unit excerpt bound must be 1..=2048 bytes",
        ));
    }
    if candidate_limit == 0 || candidate_limit > MAX_CANDIDATES {
        return Err(WikiError::invalid(
            "context unit candidate bound must be 1..=160",
        ));
    }
    if cues.len() > MAX_UNITS {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "context unit cap exceeded",
        ));
    }
    super::lexical::validate_query(query)?;
    let tokenizer = Tokenizer::new(reader.connection())?;
    let all_terms = tokenizer
        .tokens(query)?
        .into_iter()
        .map(|token| token.text)
        .collect::<std::collections::BTreeSet<_>>();
    let term_cap = all_terms.len() > MAX_CONTEXT_QUERY_TERMS;
    let query_terms = all_terms
        .into_iter()
        .take(MAX_CONTEXT_QUERY_TERMS)
        .collect::<Vec<_>>();
    let mut result = UnitSelection {
        candidates: vec![],
        omissions: vec![],
        scanned_bytes: 0,
        scanned_blocks: 0,
    };
    let mut owners = Vec::new();
    let mut units = Vec::new();
    let mut lexical_bytes = 0;
    let mut unique_owners = std::collections::BTreeSet::new();
    let mut unique_indices = std::collections::BTreeSet::new();
    for input in documents {
        if !unique_owners.insert(&input.document.path) || !unique_indices.insert(input.owner_index)
        {
            return Err(WikiError::invalid("duplicate context unit owner"));
        }
        let body = body_start(input.document);
        let eligible_units = validated_units(input.document, body, cues)?;
        if eligible_units.is_empty() {
            continue;
        }
        if term_cap {
            omission(&mut result, input.owner_index, "context_query_term_cap");
        }
        let raw = &input.document.raw_text;
        let capacity = OWNER_SCAN_BYTES.min(TOTAL_SCAN_BYTES - result.scanned_bytes);
        let end = boundary_before(raw, body.saturating_add(capacity));
        let (parsed, starts, usable_end, limited) =
            blocks(raw, body, end, MAX_STARTS - result.scanned_blocks);
        result.scanned_bytes += end - body;
        result.scanned_blocks += starts;
        if end < raw.len() {
            omission(
                &mut result,
                input.owner_index,
                "context_source_scan_byte_cap",
            );
        }
        if limited {
            omission(
                &mut result,
                input.owner_index,
                "context_source_scan_block_cap",
            );
        }
        let owner = owners.len();
        owners.push(Owner {
            input,
            end: usable_end,
            blocks: group_blocks(parsed, max_excerpt_bytes),
        });
        let mut owner_lexical_bytes = 0;
        for (span, cosine) in eligible_units {
            if span.end() > owners[owner].end as u64 {
                omission(
                    &mut result,
                    input.owner_index,
                    "context_unit_outside_scanned_body",
                );
                #[cfg(test)]
                super::context::record_lineage_event("unit_parent_decisions", || {
                    serde_json::json!({
                    "owner_index": input.owner_index, "owner": input.document.path,
                    "owner_hash": input.document.hash, "scored_unit_span": span, "cosine": cosine,
                    "parent_span": null, "child_span": null,
                    "outcome": "context_unit_outside_scanned_body"})
                });
                continue;
            }
            let text = span.slice(raw)?;
            let terms = if text.len() <= OWNER_SCAN_BYTES - owner_lexical_bytes
                && text.len() <= TOTAL_SCAN_BYTES - lexical_bytes
            {
                owner_lexical_bytes += text.len();
                lexical_bytes += text.len();
                let readable = SourceMap::markdown(text, 0);
                let mut counts = BTreeMap::new();
                for token in tokenizer.tokens(&readable.text)? {
                    *counts.entry(token.text).or_insert(0) += 1;
                }
                Some(counts)
            } else {
                omission(
                    &mut result,
                    input.owner_index,
                    "context_unit_lexical_scan_byte_cap",
                );
                None
            };
            let length = terms.as_ref().map_or(0, |terms| terms.values().sum());
            units.push(RankedUnit {
                owner,
                span,
                cosine,
                terms,
                length,
                contributions: vec![],
                score: 0.0,
            });
        }
    }
    rank_units(&mut units, &owners, &query_terms);
    units.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| unit_tie(a, b, &owners))
    });
    let mut parents = std::collections::BTreeSet::new();
    let mut focus_bytes = 0;
    let mut owner_focus_bytes = vec![0; owners.len()];
    for unit in units {
        let owner = &owners[unit.owner];
        let child = if unit.span.len() > max_excerpt_bytes as u64 {
            let bytes = unit.span.len() as usize;
            if bytes > OWNER_SCAN_BYTES - owner_focus_bytes[unit.owner]
                || bytes > TOTAL_SCAN_BYTES - focus_bytes
            {
                omission(
                    &mut result,
                    owner.input.owner_index,
                    "context_unit_focus_scan_byte_cap",
                );
                #[cfg(test)]
                trace_unit_decision(
                    owner,
                    &unit,
                    None,
                    None,
                    "context_unit_focus_scan_byte_cap",
                    result.candidates.len(),
                    candidate_limit,
                );
                continue;
            }
            owner_focus_bytes[unit.owner] += bytes;
            focus_bytes += bytes;
            super::lexical::focused_excerpt(
                reader,
                owner.input.document,
                query,
                unit.span,
                max_excerpt_bytes,
            )?
            .span
        } else {
            unit.span
        };
        if child.is_empty() {
            omission(
                &mut result,
                owner.input.owner_index,
                "context_unit_excerpt_empty",
            );
            #[cfg(test)]
            trace_unit_decision(
                owner,
                &unit,
                None,
                None,
                "context_unit_excerpt_empty",
                result.candidates.len(),
                candidate_limit,
            );
            continue;
        }
        let (parent, clipped) = if child != unit.span {
            (child, true)
        } else {
            parent_span(owner, child, max_excerpt_bytes)?
        };
        // Units are already in score order: retain the strongest representative
        // and its fallback, without adding votes from another matching child.
        if !parents.insert((unit.owner, parent.start(), parent.end())) {
            #[cfg(test)]
            trace_unit_decision(
                owner,
                &unit,
                Some(child),
                Some(parent),
                "parent_deduplicated",
                result.candidates.len(),
                candidate_limit,
            );
            continue;
        }
        if result.candidates.len() == candidate_limit {
            omission(
                &mut result,
                owner.input.owner_index,
                "context_unit_candidate_cap",
            );
            #[cfg(test)]
            trace_unit_decision(
                owner,
                &unit,
                Some(child),
                Some(parent),
                "context_unit_candidate_cap",
                result.candidates.len(),
                candidate_limit,
            );
            continue;
        }
        #[cfg(test)]
        trace_unit_decision(
            owner,
            &unit,
            Some(child),
            Some(parent),
            "retained_parent",
            result.candidates.len(),
            candidate_limit,
        );
        result.candidates.push(UnitCandidate {
            owner_index: owner.input.owner_index,
            origin_span: unit.span,
            origin_cosine: unit.cosine,
            parent_span: parent,
            child_span: child,
            score: unit.score,
            rank_contributions: unit.contributions,
            clipped,
        });
    }
    Ok(result)
}

#[cfg(test)]
fn trace_unit_decision(
    owner: &Owner<'_>,
    unit: &RankedUnit,
    child: Option<ByteSpan>,
    parent: Option<ByteSpan>,
    outcome: &str,
    candidate_count: usize,
    candidate_limit: usize,
) {
    super::context::record_lineage_event("unit_parent_decisions", || {
        let key = parent.map(|p| {
            format!(
                "unit:{}:{:020}:{:020}",
                owner.input.document.path,
                p.start(),
                p.end()
            )
        });
        serde_json::json!({"owner_index": owner.input.owner_index, "owner": owner.input.document.path,
            "owner_hash": owner.input.document.hash, "scored_unit_span": unit.span,
            "cosine": unit.cosine, "score": unit.score, "rank_contributions": unit.contributions,
            "child_span": child, "parent_span": parent, "parent_key": key,
            "parent_proposal_id": parent.zip(key.as_ref()).map(|(p,k)| super::context::lineage_proposal_id(k, &owner.input.document.path, &owner.input.document.hash, p)),
            "child_proposal_id": child.zip(key.as_ref()).map(|(p,k)| super::context::lineage_proposal_id(&format!("{k}:child"), &owner.input.document.path, &owner.input.document.hash, p)),
            "outcome": outcome, "candidate_count_before": candidate_count, "candidate_limit": candidate_limit})
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ReaderSnapshot;
    use crate::{
        app::{OfflineApp, OperationOptions, offline},
        catalog::Catalog,
        sources::{CaptureRequest, ExtractionInput, SourceOrigin},
        vault::{VaultFs, VaultRoot},
    };

    fn fixture(raw: &str) -> (tempfile::TempDir, ReaderSnapshot, DocumentRow) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        offline::init(&root, "Unit fixture", OperationOptions::default()).unwrap();
        let app = OfflineApp::new(
            VaultFs::new(VaultRoot::explicit(&root).unwrap()),
            OperationOptions::default(),
        )
        .unwrap();
        app.source_add(CaptureRequest {
            title: "Unit fixture source".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.md".into(),
            original: raw.as_bytes().to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        let reader = catalog.verified_snapshot(None).unwrap();
        let document = reader
            .projection()
            .documents
            .iter()
            .find(|document| document.owner_revision.is_some())
            .unwrap()
            .clone();
        (temp, reader, document)
    }
    fn span(start: usize, end: usize) -> ByteSpan {
        ByteSpan::new(start as u64, end as u64).unwrap()
    }
    fn cue(document: &DocumentRow, span: ByteSpan, cosine: f64) -> ContextSemanticCue {
        ContextSemanticCue {
            owner: document.path.clone(),
            observed_hash: document.hash.clone(),
            span,
            cosine,
        }
    }
    fn selected(
        reader: &ReaderSnapshot,
        document: &DocumentRow,
        cues: &[ContextSemanticCue],
        query: &str,
        bytes: usize,
        limit: usize,
    ) -> UnitSelection {
        select_units(
            reader,
            query,
            &[UnitDocument {
                owner_index: 7,
                document,
            }],
            cues,
            bytes,
            limit,
        )
        .unwrap()
    }

    #[test]
    fn distant_unit_survives_repeated_parent_matches_before_global_cap() {
        let first = "orbit detail ".repeat(70);
        let raw = format!("{first}\n\nDistant orbit condition applies.\n");
        let (_temp, reader, document) = fixture(&raw);
        let mut cues = (0..70)
            .map(|index| cue(&document, span(index * 13, index * 13 + 5), 0.9))
            .collect::<Vec<_>>();
        cues.push(cue(&document, span(first.len() + 2, raw.len()), 0.1));
        let result = selected(&reader, &document, &cues, "orbit", 1024, 2);
        assert_eq!(result.candidates.len(), 2);
        assert!(result.candidates.iter().any(|candidate| {
            candidate
                .parent_span
                .slice(&raw)
                .unwrap()
                .contains("Distant")
        }));
        assert!(
            !result
                .omissions
                .iter()
                .any(|item| item.reason == "context_unit_candidate_cap")
        );
        assert!(result.candidates.iter().all(|candidate| {
            candidate.rank_contributions.len() == 3
                && candidate.score
                    == 1.0
                        / (60.0
                            + candidate
                                .rank_contributions
                                .iter()
                                .find(|channel| channel.channel == "context_unit_selection")
                                .unwrap()
                                .rank as f64)
        }));
    }

    #[test]
    fn exact_unicode_parent_contains_child_and_complete_command_explanation() {
        let raw = "# Operation\n\nChoose café 中文 🦀 safely.\n\n```sh\nrunner --selected\n```\n\nTo include all items use `runner --all`.\n\n# Next\n\nUnrelated.\n";
        let (_temp, reader, document) = fixture(raw);
        let start = raw.find("--selected").unwrap();
        let child = span(start, start + "--selected".len());
        let result = selected(
            &reader,
            &document,
            &[cue(&document, child, 0.8)],
            "choose",
            512,
            80,
        );
        let candidate = &result.candidates[0];
        assert!(!candidate.clipped);
        assert_eq!(candidate.child_span, child);
        assert!(candidate.parent_span.start() <= child.start());
        assert!(candidate.parent_span.end() >= child.end());
        assert_eq!(
            candidate.parent_span.slice(raw).unwrap(),
            "Choose café 中文 🦀 safely.\n\n```sh\nrunner --selected\n```\n\nTo include all items use `runner --all`.\n"
        );
        assert!(candidate.parent_span.len() <= 512);
    }

    #[test]
    fn list_unit_has_one_complete_intro_parent() {
        let raw = "Priority is:\n\n- Command options\n- Environment variables\n- Global file\n\nSeparate paragraph.\n";
        let (_temp, reader, document) = fixture(raw);
        let start = raw.find("Environment").unwrap();
        let result = selected(
            &reader,
            &document,
            &[cue(&document, span(start, start + 11), 0.8)],
            "priority",
            256,
            80,
        );
        let candidate = &result.candidates[0];
        assert!(!candidate.clipped);
        let text = candidate.parent_span.slice(raw).unwrap();
        assert!(text.starts_with("Priority is:"));
        assert!(text.contains("Global file"));
        assert!(!text.contains("Separate paragraph"));
    }

    #[test]
    fn coarse_fallback_is_utf8_exact_focused_and_never_widens_unit() {
        let outside = "OUTSIDE needle\n\n";
        let body = format!(
            "{} needle café 中文 🦀 {}",
            "ordinary ".repeat(90),
            "ordinary ".repeat(30)
        );
        let raw = format!("{outside}{body}\n\nOUTSIDE needle after\n");
        let (_temp, reader, document) = fixture(&raw);
        let unit = span(outside.len(), outside.len() + body.len());
        let result = selected(
            &reader,
            &document,
            &[cue(&document, unit, 0.9)],
            "needle",
            97,
            80,
        );
        let candidate = &result.candidates[0];
        assert!(candidate.clipped);
        assert_eq!(candidate.parent_span, candidate.child_span);
        assert!(candidate.child_span.start() >= unit.start());
        assert!(candidate.child_span.end() <= unit.end());
        assert!(candidate.child_span.len() <= 97);
        let text = candidate.child_span.slice(&raw).unwrap();
        assert!(text.contains("needle café 中文 🦀"));
        assert!(!text.contains("OUTSIDE"));
    }

    #[test]
    fn oversized_structure_keeps_exact_child_and_discloses_clipping() {
        let raw = "🦀 explanatory prose ".repeat(100);
        let (_temp, reader, document) = fixture(&raw);
        let child = span(0, "🦀 explanatory prose ".len());
        let result = selected(
            &reader,
            &document,
            &[cue(&document, child, 0.6)],
            "meaning",
            97,
            80,
        );
        assert_eq!(result.candidates[0].parent_span, child);
        assert_eq!(result.candidates[0].child_span, child);
        assert!(result.candidates[0].clipped);
    }

    #[test]
    fn cosine_one_paraphrase_beats_cosine_zero_exact_word_distractor() {
        let raw = "Compiler emits `E0308` for a type mismatch.\n\nUse another execution lane to avoid blocking the event loop.\n\nNumbers E03080 are unrelated.\n";
        let (_temp, reader, document) = fixture(raw);
        let first_end = raw.find("\n\n").unwrap() + 1;
        let second_start = first_end + 1;
        let second_end = second_start + raw[second_start..].find("\n\n").unwrap() + 1;
        let cues = [
            cue(&document, span(0, first_end), 0.0),
            cue(&document, span(second_start, second_end), 1.0),
            cue(&document, span(second_end + 1, raw.len()), -0.1),
        ];
        let result = selected(
            &reader,
            &document,
            &cues,
            "E0308 asynchronous worker",
            256,
            80,
        );
        assert!(
            result.candidates[0]
                .parent_span
                .slice(raw)
                .unwrap()
                .contains("execution lane")
        );
        assert_eq!(result.candidates[0].score, 1.0 / 61.0);
        let identifier = result
            .candidates
            .iter()
            .find(|candidate| {
                candidate
                    .parent_span
                    .slice(raw)
                    .unwrap()
                    .contains("`E0308`")
            })
            .unwrap();
        assert_eq!(
            identifier
                .rank_contributions
                .iter()
                .find(|vote| vote.channel == "context_unit_bm25")
                .unwrap()
                .rank,
            1
        );
        let paraphrase = result
            .candidates
            .iter()
            .find(|candidate| {
                candidate
                    .parent_span
                    .slice(raw)
                    .unwrap()
                    .contains("execution lane")
            })
            .unwrap();
        assert_eq!(paraphrase.rank_contributions.len(), 2);
        assert_eq!(
            paraphrase.rank_contributions[0].channel,
            "context_unit_dense"
        );
        assert_eq!(paraphrase.rank_contributions[0].rank, 1);
        let distractor = result
            .candidates
            .iter()
            .find(|candidate| candidate.parent_span.slice(raw).unwrap().contains("E03080"))
            .unwrap();
        assert_eq!(distractor.rank_contributions.len(), 2);
    }

    #[test]
    fn exact_cosine_ties_use_identifier_bm25_without_overriding_any_cosine_gap() {
        let raw = "General compiler details.\n\nAn exact `E0308` identifier identifies a mismatch.\n\nSeparate background paragraph.\n";
        let (_temp, reader, document) = fixture(raw);
        let second_start = raw.find("An exact").unwrap();
        let third_start = raw.find("Separate").unwrap();
        let mut cues = [
            cue(&document, span(0, second_start - 1), 0.5),
            cue(&document, span(second_start, third_start - 1), 0.5),
            cue(&document, span(third_start, raw.len()), 0.5),
        ];
        let result = selected(&reader, &document, &cues, "E0308", 256, 80);
        let winner = &result.candidates[0];
        assert!(winner.parent_span.slice(raw).unwrap().contains("`E0308`"));
        assert_eq!(winner.score, 1.0 / 61.0);
        assert_eq!(
            winner
                .rank_contributions
                .iter()
                .find(|channel| channel.channel == "context_unit_dense")
                .unwrap()
                .rank,
            2
        );
        assert_eq!(
            winner
                .rank_contributions
                .iter()
                .find(|channel| channel.channel == "context_unit_bm25")
                .unwrap()
                .rank,
            1
        );
        assert_eq!(
            winner
                .rank_contributions
                .iter()
                .find(|channel| channel.channel == "context_unit_selection")
                .unwrap()
                .rank,
            1
        );
        // Even the smallest representable positive cosine gap outranks the
        // lexical tie breaker. There is no threshold, bin or score mixture.
        cues[0].cosine = 0.5 + f64::EPSILON;
        let result = selected(&reader, &document, &cues, "E0308", 256, 80);
        assert!(
            result.candidates[0]
                .parent_span
                .slice(raw)
                .unwrap()
                .contains("General compiler")
        );
    }

    #[test]
    fn invalid_stale_utf8_and_conflicting_units_are_rejected() {
        let raw = "🦀 valid body\n";
        let (_temp, reader, document) = fixture(raw);
        let input = [UnitDocument {
            owner_index: 7,
            document: &document,
        }];
        let good = cue(&document, span(0, raw.len()), 0.5);
        let mut stale = good.clone();
        stale.observed_hash = Blake3Hash::digest(b"stale");
        let error = select_units(&reader, "body", &input, &[stale], 128, 80)
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::FreshnessConflict);
        for invalid in [
            cue(&document, span(1, 4), 0.5),
            cue(&document, span(0, raw.len() + 1), 0.5),
            cue(&document, span(0, 0), 0.5),
            cue(&document, span(0, 4), f64::NAN),
            cue(&document, span(0, 4), 1.01),
        ] {
            assert!(select_units(&reader, "body", &input, &[invalid], 128, 80).is_err());
        }
        assert!(
            select_units(
                &reader,
                "body",
                &input,
                &[good.clone(), cue(&document, good.span, 0.4)],
                128,
                80
            )
            .is_err()
        );
        let duplicate = selected(&reader, &document, &[good.clone(), good], "body", 128, 80);
        assert_eq!(duplicate.candidates.len(), 1);
    }

    #[test]
    fn missing_units_and_managed_envelope_never_manufacture_evidence() {
        let (_temp, reader, mut document) = fixture("Plain body.\n");
        let empty = selected(&reader, &document, &[], "body", 128, 80);
        assert!(empty.candidates.is_empty());
        assert_eq!(empty.scanned_bytes, 0);
        document.owner_revision = None;
        document.raw_text =
            "---\nwiki_kind: page\nwiki_title: Metadata\n---\n\nPlain body.\n".into();
        document.hash = Blake3Hash::digest(document.raw_text.as_bytes());
        let body = body_start(&document);
        assert!(body > 0);
        let inputs = [UnitDocument {
            owner_index: 7,
            document: &document,
        }];
        assert!(
            select_units(
                &reader,
                "body",
                &inputs,
                &[cue(&document, span(0, 4), 0.5)],
                128,
                80
            )
            .is_err()
        );
        let result = selected(
            &reader,
            &document,
            &[cue(&document, span(body, document.raw_text.len()), 0.5)],
            "body",
            128,
            80,
        );
        assert!(
            !result.candidates[0]
                .parent_span
                .slice(&document.raw_text)
                .unwrap()
                .contains("wiki_kind")
        );
    }

    #[test]
    fn caps_and_unit_order_are_deterministic_and_disclosed() {
        let raw = "First orbit paragraph.\n\nSecond orbit paragraph.\n\nThird orbit paragraph.\n";
        let (_temp, reader, document) = fixture(raw);
        let starts = [0, raw.find("Second").unwrap(), raw.find("Third").unwrap()];
        let mut cues = starts
            .into_iter()
            .map(|start| cue(&document, span(start, start + 5), 0.5))
            .collect::<Vec<_>>();
        let a = selected(&reader, &document, &cues, "orbit", 128, 2);
        cues.reverse();
        let b = selected(&reader, &document, &cues, "orbit", 128, 2);
        assert_eq!(
            a.candidates
                .iter()
                .map(|candidate| (
                    candidate.parent_span,
                    candidate.score,
                    &candidate.rank_contributions
                ))
                .collect::<Vec<_>>(),
            b.candidates
                .iter()
                .map(|candidate| (
                    candidate.parent_span,
                    candidate.score,
                    &candidate.rank_contributions
                ))
                .collect::<Vec<_>>()
        );
        assert!(
            a.omissions
                .iter()
                .any(|item| item.reason == "context_unit_candidate_cap")
        );
        let inputs = [UnitDocument {
            owner_index: 7,
            document: &document,
        }];
        assert!(
            select_units(
                &reader,
                "orbit",
                &inputs,
                &vec![cues[0].clone(); MAX_UNITS + 1],
                128,
                80
            )
            .is_err()
        );
        for (bytes, limit) in [(0, 80), (2049, 80), (128, 0), (128, 161)] {
            assert!(select_units(&reader, "orbit", &inputs, &cues, bytes, limit).is_err());
        }
        let query = (0..MAX_CONTEXT_QUERY_TERMS + 1)
            .map(|index| format!("feature{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let result = selected(&reader, &document, &cues, &query, 128, 80);
        assert!(
            result
                .omissions
                .iter()
                .any(|item| item.reason == "context_query_term_cap")
        );
    }

    #[test]
    fn owner_global_bytes_and_structural_start_caps_are_truthful() {
        let (_temp, reader, base) = fixture("Seed body.\n");
        let mut documents = (0..5)
            .map(|index| {
                let mut document = base.clone();
                document.path =
                    VaultRelativePath::new(format!("sources/owner-{index}/content.md")).unwrap();
                document.raw_text = "ordinary body ".repeat(OWNER_SCAN_BYTES / 8);
                document.hash = Blake3Hash::digest(document.raw_text.as_bytes());
                document
            })
            .collect::<Vec<_>>();
        let cues = documents
            .iter()
            .map(|document| cue(document, span(0, 8), 0.5))
            .collect::<Vec<_>>();
        let inputs = documents
            .iter()
            .enumerate()
            .map(|(owner_index, document)| UnitDocument {
                owner_index,
                document,
            })
            .collect::<Vec<_>>();
        let result = select_units(&reader, "body", &inputs, &cues, 128, 80).unwrap();
        assert_eq!(result.scanned_bytes, TOTAL_SCAN_BYTES);
        assert!(result.omissions.iter().any(
            |item| item.owner_index == 4 && item.reason == "context_unit_outside_scanned_body"
        ));
        assert!(result.scanned_blocks <= MAX_STARTS);
        drop(inputs);
        documents[0].raw_text = "ordinary paragraph\n\n".repeat(MAX_STARTS + 2);
        documents[0].hash = Blake3Hash::digest(documents[0].raw_text.as_bytes());
        let near_end = documents[0].raw_text.len() - 20;
        let result = selected(
            &reader,
            &documents[0],
            &[cue(&documents[0], span(near_end, near_end + 8), 0.5)],
            "body",
            128,
            80,
        );
        assert_eq!(result.scanned_blocks, MAX_STARTS);
        assert!(result.candidates.is_empty());
        assert!(
            result
                .omissions
                .iter()
                .any(|item| item.reason == "context_source_scan_block_cap")
        );
    }

    #[test]
    fn overlapping_coarse_units_cannot_multiply_lexical_or_focus_work() {
        let (_temp, reader, mut document) = fixture("Seed body.\n");
        document.raw_text = "x".repeat(600_000);
        document.hash = Blake3Hash::digest(document.raw_text.as_bytes());
        let cues = [0, 4, 8]
            .into_iter()
            .map(|start| cue(&document, span(start, start + 400_000), 0.9))
            .collect::<Vec<_>>();
        let result = selected(&reader, &document, &cues, "missing", 128, 80);
        assert_eq!(result.candidates.len(), 2);
        for reason in [
            "context_unit_lexical_scan_byte_cap",
            "context_unit_focus_scan_byte_cap",
        ] {
            assert!(result.omissions.iter().any(|item| item.reason == reason));
        }
        assert!(
            result
                .candidates
                .iter()
                .all(|candidate| { candidate.clipped && candidate.child_span.len() <= 128 })
        );
        // A small later unit retains its dense vote when exact lexical
        // tokenization exhausted its separate cumulative work allowance.
        document.raw_text = "x".repeat(OWNER_SCAN_BYTES);
        document.hash = Blake3Hash::digest(document.raw_text.as_bytes());
        let cues = [
            cue(&document, span(0, OWNER_SCAN_BYTES - 1), 0.9),
            cue(&document, span(4, 8), 0.8),
        ];
        let result = selected(&reader, &document, &cues, "missing", 128, 80);
        let small = result
            .candidates
            .iter()
            .find(|candidate| candidate.child_span == span(4, 8))
            .unwrap();
        assert_eq!(small.rank_contributions.len(), 2);
        assert_eq!(small.rank_contributions[0].channel, "context_unit_dense");
        assert!(
            result
                .omissions
                .iter()
                .any(|item| item.reason == "context_unit_lexical_scan_byte_cap")
        );
    }
}

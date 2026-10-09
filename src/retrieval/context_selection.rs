//! Bounded local passage proposals. Owners and citations are authenticated by assembly.
use super::{
    context_types::ContextSemanticCue,
    excerpts::{SourceMap, Tokenizer},
    types::MAX_CONTEXT_QUERY_TERMS,
};
use crate::{
    catalog::{DocumentRow, query_types::QueryCatalog},
    domain::*,
    records::parse_note,
};
use pulldown_cmark::{Event, Parser, Tag};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

const OWNER_SCAN_BYTES: usize = 1024 * 1024;
const TOTAL_SCAN_BYTES: usize = 4 * OWNER_SCAN_BYTES;
const MAX_BLOCKS: usize = 4096;
const MAX_CANDIDATES: usize = 32;
const MAX_SEMANTIC_CUES: usize = 4096;

pub(crate) struct SelectionDocument<'a> {
    pub owner_index: usize,
    pub document: &'a DocumentRow,
    pub seed_spans: &'a [ByteSpan],
}
#[derive(Clone, Debug)]
pub(crate) struct SelectionCandidate {
    pub owner_index: usize,
    pub span: ByteSpan,
    pub covered_terms: Vec<usize>,
    /// Sum of global query-term weights, not an answer confidence.
    pub local_relevance: u64,
    pub seed_overlap: bool,
    pub clipped: bool,
    /// Coarse cached-unit affinity, not a passage embedding or confidence.
    pub semantic_affinity: Option<f64>,
    /// Request-local structure; never a citation, canonical identity or coverage claim.
    pub section: Option<SectionSelection>,
}
#[derive(Clone, Debug)]
pub(crate) struct SectionSelection {
    pub heading_span: ByteSpan,
    pub section_span: ByteSpan,
    pub ancestors: Vec<ByteSpan>,
    pub child_ordinal: usize,
    pub representative_ordinal: usize,
    /// Direct-section location features, separate from the cited child's terms.
    pub covered_terms: Vec<usize>,
    pub local_relevance: u64,
    pub seed_overlap: bool,
}
pub(crate) struct SelectionOmission {
    pub owner_index: usize,
    pub reason: &'static str,
}
pub(crate) struct SelectionResult {
    pub candidates: Vec<SelectionCandidate>,
    pub term_weights: Vec<u64>,
    pub omissions: Vec<SelectionOmission>,
    pub scanned_bytes: usize,
    pub scanned_blocks: usize,
}
struct Parent<'a> {
    owner: usize,
    raw: &'a str,
    body: usize,
    anchors: &'a [ByteSpan],
    semantic: Vec<(ByteSpan, f64)>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Heading,
    Prose,
    Code,
    List,
    Other,
}
struct Block {
    range: Range<usize>,
    kind: BlockKind,
    terms: Vec<usize>,
    clipped: bool,
    heading_level: Option<u8>,
}
struct OwnerBlocks<'a> {
    parent: &'a Parent<'a>,
    blocks: Vec<Block>,
    fallback_windows: Vec<(Range<usize>, Vec<usize>)>,
    structural_end: usize,
}

pub(crate) fn select_section_candidates(
    reader: &dyn QueryCatalog,
    query: &str,
    documents: &[SelectionDocument<'_>],
    max_excerpt_bytes: usize,
) -> Result<SelectionResult> {
    reader.check_query_budget()?;
    let parents = documents
        .iter()
        .map(|input| Parent {
            owner: input.owner_index,
            raw: input.document.raw_text.as_str(),
            body: if input.document.owner_revision.is_some() {
                0
            } else {
                note_body_start(&input.document.raw_text)
            },
            anchors: input.seed_spans,
            semantic: vec![],
        })
        .collect::<Vec<_>>();
    let tokenizer = Tokenizer::new(reader.connection())?;
    let check = || reader.check_query_budget();
    let result = select_with_sections_checked(
        &tokenizer,
        query,
        &parents,
        max_excerpt_bytes,
        true,
        Some(&check),
    )?;
    reader.check_query_budget()?;
    Ok(result)
}

pub(crate) fn select_candidates_with_semantics(
    reader: &dyn QueryCatalog,
    query: &str,
    documents: &[SelectionDocument<'_>],
    max_excerpt_bytes: usize,
    cues: &[ContextSemanticCue],
) -> Result<SelectionResult> {
    if cues.len() > MAX_SEMANTIC_CUES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "context semantic cue cap exceeded",
        ));
    }
    let parents = documents
        .iter()
        .map(|input| -> Result<Parent<'_>> {
            let raw = input.document.raw_text.as_str();
            let body = if input.document.owner_revision.is_some() {
                0
            } else {
                note_body_start(raw)
            };
            let semantic =
                validated_cues(&input.document.path, &input.document.hash, raw, body, cues)?;
            Ok(Parent {
                owner: input.owner_index,
                raw,
                body,
                anchors: input.seed_spans,
                semantic,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let tokenizer = Tokenizer::new(reader.connection())?;
    select(&tokenizer, query, &parents, max_excerpt_bytes)
}

fn note_body_start(raw: &str) -> usize {
    let end = boundary_before(
        raw,
        raw.len()
            .min(crate::records::ParseLimits::default().max_envelope_bytes + 16),
    );
    parse_note(&raw.as_bytes()[..end]).body_start
}

fn validated_cues(
    owner: &VaultRelativePath,
    hash: &Blake3Hash,
    raw: &str,
    body: usize,
    cues: &[ContextSemanticCue],
) -> Result<Vec<(ByteSpan, f64)>> {
    if cues.len() > MAX_SEMANTIC_CUES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "context semantic cue cap exceeded",
        ));
    }
    let mut selected = BTreeMap::new();
    for cue in cues.iter().filter(|cue| cue.owner == *owner) {
        if cue.observed_hash != *hash {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "context semantic cue owner hash differs from pinned document",
            ));
        }
        if !cue.cosine.is_finite() || !(-1.0..=1.0).contains(&cue.cosine) {
            return Err(WikiError::invalid("context semantic cue cosine invalid"));
        }
        cue.span.slice(raw)?;
        if cue.span.is_empty() || cue.span.start() < body as u64 {
            return Err(WikiError::invalid(
                "context semantic cue span outside document body",
            ));
        }
        if let Some(previous) = selected.insert((cue.span.start(), cue.span.end()), cue.cosine)
            && previous != cue.cosine
        {
            return Err(WikiError::invalid(
                "conflicting duplicate context semantic cue",
            ));
        }
    }
    selected
        .into_iter()
        .map(|((start, end), cosine)| Ok((ByteSpan::new(start, end)?, cosine)))
        .collect()
}

fn affinity(span: ByteSpan, cues: &[(ByteSpan, f64)]) -> Option<f64> {
    let mut weighted = 0.0;
    let mut bytes = 0u64;
    for &(unit, cosine) in cues {
        let overlap = unit
            .end()
            .min(span.end())
            .saturating_sub(unit.start().max(span.start()));
        if overlap > 0 {
            weighted += cosine * overlap as f64;
            bytes += overlap;
        }
    }
    (bytes > 0).then(|| (weighted / bytes as f64).clamp(-1.0, 1.0))
}

fn semantic_strength(candidate: &SelectionCandidate) -> f64 {
    candidate
        .semantic_affinity
        .map_or(0.0, |cosine| cosine.max(0.0).powi(4))
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Region {
    Unit(u64, u64),
    Unscored(u64),
}
fn dominant_region(
    candidate: &SelectionCandidate,
    cues: &[(ByteSpan, f64)],
    bytes: usize,
) -> Region {
    cues.iter()
        .filter_map(|&(span, cosine)| {
            let overlap = span
                .end()
                .min(candidate.span.end())
                .saturating_sub(span.start().max(candidate.span.start()));
            (overlap > 0).then_some((overlap, cosine, span))
        })
        .max_by(
            |(a_overlap, a_cosine, a_span), (b_overlap, b_cosine, b_span)| {
                a_overlap
                    .cmp(b_overlap)
                    .then(a_cosine.total_cmp(b_cosine))
                    .then(b_span.start().cmp(&a_span.start()))
                    .then(b_span.end().cmp(&a_span.end()))
            },
        )
        .map_or(
            Region::Unscored(candidate.span.start() / bytes.max(1) as u64),
            |(_, _, span)| Region::Unit(span.start(), span.end()),
        )
}

// This normalization influences selection only. It never changes source bytes,
// FTS discovery, embeddings, or citation identity. Keep short identifiers exact.
fn term_key(term: &str) -> String {
    if !term.is_ascii() || term.len() < 5 {
        return term.to_owned();
    }
    let mut stem = term;
    if let Some(base) = term.strip_suffix("ing").filter(|s| s.len() >= 4) {
        stem = base;
    } else if let Some(base) = term.strip_suffix("ed").filter(|s| s.len() >= 4) {
        stem = base;
    } else if let Some(base) = term
        .strip_suffix('s')
        .filter(|s| s.len() >= 4 && !s.ends_with('s'))
    {
        stem = base;
    }
    if let Some(base) = stem.strip_suffix('e').filter(|s| s.len() >= 4) {
        stem = base;
    }
    stem.to_owned()
}

// Generic English grammatical words are poor passage-location features: a
// rare "how" can otherwise outweigh a repeated technical noun. This affects
// only local context selection, never discovery or embeddings. Negation and
// restrictions (no/not/never/without/unless/only), identifiers and non-English
// tokens remain available. Content words are deliberately absent from this list.
fn function_word(term: &str) -> bool {
    matches!(
        term,
        "a" | "an"
            | "the"
            | "and"
            | "or"
            | "but"
            | "as"
            | "at"
            | "by"
            | "for"
            | "from"
            | "in"
            | "into"
            | "of"
            | "on"
            | "onto"
            | "to"
            | "with"
            | "about"
            | "after"
            | "before"
            | "between"
            | "during"
            | "over"
            | "through"
            | "how"
            | "what"
            | "when"
            | "where"
            | "which"
            | "who"
            | "whom"
            | "whose"
            | "why"
            | "i"
            | "me"
            | "my"
            | "we"
            | "us"
            | "our"
            | "you"
            | "your"
            | "he"
            | "him"
            | "his"
            | "she"
            | "her"
            | "it"
            | "its"
            | "they"
            | "them"
            | "their"
            | "this"
            | "that"
            | "these"
            | "those"
            | "there"
            | "here"
            | "am"
            | "is"
            | "are"
            | "was"
            | "were"
            | "be"
            | "been"
            | "being"
            | "do"
            | "does"
            | "did"
            | "have"
            | "has"
            | "had"
            | "having"
            | "can"
            | "could"
            | "would"
            | "should"
            | "will"
            | "shall"
            | "may"
            | "might"
            | "if"
            | "then"
            | "than"
            | "also"
            | "each"
            | "some"
            | "any"
            | "such"
    )
}

// Preserve grammatical spellings when the query marks them as identifiers:
// `can`, --with, _from, object.is and is(). A sentence-final period alone is
// not an identifier cue. SQLite already supplies original UTF-8 token ranges.
fn identifier_syntax(query: &str, range: &Range<usize>) -> bool {
    let before = &query[..range.start];
    let after = &query[range.end..];
    let previous = before.chars().next_back();
    let following = after.chars().next();
    if previous.is_some_and(|character| matches!(character, '`' | '_' | '-' | '/' | '\\' | ':'))
        || following.is_some_and(|character| matches!(character, '`' | '_' | '-' | '('))
    {
        return true;
    }
    if previous == Some('.') {
        return before[..before.len() - 1]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || character == '_');
    }
    following == Some('.')
        && after[1..]
            .chars()
            .next()
            .is_some_and(|character| character.is_alphanumeric() || character == '_')
}

fn matched_terms(
    tokenizer: &Tokenizer<'_>,
    raw: &str,
    range: Range<usize>,
    terms: &BTreeMap<String, usize>,
) -> Result<Vec<usize>> {
    let map = SourceMap::markdown(&raw[..range.end], range.start);
    Ok(tokenizer
        .tokens(&map.text)?
        .into_iter()
        .filter_map(|token| terms.get(&term_key(&token.text)).copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn boundary_before(raw: &str, mut offset: usize) -> usize {
    offset = offset.min(raw.len());
    while !raw.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn structural_blocks(
    raw: &str,
    start: usize,
    end: usize,
    limit: usize,
) -> (Vec<Block>, bool, usize) {
    let mut spans = BTreeMap::<(usize, usize), (BlockKind, Option<u8>)>::new();
    let mut limited = false;
    let mut scanned = 0;
    for (event, range) in Parser::new(&raw[start..end]).into_offset_iter() {
        let (kind, heading_level) = match event {
            Event::Start(Tag::Heading { level, .. }) => (BlockKind::Heading, Some(level as u8)),
            Event::Start(Tag::Paragraph) => (BlockKind::Prose, None),
            Event::Start(Tag::CodeBlock(_)) => (BlockKind::Code, None),
            Event::Start(Tag::List(_)) => (BlockKind::List, None),
            Event::Start(Tag::Item) => (BlockKind::Other, None),
            _ => continue,
        };
        let range = range.start + start..range.end + start;
        if range.is_empty() {
            continue;
        }
        if scanned == limit {
            limited = true;
            break;
        }
        scanned += 1;
        spans
            .entry((range.start, range.end))
            .or_insert((kind, heading_level));
    }
    let mut blocks = Vec::<Block>::new();
    let mut spans = spans.into_iter().collect::<Vec<_>>();
    // Parent and first-child ranges can have the same start. Visit the largest
    // parent first so a first item cannot split an otherwise complete list.
    spans.sort_by(|((a_start, a_end), _), ((b_start, b_end), _)| {
        a_start.cmp(b_start).then(b_end.cmp(a_end))
    });
    for ((begin, finish), (kind, heading_level)) in spans {
        // A list/item already contains its nested paragraphs and commands.
        if blocks
            .last()
            .is_some_and(|previous| previous.range.end >= finish)
        {
            continue;
        }
        blocks.push(Block {
            range: begin..finish,
            kind,
            terms: vec![],
            clipped: finish == end && end < raw.len(),
            heading_level,
        });
    }
    if blocks.is_empty() && start < end && limit > 0 {
        blocks.push(Block {
            range: start..end,
            kind: BlockKind::Prose,
            terms: vec![],
            clipped: end < raw.len(),
            heading_level: None,
        });
    }
    (blocks, limited, scanned)
}

// Keep a short explanatory paragraph with its following command or list.
// A stand-alone keyword paragraph otherwise beats its useful command-bearing
// parent on gain/byte even though the command is the missing evidence.
fn teaching_blocks(mut blocks: Vec<Block>, bytes: usize) -> Vec<Block> {
    let mut grouped = Vec::<Block>::new();
    let mut index = 0;
    while index < blocks.len() {
        let mut block = std::mem::replace(
            &mut blocks[index],
            Block {
                range: 0..0,
                kind: BlockKind::Other,
                terms: vec![],
                clipped: false,
                heading_level: None,
            },
        );
        let follows_teaching = block.kind == BlockKind::Prose
            && blocks
                .get(index + 1)
                .is_some_and(|next| matches!(next.kind, BlockKind::Code | BlockKind::List));
        if follows_teaching && blocks[index + 1].range.end - block.range.start <= bytes {
            index += 1;
            block.range.end = blocks[index].range.end;
            block.clipped |= blocks[index].clipped;
            block.kind = blocks[index].kind;
        }
        // Tiny prose fragments are usually connective context. Retain their
        // exact text with a fitting neighboring block rather than spend a
        // complete citation/header packet on the fragment alone. Headings,
        // complete commands/lists and isolated short notes remain intact.
        if block.kind == BlockKind::Prose && block.range.len() <= 48 {
            if let Some(next) = blocks.get(index + 1)
                && next.kind != BlockKind::Heading
                && next.range.end.saturating_sub(block.range.start) <= bytes
            {
                index += 1;
                block.range.end = blocks[index].range.end;
                block.clipped |= blocks[index].clipped;
                block.kind = blocks[index].kind;
            } else if let Some(previous) = grouped.last_mut()
                && previous.kind == BlockKind::Prose
                && block.range.end.saturating_sub(previous.range.start) <= bytes
            {
                previous.range.end = block.range.end;
                previous.clipped |= block.clipped;
                index += 1;
                continue;
            }
        }
        // Command explanations may alternate paragraph/code/paragraph/code,
        // and the final paragraph may contain a complementary inline command.
        // Preserve a complete bounded run rather than competing against its
        // shorter introductory paragraph. Lists retain their intro atomically.
        if block.kind == BlockKind::Code {
            for _ in 0..8 {
                let Some(next) = blocks.get(index + 1) else {
                    break;
                };
                if !matches!(next.kind, BlockKind::Prose | BlockKind::Code)
                    || next.range.end - block.range.start > bytes
                {
                    break;
                }
                let terminal_prose = next.kind == BlockKind::Prose
                    && !blocks
                        .get(index + 2)
                        .is_some_and(|after| after.kind == BlockKind::Code);
                index += 1;
                block.range.end = blocks[index].range.end;
                block.clipped |= blocks[index].clipped;
                if terminal_prose {
                    break;
                }
            }
        }
        grouped.push(block);
        index += 1;
    }
    grouped
}

fn bounded_window(raw: &str, bounds: Range<usize>, anchor: usize, bytes: usize) -> Range<usize> {
    let mut start = anchor.saturating_sub(bytes / 4).max(bounds.start);
    while start < bounds.end && !raw.is_char_boundary(start) {
        start += 1;
    }
    let mut end = boundary_before(raw, start.saturating_add(bytes).min(bounds.end));
    // Prefer whole lines if a line boundary fits; otherwise retain a bounded
    // exact UTF-8 slice and explicitly mark the proposal clipped.
    if start > bounds.start
        && let Some(next) = raw[start..end].find('\n')
    {
        let line_start = start + next + 1;
        if line_start <= anchor {
            start = line_start;
        }
    }
    if end < bounds.end
        && let Some(previous) = raw[start..end].rfind('\n')
    {
        let line_end = start + previous + 1;
        if line_end > anchor {
            end = line_end;
        }
    }
    start..end
}

fn query_terms(tokenizer: &Tokenizer<'_>, query: &str) -> Result<(BTreeMap<String, usize>, bool)> {
    let mut terms = BTreeMap::new();
    let mut term_cap = false;
    for token in tokenizer.tokens(query)? {
        if function_word(&token.text) && !identifier_syntax(query, &token.span) {
            continue;
        }
        let key = term_key(&token.text);
        if terms.contains_key(&key) {
            continue;
        }
        if terms.len() == MAX_CONTEXT_QUERY_TERMS {
            term_cap = true;
            break;
        }
        terms.insert(key, terms.len());
    }
    Ok((terms, term_cap))
}

#[cfg(test)]
pub(super) fn location_terms_for_test(
    reader: &dyn QueryCatalog,
    query: &str,
    raw: &str,
    spans: &[ByteSpan],
) -> Result<Vec<Vec<usize>>> {
    let tokenizer = Tokenizer::new(reader.connection())?;
    let (terms, _) = query_terms(&tokenizer, query)?;
    spans
        .iter()
        .map(|span| {
            span.slice(raw)?;
            matched_terms(
                &tokenizer,
                raw,
                span.start() as usize..span.end() as usize,
                &terms,
            )
        })
        .collect()
}

fn select(
    tokenizer: &Tokenizer<'_>,
    query: &str,
    parents: &[Parent<'_>],
    bytes: usize,
) -> Result<SelectionResult> {
    select_with_sections(tokenizer, query, parents, bytes, false)
}

// Partition once, rather than manufacturing overlapping windows around each
// occurrence. A fitting teaching group is indivisible; only oversized groups
// may continue into adjacent clipped children.
fn section_ranges(
    raw: &str,
    blocks: &[Block],
    section: Range<usize>,
    bytes: usize,
    constructed: &mut usize,
    check: Option<&dyn Fn() -> Result<()>>,
) -> Result<(Vec<(Range<usize>, bool)>, bool, bool)> {
    check_selection_budget(check)?;
    if section.len() <= bytes {
        if !reserve_section_child(constructed, check)? {
            return Ok((vec![], false, true));
        }
        return Ok((
            vec![(section, blocks.iter().any(|block| block.clipped))],
            false,
            false,
        ));
    }
    let mut ranges = Vec::new();
    let mut pending: Option<(Range<usize>, bool)> = None;
    let mut unrepresentable = false;
    for (index, block) in blocks.iter().enumerate() {
        if index % 64 == 0 {
            check_selection_budget(check)?;
        }
        if block.range.len() > bytes {
            if let Some(previous) = pending.take() {
                if !reserve_section_child(constructed, check)? {
                    return Ok((ranges, unrepresentable, true));
                }
                ranges.push(previous);
            }
            let mut start = block.range.start;
            while start < block.range.end {
                if !reserve_section_child(constructed, check)? {
                    return Ok((ranges, unrepresentable, true));
                }
                let maximum =
                    boundary_before(raw, start.saturating_add(bytes).min(block.range.end));
                if maximum == start {
                    // An excerpt smaller than one UTF-8 scalar cannot cite it.
                    // Advance without inventing bytes or exceeding the cap.
                    start += raw[start..]
                        .chars()
                        .next()
                        .expect("within block")
                        .len_utf8();
                    unrepresentable = true;
                    continue;
                }
                let end = if maximum < block.range.end {
                    raw[start..maximum]
                        .rfind('\n')
                        .map_or(maximum, |line| start + line + 1)
                } else {
                    maximum
                };
                ranges.push((start..end, true));
                start = end;
            }
            continue;
        }
        if let Some((range, clipped)) = pending.as_mut()
            && block.range.end - range.start <= bytes
        {
            range.end = block.range.end;
            *clipped |= block.clipped;
        } else {
            if let Some(previous) = pending.take() {
                if !reserve_section_child(constructed, check)? {
                    return Ok((ranges, unrepresentable, true));
                }
                ranges.push(previous);
            }
            pending = Some((block.range.clone(), block.clipped));
        }
    }
    if let Some(previous) = pending {
        if !reserve_section_child(constructed, check)? {
            return Ok((ranges, unrepresentable, true));
        }
        ranges.push(previous);
    }
    Ok((ranges, unrepresentable, false))
}

fn check_selection_budget(check: Option<&dyn Fn() -> Result<()>>) -> Result<()> {
    if let Some(check) = check {
        check()?;
    }
    Ok(())
}

fn reserve_section_child(
    constructed: &mut usize,
    check: Option<&dyn Fn() -> Result<()>>,
) -> Result<bool> {
    if *constructed % 64 == 0 {
        check_selection_budget(check)?;
    }
    if *constructed == MAX_BLOCKS {
        return Ok(false);
    }
    *constructed += 1;
    Ok(true)
}

fn section_children(
    tokenizer: &Tokenizer<'_>,
    owner: &OwnerBlocks<'_>,
    terms: &BTreeMap<String, usize>,
    weights: &[u64],
    bytes: usize,
    constructed: &mut usize,
    check: Option<&dyn Fn() -> Result<()>>,
) -> Result<(Vec<SelectionCandidate>, bool, bool)> {
    let parent = owner.parent;
    let headings = owner
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.kind == BlockKind::Heading)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut ancestors = Vec::<(u8, ByteSpan)>::new();
    let mut candidates = Vec::new();
    let mut unrepresentable = false;
    let mut construction_limited = false;
    for (position, &index) in headings.iter().enumerate() {
        check_selection_budget(check)?;
        let heading = &owner.blocks[index];
        let level = heading.heading_level.expect("heading level recorded");
        ancestors.retain(|(ancestor_level, _)| *ancestor_level < level);
        let heading_span = ByteSpan::new(heading.range.start as u64, heading.range.end as u64)?;
        ancestors.push((level, heading_span));
        let next = headings
            .get(position + 1)
            .copied()
            .unwrap_or(owner.blocks.len());
        let end = headings
            .get(position + 1)
            .map_or(owner.structural_end, |&next| owner.blocks[next].range.start);
        let section_span = ByteSpan::new(heading.range.start as u64, end as u64)?;
        let blocks = &owner.blocks[index..next];
        let covered_terms = blocks
            .iter()
            .flat_map(|block| block.terms.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let local_relevance: u64 = covered_terms.iter().map(|&term| weights[term]).sum();
        let seed_overlap = parent.anchors.iter().any(|anchor| {
            anchor.start() < section_span.end() && section_span.start() < anchor.end()
        });
        if local_relevance == 0 && !seed_overlap {
            continue;
        }
        let (ranges, missing, limited) = section_ranges(
            parent.raw,
            blocks,
            heading.range.start..end,
            bytes,
            constructed,
            check,
        )?;
        unrepresentable |= missing;
        construction_limited |= limited;
        let mut children = Vec::new();
        for (child_ordinal, (range, clipped)) in ranges.into_iter().enumerate() {
            if child_ordinal % 64 == 0 {
                check_selection_budget(check)?;
            }
            // Markup alone cannot spend a passage slot, including a child
            // produced by splitting a very small excerpt allowance.
            if SourceMap::markdown(&parent.raw[..range.end], range.start)
                .text
                .trim()
                .is_empty()
            {
                continue;
            }
            let span = ByteSpan::new(range.start as u64, range.end as u64)?;
            let actual_terms = matched_terms(tokenizer, parent.raw, range, terms)?;
            children.push(SelectionCandidate {
                owner_index: parent.owner,
                span,
                local_relevance: actual_terms.iter().map(|&term| weights[term]).sum(),
                covered_terms: actual_terms,
                seed_overlap: parent
                    .anchors
                    .iter()
                    .any(|anchor| anchor.start() < span.end() && span.start() < anchor.end()),
                clipped,
                semantic_affinity: None,
                section: Some(SectionSelection {
                    heading_span,
                    section_span,
                    ancestors: ancestors.iter().map(|&(_, span)| span).collect(),
                    child_ordinal,
                    representative_ordinal: 0,
                    covered_terms: covered_terms.clone(),
                    local_relevance,
                    seed_overlap,
                }),
            });
        }
        let representative = children.iter().max_by(|a, b| {
            a.local_relevance
                .cmp(&b.local_relevance)
                .then(b.clipped.cmp(&a.clipped))
                .then(b.span.start().cmp(&a.span.start()))
                .then(b.span.end().cmp(&a.span.end()))
        });
        if let Some(representative) = representative {
            let ordinal = representative
                .section
                .as_ref()
                .expect("section child")
                .child_ordinal;
            for child in &mut children {
                child
                    .section
                    .as_mut()
                    .expect("section child")
                    .representative_ordinal = ordinal;
            }
        }
        candidates.extend(children);
        if limited {
            break;
        }
    }
    check_selection_budget(check)?;
    Ok((candidates, unrepresentable, construction_limited))
}

fn section_pool(
    candidates: Vec<SelectionCandidate>,
    weights: &[u64],
    check: Option<&dyn Fn() -> Result<()>>,
) -> Result<(Vec<SelectionCandidate>, bool)> {
    let mut representatives = Vec::new();
    let mut continuations = Vec::new();
    for (index, candidate) in candidates.into_iter().enumerate() {
        if index % 64 == 0 {
            check_selection_budget(check)?;
        }
        if candidate
            .section
            .as_ref()
            .is_none_or(|section| section.child_ordinal == section.representative_ordinal)
        {
            representatives.push(candidate);
        } else {
            continuations.push(candidate);
        }
    }
    let relevance = |candidate: &SelectionCandidate| {
        candidate
            .section
            .as_ref()
            .map_or(candidate.local_relevance, |section| section.local_relevance)
    };
    let seed = |candidate: &SelectionCandidate| {
        candidate
            .section
            .as_ref()
            .map_or(candidate.seed_overlap, |section| section.seed_overlap)
    };
    let position = |candidate: &SelectionCandidate| {
        candidate
            .section
            .as_ref()
            .map_or(candidate.span.start(), |section| {
                section.heading_span.start()
            })
    };
    representatives.sort_by(|a, b| {
        relevance(b)
            .cmp(&relevance(a))
            .then(seed(b).cmp(&seed(a)))
            .then(position(a).cmp(&position(b)))
            .then(a.span.end().cmp(&b.span.end()))
    });
    check_selection_budget(check)?;
    let total = representatives.len() + continuations.len();
    representatives.truncate(MAX_CANDIDATES);
    let admitted = representatives
        .iter()
        .filter_map(|candidate| {
            candidate
                .section
                .as_ref()
                .map(|section| section.heading_span.start())
        })
        .collect::<BTreeSet<_>>();
    // Never retain a continuation whose sole nominee was excluded by the cap.
    continuations.retain(|candidate| {
        admitted.contains(
            &candidate
                .section
                .as_ref()
                .expect("continuation section")
                .heading_span
                .start(),
        )
    });
    let mut covered = representatives
        .iter()
        .flat_map(|candidate| candidate.covered_terms.iter().copied())
        .collect::<BTreeSet<_>>();
    while representatives.len() < MAX_CANDIDATES && !continuations.is_empty() {
        check_selection_budget(check)?;
        let gain = |candidate: &SelectionCandidate| {
            candidate
                .covered_terms
                .iter()
                .filter(|term| !covered.contains(*term))
                .map(|&term| weights[term])
                .sum::<u64>()
        };
        let best = (0..continuations.len())
            .max_by(|&a, &b| {
                let a = &continuations[a];
                let b = &continuations[b];
                gain(a)
                    .cmp(&gain(b))
                    .then(relevance(a).cmp(&relevance(b)))
                    .then(b.clipped.cmp(&a.clipped))
                    .then(b.span.start().cmp(&a.span.start()))
                    .then(b.span.end().cmp(&a.span.end()))
            })
            .expect("nonempty continuations");
        let candidate = continuations.remove(best);
        covered.extend(candidate.covered_terms.iter().copied());
        representatives.push(candidate);
    }
    Ok((representatives, total > MAX_CANDIDATES))
}

fn select_with_sections(
    tokenizer: &Tokenizer<'_>,
    query: &str,
    parents: &[Parent<'_>],
    bytes: usize,
    preserve_sections: bool,
) -> Result<SelectionResult> {
    select_with_sections_checked(tokenizer, query, parents, bytes, preserve_sections, None)
}

fn select_with_sections_checked(
    tokenizer: &Tokenizer<'_>,
    query: &str,
    parents: &[Parent<'_>],
    bytes: usize,
    preserve_sections: bool,
    check: Option<&dyn Fn() -> Result<()>>,
) -> Result<SelectionResult> {
    check_selection_budget(check)?;
    if bytes == 0 || bytes > 2048 {
        return Err(WikiError::invalid(
            "context selection excerpt bound must be 1..=2048 bytes",
        ));
    }
    let mut result = SelectionResult {
        candidates: vec![],
        term_weights: vec![],
        omissions: vec![],
        scanned_bytes: 0,
        scanned_blocks: 0,
    };
    let (terms, term_cap) = query_terms(tokenizer, query)?;
    let mut owners = Vec::new();
    let mut frequencies = vec![0usize; terms.len()];
    for parent in parents {
        check_selection_budget(check)?;
        if parent.body > parent.raw.len() || !parent.raw.is_char_boundary(parent.body) {
            return Err(WikiError::invalid(
                "context selection body boundary invalid",
            ));
        }
        for span in parent.anchors {
            span.slice(parent.raw)?;
        }
        if term_cap {
            result.omissions.push(SelectionOmission {
                owner_index: parent.owner,
                reason: "context_query_term_cap",
            });
        }
        let capacity = OWNER_SCAN_BYTES.min(TOTAL_SCAN_BYTES.saturating_sub(result.scanned_bytes));
        // Reserve the bounded semantic fallback scans before scanning the body.
        // Overlapping fallback/body bytes count twice, conservatively; neither
        // an outside-prefix anchor nor a retry gets unmetered tokenization.
        let anchor_count = parent.anchors.len().min(2);
        let reserved = anchor_count.saturating_mul(bytes).min(capacity);
        let body_capacity = capacity - reserved;
        let end = boundary_before(parent.raw, parent.body.saturating_add(body_capacity));
        let remaining = MAX_BLOCKS.saturating_sub(result.scanned_blocks);
        let (blocks, block_limit, scanned_blocks) =
            structural_blocks(parent.raw, parent.body, end, remaining);
        result.scanned_bytes += end - parent.body;
        result.scanned_blocks += scanned_blocks;
        if end < parent.raw.len() {
            result.omissions.push(SelectionOmission {
                owner_index: parent.owner,
                reason: "context_source_scan_byte_cap",
            });
        }
        if block_limit || remaining == 0 {
            result.omissions.push(SelectionOmission {
                owner_index: parent.owner,
                reason: "context_source_scan_block_cap",
            });
        }
        let structural_end = if block_limit {
            blocks.last().map_or(parent.body, |block| block.range.end)
        } else {
            end
        };
        let mut blocks = teaching_blocks(blocks, bytes);
        for (index, block) in blocks.iter_mut().enumerate() {
            if index % 64 == 0 {
                check_selection_budget(check)?;
            }
            block.terms = matched_terms(tokenizer, parent.raw, block.range.clone(), &terms)?;
            for &term in &block.terms {
                frequencies[term] += 1;
            }
        }
        let mut fallback_windows = Vec::new();
        let mut owner_usage = end - parent.body;
        if parent.anchors.len() > 2 {
            result.omissions.push(SelectionOmission {
                owner_index: parent.owner,
                reason: "context_source_anchor_cap",
            });
        }
        for anchor in parent.anchors.iter().take(2) {
            if anchor.is_empty() {
                continue;
            }
            let allowance = (capacity - owner_usage).min(bytes);
            if allowance == 0 {
                continue;
            }
            let range = bounded_window(
                parent.raw,
                parent.body..parent.raw.len(),
                anchor.start() as usize,
                allowance,
            );
            let matched = matched_terms(tokenizer, parent.raw, range.clone(), &terms)?;
            owner_usage += range.len();
            result.scanned_bytes += range.len();
            fallback_windows.push((range, matched));
        }
        owners.push(OwnerBlocks {
            parent,
            blocks,
            fallback_windows,
            structural_end,
        });
    }
    let total_blocks: usize = owners.iter().map(|owner| owner.blocks.len()).sum();
    result.term_weights = frequencies
        .iter()
        .map(|&frequency| {
            if frequency == 0 {
                0
            } else {
                ((1.0 + ((total_blocks + 1) as f64 / frequency as f64).ln()) * 1_000_000.0).round()
                    as u64
            }
        })
        .collect();
    let mut constructed = 0;
    for owner in owners {
        check_selection_budget(check)?;
        let parent = owner.parent;
        let first_heading = preserve_sections
            .then(|| {
                owner
                    .blocks
                    .iter()
                    .find(|block| block.kind == BlockKind::Heading)
            })
            .flatten()
            .map(|block| block.range.start);
        let mut proposals = BTreeMap::<(usize, usize), SelectionCandidate>::new();
        let mut propose =
            |range: Range<usize>, covered_terms: Vec<usize>, clipped: bool| -> Result<()> {
                if range.is_empty() || range.end - range.start > bytes {
                    return Ok(());
                }
                // HTML anchors and markup-only windows have exact bytes but
                // supply no readable evidence. Apply this to merged windows
                // and semantic fallbacks as well as stand-alone blocks.
                if SourceMap::markdown(&parent.raw[..range.end], range.start)
                    .text
                    .trim()
                    .is_empty()
                {
                    return Ok(());
                }
                let span = ByteSpan::new(range.start as u64, range.end as u64)?;
                let seed_overlap = parent
                    .anchors
                    .iter()
                    .any(|anchor| anchor.start() < span.end() && span.start() < anchor.end());
                let local_relevance = covered_terms
                    .iter()
                    .map(|&term| result.term_weights[term])
                    .sum();
                let candidate = SelectionCandidate {
                    owner_index: parent.owner,
                    span,
                    covered_terms,
                    local_relevance,
                    seed_overlap,
                    clipped,
                    semantic_affinity: affinity(span, &parent.semantic),
                    section: None,
                };
                proposals
                    .entry((range.start, range.end))
                    .and_modify(|old| old.clipped &= clipped)
                    .or_insert(candidate);
                Ok(())
            };
        for (index, block) in owner.blocks.iter().enumerate() {
            if index % 64 == 0 {
                check_selection_budget(check)?;
            }
            if first_heading.is_some_and(|start| block.range.start >= start) {
                continue;
            }
            if block.range.len() > bytes {
                let map = SourceMap::markdown(&parent.raw[..block.range.end], block.range.start);
                let mut anchors = tokenizer
                    .tokens(&map.text)?
                    .into_iter()
                    .filter(|token| terms.contains_key(&term_key(&token.text)))
                    .filter_map(|token| map.original_span(token.span).map(|span| span.start))
                    .take(64)
                    .collect::<Vec<_>>();
                anchors.extend(
                    parent
                        .anchors
                        .iter()
                        .filter(|anchor| {
                            anchor.start() < block.range.end as u64
                                && anchor.end() > block.range.start as u64
                        })
                        .map(|anchor| (anchor.start() as usize).max(block.range.start)),
                );
                let mut semantic_anchors = parent
                    .semantic
                    .iter()
                    .filter(|(span, _)| {
                        span.start() < block.range.end as u64
                            && span.end() > block.range.start as u64
                    })
                    .collect::<Vec<_>>();
                semantic_anchors.sort_by(|(a_span, a_score), (b_span, b_score)| {
                    b_score
                        .total_cmp(a_score)
                        .then(a_span.start().cmp(&b_span.start()))
                });
                for &(span, _) in semantic_anchors.into_iter().take(16) {
                    let start = (span.start() as usize).max(block.range.start);
                    let end = (span.end() as usize).min(block.range.end);
                    anchors.extend([
                        start,
                        boundary_before(parent.raw, start + (end - start) / 2),
                        end.saturating_sub(bytes / 2).max(start),
                    ]);
                }
                for anchor in anchors {
                    let range = bounded_window(parent.raw, block.range.clone(), anchor, bytes);
                    let matched = matched_terms(tokenizer, parent.raw, range.clone(), &terms)?;
                    propose(range, matched, true)?;
                }
                continue;
            }
            let mut end = block.range.end;
            let mut covered = block.terms.iter().copied().collect::<BTreeSet<_>>();
            let mut clipped = block.clipped;
            // Short headings are presentation context, not useful stand-alone passages.
            if block.kind != BlockKind::Heading {
                propose(
                    block.range.clone(),
                    covered.iter().copied().collect(),
                    clipped,
                )?;
            }
            for next in owner.blocks.iter().skip(index + 1).take(8) {
                if next.kind == BlockKind::Heading
                    || next.range.end.saturating_sub(block.range.start) > bytes
                {
                    break;
                }
                end = end.max(next.range.end);
                covered.extend(&next.terms);
                clipped |= next.clipped;
                propose(
                    block.range.start..end,
                    covered.iter().copied().collect(),
                    clipped,
                )?;
            }
        }
        // Preserve semantic discovery when the query is a paraphrase, when its
        // terms are absent, or when bounded source scanning misses an anchor.
        for (range, matched) in &owner.fallback_windows {
            if let Some(start) = first_heading
                && range.start < owner.structural_end
                && range.end > start
            {
                // Only the in-section piece is already represented. Preserve
                // exact outside pieces inside this already charged byte window.
                for outside in [
                    range.start..range.end.min(start),
                    range.start.max(owner.structural_end)..range.end,
                ] {
                    if !outside.is_empty() {
                        check_selection_budget(check)?;
                        let matched =
                            matched_terms(tokenizer, parent.raw, outside.clone(), &terms)?;
                        propose(outside, matched, true)?;
                    }
                }
                continue;
            }
            propose(range.clone(), matched.clone(), true)?;
        }
        if first_heading.is_some() {
            let (section_candidates, unrepresentable, construction_limited) = section_children(
                tokenizer,
                &owner,
                &terms,
                &result.term_weights,
                bytes,
                &mut constructed,
                check,
            )?;
            if unrepresentable {
                result.omissions.push(SelectionOmission {
                    owner_index: parent.owner,
                    reason: "context_source_excerpt_byte_cap",
                });
            }
            if construction_limited {
                result.omissions.push(SelectionOmission {
                    owner_index: parent.owner,
                    reason: "context_source_section_child_cap",
                });
            }
            for candidate in section_candidates {
                proposals.insert(
                    (
                        candidate.span.start() as usize,
                        candidate.span.end() as usize,
                    ),
                    candidate,
                );
            }
        }
        let mut candidates = proposals
            .into_values()
            .filter(|candidate| {
                candidate.local_relevance > 0
                    || candidate.seed_overlap
                    || candidate.semantic_affinity.is_some()
                    || candidate
                        .section
                        .as_ref()
                        .is_some_and(|section| section.local_relevance > 0 || section.seed_overlap)
            })
            .collect::<Vec<_>>();
        if first_heading.is_some() {
            let (kept, capped) = section_pool(candidates, &result.term_weights, check)?;
            if capped {
                result.omissions.push(SelectionOmission {
                    owner_index: parent.owner,
                    reason: "context_source_candidate_cap",
                });
            }
            result.candidates.extend(kept);
            continue;
        }
        candidates.sort_by(|a, b| {
            semantic_strength(b).total_cmp(&semantic_strength(a)).then(
                b.local_relevance
                    .cmp(&a.local_relevance)
                    .then(a.clipped.cmp(&b.clipped))
                    .then(b.seed_overlap.cmp(&a.seed_overlap))
                    .then(a.span.start().cmp(&b.span.start()))
                    .then(a.span.end().cmp(&b.span.end())),
            )
        });
        // First reserve at most two proposals per dominant semantic unit (or
        // unscored source region). This is a deterministic region quota, not
        // vector MMR: no candidate-to-candidate vector similarity is claimed.
        // Fill remaining slots using lexical complements. A slightly weaker
        // distant unit must survive many windows from the strongest unit.
        let mut kept = Vec::new();
        let mut covered = BTreeSet::<usize>::new();
        let mut regions = BTreeMap::<Region, usize>::new();
        let diversify = !parent.semantic.is_empty();
        // Compute source-region membership once; do not rescan every unit for
        // every candidate on each of the bounded pool-selection rounds.
        let candidate_regions = candidates
            .iter()
            .map(|candidate| {
                (
                    (candidate.span.start(), candidate.span.end()),
                    dominant_region(candidate, &parent.semantic, bytes),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let region = |candidate: &SelectionCandidate| {
            candidate_regions[&(candidate.span.start(), candidate.span.end())]
        };
        let mut complementary_fill = false;
        while !candidates.is_empty() && kept.len() < MAX_CANDIDATES {
            let best = candidates
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    !diversify
                        || complementary_fill
                        || regions.get(&region(candidate)).copied().unwrap_or(0) < 2
                })
                .max_by(|(ai, a), (bi, b)| {
                    let gain = |candidate: &SelectionCandidate| {
                        candidate
                            .covered_terms
                            .iter()
                            .filter(|term| !covered.contains(*term))
                            .map(|&term| result.term_weights[term])
                            .sum::<u64>()
                    };
                    if complementary_fill {
                        gain(a)
                            .cmp(&gain(b))
                            .then(semantic_strength(a).total_cmp(&semantic_strength(b)))
                            .then(bi.cmp(ai))
                    } else {
                        semantic_strength(a)
                            .total_cmp(&semantic_strength(b))
                            .then(gain(a).cmp(&gain(b)))
                            .then(bi.cmp(ai))
                    }
                })
                .map(|(index, _)| index);
            let Some(best) = best else {
                complementary_fill = true;
                continue;
            };
            let candidate = candidates.remove(best);
            *regions.entry(region(&candidate)).or_default() += 1;
            covered.extend(candidate.covered_terms.iter().copied());
            kept.push(candidate);
        }
        if !candidates.is_empty() {
            result.omissions.push(SelectionOmission {
                owner_index: parent.owner,
                reason: "context_source_candidate_cap",
            });
        }
        result.candidates.extend(kept);
    }
    Ok(result)
}

#[cfg(test)]
#[path = "section_context_selection_tests.rs"]
mod section_context_selection_tests;

#[cfg(test)]
mod tests {
    use super::*;
    fn selection(raw: &str, query: &str, bytes: usize, anchors: &[ByteSpan]) -> SelectionResult {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let tokenizer = Tokenizer::new(&connection).unwrap();
        select(
            &tokenizer,
            query,
            &[Parent {
                owner: 0,
                raw,
                body: 0,
                anchors,
                semantic: vec![],
            }],
            bytes,
        )
        .unwrap()
    }
    #[test]
    fn finds_late_source_block_and_keeps_explanation_with_commands() {
        let raw = "# General\n\nEarly unrelated material.\n\n# Special operation\n\nIgnore expensive cases during ordinary execution.\n\n```sh\nrunner test --ignored\nrunner test --include-ignored\n```\n";
        let seed = ByteSpan::new(0, 37).unwrap();
        let selected = selection(raw, "How are ignored cases executed?", 512, &[seed]);
        let coherent = selected
            .candidates
            .iter()
            .find(|candidate| {
                let text = candidate.span.slice(raw).unwrap();
                !candidate.clipped
                    && text.contains("Ignore expensive")
                    && text.contains("--include-ignored")
            })
            .unwrap();
        assert!(!coherent.covered_terms.is_empty());
        assert!(!coherent.seed_overlap);
        for candidate in selected.candidates {
            assert!(candidate.span.slice(raw).unwrap().len() <= 512);
        }
    }
    #[test]
    fn retains_complete_priority_list_with_intro_and_terminal_command_explanation() {
        let raw = "# Priority\n\nChoose settings in priority order:\n\n- Command options\n- Environment variables\n- Local file\n- Global file\n\n# Selection\n\nTo select a subset use this invocation.\n\n```sh\nrunner --selected\n```\n\nTo include all items instead use `runner --all`.\n";
        let selected = selection(
            raw,
            "What is the priority and how are all items selected?",
            512,
            &[],
        );
        assert!(selected.candidates.iter().any(|candidate| {
            let text = candidate.span.slice(raw).unwrap();
            !candidate.clipped && text.contains("priority order") && text.contains("Global file")
        }));
        assert!(selected.candidates.iter().any(|candidate| {
            let text = candidate.span.slice(raw).unwrap();
            !candidate.clipped
                && text.contains("runner --selected")
                && text.contains("runner --all")
        }));
        assert!(!selected.candidates.iter().any(|candidate| {
            let text = candidate.span.slice(raw).unwrap();
            text.contains("priority order") && !text.contains("Global file")
        }));
    }
    #[test]
    fn grammatical_question_words_do_not_outvote_repeated_content_terms() {
        let raw = format!(
            "# Questions\n\nHow should one ask about this?\n\n{}",
            "The worker selects items without flag123 for 中文 users.\n\n".repeat(40)
        );
        let selected = selection(
            &raw,
            "How should items be selected without 中文 flag123?",
            160,
            &[],
        );
        let first = selected
            .candidates
            .first()
            .unwrap()
            .span
            .slice(&raw)
            .unwrap();
        assert!(first.contains("worker selects items"));
        // A coherent window may include the question paragraph as surrounding
        // context. That paragraph must not produce a candidate on its own.
        assert!(!selected.candidates.iter().any(|candidate| {
            let text = candidate.span.slice(&raw).unwrap();
            text.contains("How should") && !text.contains("worker selects items")
        }));
        assert!(selected.candidates.iter().any(|candidate| {
            candidate
                .span
                .slice(&raw)
                .unwrap()
                .contains("without flag123 for 中文")
        }));
    }
    #[test]
    fn grammar_spellings_marked_as_query_identifiers_remain_retrievable() {
        for (raw, query) in [
            (
                "The `can` operation is supported.\n",
                "How does `can` work?",
            ),
            (
                "Supply --with for this operation.\n",
                "What does --with mean?",
            ),
            (
                "Read object.is for the result.\n",
                "How does object.is work?",
            ),
            (
                "The _from field stores the start.\n",
                "What does _from mean?",
            ),
            ("Call is() for this check.\n", "How does is() work?"),
        ] {
            let selected = selection(raw, query, 128, &[]);
            assert!(
                selected
                    .candidates
                    .iter()
                    .any(|candidate| candidate.local_relevance > 0),
                "{query}"
            );
        }
        // Punctuation at the end of normal grammar does not revive a stopword.
        let selected = selection("How is it?\n", "How is it.", 128, &[]);
        assert!(selected.candidates.is_empty());
        // Isolate the identifier feature: `object` does not occur anywhere in
        // the source, so it cannot lend relevance to an adjacent merged block.
        let raw = "The is operation is supported.\n";
        let selected = selection(raw, "How does object.is work?", 128, &[]);
        assert!(selected.candidates.iter().any(|candidate| {
            candidate.span.slice(raw).unwrap().contains("is operation")
                && candidate.local_relevance > 0
        }));
    }
    #[test]
    fn complementary_terms_survive_candidate_cap_and_unicode_is_exact() {
        let raw = format!(
            "{}\n# Second facet\n\nOrbit behavior around café 中文 🦀.\n",
            "Quasar behavior in a repeated paragraph.\n\n".repeat(70)
        );
        let selected = selection(&raw, "quasar orbit", 128, &[]);
        assert_eq!(selected.candidates.len(), MAX_CANDIDATES);
        assert!(
            selected.candidates.iter().any(|candidate| candidate
                .span
                .slice(&raw)
                .unwrap()
                .contains("Orbit"))
        );
        for candidate in selected.candidates {
            candidate.span.slice(&raw).unwrap();
        }
        assert!(
            selected
                .omissions
                .iter()
                .any(|omission| omission.reason == "context_source_candidate_cap")
        );
    }
    #[test]
    fn huge_block_and_semantic_fallback_are_bounded_and_labeled() {
        let raw = "🦀 explanatory prose ".repeat(100);
        let seed = ByteSpan::new(0, "🦀 explanatory prose ".len() as u64).unwrap();
        let selected = selection(&raw, "different vocabulary", 97, &[seed]);
        assert!(!selected.candidates.is_empty());
        for candidate in selected.candidates {
            assert!(candidate.clipped);
            assert!(candidate.span.slice(&raw).unwrap().len() <= 97);
        }
    }
    #[test]
    fn scan_and_query_limits_are_reported_without_invalid_spans() {
        let raw = "ordinary prose\n\n".repeat(OWNER_SCAN_BYTES / 8);
        let query = (0..MAX_CONTEXT_QUERY_TERMS + 1)
            .map(|i| format!("feature{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let selected = selection(&raw, &query, 128, &[]);
        assert!(selected.scanned_bytes <= OWNER_SCAN_BYTES);
        assert!(selected.scanned_blocks <= MAX_BLOCKS);
        assert!(selected.term_weights.len() <= MAX_CONTEXT_QUERY_TERMS);
        assert!(
            selected
                .omissions
                .iter()
                .any(|omission| omission.reason == "context_query_term_cap")
        );
        assert!(
            selected
                .omissions
                .iter()
                .any(|omission| omission.reason == "context_source_scan_byte_cap")
        );
        assert!(
            selected
                .omissions
                .iter()
                .any(|omission| omission.reason == "context_source_scan_block_cap")
        );
    }
    #[test]
    fn cached_semantics_retrieves_paraphrases_and_prioritizes_late_units_under_cap() {
        let raw = (0..100)
            .map(|index| format!("The distant signal at position {index} is documented.\n\n"))
            .collect::<String>();
        let late = raw.find("The distant signal at position 99").unwrap();
        let semantic = vec![
            (ByteSpan::new(0, late as u64).unwrap(), 0.1),
            (ByteSpan::new(late as u64, raw.len() as u64).unwrap(), 0.9),
        ];
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let tokenizer = Tokenizer::new(&connection).unwrap();
        let run = || {
            select(
                &tokenizer,
                "unrelated paraphrase vocabulary",
                &[Parent {
                    owner: 0,
                    raw: &raw,
                    body: 0,
                    anchors: &[],
                    semantic: semantic.clone(),
                }],
                128,
            )
            .unwrap()
        };
        let selected = run();
        assert_eq!(selected.candidates.len(), MAX_CANDIDATES);
        assert!(
            selected
                .candidates
                .first()
                .unwrap()
                .span
                .slice(&raw)
                .unwrap()
                .contains("position 99")
        );
        assert!(selected.candidates.iter().all(
            |candidate| candidate.local_relevance == 0 && candidate.semantic_affinity.is_some()
        ));
        let replay = run();
        assert_eq!(
            selected
                .candidates
                .iter()
                .map(|candidate| candidate.span)
                .collect::<Vec<_>>(),
            replay
                .candidates
                .iter()
                .map(|candidate| candidate.span)
                .collect::<Vec<_>>()
        );
        assert!(
            selection(&raw, "unrelated paraphrase vocabulary", 128, &[])
                .candidates
                .is_empty()
        );
    }
    #[test]
    fn available_semantic_coverage_is_weighted_and_missing_is_not_zero() {
        let cues = vec![
            (ByteSpan::new(0, 6).unwrap(), 0.2),
            (ByteSpan::new(6, 11).unwrap(), 0.8),
        ];
        let mixed = affinity(ByteSpan::new(3, 11).unwrap(), &cues).unwrap();
        assert!((mixed - 0.575).abs() < 1e-12);
        assert_eq!(affinity(ByteSpan::new(12, 16).unwrap(), &cues), None);
        assert_eq!(affinity(ByteSpan::new(0, 4).unwrap(), &[]), None);
        let lexical = selection("A documented quasar.\n", "quasar", 128, &[]);
        assert!(
            lexical
                .candidates
                .iter()
                .all(|candidate| candidate.semantic_affinity.is_none())
        );
    }
    #[test]
    fn semantic_cues_reject_stale_invalid_and_over_budget_hints() {
        let raw = "🦀 alpha\n";
        let owner = VaultRelativePath::new("fixture.md").unwrap();
        let hash = Blake3Hash::digest(raw.as_bytes());
        let cue = ContextSemanticCue {
            owner: owner.clone(),
            observed_hash: hash.clone(),
            span: ByteSpan::new(0, raw.len() as u64).unwrap(),
            cosine: 0.7,
        };
        assert!(validated_cues(&owner, &hash, raw, 0, std::slice::from_ref(&cue)).is_ok());
        for invalid in [
            ContextSemanticCue {
                observed_hash: Blake3Hash::digest(b"stale"),
                ..cue.clone()
            },
            ContextSemanticCue {
                span: ByteSpan::new(1, 4).unwrap(),
                ..cue.clone()
            },
            ContextSemanticCue {
                span: ByteSpan::new(0, 100).unwrap(),
                ..cue.clone()
            },
            ContextSemanticCue {
                span: ByteSpan::new(4, 4).unwrap(),
                ..cue.clone()
            },
            ContextSemanticCue {
                cosine: f64::NAN,
                ..cue.clone()
            },
            ContextSemanticCue {
                cosine: f64::INFINITY,
                ..cue.clone()
            },
            ContextSemanticCue {
                cosine: 1.1,
                ..cue.clone()
            },
        ] {
            assert!(validated_cues(&owner, &hash, raw, 0, &[invalid]).is_err());
        }
        assert!(validated_cues(&owner, &hash, raw, 4, std::slice::from_ref(&cue)).is_err());
        assert!(
            validated_cues(
                &owner,
                &hash,
                raw,
                0,
                &vec![cue.clone(); MAX_SEMANTIC_CUES + 1]
            )
            .is_err()
        );
        let other = VaultRelativePath::new("other.md").unwrap();
        assert!(
            validated_cues(&other, &hash, raw, 0, std::slice::from_ref(&cue))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            validated_cues(&owner, &hash, raw, 0, &[cue.clone(), cue.clone()])
                .unwrap()
                .len(),
            1
        );
        assert!(
            validated_cues(
                &owner,
                &hash,
                raw,
                0,
                &[cue.clone(), ContextSemanticCue { cosine: 0.3, ..cue }]
            )
            .is_err()
        );
    }
    #[test]
    fn distant_semantic_region_survives_many_stronger_unit_window_variants() {
        let primary =
            "The primary mechanism has several ordinary implementation details.\n\n".repeat(70);
        let raw = format!(
            "{primary}# Complement\n\nThe secondary mechanism supplies a different necessary facet.\n"
        );
        let semantic = vec![
            (ByteSpan::new(0, primary.len() as u64).unwrap(), 0.9),
            (
                ByteSpan::new(primary.len() as u64, raw.len() as u64).unwrap(),
                0.8,
            ),
        ];
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let tokenizer = Tokenizer::new(&connection).unwrap();
        let run = || {
            select(
                &tokenizer,
                "unrelated request",
                &[Parent {
                    owner: 0,
                    raw: &raw,
                    body: 0,
                    anchors: &[],
                    semantic: semantic.clone(),
                }],
                256,
            )
            .unwrap()
        };
        let selected = run();
        assert_eq!(selected.candidates.len(), MAX_CANDIDATES);
        assert!(selected.candidates.iter().any(|candidate| {
            candidate
                .span
                .slice(&raw)
                .unwrap()
                .contains("secondary mechanism")
        }));
        assert_eq!(
            selected
                .candidates
                .iter()
                .map(|candidate| candidate.span)
                .collect::<Vec<_>>(),
            run()
                .candidates
                .iter()
                .map(|candidate| candidate.span)
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn markup_only_windows_are_rejected_and_tiny_prose_keeps_neighboring_context() {
        let anchors = "<a id=first></a>\n\n<a id=second></a>\n";
        let raw = "# Context\n\nAside.\n\nA substantial explanation supplies the actual behavior of this mechanism.\n";
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let tokenizer = Tokenizer::new(&connection).unwrap();
        for source in [anchors, raw] {
            let selected = select(
                &tokenizer,
                "unrelated request",
                &[Parent {
                    owner: 0,
                    raw: source,
                    body: 0,
                    anchors: &[],
                    semantic: vec![(ByteSpan::new(0, source.len() as u64).unwrap(), 0.9)],
                }],
                256,
            )
            .unwrap();
            if source == anchors {
                assert!(selected.candidates.is_empty());
            } else {
                assert!(!selected.candidates.is_empty());
                assert!(
                    !selected.candidates.iter().any(|candidate| candidate
                        .span
                        .slice(source)
                        .unwrap()
                        .trim()
                        == "Aside.")
                );
                assert!(selected.candidates.iter().any(|candidate| {
                    let text = candidate.span.slice(source).unwrap();
                    text.contains("Aside.") && text.contains("substantial explanation")
                }));
            }
        }
        // A short isolated non-English note remains useful content.
        assert!(
            !selection("安全な設定です。\n", "安全な設定です", 128, &[])
                .candidates
                .is_empty()
        );
    }
}

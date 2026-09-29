//! Safe literal/lexical document discovery. No cache opens, sync, or model calls.
use super::{
    cursor,
    excerpts::{SourceMap, Tokenizer},
    filters,
    literal::literal_matches,
    types::*,
};
use crate::{
    catalog::{DocumentRow, ReaderSnapshot, SnapshotVerification},
    domain::*,
    records::parse_note,
};
use rusqlite::{params_from_iter, types::Value};
use std::{cmp::Ordering, collections::BTreeMap, ops::Range};

pub fn lexical_expression(query: &str) -> Result<String> {
    validate_query(query)?;
    let terms: Vec<_> = query.split_whitespace().collect();
    if terms.len() > 64 {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "lexical query exceeds 64 whitespace terms",
        ));
    }
    Ok(terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR "))
}
fn validate_query(query: &str) -> Result<()> {
    if query.trim().is_empty() {
        return Err(WikiError::new(ErrorCode::Usage, "query must not be blank"));
    }
    if query.len() > 4096 {
        return Err(WikiError::new(ErrorCode::Usage, "query exceeds 4096 bytes"));
    }
    if query.contains('\0') {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "query must not contain NUL",
        ));
    }
    Ok(())
}
/// Validate a request without opening a cache or creating a tokenizer.
pub fn validate_plan(query: &str, plan: &QueryPlan) -> Result<QueryPlan> {
    validate_query(query)?;
    validate_limits(&plan.limits)?;
    if plan.mode == SearchMode::Lexical {
        lexical_expression(query)?;
    }
    let mut plan = plan.clone();
    plan.filters = filters::normalize(&plan.filters)?;
    Ok(plan)
}
fn validate_limits(limits: &SearchLimits) -> Result<()> {
    if limits.hits == 0
        || limits.hits > 50
        || limits.candidates == 0
        || limits.candidates > 80
        || limits.excerpt_bytes == 0
        || limits.excerpt_bytes > 2048
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "search limits require 1..50 hits, 1..80 candidates and 1..2048 excerpt bytes",
        ));
    }
    Ok(())
}
#[derive(Clone)]
struct Candidate {
    document: DocumentRow,
    tier: u8,
    score: Option<f64>,
    reasons: Vec<RetrievalReason>,
    ranks: Vec<RankContribution>,
    identity: bool,
}
fn compare(a: &Candidate, b: &Candidate) -> Ordering {
    a.tier
        .cmp(&b.tier)
        .then_with(|| a.score.unwrap_or(0.0).total_cmp(&b.score.unwrap_or(0.0)))
        .then_with(|| {
            a.document
                .record_id
                .as_ref()
                .map(RecordId::as_str)
                .unwrap_or(a.document.path.as_str())
                .as_bytes()
                .cmp(
                    b.document
                        .record_id
                        .as_ref()
                        .map(RecordId::as_str)
                        .unwrap_or(b.document.path.as_str())
                        .as_bytes(),
                )
        })
        .then(a.document.path.cmp(&b.document.path))
}

pub fn search(reader: &ReaderSnapshot, query: &str, plan: &QueryPlan) -> Result<HitSet> {
    search_inner(reader, query, plan, None)
}
/// Context eligibility is applied before every candidate and hit limit.
pub(crate) fn search_context(
    reader: &ReaderSnapshot,
    query: &str,
    plan: &QueryPlan,
    historical: bool,
) -> Result<HitSet> {
    search_inner(reader, query, plan, Some(historical))
}
fn search_inner(
    reader: &ReaderSnapshot,
    query: &str,
    plan: &QueryPlan,
    context_scope: Option<bool>,
) -> Result<HitSet> {
    if !matches!(plan.mode, SearchMode::Literal | SearchMode::Lexical) {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "semantic and hybrid retrieval require the embedding application",
        ));
    }
    let plan = validate_plan(query, plan)?;
    let base_fingerprint = cursor::fingerprint(query, &plan)?;
    let fingerprint = if let Some(historical) = context_scope {
        Blake3Hash::digest(
            serde_json::to_vec(&("lwiki-context-documents-v1", base_fingerprint, historical))
                .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
        )
    } else {
        base_fingerprint
    };
    let context_policy = context_scope
        .map(filters::context_policy)
        .unwrap_or("1".into());
    let offset = cursor::offset(
        reader,
        &fingerprint,
        plan.cursor.as_deref(),
        plan.limits.candidates,
    )?;
    let tokenizer = if plan.mode == SearchMode::Lexical {
        Some(Tokenizer::new(reader.connection())?)
    } else {
        None
    };
    let expression = if plan.mode == SearchMode::Lexical {
        let expression = lexical_expression(query)?;
        if tokenizer
            .as_ref()
            .expect("lexical tokenizer")
            .tokens(query)?
            .is_empty()
        {
            let mut error =
                WikiError::new(ErrorCode::Usage, "lexical query has no searchable tokens");
            error.hint = Some("Use literal mode for punctuation-only queries".into());
            return Err(error);
        }
        Some(expression)
    } else {
        None
    };
    let mut candidates: BTreeMap<VaultRelativePath, Candidate> = BTreeMap::new();
    let mut overflow = false;
    let mut literal_count = 0usize;
    if let Some(expression) = &expression {
        for (tier, condition, reason, channel, fts, identity) in [
            (
                0,
                "d.record_id=?1",
                RetrievalReason::ExactId,
                "exact_id",
                false,
                false,
            ),
            (
                1,
                "d.title=?1",
                RetrievalReason::ExactTitle,
                "exact_title",
                false,
                false,
            ),
            (
                1,
                "EXISTS(SELECT 1 FROM json_each(d.row_json,'$.aliases') WHERE value=?1)",
                RetrievalReason::ExactAlias,
                "exact_alias",
                false,
                false,
            ),
            (
                2,
                "documents_fts MATCH ?1",
                RetrievalReason::Lexical,
                "lexical",
                true,
                false,
            ),
            (
                2,
                "documents_fts MATCH ?1",
                RetrievalReason::Lexical,
                "identity_lexical",
                true,
                true,
            ),
        ] {
            let text = if fts {
                if identity {
                    format!("{{title aliases}} : ({expression})")
                } else {
                    expression.clone()
                }
            } else {
                query.into()
            };
            let mut values = vec![
                Value::Text(text),
                Value::Integer(i64::try_from(reader.snapshot().generation).map_err(|_| {
                    WikiError::new(ErrorCode::IndexCorrupt, "generation outside SQL range")
                })?),
            ];
            let common = filters::sql(&plan.filters, &mut values);
            let policy = if identity {
                filters::identity_policy().to_owned()
            } else if !fts {
                format!(
                    "({}) OR ({})",
                    filters::normal_policy(&plan.filters),
                    filters::identity_policy()
                )
            } else {
                filters::normal_policy(&plan.filters)
            };
            let bound = filters::bind(
                &mut values,
                Value::Integer((plan.limits.candidates + 1) as i64),
            );
            let joins = if fts {
                "JOIN documents_fts ON documents_fts.doc_row=d.doc_row AND documents_fts.gen=d.gen"
            } else {
                ""
            };
            let score = if fts {
                if identity {
                    "bm25(documents_fts,8,6,0,0,0,0,0)"
                } else {
                    "bm25(documents_fts,8,6,3,2,1,0,0)"
                }
            } else {
                "NULL"
            };
            let sql = format!(
                "SELECT d.row_json,{score} AS score FROM documents d {joins} LEFT JOIN records r ON r.gen=d.gen AND r.id=d.record_id WHERE d.gen=?2 AND ({condition}) AND ({common}) AND ({policy}) AND ({context_policy}) ORDER BY score,coalesce(d.record_id,d.path),d.path LIMIT {bound}"
            );
            let mut statement = reader.connection().prepare(&sql).map_err(sql_error)?;
            let rows = statement
                .query_map(params_from_iter(values), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<f64>>(1)?))
                })
                .map_err(sql_error)?;
            for (index, row) in rows.enumerate() {
                if index == plan.limits.candidates {
                    overflow = true;
                    break;
                }
                let (serialized, score) = row.map_err(sql_error)?;
                let document: DocumentRow = serde_json::from_str(&serialized)
                    .map_err(|e| WikiError::new(ErrorCode::IndexCorrupt, e.to_string()))?;
                if score.is_some_and(|score| !score.is_finite()) {
                    return Err(WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "non-finite lexical rank",
                    ));
                }
                let identity = identity || filters::identity_only(reader, &document, &plan.filters);
                let contribution = RankContribution {
                    channel: channel.into(),
                    rank: index + 1,
                    score,
                };
                let candidate = Candidate {
                    document: document.clone(),
                    tier,
                    score,
                    reasons: vec![reason],
                    ranks: vec![contribution.clone()],
                    identity,
                };
                if let Some(existing) = candidates.get_mut(&document.path) {
                    if !existing.reasons.contains(&reason) {
                        existing.reasons.push(reason);
                    }
                    existing.ranks.push(contribution);
                    if compare(&candidate, existing) == Ordering::Less {
                        existing.tier = tier;
                        existing.score = score;
                        existing.identity = identity;
                    }
                } else {
                    candidates.insert(document.path.clone(), candidate);
                }
            }
        }
    } else {
        let mut values = vec![Value::Integer(
            i64::try_from(reader.snapshot().generation).map_err(|_| {
                WikiError::new(ErrorCode::IndexCorrupt, "generation outside SQL range")
            })?,
        )];
        let common = filters::sql(&plan.filters, &mut values);
        let policy = filters::normal_policy(&plan.filters);
        let sql = format!(
            "SELECT d.row_json FROM documents d LEFT JOIN records r ON r.gen=d.gen AND r.id=d.record_id WHERE d.gen=?1 AND ({common}) AND ({policy}) AND ({context_policy}) ORDER BY coalesce(d.record_id,d.path),d.path"
        );
        let mut statement = reader.connection().prepare(&sql).map_err(sql_error)?;
        let rows = statement
            .query_map(params_from_iter(values), |row| row.get::<_, String>(0))
            .map_err(sql_error)?;
        for row in rows {
            let document: DocumentRow = serde_json::from_str(&row.map_err(sql_error)?)
                .map_err(|e| WikiError::new(ErrorCode::IndexCorrupt, e.to_string()))?;
            if !literal_matches(&document.raw_text, query).is_empty() {
                literal_count += 1;
                if candidates.len() == plan.limits.candidates {
                    continue;
                }
                let rank = literal_count;
                candidates.insert(
                    document.path.clone(),
                    Candidate {
                        document,
                        tier: 0,
                        score: None,
                        reasons: vec![RetrievalReason::Literal],
                        ranks: vec![RankContribution {
                            channel: "literal".into(),
                            rank,
                            score: None,
                        }],
                        identity: false,
                    },
                );
            }
        }
    }
    let candidate_count = candidates.len().max(literal_count);
    let mut candidates: Vec<_> = candidates.into_values().collect();
    candidates.sort_by(compare);
    let omitted_candidates = candidate_count
        .saturating_sub(plan.limits.candidates)
        .max(usize::from(overflow));
    candidates.truncate(plan.limits.candidates);
    let end = (offset + plan.limits.hits).min(candidates.len());
    let mut hits = Vec::new();
    for candidate in candidates
        .iter()
        .skip(offset)
        .take(end.saturating_sub(offset))
    {
        let document = &candidate.document;
        let record = filters::row(reader, document);
        let record_ref = if let (Some(id), Some(kind)) = (&document.record_id, document.kind) {
            Some(RecordRef {
                vault_id: reader.projection().vault_id.clone(),
                record_id: id.clone(),
                expected_kind: kind,
            })
        } else {
            document
                .owner_revision
                .as_ref()
                .and_then(|id| reader.projection().records.get(id))
                .filter(|row| row.record.kind() == RecordKind::Revision)
                .map(|row| RecordRef {
                    vault_id: reader.projection().vault_id.clone(),
                    record_id: row.record.id().clone(),
                    expected_kind: RecordKind::Revision,
                })
        };
        let mut reasons = candidate.reasons.clone();
        if candidate.identity {
            reasons.push(RetrievalReason::Identity);
        }
        let matches = match_ranges(
            tokenizer.as_ref(),
            document,
            query,
            plan.mode,
            candidate.identity,
            plan.limits.excerpt_bytes,
        )?;
        let excerpt = excerpt(
            reader,
            document,
            &matches,
            plan.limits.excerpt_bytes,
            plan.mode,
            candidate.identity,
        )?;
        hits.push(SearchHit {
            locator: DocumentLocator {
                record: record_ref,
                path: document.path.clone(),
                observed_hash: document.hash.clone(),
            },
            title: document.title.clone(),
            kind: document.kind,
            authored_status: record.and_then(|row| row.authored_status.clone()),
            eligibility: document.eligibility,
            identity_eligibility: record.and_then(|row| row.identity_eligibility),
            excerpt,
            reasons,
            rank_contributions: candidate.ranks.clone(),
            source_id: document.source_id.clone(),
            owner_revision: document.owner_revision.clone(),
        });
    }
    let next_cursor = if end < candidates.len() {
        Some(cursor::encode(reader, fingerprint, end)?)
    } else {
        None
    };
    let mut warnings = Vec::new();
    if omitted_candidates > 0 {
        warnings.push("candidate_cap_reached; omitted_candidates is a lower bound".into());
    }
    let dependency_fingerprint = Blake3Hash::digest(
        serde_json::to_vec(&reader.projection().dependencies)
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
    );
    Ok(HitSet {
        network_used: false,
        graph: None,
        hits,
        next_cursor,
        truncated: end < candidates.len() || omitted_candidates > 0,
        candidate_count,
        omitted_candidates,
        snapshot: reader.snapshot().clone(),
        verification: reader.verification().clone(),
        dependency_fingerprint,
        warnings,
    })
}
fn sql_error(error: rusqlite::Error) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, format!("retrieval SQL: {error}"))
}

fn match_ranges(
    tokenizer: Option<&Tokenizer<'_>>,
    document: &DocumentRow,
    query: &str,
    mode: SearchMode,
    identity: bool,
    excerpt_bytes: usize,
) -> Result<Vec<Range<usize>>> {
    if mode == SearchMode::Literal {
        return Ok(literal_matches(&document.raw_text, query));
    }
    if identity {
        return Ok(vec![]);
    }
    let tokenizer = tokenizer
        .ok_or_else(|| WikiError::new(ErrorCode::Internal, "lexical tokenizer missing"))?;
    let offset = if document.owner_revision.is_some() {
        0
    } else {
        let note = parse_note(document.raw_text.as_bytes());
        note.raw.len() - note.body().len()
    };
    let map = SourceMap::markdown(&document.raw_text, offset);
    let tokens = tokenizer.tokens(&map.text)?;
    let mut matches = Vec::new();
    // Prefer the complete query over an early isolated word. Search the full
    // document before the per-term match cap, so roundup pages lead to the
    // named section even when an earlier query word occurs many times.
    let phrase = tokenizer.tokens(query)?;
    if phrase.len() > 1 {
        for window in tokens.windows(phrase.len()) {
            if window.iter().zip(&phrase).all(|(a, b)| a.text == b.text) {
                let normalized = window[0].span.start..window[window.len() - 1].span.end;
                if let Some(original) = map.original_span(normalized.clone())
                    && original.len() <= excerpt_bytes.saturating_mul(2) / 3
                    && document.raw_text.get(original.clone()) == map.text.get(normalized)
                {
                    matches.push(original);
                    if matches.len() == 64 {
                        break;
                    }
                }
            }
        }
        if !matches.is_empty() {
            return Ok(matches);
        }
    }
    for term in query.split_whitespace() {
        let phrase = tokenizer.tokens(term)?;
        if phrase.is_empty() {
            continue;
        }
        for window in tokens.windows(phrase.len()) {
            if window
                .iter()
                .zip(&phrase)
                .all(|(source, query)| source.text == query.text)
            {
                let normalized = window.first().expect("nonempty").span.start
                    ..window.last().expect("nonempty").span.end;
                if let Some(original) = map.original_span(normalized.clone())
                    && document.raw_text.get(original.clone()) == map.text.get(normalized)
                {
                    matches.push(original);
                }
            }
            if matches.len() >= 64 {
                break;
            }
        }
        if matches.len() >= 64 {
            break;
        }
    }
    matches.sort_by_key(|span| span.start);
    matches.dedup();
    Ok(matches)
}
pub(crate) fn excerpt(
    reader: &ReaderSnapshot,
    document: &DocumentRow,
    matches: &[Range<usize>],
    bytes: usize,
    mode: SearchMode,
    identity: bool,
) -> Result<SearchExcerpt> {
    let raw = &document.raw_text;
    let body = if document.owner_revision.is_some() {
        0
    } else {
        let note = parse_note(raw.as_bytes());
        note.raw.len() - note.body().len()
    };
    if identity {
        return Ok(SearchExcerpt {
            text: String::new(),
            span: ByteSpan::new(body as u64, body as u64)?,
            matched_spans: vec![],
            label: ExcerptLabel::NoteText,
            citation: None,
        });
    }
    let anchor = matches.first().map_or(body, |span| span.start);
    let mut start = anchor.saturating_sub(bytes / 3).max(
        if mode == SearchMode::Lexical || matches.is_empty() {
            body
        } else {
            0
        },
    );
    while start < raw.len() && !raw.is_char_boundary(start) {
        start += 1;
    }
    let mut end = (start + bytes).min(raw.len());
    while end > start && !raw.is_char_boundary(end) {
        end -= 1;
    }
    let span = ByteSpan::new(start as u64, end as u64)?;
    let matched_spans = matches
        .iter()
        .filter(|matched| matched.start >= start && matched.end <= end)
        .map(|matched| ByteSpan::new(matched.start as u64, matched.end as u64))
        .collect::<Result<Vec<_>>>()?;
    let text = raw
        .get(start..end)
        .ok_or_else(|| WikiError::new(ErrorCode::IndexCorrupt, "excerpt boundaries invalid"))?
        .to_owned();
    let citation = if !span.is_empty()
        && matches!(
            reader.verification(),
            SnapshotVerification::VerifiedSnapshot { .. }
        )
        && matches!(
            document.eligibility,
            Eligibility::Current | Eligibility::Historical | Eligibility::Withdrawn
        ) {
        if let (Some(source), Some(revision)) = (&document.source_id, &document.owner_revision) {
            Some(CitationRef::Source(SourceSpanRef {
                source_id: source.clone(),
                source_revision: revision.clone(),
                span,
                quote_hash: Blake3Hash::digest(text.as_bytes()),
            }))
        } else {
            None
        }
    } else {
        None
    };
    Ok(SearchExcerpt {
        text,
        span,
        matched_spans,
        label: if citation.is_some() {
            ExcerptLabel::CapturedSource
        } else {
            ExcerptLabel::NoteText
        },
        citation,
    })
}

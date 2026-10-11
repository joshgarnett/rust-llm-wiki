//! Safe literal/lexical document discovery. No cache opens, sync, or model calls.
use super::{
    cursor,
    excerpts::{SourceMap, Tokenizer},
    filters,
    literal::literal_matches,
    types::*,
};
use crate::{
    catalog::{DocumentRow, ReaderSnapshot, SnapshotVerification, query_types::QueryCatalog},
    domain::*,
    records::parse_note,
};
use rusqlite::{
    Connection, Row, params_from_iter,
    types::{Value, ValueRef},
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

pub fn lexical_expression(query: &str) -> Result<String> {
    validate_query(query)?;
    let terms: Vec<_> = query.split_whitespace().collect();
    if terms.len() > MAX_LEXICAL_TERMS {
        return Err(WikiError::new(
            ErrorCode::Usage,
            format!("lexical query exceeds {MAX_LEXICAL_TERMS} whitespace terms"),
        ));
    }
    Ok(terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR "))
}
pub(crate) fn validate_query(query: &str) -> Result<()> {
    if query.trim().is_empty() {
        return Err(WikiError::new(ErrorCode::Usage, "query must not be blank"));
    }
    if query.len() > MAX_QUERY_BYTES {
        return Err(WikiError::new(
            ErrorCode::Usage,
            format!("query exceeds {MAX_QUERY_BYTES} UTF-8 bytes"),
        ));
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
    if matches!(plan.mode, SearchMode::Lexical | SearchMode::Hybrid) {
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
    search_catalog(reader, query, plan)
}
pub(crate) fn search_catalog(
    reader: &dyn QueryCatalog,
    query: &str,
    plan: &QueryPlan,
) -> Result<HitSet> {
    search_inner(reader, query, plan, None, false)
}
/// Context eligibility is applied before every candidate and hit limit.
pub(crate) fn search_context(
    reader: &ReaderSnapshot,
    query: &str,
    plan: &QueryPlan,
    historical: bool,
) -> Result<HitSet> {
    search_context_catalog(reader, query, plan, historical)
}
pub(crate) fn search_context_catalog(
    reader: &dyn QueryCatalog,
    query: &str,
    plan: &QueryPlan,
    historical: bool,
) -> Result<HitSet> {
    search_inner(reader, query, plan, Some(historical), false)
}

/// Indexed evidence starts with captured sources; other domains remain explicit.
pub(crate) fn search_indexed_sources(
    reader: &dyn QueryCatalog,
    query: &str,
    plan: &QueryPlan,
) -> Result<HitSet> {
    if plan.mode != SearchMode::Lexical {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "indexed evidence requires lexical mode",
        ));
    }
    validate_source_filters(&plan.filters)?;
    search_inner(reader, query, plan, Some(false), true)
}

fn search_inner(
    reader: &dyn QueryCatalog,
    query: &str,
    plan: &QueryPlan,
    context_scope: Option<bool>,
    source_only: bool,
) -> Result<HitSet> {
    if !matches!(plan.mode, SearchMode::Literal | SearchMode::Lexical) {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "semantic and hybrid retrieval require the embedding application",
        ));
    }
    let plan = validate_plan(query, plan)?;
    let base_fingerprint = cursor::fingerprint(query, &plan)?;
    let base_fingerprint = if reader.query_scope() == "strict_catalog" {
        base_fingerprint
    } else {
        Blake3Hash::digest(
            serde_json::to_vec(&(reader.query_scope(), base_fingerprint))
                .map_err(|error| WikiError::new(ErrorCode::Internal, error.to_string()))?,
        )
    };
    let fingerprint = if let Some(historical) = context_scope {
        Blake3Hash::digest(
            serde_json::to_vec(&("lwiki-context-documents-v1", base_fingerprint, historical))
                .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
        )
    } else {
        base_fingerprint
    };
    let context_policy = context_scope
        .map(|historical| filters::catalog_context_policy(historical, reader.normalized_layout()))
        .unwrap_or("1".into());
    let offset = cursor::offset(
        reader,
        &fingerprint,
        plan.cursor.as_deref(),
        plan.limits.candidates,
    )?;
    if source_only {
        validate_source_indexes(reader.connection(), reader.normalized_layout())?;
    } else if reader.normalized_layout() {
        validate_general_indexes(reader.connection())?;
    }
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
    if source_only {
        (candidates, overflow) = source_candidates(
            reader,
            query,
            expression.as_deref().expect("indexed lexical expression"),
            &plan,
        )?;
    } else if reader.normalized_layout() && plan.mode == SearchMode::Literal {
        (candidates, literal_count) =
            normalized_literal_candidates(reader, query, &plan, &context_policy)
                .map_err(literal_budget_error)?;
    } else if reader.normalized_layout() {
        (candidates, overflow) = normalized_candidates(
            reader,
            query,
            expression
                .as_deref()
                .expect("normalized lexical expression"),
            &plan,
            &context_policy,
        )?;
    } else if let Some(expression) = &expression {
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
            let mut rows = statement
                .query(params_from_iter(values))
                .map_err(sql_error)?;
            let mut index = 0;
            while let Some(row) = rows.next().map_err(sql_error)? {
                if index == plan.limits.candidates {
                    overflow = true;
                    break;
                }
                let document = reader.decode_document(row, 0)?;
                let score: Option<f64> = row.get(1).map_err(sql_error)?;
                if score.is_some_and(|score| !score.is_finite()) {
                    return Err(WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "non-finite lexical rank",
                    ));
                }
                let identity = identity
                    || (!plan.filters.include_historical
                        && record_for(reader, &document)?.is_some_and(|record| {
                            record.identity_eligibility == Some(Eligibility::Current)
                                && record.description_eligibility != Some(Eligibility::Current)
                        }));
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
                index += 1;
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
        let mut rows = statement
            .query(params_from_iter(values))
            .map_err(sql_error)?;
        while let Some(row) = rows.next().map_err(sql_error)? {
            let document = reader.decode_document(row, 0)?;
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
        let record = record_for(reader, document)?;
        let record_ref = if let (Some(id), Some(kind)) = (&document.record_id, document.kind) {
            Some(RecordRef {
                vault_id: reader.vault_id().clone(),
                record_id: id.clone(),
                expected_kind: kind,
            })
        } else {
            document
                .owner_revision
                .as_ref()
                .map(|id| reader.record(id))
                .transpose()?
                .flatten()
                .filter(|row| row.record.kind() == RecordKind::Revision)
                .map(|row| RecordRef {
                    vault_id: reader.vault_id().clone(),
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
            authored_status: record.as_ref().and_then(|row| row.authored_status.clone()),
            eligibility: document.eligibility,
            identity_eligibility: record.as_ref().and_then(|row| row.identity_eligibility),
            excerpt,
            secondary_excerpts: Vec::new(),
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
    let invalid_hits = hits
        .iter()
        .filter(|hit| {
            hit.eligibility == Eligibility::Invalid
                && !hit.reasons.contains(&RetrievalReason::Identity)
        })
        .collect::<Vec<_>>();
    if !invalid_hits.is_empty() {
        let paths = invalid_hits
            .iter()
            .map(|hit| hit.locator.path.clone())
            .collect::<BTreeSet<_>>();
        let mut codes: BTreeMap<VaultRelativePath, BTreeSet<ErrorCode>> = BTreeMap::new();
        for diagnostic in reader.diagnostics(&paths)? {
            codes
                .entry(diagnostic.path)
                .or_default()
                .insert(diagnostic.code);
        }
        for hit in invalid_hits.iter().take(3) {
            let reason = codes
                .get(&hit.locator.path)
                .and_then(|codes| codes.iter().next())
                .copied()
                .unwrap_or(ErrorCode::RecordInvalid);
            warnings.push(format!(
                "invalid_note_hit: {} ({reason}); text discovery only, not verified evidence; run check for details",
                hit.locator.path
            ));
        }
        if invalid_hits.len() > 3 {
            warnings.push(format!(
                "{} additional invalid note hits; run check for details",
                invalid_hits.len() - 3
            ));
        }
    }
    let dependency_fingerprint = reader.dependency_fingerprint()?;
    if plan.mode == SearchMode::Literal {
        reader.check_query_budget().map_err(literal_budget_error)?;
    }
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
const SOURCE_PREDICATE: &str =
    "owner_revision IS NOT NULL AND source_id IS NOT NULL AND eligibility='current'";
const SOURCE_INDEXES: [(&str, &str); 3] = [
    ("source_document_ids", "source_id"),
    ("source_revision_ids", "owner_revision"),
    ("source_document_titles", "title"),
];

const GENERAL_TITLE_INDEX: &str = "CREATE INDEX document_titles ON documents(title,record_id,path)";
const GENERAL_CANDIDATE_INDEX: &str = "CREATE INDEX document_candidate_metadata ON documents(doc_row,path,record_id,kind,eligibility,source_id,owner_revision)";

fn validate_general_indexes(connection: &Connection) -> Result<()> {
    // Layout 0/1 builders did not publish the complete registry/fact membership
    // used by this query. An index alone cannot make their missing alias rows
    // authoritative; refuse before silently demoting exact alias to FTS.
    let layout: i64 = connection
        .query_row(
            "SELECT proof_layout_version FROM catalog_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if layout != 2 {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "all-document normalized retrieval requires complete proof layout 2",
        ));
    }
    for (name, expected, message) in [
        (
            "document_titles",
            GENERAL_TITLE_INDEX,
            "catalog lacks the bounded all-document exact-title search index",
        ),
        (
            "document_candidate_metadata",
            GENERAL_CANDIDATE_INDEX,
            "catalog lacks the bounded all-document FTS candidate metadata index",
        ),
    ] {
        let mut statement = connection.prepare(
            "SELECT sql FROM sqlite_schema WHERE type='index' AND name=?1 AND tbl_name='documents' LIMIT 2",
        ).map_err(sql_error)?;
        let mut rows = statement.query([name]).map_err(sql_error)?;
        let valid = if let Some(row) = rows.next().map_err(sql_error)? {
            let actual = sql_text(row, 0)?;
            actual.len() <= 4096 && normalize_index_sql(actual) == normalize_index_sql(expected)
        } else {
            false
        };
        if !valid || rows.next().map_err(sql_error)?.is_some() {
            return Err(WikiError::new(ErrorCode::CapabilityUnavailable, message));
        }
    }
    Ok(())
}

/// Only scalar keys and scores enter the ranking sorter. Exact identity, title
/// and alias legs enter through their own indexed lookups; lexical cost depends
/// on matching postings. Metadata policies and filters precede the cap.
fn normalized_candidate_query(
    query: &str,
    expression: &str,
    plan: &QueryPlan,
    context_policy: &str,
    leg: usize,
) -> (String, Vec<Value>) {
    let fts = leg >= 3;
    let identity = leg == 4;
    let mut values = vec![Value::Text(if identity {
        format!("{{title aliases}} : ({expression})")
    } else if fts {
        expression.into()
    } else {
        query.into()
    })];
    let common = filters::catalog_sql(&plan.filters, &mut values, true);
    let identity_policy = filters::catalog_identity_policy(true);
    let policy = if identity {
        identity_policy.to_owned()
    } else if !fts {
        format!(
            "({}) OR ({identity_policy})",
            filters::catalog_normal_policy(&plan.filters, true)
        )
    } else {
        filters::catalog_normal_policy(&plan.filters, true)
    };
    let limit = filters::bind(
        &mut values,
        Value::Integer((plan.limits.candidates + 1) as i64),
    );
    let (access, condition) = match leg {
        0 => (
            "documents d INDEXED BY document_record_ids",
            "d.record_id=?1",
        ),
        1 => ("documents d INDEXED BY document_titles", "d.title=?1"),
        2 => (
            "registry_match_keys k INDEXED BY sqlite_autoindex_registry_match_keys_1 CROSS JOIN documents d INDEXED BY sqlite_autoindex_documents_1",
            "k.kind='alias' AND k.value=?1 AND d.path=k.path AND d.record_id=k.record_id",
        ),
        _ => (
            "documents_fts JOIN documents d INDEXED BY document_candidate_metadata ON d.doc_row=documents_fts.rowid",
            "documents_fts MATCH ?1",
        ),
    };
    let score = if identity {
        "bm25(documents_fts,8,6,0,0,0)"
    } else if fts {
        "bm25(documents_fts,8,6,3,2,1)"
    } else {
        "NULL"
    };
    let columns = crate::catalog::normalized_schema::DOCUMENT_COLUMNS
        .split(',')
        .map(|column| format!("d.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let fts_columns = if fts {
        ",f.title,f.aliases,f.headings,f.tags,f.body"
    } else {
        ""
    };
    let fts_lookup = if fts {
        "JOIN documents_fts f ON f.rowid=c.doc_row"
    } else {
        ""
    };
    // Alias registry membership guarantees a nonnull equal record ID and path.
    // Its primary key already provides the exact tie order; retaining the
    // coalesce expression in this ORDER BY would sort the complete alias bucket.
    let order = if leg == 2 {
        "k.record_id,k.path"
    } else {
        "score,tie,d.path"
    };
    let sql = format!(
        "WITH candidate_ids AS MATERIALIZED (\
         SELECT d.doc_row,{score} AS score,coalesce(d.record_id,d.path) AS tie,d.path \
         FROM {access} LEFT JOIN records r ON r.id=d.record_id \
         WHERE ({condition}) AND ({common}) AND ({policy}) AND ({context_policy}) \
         ORDER BY {order} LIMIT {limit}) \
         SELECT {columns},c.score,d.aliases_text,d.tags_text{fts_columns} \
         FROM candidate_ids c JOIN documents d ON d.doc_row=c.doc_row {fts_lookup} \
         ORDER BY c.score,c.tie,c.path"
    );
    (sql, values)
}

fn validate_general_document(
    reader: &dyn QueryCatalog,
    row: &Row<'_>,
    document: &DocumentRow,
    fts: bool,
) -> Result<()> {
    let aliases = document.aliases.join(" ");
    let tags = document.tags.join(" ");
    let hash_matches = Blake3Hash::digest(document.raw_text.as_bytes()) == document.hash;
    // Non-UTF8 canonical notes deliberately retain their original byte hash
    // with empty cached text and an exact parse diagnostic. They remain invalid
    // title/path discovery, just as in the legacy reader and cached read route.
    let unreadable_note = !hash_matches
        && document.owner_revision.is_none()
        && document.source_id.is_none()
        && document.record_id.is_none()
        && document.kind.is_none()
        && document.eligibility == Eligibility::Invalid
        && document.raw_text.is_empty()
        && document.body.is_empty()
        && document.headings.is_empty()
        && document.aliases.is_empty()
        && document.tags.is_empty()
        && reader
            .diagnostics(&BTreeSet::from([document.path.clone()]))?
            .iter()
            .any(|diagnostic| {
                diagnostic.code == ErrorCode::RecordInvalid
                    && diagnostic
                        .details
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        == Some("note is not UTF-8")
            });
    if sql_text(row, 15)? != aliases
        || sql_text(row, 16)? != tags
        || (!hash_matches && !unreadable_note)
        || (fts
            && (sql_text(row, 17)? != document.title
                || sql_text(row, 18)? != aliases
                || sql_text(row, 19)? != document.headings
                || sql_text(row, 20)? != tags
                || sql_text(row, 21)? != document.body))
    {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "selected document differs from its normalized derivatives",
        ));
    }
    Ok(())
}

fn literal_budget_error(mut error: WikiError) -> WikiError {
    if error.code == ErrorCode::BudgetExceeded && error.hint.is_none() {
        error.hint = Some(
            "Narrow literal discovery with --source-id or --path-prefix; substring scanning is bounded separately from returned hits".into(),
        );
    }
    error
}

/// Substring presence is SQL data, never an FTS expression or a character
/// offset. COUNT and capped key selection share this reader's transaction and
/// cumulative meter. LIMIT bounds decoding, not the native substring scan.
fn normalized_literal_candidates(
    reader: &dyn QueryCatalog,
    query: &str,
    plan: &QueryPlan,
    context_policy: &str,
) -> Result<(BTreeMap<VaultRelativePath, Candidate>, usize)> {
    let mut values = vec![Value::Text(query.into())];
    let common = filters::catalog_sql(&plan.filters, &mut values, true);
    let policy = filters::catalog_normal_policy(&plan.filters, true);
    let predicate =
        format!("({common}) AND ({policy}) AND ({context_policy}) AND instr(d.raw_text,?1)>0");
    let count_sql = format!(
        "SELECT count(*) FROM documents d LEFT JOIN records r ON r.id=d.record_id WHERE {predicate}"
    );
    let count: i64 = reader
        .connection()
        .query_row(&count_sql, params_from_iter(values.iter()), |row| {
            row.get(0)
        })
        .map_err(sql_error)?;
    reader.check_query_budget()?;
    let count = usize::try_from(count).map_err(|_| {
        WikiError::new(ErrorCode::IndexCorrupt, "literal match count outside range")
    })?;
    let limit = filters::bind(&mut values, Value::Integer(plan.limits.candidates as i64));
    let columns = crate::catalog::normalized_schema::DOCUMENT_COLUMNS
        .split(',')
        .map(|column| format!("d.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "WITH candidate_ids AS MATERIALIZED (\
         SELECT d.doc_row,coalesce(d.record_id,d.path) AS tie,d.path \
         FROM documents d LEFT JOIN records r ON r.id=d.record_id \
         WHERE {predicate} ORDER BY tie COLLATE BINARY,d.path COLLATE BINARY LIMIT {limit}) \
         SELECT {columns},NULL,d.aliases_text,d.tags_text \
         FROM candidate_ids c JOIN documents d ON d.doc_row=c.doc_row \
         ORDER BY c.tie COLLATE BINARY,c.path COLLATE BINARY"
    );
    let mut statement = reader.connection().prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params_from_iter(values))
        .map_err(sql_error)?;
    let mut candidates = BTreeMap::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let document = reader.decode_document(row, 0)?;
        validate_general_document(reader, row, &document, false)?;
        // The decoder checks typed columns; these checks additionally preserve
        // adopted-record integrity without granting an identity-ranking tier.
        let record = record_for(reader, &document)?;
        if document.record_id.is_some() && record.is_none() {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "selected literal document lacks its adopted record",
            ));
        }
        if let Some(record) = record {
            if record.path != document.path
                || record.hash != document.hash
                || Some(record.record.kind()) != document.kind
                || record.record.title() != document.title
                || crate::catalog::scan::list(&record.record, "aliases") != document.aliases
                || crate::catalog::scan::list(&record.record, "tags") != document.tags
                || record.eligibility != document.eligibility
            {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "selected literal document differs from its adopted record",
                ));
            }
        }
        if literal_matches(&document.raw_text, query).is_empty() {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "selected literal document does not contain the exact query",
            ));
        }
        let rank = candidates.len() + 1;
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
    reader.check_query_budget()?;
    if candidates.len() != count.min(plan.limits.candidates) {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "literal candidate count differs within its pinned snapshot",
        ));
    }
    Ok((candidates, count))
}

fn normalized_candidates(
    reader: &dyn QueryCatalog,
    query: &str,
    expression: &str,
    plan: &QueryPlan,
    context_policy: &str,
) -> Result<(BTreeMap<VaultRelativePath, Candidate>, bool)> {
    let mut candidates: BTreeMap<VaultRelativePath, Candidate> = BTreeMap::new();
    let mut overflow = false;
    for (leg, (tier, reason, channel)) in [
        (0, RetrievalReason::ExactId, "exact_id"),
        (1, RetrievalReason::ExactTitle, "exact_title"),
        (1, RetrievalReason::ExactAlias, "exact_alias"),
        (2, RetrievalReason::Lexical, "lexical"),
        (2, RetrievalReason::Lexical, "identity_lexical"),
    ]
    .into_iter()
    .enumerate()
    {
        let (sql, values) =
            normalized_candidate_query(query, expression, plan, context_policy, leg);
        let mut statement = reader.connection().prepare(&sql).map_err(sql_error)?;
        let mut rows = statement
            .query(params_from_iter(values))
            .map_err(sql_error)?;
        let mut rank = 0;
        while let Some(row) = rows.next().map_err(sql_error)? {
            if rank == plan.limits.candidates {
                overflow = true;
                break;
            }
            let document = reader.decode_document(row, 0)?;
            validate_general_document(reader, row, &document, leg >= 3)?;
            if leg == 2 && !document.aliases.iter().any(|alias| alias == query) {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "selected alias key differs from its document",
                ));
            }
            let score: Option<f64> = row.get(14).map_err(sql_error)?;
            if score.is_some_and(|score| !score.is_finite()) {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "non-finite lexical rank",
                ));
            }
            let record = record_for(reader, &document)?;
            if document.record_id.is_some() && record.is_none() {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "selected document lacks its adopted record",
                ));
            }
            if let Some(record) = &record {
                if record.path != document.path
                    || record.hash != document.hash
                    || Some(record.record.kind()) != document.kind
                    || record.record.title() != document.title
                    || crate::catalog::scan::list(&record.record, "aliases") != document.aliases
                    || crate::catalog::scan::list(&record.record, "tags") != document.tags
                    || record.eligibility != document.eligibility
                {
                    return Err(WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "selected document differs from its adopted record",
                    ));
                }
            }
            let identity = leg == 4
                || (!plan.filters.include_historical
                    && record.is_some_and(|record| {
                        record.identity_eligibility == Some(Eligibility::Current)
                            && record.description_eligibility != Some(Eligibility::Current)
                    }));
            rank += 1;
            let contribution = RankContribution {
                channel: channel.into(),
                rank,
                score,
            };
            let candidate = Candidate {
                document,
                tier,
                score,
                reasons: vec![reason],
                ranks: vec![contribution.clone()],
                identity,
            };
            if let Some(existing) = candidates.get_mut(&candidate.document.path) {
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
                candidates.insert(candidate.document.path.clone(), candidate);
            }
        }
    }
    Ok((candidates, overflow))
}

fn validate_source_filters(filters: &SearchFilters) -> Result<()> {
    if !filters.kinds.is_empty()
        || !filters.tags.is_empty()
        || !filters.authored_statuses.is_empty()
        || filters.include_proposed
        || filters.include_historical
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "indexed source evidence supports only source ID and path prefix filters",
        ));
    }
    Ok(())
}

fn missing_source_index() -> WikiError {
    let mut error = WikiError::new(
        ErrorCode::CapabilityUnavailable,
        "catalog lacks the bounded current-source search indexes",
    );
    error.hint = Some("run index rebuild".into());
    error
}

/// Validate the complete access-index definition. A matching name or column
/// prefix alone does not exclude an incompatible partial predicate/collation.
fn validate_source_indexes(connection: &Connection, normalized: bool) -> Result<()> {
    for (name, key) in SOURCE_INDEXES {
        let generation = if normalized { "" } else { "gen," };
        let expected = format!(
            "CREATE INDEX {name} ON documents({generation}{key},path) WHERE {SOURCE_PREDICATE}"
        );
        let mut statement = connection
            .prepare("SELECT sql FROM sqlite_schema WHERE type='index' AND name=?1 AND tbl_name='documents' LIMIT 2")
            .map_err(sql_error)?;
        let mut rows = statement.query([name]).map_err(sql_error)?;
        let Some(row) = rows.next().map_err(sql_error)? else {
            return Err(missing_source_index());
        };
        let actual = sql_text(row, 0)?;
        // SQLite preserves DDL text. Ignore whitespace/case outside quotes;
        // quoted identifiers and string values must retain their meaning.
        if actual.len() > 4096 || normalize_index_sql(actual) != normalize_index_sql(&expected) {
            return Err(missing_source_index());
        }
        if rows.next().map_err(sql_error)?.is_some() {
            return Err(missing_source_index());
        }
    }
    Ok(())
}

fn normalize_index_sql(sql: &str) -> String {
    let mut quoted = false;
    sql.trim_end_matches(';')
        .chars()
        .filter_map(|character| {
            if character == '\'' {
                quoted = !quoted;
                Some(character)
            } else if quoted {
                Some(character)
            } else if character.is_whitespace() {
                None
            } else {
                Some(character.to_ascii_lowercase())
            }
        })
        .collect()
}

fn source_filters_sql(filters: &SearchFilters, values: &mut Vec<Value>) -> String {
    let mut clauses = Vec::new();
    if !filters.source_ids.is_empty() {
        let ids = filters
            .source_ids
            .iter()
            .map(|id| filters::bind(values, Value::Text(id.as_str().into())))
            .collect::<Vec<_>>();
        clauses.push(format!("d.source_id IN ({})", ids.join(",")));
    }
    if let Some(prefix) = &filters.path_prefix {
        // Keep the established byte-prefix semantics. These predicates inspect
        // SQL columns only, before the candidate limit and JSON decoding.
        let bound = filters::bind(values, Value::Text(prefix.clone()));
        clauses.push(format!("substr(d.path,1,length({bound}))={bound}"));
    }
    if clauses.is_empty() {
        "1".into()
    } else {
        clauses.join(" AND ")
    }
}

fn source_candidate_query(
    normalized: bool,
    generation: i64,
    query: &str,
    expression: &str,
    plan: &QueryPlan,
    key: &str,
    index: Option<&str>,
) -> (String, Vec<Value>) {
    if normalized {
        return normalized_source_candidate_query(query, expression, plan, key, index);
    }
    let fts = index.is_none();
    let mut values = vec![
        Value::Text(if fts { expression.into() } else { query.into() }),
        Value::Integer(generation),
    ];
    let common = source_filters_sql(&plan.filters, &mut values);
    let bound = filters::bind(
        &mut values,
        Value::Integer((plan.limits.candidates + 1) as i64),
    );
    let access = index.map_or(String::new(), |index| format!("INDEXED BY {index}"));
    let condition = if fts {
        "documents_fts MATCH ?1".into()
    } else {
        format!("d.{key}=?1")
    };
    let join = if fts {
        "JOIN documents_fts ON documents_fts.doc_row=d.doc_row AND documents_fts.gen=d.gen"
    } else {
        ""
    };
    let score = if fts {
        "bm25(documents_fts,8,6,3,2,1,0,0)"
    } else {
        "NULL"
    };
    let fts_id = if fts { "documents_fts.rowid" } else { "NULL" };
    let fts_columns = if fts {
        ",f.title,f.aliases,f.headings,f.tags,f.body"
    } else {
        ""
    };
    let fts_lookup = if fts {
        "JOIN documents_fts f ON f.rowid=c.fts_rowid"
    } else {
        ""
    };
    let sql = format!(
        "WITH candidate_ids AS MATERIALIZED (\
         SELECT d.doc_row,{score} AS score,{fts_id} AS fts_rowid,d.path \
         FROM documents d {access} {join} \
         WHERE d.gen=?2 AND d.owner_revision IS NOT NULL AND d.source_id IS NOT NULL \
         AND d.eligibility='current' AND d.record_id IS NULL AND d.kind IS NULL \
         AND ({condition}) AND ({common}) ORDER BY score,d.path LIMIT {bound}) \
         SELECT d.row_json,c.score,d.path,d.record_id,d.kind,d.file_hash,d.title,d.body,\
         d.raw_text,d.source_id,d.owner_revision,d.eligibility{fts_columns} \
         FROM candidate_ids c JOIN documents d ON d.doc_row=c.doc_row {fts_lookup} \
         ORDER BY c.score,c.path"
    );
    (sql, values)
}

/// Normalized FTS external content uses doc_row as its rowid. The materialized
/// sorter retains keys/ranks only; metadata and source scope precede its LIMIT.
fn normalized_source_candidate_query(
    query: &str,
    expression: &str,
    plan: &QueryPlan,
    key: &str,
    index: Option<&str>,
) -> (String, Vec<Value>) {
    let fts = index.is_none();
    let mut values = vec![Value::Text(if fts {
        expression.into()
    } else {
        query.into()
    })];
    let common = source_filters_sql(&plan.filters, &mut values);
    let bound = filters::bind(
        &mut values,
        Value::Integer((plan.limits.candidates + 1) as i64),
    );
    let access = index.map_or(String::new(), |index| format!("INDEXED BY {index}"));
    let condition = if fts {
        "documents_fts MATCH ?1".into()
    } else {
        format!("d.{key}=?1")
    };
    let join = if fts {
        "JOIN documents_fts ON documents_fts.rowid=d.doc_row"
    } else {
        ""
    };
    let score = if fts {
        "bm25(documents_fts,8,6,3,2,1)"
    } else {
        "NULL"
    };
    let fts_columns = if fts {
        ",f.title,f.aliases,f.headings,f.tags,f.body"
    } else {
        ""
    };
    let fts_lookup = if fts {
        "JOIN documents_fts f ON f.rowid=c.doc_row"
    } else {
        ""
    };
    let columns = crate::catalog::normalized_schema::DOCUMENT_COLUMNS
        .split(',')
        .map(|column| format!("d.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "WITH candidate_ids AS MATERIALIZED (\
         SELECT d.doc_row,{score} AS score,d.path FROM documents d {access} {join} \
         WHERE d.owner_revision IS NOT NULL AND d.source_id IS NOT NULL \
         AND d.eligibility='current' AND d.record_id IS NULL AND d.kind IS NULL \
         AND ({condition}) AND ({common}) ORDER BY score,d.path LIMIT {bound}) \
         SELECT {columns},c.score,d.aliases_text,d.tags_text{fts_columns} \
         FROM candidate_ids c JOIN documents d ON d.doc_row=c.doc_row {fts_lookup} \
         ORDER BY c.score,c.path"
    );
    (sql, values)
}

/// Candidate keys/ranks are capped before fetching document JSON. In
/// particular, an FTS sorter never carries every matching document's raw text.
fn source_candidates(
    reader: &dyn QueryCatalog,
    query: &str,
    expression: &str,
    plan: &QueryPlan,
) -> Result<(BTreeMap<VaultRelativePath, Candidate>, bool)> {
    let generation = i64::try_from(reader.snapshot().generation)
        .map_err(|_| WikiError::new(ErrorCode::IndexCorrupt, "generation outside SQL range"))?;
    let mut candidates: BTreeMap<VaultRelativePath, Candidate> = BTreeMap::new();
    let mut overflow = false;
    for (tier, key, index, reason, channel) in [
        (
            0,
            "source_id",
            Some("source_document_ids"),
            RetrievalReason::ExactId,
            "exact_id",
        ),
        (
            0,
            "owner_revision",
            Some("source_revision_ids"),
            RetrievalReason::ExactId,
            "exact_id",
        ),
        (
            1,
            "title",
            Some("source_document_titles"),
            RetrievalReason::ExactTitle,
            "exact_title",
        ),
        (2, "", None, RetrievalReason::Lexical, "lexical"),
    ] {
        let fts = index.is_none();
        let (sql, values) = source_candidate_query(
            reader.normalized_layout(),
            generation,
            query,
            expression,
            plan,
            key,
            index,
        );
        let mut statement = reader.connection().prepare(&sql).map_err(sql_error)?;
        let mut rows = statement
            .query(params_from_iter(values))
            .map_err(sql_error)?;
        let mut rank = 0;
        while let Some(row) = rows.next().map_err(sql_error)? {
            if rank == plan.limits.candidates {
                overflow = true;
                break;
            }
            let document = reader.decode_document(row, 0)?;
            validate_source_document(row, &document, fts, reader.normalized_layout())?;
            let score: Option<f64> = row
                .get(if reader.normalized_layout() { 14 } else { 1 })
                .map_err(sql_error)?;
            if score.is_some_and(|score| !score.is_finite()) {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "non-finite lexical rank",
                ));
            }
            rank += 1;
            let contribution = RankContribution {
                channel: channel.into(),
                rank,
                score,
            };
            let candidate = Candidate {
                document,
                tier,
                score,
                reasons: vec![reason],
                ranks: vec![contribution.clone()],
                identity: false,
            };
            if let Some(existing) = candidates.get_mut(&candidate.document.path) {
                if !existing.reasons.contains(&reason) {
                    existing.reasons.push(reason);
                }
                existing.ranks.push(contribution);
                if compare(&candidate, existing) == Ordering::Less {
                    existing.tier = tier;
                    existing.score = score;
                }
            } else {
                candidates.insert(candidate.document.path.clone(), candidate);
            }
        }
    }
    Ok((candidates, overflow))
}

fn sql_text<'a>(row: &'a Row<'_>, column: usize) -> Result<&'a str> {
    row.get_ref(column)
        .map_err(sql_error)?
        .as_str()
        .map_err(|error| WikiError::new(ErrorCode::IndexCorrupt, error.to_string()))
}

fn validate_source_document(
    row: &Row<'_>,
    document: &DocumentRow,
    fts: bool,
    normalized: bool,
) -> Result<()> {
    if normalized {
        let consistent = document.record_id.is_none()
            && document.kind.is_none()
            && document.source_id.is_some()
            && document.owner_revision.is_some()
            && document.eligibility == Eligibility::Current
            && document.aliases.is_empty()
            && document.tags.is_empty()
            && sql_text(row, 15)?.is_empty()
            && sql_text(row, 16)?.is_empty()
            && Blake3Hash::digest(document.raw_text.as_bytes()) == document.hash;
        let fts_consistent = !fts
            || (sql_text(row, 17)? == document.title
                && sql_text(row, 18)?.is_empty()
                && sql_text(row, 19)? == document.headings
                && sql_text(row, 20)?.is_empty()
                && sql_text(row, 21)? == document.body);
        if !consistent || !fts_consistent {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "selected source document differs from its normalized columns",
            ));
        }
        return Ok(());
    }
    let consistent = sql_text(row, 2)? == document.path.as_str()
        && matches!(row.get_ref(3).map_err(sql_error)?, ValueRef::Null)
        && matches!(row.get_ref(4).map_err(sql_error)?, ValueRef::Null)
        && document.record_id.is_none()
        && document.kind.is_none()
        && sql_text(row, 5)? == document.hash.as_str()
        && sql_text(row, 6)? == document.title
        && sql_text(row, 7)? == document.body
        && sql_text(row, 8)? == document.raw_text
        && Some(sql_text(row, 9)?) == document.source_id.as_ref().map(RecordId::as_str)
        && Some(sql_text(row, 10)?) == document.owner_revision.as_ref().map(RecordId::as_str)
        && sql_text(row, 11)? == "current"
        && document.eligibility == Eligibility::Current
        && document.aliases.is_empty()
        && document.tags.is_empty()
        && Blake3Hash::digest(document.raw_text.as_bytes()) == document.hash;
    let fts_consistent = !fts
        || (sql_text(row, 12)? == document.title
            && sql_text(row, 13)?.is_empty()
            && sql_text(row, 14)? == document.headings
            && sql_text(row, 15)?.is_empty()
            && sql_text(row, 16)? == document.body);
    if !consistent || !fts_consistent {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "selected source document differs from its indexed columns",
        ));
    }
    Ok(())
}

fn sql_error(error: rusqlite::Error) -> WikiError {
    crate::catalog::sql::sql_error(error)
}

fn record_for(
    reader: &dyn QueryCatalog,
    document: &DocumentRow,
) -> Result<Option<crate::catalog::RecordRow>> {
    document
        .record_id
        .as_ref()
        .map(|id| reader.record(id))
        .transpose()
        .map(Option::flatten)
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
    query_matches(
        tokenizer,
        &document.raw_text,
        offset..document.raw_text.len(),
        query,
        excerpt_bytes,
    )
}

fn query_matches(
    tokenizer: &Tokenizer<'_>,
    raw: &str,
    bounds: Range<usize>,
    query: &str,
    excerpt_bytes: usize,
) -> Result<Vec<Range<usize>>> {
    let map = SourceMap::markdown(&raw[..bounds.end], bounds.start);
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
                    && raw.get(original.clone()) == map.text.get(normalized)
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
    // Score all windows, rather than allowing the first frequent query term
    // to consume the match cap. Distinct normalized query tokens vote once
    // per window, weighted by inverse frequency in this document. This is
    // language independent and keeps repeated common words from overwhelming
    // late identifiers. Document ranking remains the FTS ranking above.
    let mut terms = BTreeMap::new();
    for token in &phrase {
        let next = terms.len();
        terms.entry(token.text.as_str()).or_insert(next);
    }
    let mut frequencies = vec![0usize; terms.len()];
    let mut occurrences = Vec::new();
    for token in &tokens {
        let Some(&term) = terms.get(token.text.as_str()) else {
            continue;
        };
        frequencies[term] += 1;
        if let Some(original) = map.original_span(token.span.clone())
            && raw.get(original.clone()) == map.text.get(token.span.clone())
        {
            occurrences.push((original, term));
        }
    }
    if occurrences.is_empty() {
        return Ok(vec![]);
    }
    // Fixed-point weights make equal-scoring windows choose the earliest
    // original span without floating-point accumulation/tie drift.
    let weights = frequencies
        .iter()
        .map(|&frequency| {
            if frequency == 0 {
                0
            } else {
                ((1.0 + (tokens.len() as f64 / frequency as f64).ln()) * 1_000_000.0).round() as u64
            }
        })
        .collect::<Vec<_>>();
    let mut counts = vec![0usize; terms.len()];
    let (mut left, mut right, mut score, mut best_score, mut best) = (0, 0, 0u64, 0u64, 0);
    for (index, (anchor, _)) in occurrences.iter().enumerate() {
        let window = excerpt_window(raw, bounds.clone(), anchor.start, excerpt_bytes);
        while right < occurrences.len() && occurrences[right].0.end <= window.end {
            let term = occurrences[right].1;
            if counts[term] == 0 {
                score += weights[term];
            }
            counts[term] += 1;
            right += 1;
        }
        while left < right && occurrences[left].0.start < window.start {
            let term = occurrences[left].1;
            counts[term] -= 1;
            if counts[term] == 0 {
                score -= weights[term];
            }
            left += 1;
        }
        if score > best_score {
            best_score = score;
            best = index;
        }
    }
    let anchor = occurrences[best].0.clone();
    let window = excerpt_window(raw, bounds, anchor.start, excerpt_bytes);
    let mut matches = vec![anchor.clone()];
    for (span, _) in occurrences {
        if span != anchor && span.start >= window.start && span.end <= window.end {
            matches.push(span);
            if matches.len() == 64 {
                break;
            }
        }
    }
    Ok(matches)
}

fn excerpt_window(raw: &str, bounds: Range<usize>, anchor: usize, bytes: usize) -> Range<usize> {
    let mut start = anchor.saturating_sub(bytes / 3).max(bounds.start);
    while start < bounds.end && !raw.is_char_boundary(start) {
        start += 1;
    }
    let mut end = start.saturating_add(bytes).min(bounds.end);
    while end > start && !raw.is_char_boundary(end) {
        end -= 1;
    }
    start..end
}

/// Focus within a winning embedding unit without widening its citation scope.
/// Token matching guides the window, but all returned text/spans/hash still
/// come from the exact original UTF-8 source bytes.
pub(crate) fn focused_excerpt(
    reader: &dyn QueryCatalog,
    document: &DocumentRow,
    query: &str,
    span: ByteSpan,
    bytes: usize,
) -> Result<SearchExcerpt> {
    validate_query(query)?;
    span.slice(&document.raw_text)?;
    let bounds = span.start() as usize..span.end() as usize;
    let tokenizer = Tokenizer::new(reader.connection())?;
    let matches = query_matches(&tokenizer, &document.raw_text, bounds.clone(), query, bytes)?;
    let anchor = matches
        .first()
        .map_or(bounds.start, |matched| matched.start);
    let window = excerpt_window(&document.raw_text, bounds, anchor, bytes);
    build_excerpt(reader, document, window, &matches)
}
pub(crate) fn excerpt(
    reader: &dyn QueryCatalog,
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
    let minimum = if mode == SearchMode::Lexical || matches.is_empty() {
        body
    } else {
        0
    };
    let window = excerpt_window(raw, minimum..raw.len(), anchor, bytes);
    build_excerpt(reader, document, window, matches)
}

fn build_excerpt(
    reader: &dyn QueryCatalog,
    document: &DocumentRow,
    window: Range<usize>,
    matches: &[Range<usize>],
) -> Result<SearchExcerpt> {
    let raw = &document.raw_text;
    let start = window.start;
    let end = window.end;
    let span = ByteSpan::new(start as u64, end as u64)?;
    let mut matched_spans = matches
        .iter()
        .filter(|matched| matched.start >= start && matched.end <= end)
        .map(|matched| ByteSpan::new(matched.start as u64, matched.end as u64))
        .collect::<Result<Vec<_>>>()?;
    matched_spans.sort_by_key(|span| (span.start(), span.end()));
    matched_spans.dedup();
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

#[cfg(test)]
mod indexed_source_tests {
    use super::*;
    use crate::catalog::{CatalogDiagnostic, RecordRow};
    use rusqlite::params;
    use std::cell::Cell;

    struct Reader {
        connection: Connection,
        snapshot: ReadSnapshot,
        vault: RecordId,
        decoded: Cell<usize>,
    }
    impl QueryCatalog for Reader {
        fn connection(&self) -> &Connection {
            &self.connection
        }
        fn snapshot(&self) -> &ReadSnapshot {
            &self.snapshot
        }
        fn vault_id(&self) -> &RecordId {
            &self.vault
        }
        fn verification(&self) -> &SnapshotVerification {
            &SnapshotVerification::IndexSnapshot
        }
        fn record(&self, _id: &RecordId) -> Result<Option<RecordRow>> {
            Ok(None)
        }
        fn document(&self, _path: &VaultRelativePath) -> Result<Option<DocumentRow>> {
            Ok(None)
        }
        fn diagnostics(
            &self,
            _paths: &BTreeSet<VaultRelativePath>,
        ) -> Result<Vec<CatalogDiagnostic>> {
            Ok(vec![])
        }
        fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
            Ok(Blake3Hash::digest(b"indexed test scope"))
        }
        fn query_scope(&self) -> &'static str {
            "indexed_evidence"
        }
        fn decode_document(&self, row: &Row<'_>, column: usize) -> Result<DocumentRow> {
            self.decoded.set(self.decoded.get() + 1);
            serde_json::from_str(sql_text(row, column)?)
                .map_err(|error| WikiError::new(ErrorCode::IndexCorrupt, error.to_string()))
        }
    }
    fn reader() -> Reader {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(
            "CREATE TABLE documents(doc_row INTEGER PRIMARY KEY,gen INTEGER,path TEXT,record_id TEXT,kind TEXT,file_hash TEXT,title TEXT,body TEXT,raw_text TEXT,source_id TEXT,owner_revision TEXT,eligibility TEXT,row_json TEXT); \
             CREATE VIRTUAL TABLE documents_fts USING fts5(title,aliases,headings,tags,body,gen UNINDEXED,doc_row UNINDEXED,tokenize='unicode61 remove_diacritics 2');"
        ).unwrap();
        for (name, key) in SOURCE_INDEXES {
            connection
                .execute_batch(&format!(
                    "CREATE INDEX {name} ON documents(gen,{key},path) WHERE {SOURCE_PREDICATE}"
                ))
                .unwrap();
        }
        Reader {
            connection,
            snapshot: ReadSnapshot::canonical(
                1,
                Blake3Hash::digest(b"parser"),
                Blake3Hash::digest(b"manifest"),
            ),
            vault: RecordId::new("vault_source_sql").unwrap(),
            decoded: Cell::new(0),
        }
    }
    fn source(reader: &Reader, name: &str, title: &str) -> DocumentRow {
        let raw_text = "# Evidence\n\nA captured needle café fact.\n".to_owned();
        let document = DocumentRow {
            path: VaultRelativePath::new(format!("sources/{name}/content.md")).unwrap(),
            hash: Blake3Hash::digest(raw_text.as_bytes()),
            record_id: None,
            kind: None,
            title: title.into(),
            aliases: vec![],
            headings: "Evidence".into(),
            tags: vec![],
            body: "A captured needle café fact.".into(),
            raw_text,
            source_id: Some(RecordId::new(format!("source_{name}")).unwrap()),
            owner_revision: Some(RecordId::new(format!("revision_{name}")).unwrap()),
            eligibility: Eligibility::Current,
            reasons: vec![],
        };
        reader.connection.execute(
            "INSERT INTO documents(gen,path,record_id,kind,file_hash,title,body,raw_text,source_id,owner_revision,eligibility,row_json) VALUES(1,?1,NULL,NULL,?2,?3,?4,?5,?6,?7,'current',?8)",
            params![document.path.as_str(),document.hash.as_str(),document.title,document.body,document.raw_text,document.source_id.as_ref().unwrap().as_str(),document.owner_revision.as_ref().unwrap().as_str(),serde_json::to_string(&document).unwrap()],
        ).unwrap();
        reader.connection.execute(
            "INSERT INTO documents_fts(title,aliases,headings,tags,body,gen,doc_row) VALUES(?1,'',?2,'',?3,1,?4)",
            params![document.title,document.headings,document.body,reader.connection.last_insert_rowid()],
        ).unwrap();
        document
    }

    fn publish_normalized_sources(
        catalog: &crate::catalog::Catalog,
        documents: &[DocumentRow],
    ) -> crate::catalog::normalized_build::CompletedCatalog {
        use crate::{
            catalog::{
                RetrievalSink,
                file_types::{BuildIdentity, CatalogSelection},
                normalized_build::{BuildLimits, NormalizedBuilder},
                scan, selector,
            },
            vault::WriterPermit,
        };
        let writer =
            WriterPermit::acquire(catalog.fs.root(), std::time::Duration::from_secs(1)).unwrap();
        let epoch = catalog
            .operation_state()
            .unwrap()
            .map_or(1, |authority| authority.publication().epoch + 1);
        let identity = BuildIdentity {
            selection: CatalogSelection::new(catalog.vault_id.clone(), epoch).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&catalog.fs, &writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&catalog.fs, &writer, identity, BuildLimits::default())
                .unwrap();
        let input = scan::scan_input(&catalog.fs, &catalog.vault_id).unwrap();
        let projection = scan::project_with_sink(&catalog.fs, &input, false, &mut builder).unwrap();
        for document in documents {
            builder.document(document.clone()).unwrap();
        }
        let completed = builder.finish(&projection).unwrap();
        selector::publish(
            &catalog.fs,
            &writer,
            &completed.identity.selection,
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        completed
    }

    fn normalized_fixture(
        documents: &[DocumentRow],
    ) -> (
        tempfile::TempDir,
        crate::catalog::Catalog,
        crate::catalog::normalized_build::CompletedCatalog,
    ) {
        use crate::vault::{VaultFs, VaultRoot};
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: vault_source_sql\nwiki_kind: vault\ntitle: Source fixture\n---\nFixture\n").unwrap();
        let catalog = crate::catalog::Catalog::new(
            VaultFs::new(VaultRoot::explicit(temp.path()).unwrap()),
            RecordId::new("vault_source_sql").unwrap(),
        );
        let completed = publish_normalized_sources(&catalog, documents);
        (temp, catalog, completed)
    }

    #[test]
    fn real_normalized_indexed_source_queries_match_legacy_candidates_and_spans() {
        let legacy = reader();
        let alpha = source(&legacy, "alpha", "Captured title");
        let beta = source(&legacy, "beta", "Captured title");
        let (_temp, catalog, _completed) = normalized_fixture(&[alpha.clone(), beta.clone()]);
        let normalized = catalog
            .query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
            .unwrap();
        assert!(normalized.normalized_layout());
        for query in [
            "source_alpha",
            "revision_beta",
            "Captured title",
            "needle",
            "cafe",
        ] {
            for filters in [
                SearchFilters::default(),
                SearchFilters {
                    source_ids: vec![beta.source_id.clone().unwrap()],
                    path_prefix: Some("sources/be".into()),
                    ..Default::default()
                },
            ] {
                let plan = QueryPlan {
                    filters,
                    limits: SearchLimits {
                        candidates: 1,
                        hits: 1,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let old = search_indexed_sources(&legacy, query, &plan).unwrap();
                let new = search_indexed_sources(&normalized, query, &plan).unwrap();
                assert_eq!(new.candidate_count, old.candidate_count, "{query}");
                assert_eq!(new.truncated, old.truncated, "{query}");
                assert_eq!(new.hits.len(), old.hits.len(), "{query}");
                for (new, old) in new.hits.iter().zip(&old.hits) {
                    assert_eq!(new.locator, old.locator, "{query}");
                    assert_eq!(new.reasons, old.reasons, "{query}");
                    assert_eq!(new.excerpt, old.excerpt, "{query}");
                    assert_eq!(
                        new.rank_contributions
                            .iter()
                            .map(|rank| (&rank.channel, rank.rank))
                            .collect::<Vec<_>>(),
                        old.rank_contributions
                            .iter()
                            .map(|rank| (&rank.channel, rank.rank))
                            .collect::<Vec<_>>(),
                        "{query}"
                    );
                }
            }
        }
    }

    #[test]
    fn normalized_scope_filters_avoid_unselected_corruption_and_charge_selected_rows() {
        let legacy = reader();
        let alpha = source(&legacy, "alpha", "Captured title");
        let beta = source(&legacy, "beta", "Captured title");
        let (_temp, catalog, completed) = normalized_fixture(&[alpha, beta.clone()]);
        let database = Connection::open(&completed.path).unwrap();
        database
            .execute_batch("UPDATE documents SET aliases_json='{' WHERE source_id='source_alpha'")
            .unwrap();
        let normalized = catalog
            .query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
            .unwrap();
        let plan = QueryPlan {
            filters: SearchFilters {
                source_ids: vec![beta.source_id.unwrap()],
                path_prefix: Some("sources/be".into()),
                ..Default::default()
            },
            limits: SearchLimits {
                candidates: 1,
                hits: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let hits = search_indexed_sources(&normalized, "needle", &plan).unwrap();
        assert_eq!(hits.hits[0].locator.path, beta.path);
        assert_eq!(normalized.usage().rows, 1);
        assert_eq!(
            search_indexed_sources(&normalized, "source_alpha", &QueryPlan::default())
                .unwrap_err()
                .code,
            ErrorCode::IndexCorrupt
        );
        assert_eq!(normalized.usage().rows, 2);
        let limited = catalog
            .query_snapshot(crate::catalog::query_types::QueryReadLimits {
                max_row_bytes: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            search_indexed_sources(&limited, "needle", &plan)
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(limited.usage().rows, 0);
    }

    #[test]
    fn normalized_source_hash_and_empty_external_content_derivatives_fail_closed() {
        for mutation in [
            "UPDATE documents SET raw_text='tampered' WHERE source_id='source_alpha'",
            "UPDATE documents SET aliases_json='[\"alias\"]' WHERE source_id='source_alpha'",
            "UPDATE documents SET tags_json='[\"tag\"]' WHERE source_id='source_alpha'",
            "UPDATE documents SET aliases_text='alias' WHERE source_id='source_alpha'",
            "UPDATE documents SET tags_text='tag' WHERE source_id='source_alpha'",
        ] {
            let legacy = reader();
            let alpha = source(&legacy, "alpha", "Captured title");
            let (_temp, catalog, completed) = normalized_fixture(&[alpha]);
            let writer = Connection::open(completed.path).unwrap();
            crate::catalog::selector::configure_wal(&writer).unwrap();
            writer.execute_batch(mutation).unwrap();
            drop(writer);
            let normalized = catalog
                .query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
                .unwrap();
            assert_eq!(
                search_indexed_sources(&normalized, "needle", &QueryPlan::default())
                    .unwrap_err()
                    .code,
                ErrorCode::IndexCorrupt,
                "{mutation}"
            );
            assert_eq!(normalized.usage().rows, 1);
        }
    }

    #[test]
    fn normalized_candidate_plans_have_no_generation_and_fetch_payload_after_limit() {
        let legacy = reader();
        let alpha = source(&legacy, "alpha", "Captured title");
        let (_temp, catalog, _completed) = normalized_fixture(&[alpha]);
        let normalized = catalog
            .query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
            .unwrap();
        for (key, index) in [
            ("source_id", Some("source_document_ids")),
            ("owner_revision", Some("source_revision_ids")),
            ("title", Some("source_document_titles")),
            ("", None),
        ] {
            let (sql, values) = source_candidate_query(
                true,
                1,
                "needle",
                "\"needle\"",
                &QueryPlan::default(),
                key,
                index,
            );
            assert!(!sql.contains("d.gen"));
            let (keys, payload) = sql.split_once("SELECT d.path,d.file_hash").unwrap();
            assert!(keys.contains("AS MATERIALIZED"));
            assert!(keys.contains("LIMIT ?"));
            assert!(!keys.contains("raw_text"));
            assert!(!keys.contains("body"));
            assert!(payload.contains("JOIN documents d ON d.doc_row=c.doc_row"));
            let details: Vec<String> = normalized
                .connection()
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(params_from_iter(values), |row| row.get(3))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("MATERIALIZE candidate_ids")),
                "{details:?}"
            );
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("SEARCH d USING INTEGER PRIMARY KEY")),
                "{details:?}"
            );
            if let Some(index) = index {
                assert!(
                    details
                        .iter()
                        .any(|detail| detail.contains("SEARCH d USING INDEX")
                            && detail.contains(index)
                            && detail.contains(&format!("{key}=?"))),
                    "{details:?}"
                );
            } else {
                assert!(
                    details.iter().any(|detail| detail
                        .contains("SCAN documents_fts VIRTUAL TABLE INDEX")
                        && detail.contains('M')),
                    "{details:?}"
                );
            }
        }
    }

    #[test]
    fn normalized_cursors_reject_new_physical_file_and_advanced_rebuild_epoch() {
        let legacy = reader();
        let alpha = source(&legacy, "alpha", "Captured title");
        let beta = source(&legacy, "beta", "Captured title");
        let docs = [alpha, beta];
        let (_temp, catalog, _completed) = normalized_fixture(&docs);
        let old = catalog
            .query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
            .unwrap();
        let plan = QueryPlan {
            limits: SearchLimits {
                candidates: 2,
                hits: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let cursor = search_indexed_sources(&old, "needle", &plan)
            .unwrap()
            .next_cursor
            .unwrap();
        publish_normalized_sources(&catalog, &docs);
        let new = catalog
            .query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
            .unwrap();
        assert_eq!(old.snapshot().generation + 1, new.snapshot().generation);
        assert_eq!(
            old.snapshot().parser_fingerprint,
            new.snapshot().parser_fingerprint
        );
        assert_ne!(old.publication_id(), new.publication_id());
        old.verify_operations(&catalog).unwrap();
        new.verify_operations(&catalog).unwrap();
        let plan = QueryPlan {
            cursor: Some(cursor),
            ..plan
        };
        assert_eq!(
            search_indexed_sources(&new, "needle", &plan)
                .unwrap_err()
                .code,
            ErrorCode::CursorStale
        );
        assert!(search_indexed_sources(&old, "needle", &plan).is_ok());
    }

    #[test]
    fn source_sql_exact_ids_title_and_fts_share_exact_byte_excerpts() {
        let reader = reader();
        let document = source(&reader, "alpha", "Captured title");
        for (query, reason) in [
            ("source_alpha", RetrievalReason::ExactId),
            ("revision_alpha", RetrievalReason::ExactId),
            ("Captured title", RetrievalReason::ExactTitle),
            ("needle", RetrievalReason::Lexical),
            ("cafe", RetrievalReason::Lexical),
        ] {
            let hits = search_indexed_sources(&reader, query, &QueryPlan::default()).unwrap();
            assert_eq!(hits.hits.len(), 1, "{query}");
            let hit = &hits.hits[0];
            assert_eq!(hit.locator.path, document.path);
            assert!(hit.reasons.contains(&reason), "{query}");
            assert!(!hit.reasons.contains(&RetrievalReason::Identity));
            assert_eq!(
                hit.excerpt.span.slice(&document.raw_text).unwrap(),
                hit.excerpt.text
            );
            assert!(hit.excerpt.citation.is_none());
        }
    }

    #[test]
    fn source_filters_reject_unsupported_requests_before_sql() {
        let mut reader = reader();
        reader.connection = Connection::open_in_memory().unwrap();
        for filters in [
            SearchFilters {
                kinds: vec![RecordKind::Source],
                ..SearchFilters::default()
            },
            SearchFilters {
                tags: vec!["tag".into()],
                ..SearchFilters::default()
            },
            SearchFilters {
                authored_statuses: vec!["reviewed".into()],
                ..SearchFilters::default()
            },
            SearchFilters {
                include_historical: true,
                ..SearchFilters::default()
            },
            SearchFilters {
                include_proposed: true,
                ..SearchFilters::default()
            },
        ] {
            let error = search_indexed_sources(
                &reader,
                "needle",
                &QueryPlan {
                    filters,
                    ..QueryPlan::default()
                },
            )
            .unwrap_err();
            assert_eq!(error.code, ErrorCode::Usage);
        }
        assert_eq!(reader.decoded.get(), 0);
    }

    #[test]
    fn source_id_and_path_filters_apply_before_the_cap() {
        let reader = reader();
        source(&reader, "alpha", "Captured title");
        let selected = source(&reader, "beta", "Captured title");
        let plan = QueryPlan {
            filters: SearchFilters {
                source_ids: vec![RecordId::new("source_beta").unwrap()],
                path_prefix: Some("sources/be".into()),
                ..SearchFilters::default()
            },
            limits: SearchLimits {
                candidates: 1,
                hits: 1,
                ..SearchLimits::default()
            },
            ..QueryPlan::default()
        };
        let hits = search_indexed_sources(&reader, "needle", &plan).unwrap();
        assert_eq!(hits.hits.len(), 1);
        assert_eq!(hits.hits[0].locator.path, selected.path);
        assert!(!hits.truncated);
        assert_eq!(reader.decoded.get(), 1);
    }

    #[test]
    fn source_sql_does_not_decode_matching_managed_or_ineligible_documents() {
        let reader = reader();
        let selected = source(&reader, "selected", "Captured title");
        for (name, eligibility, kind) in [
            ("historical", "historical", None),
            ("invalid", "invalid", None),
            ("entity", "current", Some("entity")),
        ] {
            source(&reader, name, "Captured title");
            reader
                .connection
                .execute(
                    "UPDATE documents SET eligibility=?1,kind=?2,row_json='{' WHERE source_id=?3",
                    params![eligibility, kind, format!("source_{name}")],
                )
                .unwrap();
        }
        let hits = search_indexed_sources(&reader, "needle", &QueryPlan::default()).unwrap();
        assert_eq!(hits.hits.len(), 1);
        assert_eq!(hits.hits[0].locator.path, selected.path);
        assert_eq!(reader.decoded.get(), 1);
    }

    #[test]
    fn source_index_missing_or_incompatible_fails_with_rebuild_hint() {
        for replacement in [
            "",
            "CREATE INDEX source_document_ids ON documents(source_id,gen,path)",
            "CREATE INDEX source_document_ids ON documents(gen,source_id,path) WHERE eligibility='historical'",
            "CREATE INDEX source_document_ids ON documents(gen,source_id COLLATE NOCASE,path) WHERE owner_revision IS NOT NULL AND source_id IS NOT NULL AND eligibility='current'",
        ] {
            let reader = reader();
            reader
                .connection
                .execute_batch("DROP INDEX source_document_ids")
                .unwrap();
            reader.connection.execute_batch(replacement).unwrap();
            let error =
                search_indexed_sources(&reader, "needle", &QueryPlan::default()).unwrap_err();
            assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
            assert_eq!(error.hint.as_deref(), Some("run index rebuild"));
            assert_eq!(reader.decoded.get(), 0);
        }
    }

    #[test]
    fn selected_source_sql_json_and_fts_disagreement_fails_closed() {
        for mutation in [
            "UPDATE documents SET title='tampered title'",
            "UPDATE documents SET path='sources/tampered/content.md'",
            "UPDATE documents SET source_id='source_tampered'",
            "UPDATE documents SET owner_revision='revision_tampered'",
            "UPDATE documents SET file_hash='tampered'",
            "UPDATE documents SET body='tampered body'",
            "UPDATE documents SET raw_text='tampered raw'",
            "UPDATE documents_fts SET title='tampered title'",
            "UPDATE documents_fts SET aliases='tampered alias'",
            "UPDATE documents_fts SET headings='tampered heading'",
            "UPDATE documents_fts SET tags='tampered tag'",
            "UPDATE documents_fts SET body='tampered needle body'",
        ] {
            let reader = reader();
            source(&reader, "alpha", "Captured title");
            reader.connection.execute_batch(mutation).unwrap();
            assert_eq!(
                search_indexed_sources(&reader, "needle", &QueryPlan::default())
                    .unwrap_err()
                    .code,
                ErrorCode::IndexCorrupt,
                "{mutation}"
            );
        }
    }

    #[test]
    fn source_candidate_cap_and_cursor_bind_filters() {
        let reader = reader();
        source(&reader, "alpha", "Captured title");
        source(&reader, "beta", "Captured title");
        source(&reader, "gamma", "Captured title");
        let mut plan = QueryPlan {
            limits: SearchLimits {
                candidates: 2,
                hits: 1,
                ..SearchLimits::default()
            },
            ..QueryPlan::default()
        };
        let first = search_indexed_sources(&reader, "needle", &plan).unwrap();
        assert_eq!(first.candidate_count, 2);
        assert_eq!(first.omitted_candidates, 1);
        assert!(first.truncated);
        assert_eq!(reader.decoded.get(), 2);
        plan.cursor = first.next_cursor;
        let second = search_indexed_sources(&reader, "needle", &plan).unwrap();
        assert_ne!(first.hits[0].locator.path, second.hits[0].locator.path);
        plan.filters.path_prefix = Some("sources/alpha".into());
        assert_eq!(
            search_indexed_sources(&reader, "needle", &plan)
                .unwrap_err()
                .code,
            ErrorCode::CursorStale
        );
    }

    #[test]
    fn source_sql_vm_interruption_is_a_budget_failure() {
        let reader = reader();
        source(&reader, "alpha", "Captured title");
        reader
            .connection
            .progress_handler(1, Some(|| true))
            .unwrap();
        let error = match source_candidates(&reader, "needle", "\"needle\"", &QueryPlan::default())
        {
            Ok(_) => panic!("interrupted source candidate query unexpectedly completed"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert_eq!(reader.decoded.get(), 0);
    }

    fn query_plan(reader: &Reader, key: &str, index: Option<&str>) -> (String, Vec<String>) {
        let (sql, values) = source_candidate_query(
            false,
            1,
            "needle",
            "\"needle\"",
            &QueryPlan::default(),
            key,
            index,
        );
        let mut statement = reader
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let details = statement
            .query_map(params_from_iter(values), |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        (sql, details)
    }

    #[test]
    fn production_source_query_plans_use_bound_key_indexes_and_materialized_candidates() {
        let reader = reader();
        source(&reader, "alpha", "Captured title");
        for (index, key) in SOURCE_INDEXES {
            let (sql, details) = query_plan(&reader, key, Some(index));
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("SEARCH d USING INDEX")
                        && detail.contains(index)
                        && detail.contains("gen=?")
                        && detail.contains(&format!("{key}=?"))),
                "{key}: {details:?}"
            );
            assert_materialized_payload_fetch(&sql, &details);
        }
    }

    fn assert_materialized_payload_fetch(sql: &str, details: &[String]) {
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("MATERIALIZE candidate_ids")),
            "{details:?}"
        );
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("SEARCH d USING INTEGER PRIMARY KEY (rowid=?)")),
            "{details:?}"
        );
        let (keys, payload) = sql.split_once("SELECT d.row_json").unwrap();
        assert!(keys.contains("AS MATERIALIZED"));
        assert!(keys.contains("LIMIT ?"));
        assert!(!keys.contains("row_json"));
        assert!(!keys.contains("raw_text"));
        assert!(payload.contains("JOIN documents d ON d.doc_row=c.doc_row"));
    }

    #[test]
    fn production_source_fts_plan_uses_match_then_document_primary_key() {
        let reader = reader();
        source(&reader, "alpha", "Captured title");
        let (sql, details) = query_plan(&reader, "", None);
        assert!(
            details.iter().any(
                |detail| detail.contains("SCAN documents_fts VIRTUAL TABLE INDEX")
                    && detail.split_once("INDEX").unwrap().1.contains('M')
            ),
            "{details:?}"
        );
        // There are separate PK lookups for candidates and the capped payload
        // fetch. A generation-range document scan would violate this path.
        assert_eq!(
            details
                .iter()
                .filter(|detail| detail.contains("SEARCH d USING INTEGER PRIMARY KEY (rowid=?)"))
                .count(),
            2,
            "{details:?}"
        );
        assert!(
            !details.iter().any(|detail| detail.starts_with("SCAN d ")
                || (detail.starts_with("SEARCH d USING INDEX") && detail.contains("gen=?"))),
            "{details:?}"
        );
        assert!(
            details.iter().any(
                |detail| detail.contains("SCAN f VIRTUAL TABLE INDEX") && detail.contains(":=")
            ),
            "{details:?}"
        );
        assert_materialized_payload_fetch(&sql, &details);
    }

    #[test]
    fn common_term_decodes_only_eighty_capped_current_sources() {
        let reader = reader();
        // Every indexed row matches the same frequent term. Unselected cache
        // JSON is deliberately unreadable, including the 81st current source.
        for number in 0..81 {
            source(&reader, &format!("a{number:03}"), "Captured title");
        }
        reader
            .connection
            .execute(
                "UPDATE documents SET row_json='{' WHERE source_id='source_a080'",
                [],
            )
            .unwrap();
        for number in 0..512 {
            source(&reader, &format!("unselected{number:03}"), "Captured title");
        }
        reader.connection.execute("UPDATE documents SET kind='entity',row_json='{' WHERE source_id LIKE 'source_unselected%'", []).unwrap();
        let hits = search_indexed_sources(&reader, "needle", &QueryPlan::default()).unwrap();
        assert_eq!(hits.candidate_count, 80);
        assert_eq!(hits.omitted_candidates, 1);
        assert!(hits.truncated);
        assert_eq!(hits.hits.len(), 10);
        assert_eq!(reader.decoded.get(), 80);
        assert!(hits.hits.iter().all(|hit| {
            hit.source_id
                .as_ref()
                .unwrap()
                .as_str()
                .starts_with("source_a")
        }));
    }
}

#[cfg(test)]
#[path = "general_lexical_tests.rs"]
mod general_lexical_tests;

#[cfg(test)]
#[path = "candidate_metadata_tests.rs"]
mod candidate_metadata_tests;

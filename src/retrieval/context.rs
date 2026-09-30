//! Pure context assembly; only the verification coordinator seals output.
use super::{bundles, context_types::*, types::*};
use crate::{
    catalog::ReaderSnapshot,
    domain::*,
    graph::{GraphResult, NavigationEdge},
};
use std::collections::BTreeMap;

pub struct ContextDraft {
    pub(super) text: String,
    pub(super) passages: Vec<ContextPassage>,
    pub(super) bundles: Vec<EvidenceBundle>,
    pub(super) omissions: Vec<ContextOmission>,
    pub(super) usage: ContextUsage,
    pub(super) snapshot: ReadSnapshot,
    pub(super) dependency_fingerprint: Blake3Hash,
    pub(super) truncated: bool,
    pub(super) warnings: Vec<String>,
}
impl ContextDraft {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn passages(&self) -> &[ContextPassage] {
        &self.passages
    }
    pub fn bundles(&self) -> &[EvidenceBundle] {
        &self.bundles
    }
    pub fn omissions(&self) -> &[ContextOmission] {
        &self.omissions
    }
    pub fn usage(&self) -> &ContextUsage {
        &self.usage
    }
}
pub fn validate_request(query: &str, request: &ContextRequest) -> Result<ContextRequest> {
    super::lexical::validate_plan(query, &request.documents)?;
    if request
        .graph
        .as_ref()
        .is_some_and(|g| g.seed_mode == crate::graph::GraphSeedMode::Lexical)
    {
        super::lexical::lexical_expression(query)?;
    }
    normalize_request(request)
}
fn normalize_request(request: &ContextRequest) -> Result<ContextRequest> {
    let mut normalized = request.clone();
    normalized.documents = super::lexical::validate_plan("context", &request.documents)?;
    if let Some(g) = &request.graph {
        normalized.graph = Some(crate::graph::query::validate_plan(g)?)
    }
    if request.target != ContextTarget::Documents && request.graph.is_none() {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "graph context requires a graph plan",
        ));
    }
    let b = &request.budget;
    let v = &request.verification_budget;
    if b.max_bytes == 0
        || b.max_bytes > 12000
        || b.max_tokens == 0
        || b.max_tokens > 3000
        || b.instruction_bytes
            .checked_add(b.output_bytes)
            .is_none_or(|n| n > b.max_bytes)
        || b.instruction_tokens
            .checked_add(b.output_tokens)
            .is_none_or(|n| n > b.max_tokens)
        || v.max_bytes == 0
        || v.max_bytes > 256 * 1024 * 1024
        || v.max_files == 0
        || v.max_files > 16384
        || v.max_entries == 0
        || v.max_entries > 65536
        || v.max_elapsed_ms == 0
        || v.max_elapsed_ms > 30000
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "context or verification budget exceeds its ceiling or reservation",
        ));
    }
    normalized.documents.filters.include_historical = request.scope != ContextScope::Current;
    normalized.documents.filters.include_proposed = false;
    if let Some(g) = &mut normalized.graph {
        g.filters.include_historical = request.scope != ContextScope::Current;
        g.filters.include_proposed = false;
    }
    Ok(normalized)
}
pub(super) fn dependencies(reader: &ReaderSnapshot) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(
        serde_json::to_vec(&reader.projection().dependencies)
            .map_err(|e| WikiError::invalid(e.to_string()))?,
    ))
}
fn document_passage(
    reader: &ReaderSnapshot,
    hit: &SearchHit,
    excerpt: &SearchExcerpt,
    request: &ContextRequest,
    rank: usize,
) -> Result<Option<ContextPassage>> {
    let Some(d) = reader
        .projection()
        .documents
        .iter()
        .find(|d| d.path == hit.locator.path)
    else {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "context hit path absent from pinned projection",
        ));
    };
    if d.hash != hit.locator.observed_hash {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "context hit hash differs from pinned projection",
        ));
    }
    if !matches_filters(reader, d, &request.documents.filters) {
        return Ok(None);
    }
    if d.owner_revision.is_some()
        && !request.documents.filters.source_ids.is_empty()
        && !d
            .source_id
            .as_ref()
            .is_some_and(|id| request.documents.filters.source_ids.contains(id))
    {
        return Ok(None);
    }
    let canonical = d
        .record_id
        .as_ref()
        .and_then(|id| reader.projection().records.get(id));
    let allowed = if d.owner_revision.is_some() {
        d.eligibility != Eligibility::Invalid
            && (request.scope != ContextScope::Current || d.eligibility == Eligibility::Current)
    } else {
        canonical.is_some_and(|r| match r.record.kind() {
            RecordKind::Page => {
                r.authored_status.as_deref() != Some("draft")
                    && r.eligibility != Eligibility::Invalid
                    && (request.scope != ContextScope::Current
                        || r.eligibility == Eligibility::Current
                            && r.authored_status.as_deref() == Some("reviewed"))
            }
            RecordKind::Entity => {
                matches!(r.description_eligibility, Some(Eligibility::Current))
                    || (request.scope != ContextScope::Current
                        && matches!(
                            r.description_eligibility,
                            Some(Eligibility::Historical | Eligibility::Stale)
                        ))
            }
            _ => false,
        })
    };
    if !allowed || excerpt.span.is_empty() {
        return Ok(None);
    }
    let span = excerpt.span;
    let text = span.slice(&d.raw_text)?;
    if text.len() > request.documents.limits.excerpt_bytes {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "context hit span exceeds excerpt bound",
        ));
    }
    let mut citations = Vec::new();
    let record = if let Some(row) = canonical {
        Some(bundles::reference(reader, row))
    } else {
        d.owner_revision.as_ref().map(|id| RecordRef {
            vault_id: reader.projection().vault_id.clone(),
            record_id: id.clone(),
            expected_kind: RecordKind::Revision,
        })
    };
    if hit.locator.record != record {
        return Err(WikiError::invalid(
            "context hit identity differs from canonical owner",
        ));
    }
    let captured = d.owner_revision.is_some();
    if captured && request.scope != ContextScope::Snapshot {
        citations.push(CitationRef::Source(SourceSpanRef {
            source_id: d
                .source_id
                .clone()
                .ok_or_else(|| WikiError::invalid("captured source missing"))?,
            source_revision: d.owner_revision.clone().expect("owner"),
            span,
            quote_hash: Blake3Hash::digest(text.as_bytes()),
        }))
    }
    Ok(Some(ContextPassage {
        locator: DocumentLocator {
            record,
            path: d.path.clone(),
            observed_hash: d.hash.clone(),
        },
        text: text.into(),
        span,
        label: if captured {
            ExcerptLabel::CapturedSource
        } else {
            ExcerptLabel::NoteText
        },
        eligibility: d.eligibility,
        citations,
        contributors: vec![],
        rank_contributions: vec![RankContribution {
            channel: "direct_document_owner".into(),
            rank,
            score: None,
        }],
        support_group: captured.then(|| d.hash.clone()),
    }))
}
fn matches_filters(
    reader: &ReaderSnapshot,
    d: &crate::catalog::DocumentRow,
    f: &SearchFilters,
) -> bool {
    let row = d
        .record_id
        .as_ref()
        .and_then(|id| reader.projection().records.get(id));
    (f.kinds.is_empty() || d.kind.is_some_and(|kind| f.kinds.contains(&kind)))
        && f.tags.iter().all(|tag| d.tags.contains(tag))
        && f.path_prefix
            .as_ref()
            .is_none_or(|prefix| d.path.as_str().starts_with(prefix))
        && (f.authored_statuses.is_empty()
            || row
                .and_then(|r| r.authored_status.as_ref())
                .is_some_and(|s| f.authored_statuses.contains(s)))
        && (f.source_ids.is_empty()
            || d.source_id
                .as_ref()
                .is_some_and(|s| f.source_ids.contains(s))
            || row.is_some_and(|r| {
                r.record.kind() == RecordKind::Source && f.source_ids.contains(r.record.id())
                    || r.record
                        .string("wiki_source_id")
                        .is_some_and(|s| f.source_ids.iter().any(|id| id.as_str() == s))
                    || r.record
                        .field("wiki_source_ids")
                        .and_then(|v| v.as_array())
                        .is_some_and(|values| {
                            values
                                .iter()
                                .filter_map(|v| v.as_str())
                                .any(|s| f.source_ids.iter().any(|id| id.as_str() == s))
                        })
                    || reader.projection().records.values().any(|e| {
                        e.record.kind() == RecordKind::Evidence
                            && e.record.string("wiki_assertion_id") == Some(r.record.id().as_str())
                            && e.record
                                .string("wiki_source_id")
                                .is_some_and(|s| f.source_ids.iter().any(|id| id.as_str() == s))
                    })
            }))
}
struct Packet {
    passages: Vec<ContextPassage>,
    bundle: Option<EvidenceBundle>,
    navigation: Option<NavigationEdge>,
    key: String,
    score: f64,
}
fn navigation_end<'a>(
    reader: &'a ReaderSnapshot,
    locator: &DocumentLocator,
) -> Result<&'a crate::catalog::DocumentRow> {
    let reference = locator
        .record
        .as_ref()
        .ok_or_else(|| WikiError::invalid("navigation endpoint has no record"))?;
    let row = reader
        .projection()
        .records
        .get(&reference.record_id)
        .ok_or_else(|| {
            WikiError::new(ErrorCode::FreshnessConflict, "navigation endpoint absent")
        })?;
    if reference != &bundles::reference(reader, row)
        || locator.path != row.path
        || locator.observed_hash != row.hash
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "navigation endpoint differs from pinned record",
        ));
    }
    reader
        .projection()
        .documents
        .iter()
        .find(|d| d.path == locator.path && d.hash == locator.observed_hash)
        .ok_or_else(|| WikiError::new(ErrorCode::FreshnessConflict, "navigation document absent"))
}
fn citation_state(
    reader: &ReaderSnapshot,
    citation: &CitationRef,
) -> Result<crate::sources::CitationState> {
    use crate::sources::CitationState;
    let (source_id, revision) = match citation {
        CitationRef::Source(r) => (&r.source_id, &r.source_revision),
        CitationRef::Assertion(r) => (&r.source_id, &r.source_revision),
    };
    let source = reader
        .projection()
        .records
        .get(source_id)
        .ok_or_else(|| WikiError::invalid("citation source absent from pinned projection"))?;
    let mut state = if source.record.string("wiki_status") == Some("withdrawn") {
        CitationState::Withdrawn
    } else if source.record.string("wiki_current_revision") == Some(revision.as_str()) {
        CitationState::Current
    } else {
        CitationState::Historical
    };
    if let CitationRef::Assertion(r) = citation {
        let evidence = reader
            .projection()
            .records
            .get(&r.evidence_id)
            .ok_or_else(|| WikiError::invalid("citation evidence absent from pinned projection"))?;
        let assertion = reader
            .projection()
            .records
            .get(&r.assertion_id)
            .ok_or_else(|| {
                WikiError::invalid("citation assertion absent from pinned projection")
            })?;
        if state == CitationState::Current
            && (evidence.record.string("wiki_status") != Some("active")
                || assertion.record.string("wiki_status") != Some("accepted"))
        {
            state = CitationState::Historical;
        }
    }
    Ok(state)
}
fn render(
    reader: &ReaderSnapshot,
    scope: ContextScope,
    passages: &[ContextPassage],
    bundles: &[EvidenceBundle],
    navigation: &[NavigationEdge],
) -> Result<(String, usize)> {
    let mut text = if scope == ContextScope::Snapshot {
        "[context index_snapshot; unverified]\n\n".to_owned()
    } else {
        String::new()
    };
    let mut graph_bytes = 0;
    for (i, p) in passages.iter().enumerate() {
        let start = text.len();
        text.push_str(&format!(
            "[passage {i}; {:?}; {:?}; path {}; bytes {}..{}]\n",
            p.label,
            p.eligibility,
            p.locator.path,
            p.span.start(),
            p.span.end()
        ));
        if let Some(r) = &p.locator.record {
            text.push_str(&format!("Record: {} ({})\n", r.record_id, r.expected_kind))
        }
        for c in &p.citations {
            let state = citation_state(reader, c)?;
            text.push_str(&format!(
                "Citation ({state:?}): {}\n",
                serde_json::to_string(c).map_err(|e| WikiError::invalid(e.to_string()))?
            ))
        }
        for c in &p.contributors {
            text.push_str(&format!(
                "Contributor: {}\n",
                serde_json::to_string(c).map_err(|e| WikiError::invalid(e.to_string()))?
            ))
        }
        text.push_str(&p.text);
        text.push_str("\n\n");
        if !p.contributors.is_empty() {
            graph_bytes += text.len() - start
        }
    }
    for b in bundles {
        let start = text.len();
        let path_labels = b
            .path
            .iter()
            .map(|step| {
                let row = &reader.projection().records[&step.assertion.record_id];
                format!(
                    "{}: {:?}, status {:?}",
                    step.assertion.record_id, row.eligibility, row.authored_status
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        text.push_str(&format!("[assertion {}; {:?}; status {:?}; disputed {}]\n{} {} {}\nQualifiers: {}\nPath: {}\nPath states: {}\nPassages: {:?}; omitted support {}; omitted contradiction {}\n\n",b.assertion.record_id,b.eligibility,b.authored_status,b.disputed,b.subject.record_id,b.predicate,serde_json::to_string(&b.object).map_err(|e|WikiError::invalid(e.to_string()))?,serde_json::to_string(&b.qualifiers).map_err(|e|WikiError::invalid(e.to_string()))?,serde_json::to_string(&b.path).map_err(|e|WikiError::invalid(e.to_string()))?,path_labels,b.passage_indices,b.omitted_support,b.omitted_contradictions));
        graph_bytes += text.len() - start
    }
    for edge in navigation {
        let start = text.len();
        text.push_str(&format!(
            "[navigation; {:?}; no asserted relationship]\nFrom: {} ({})\nTo: {} ({})\n\n",
            edge.reason,
            edge.from.path,
            edge.from
                .record
                .as_ref()
                .expect("validated navigation")
                .record_id,
            edge.to.path,
            edge.to
                .record
                .as_ref()
                .expect("validated navigation")
                .record_id,
        ));
        graph_bytes += text.len() - start;
    }
    Ok((text, graph_bytes))
}
pub fn assemble(
    reader: &ReaderSnapshot,
    request: &ContextRequest,
    hits: &HitSet,
    graph: Option<&GraphResult>,
) -> Result<ContextDraft> {
    let request = normalize_request(request)?;
    let request = &request;
    if hits.hits.len() > request.documents.limits.hits
        || graph.is_some_and(|g| {
            g.assertions.len() + g.navigation.len()
                > request.graph.as_ref().map_or(0, |p| p.limits.hits)
        })
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "context candidate payload exceeds display limit",
        ));
    }
    let dependency_fingerprint = dependencies(reader)?;
    if hits.snapshot != *reader.snapshot()
        || hits.dependency_fingerprint != dependency_fingerprint
        || graph.is_some_and(|g| {
            g.snapshot != *reader.snapshot() || g.dependency_fingerprint != dependency_fingerprint
        })
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "context candidates are from another pinned snapshot",
        ));
    }
    let mut packets = Vec::new();
    let mut omissions = Vec::new();
    let mut direct: BTreeMap<String, usize> = BTreeMap::new();
    let mut graph_ranks: BTreeMap<String, usize> = BTreeMap::new();
    if request.target != ContextTarget::Graph {
        for (i, hit) in hits.hits.iter().enumerate() {
            for (passage_index, excerpt) in std::iter::once(&hit.excerpt)
                .chain(hit.secondary_excerpts.iter().take(1))
                .enumerate()
            {
                if let Some(p) = document_passage(reader, hit, excerpt, request, i + 1)? {
                    direct
                        .entry(bundles::owner(&p))
                        .and_modify(|r| *r = (*r).min(i + 1))
                        .or_insert(i + 1);
                    packets.push(Packet {
                        key: format!("document:{}:{passage_index}", p.locator.path),
                        passages: vec![p],
                        bundle: None,
                        navigation: None,
                        score: 0.,
                    })
                } else {
                    omissions.push(ContextOmission {
                        record_id: hit.locator.record.as_ref().map(|r| r.record_id.clone()),
                        path: Some(hit.locator.path.clone()),
                        reason: "discovery_only_or_empty".into(),
                        count: 1,
                    })
                }
            }
        }
    }
    if request.target != ContextTarget::Documents
        && let Some(graph) = graph
    {
        for (i, edge) in graph.assertions.iter().enumerate() {
            let graph_filters = &request
                .graph
                .as_ref()
                .expect("validated graph plan")
                .filters;
            if !std::iter::once(&edge.record_ref)
                .chain(edge.path.iter().map(|s| &s.assertion))
                .all(|reference| {
                    reader
                        .projection()
                        .records
                        .get(&reference.record_id)
                        .and_then(|r| {
                            reader
                                .projection()
                                .documents
                                .iter()
                                .find(|d| d.path == r.path)
                        })
                        .is_some_and(|d| matches_filters(reader, d, graph_filters))
                })
            {
                omissions.push(ContextOmission {
                    record_id: Some(edge.record_ref.record_id.clone()),
                    path: Some(edge.locator.path.clone()),
                    reason: "candidate_outside_requested_filters".into(),
                    count: 1,
                });
                continue;
            }
            if edge.path.len() > request.graph.as_ref().map_or(1, |g| g.limits.depth.max(1)) {
                return Err(WikiError::invalid("graph path exceeds requested depth"));
            }
            if let Some(selected) = bundles::select(
                reader,
                edge,
                request.scope,
                i + 1,
                &graph_filters.source_ids,
            )? {
                for p in &selected.passages {
                    graph_ranks
                        .entry(bundles::owner(p))
                        .and_modify(|r| *r = (*r).min(i + 1))
                        .or_insert(i + 1);
                }
                packets.push(Packet {
                    passages: selected.passages,
                    bundle: Some(selected.bundle),
                    navigation: None,
                    key: format!("assertion:{}", edge.record_ref.record_id),
                    score: 0.,
                })
            } else {
                omissions.push(ContextOmission {
                    record_id: Some(edge.record_ref.record_id.clone()),
                    path: Some(edge.locator.path.clone()),
                    reason: "no_eligible_original_support".into(),
                    count: 1,
                })
            }
        }
        if request.graph.as_ref().is_some_and(|g| g.include_navigation) {
            let graph_filters = &request
                .graph
                .as_ref()
                .expect("validated graph plan")
                .filters;
            for (rank, edge) in graph.navigation.iter().enumerate() {
                let from = navigation_end(reader, &edge.from)?;
                let to = navigation_end(reader, &edge.to)?;
                if !crate::graph::traverse::navigation_is_canonical(reader, edge) {
                    return Err(WikiError::invalid(
                        "context navigation differs from canonical links or provenance",
                    ));
                }
                if !matches_filters(reader, from, graph_filters)
                    || !matches_filters(reader, to, graph_filters)
                {
                    omissions.push(ContextOmission {
                        record_id: edge.to.record.as_ref().map(|r| r.record_id.clone()),
                        path: Some(edge.to.path.clone()),
                        reason: "navigation_outside_requested_filters".into(),
                        count: 1,
                    });
                    continue;
                }
                packets.push(Packet {
                    passages: vec![],
                    bundle: None,
                    navigation: Some(edge.clone()),
                    key: format!("navigation:{rank}:{}:{}", edge.from.path, edge.to.path),
                    score: 0.,
                });
            }
            if graph.coverage.omitted_navigation > 0 {
                omissions.push(ContextOmission {
                    record_id: None,
                    path: None,
                    reason: "graph_navigation_traversal_cap".into(),
                    count: graph.coverage.omitted_navigation,
                });
            }
            if graph.coverage.depth_limited {
                omissions.push(ContextOmission {
                    record_id: None,
                    path: None,
                    reason: "graph_depth_limit_may_omit_navigation".into(),
                    count: 1,
                });
            }
            if graph.next_cursor.is_some() {
                omissions.push(ContextOmission {
                    record_id: None,
                    path: None,
                    reason: "graph_results_continue_on_next_page".into(),
                    count: 1,
                });
            }
        }
    }
    for packet in &mut packets {
        let mut best = Vec::new();
        for p in &mut packet.passages {
            let key = bundles::owner(p);
            let mut ranks = Vec::new();
            if let Some(rank) = direct.get(&key) {
                ranks.push(RankContribution {
                    channel: "direct_document_owner".into(),
                    rank: *rank,
                    score: None,
                });
            }
            if let Some(rank) = graph_ranks.get(&key) {
                ranks.push(RankContribution {
                    channel: "graph_source_owner".into(),
                    rank: *rank,
                    score: None,
                });
            }
            let score = ranks.iter().map(|r| 1.0 / (60.0 + r.rank as f64)).sum();
            if score > packet.score {
                packet.score = score;
                best = ranks.clone();
            }
            p.rank_contributions = ranks;
        }
        if let Some(bundle) = &mut packet.bundle {
            bundle.rank_contributions = best;
        }
    }
    packets.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.key.cmp(&b.key)));
    let reserved_bytes = request.budget.instruction_bytes + request.budget.output_bytes;
    let reserved_tokens = request.budget.instruction_tokens + request.budget.output_tokens;
    let available_bytes = request.budget.max_bytes - reserved_bytes;
    let available_tokens = request.budget.max_tokens - reserved_tokens;
    let mut passages: Vec<ContextPassage> = Vec::new();
    let mut bundles: Vec<EvidenceBundle> = Vec::new();
    let mut navigation: Vec<NavigationEdge> = Vec::new();
    let (mut text, mut graph_bytes) =
        render(reader, request.scope, &passages, &bundles, &navigation)?;
    if text.len() > available_bytes || text.len().div_ceil(4) > available_tokens {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "scope label does not fit reserved context budget",
        ));
    }
    for packet in packets {
        let mut next = passages.clone();
        let mut next_bundles = bundles.clone();
        let mut next_navigation = navigation.clone();
        if let Some(edge) = &packet.navigation {
            next_navigation.push(edge.clone());
        }
        let mut indices = Vec::new();
        for p in packet.passages {
            let mut found = None;
            for (i, existing) in next.iter_mut().enumerate() {
                if bundles::merge(existing, &p, reader)? {
                    found = Some(i);
                    break;
                }
            }
            let index = found.unwrap_or_else(|| {
                let i = next.len();
                next.push(p);
                i
            });
            if !indices.contains(&index) {
                indices.push(index)
            }
        }
        let mapping = bundles::coalesce(&mut next, reader)?;
        for bundle in &mut next_bundles {
            for index in &mut bundle.passage_indices {
                *index = mapping[*index]
            }
            bundle.passage_indices.sort_unstable();
            bundle.passage_indices.dedup();
        }
        for index in &mut indices {
            *index = mapping[*index]
        }
        indices.sort_unstable();
        indices.dedup();
        let id = packet
            .bundle
            .as_ref()
            .map(|b| b.assertion.record_id.clone());
        if let Some(mut bundle) = packet.bundle {
            bundle.passage_indices = indices;
            next_bundles.push(bundle)
        }
        let mut counts = BTreeMap::new();
        for p in &next {
            *counts.entry(bundles::owner(p)).or_insert(0usize) += 1
        }
        let (rendered, next_graph) = render(
            reader,
            request.scope,
            &next,
            &next_bundles,
            &next_navigation,
        )?;
        let bytes = rendered.len();
        let tokens = bytes.div_ceil(4);
        let graph_tokens = next_graph.div_ceil(4);
        let reason = if counts.values().any(|n| *n > 2) {
            Some("document_passage_cap")
        } else if bytes > available_bytes || tokens > available_tokens {
            Some("required_bundle_or_passage_does_not_fit")
        } else if request.target == ContextTarget::Combined
            && (next_graph > available_bytes / 2 || graph_tokens > available_tokens / 2)
        {
            Some("combined_graph_share_cap")
        } else {
            None
        };
        if let Some(reason) = reason {
            omissions.push(ContextOmission {
                record_id: id.or_else(|| {
                    packet
                        .navigation
                        .as_ref()
                        .and_then(|n| n.to.record.as_ref().map(|r| r.record_id.clone()))
                }),
                path: packet.navigation.as_ref().map(|n| n.to.path.clone()),
                reason: if packet.navigation.is_some() {
                    format!("navigation_{reason}")
                } else {
                    reason.into()
                },
                count: 1,
            });
            continue;
        }
        passages = next;
        bundles = next_bundles;
        navigation = next_navigation;
        text = rendered;
        graph_bytes = next_graph;
    }
    let mut warnings = vec![
        "tokens are UTF-8 byte estimates (ceil(bytes/4)); no exact tokenizer accounting".into(),
    ];
    if request.scope == ContextScope::Snapshot {
        warnings.push("unverified index snapshot; citations suppressed".into())
    }
    if request
        .graph
        .as_ref()
        .is_some_and(|g| !g.filters.source_ids.is_empty())
    {
        warnings.push("source filter scopes emitted graph evidence; omitted support and contradiction counts include evidence outside the selected sources".into());
    }
    let truncated = !omissions.is_empty()
        || bundles
            .iter()
            .any(|b| b.omitted_support + b.omitted_contradictions > 0)
        || hits.truncated
        || graph.is_some_and(|g| g.truncated);
    let rendered_bytes = text.len();
    let estimated_tokens = rendered_bytes.div_ceil(4);
    Ok(ContextDraft {
        text,
        passages,
        bundles,
        omissions,
        usage: ContextUsage {
            rendered_bytes,
            estimated_tokens,
            token_accounting: TokenAccounting::EstimatedUtf8BytesDiv4Ceil,
            reserved_bytes,
            reserved_tokens,
            graph_bytes,
            graph_estimated_tokens: graph_bytes.div_ceil(4),
            verification_bytes: 0,
            verification_files: 0,
            verification_entries: 0,
        },
        snapshot: reader.snapshot().clone(),
        dependency_fingerprint,
        truncated,
        warnings,
    })
}

//! Pure context assembly; only the verification coordinator seals output.
use super::context_selection_packet::{self, SelectionAction, SelectionCard, SelectionPacket};
use super::{bundles, context_types::*, types::*};
use crate::{
    catalog::{
        CatalogDiagnostic, DocumentRow, ReaderSnapshot, RecordRow, SnapshotVerification,
        query_types::QueryCatalog,
    },
    domain::*,
    graph::{GraphResult, NavigationEdge},
};
use std::collections::{BTreeMap, BTreeSet};

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
    pub(super) selection_packet: Option<SelectionPacket>,
}
impl ContextDraft {
    pub(super) fn verification_passages(&self) -> impl Iterator<Item = &ContextPassage> {
        self.passages.iter().chain(
            self.selection_packet
                .iter()
                .flat_map(|packet| packet.cards.iter().map(|card| &card.passage)),
        )
    }
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
pub fn validate_selection_action(request: &ContextRequest, action: &SelectionAction) -> Result<()> {
    if let SelectionAction::PrepareOriginals(originals)
        | SelectionAction::ApplyOriginals { request: originals, .. } = action
    {
        let filters = &request.documents.filters;
        if request.scope != ContextScope::IndexedDocuments
            || request.target != ContextTarget::Documents
            || request.graph.is_some()
            || request.documents.mode != SearchMode::Lexical
            || !filters.kinds.is_empty()
            || !filters.tags.is_empty()
            || !filters.authored_statuses.is_empty()
            || filters.include_proposed
            || request.documents.cursor.is_some()
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "original selection requires lexical indexed-documents with source/path filters only",
            ));
        }
        return super::context_original_selection::validate_original_request(originals);
    }
    if !matches!(action, SelectionAction::Automatic)
        && (!matches!(
            request.scope,
            ContextScope::Current | ContextScope::IndexedDocuments
        ) || request.target != ContextTarget::Documents
            || request.documents.mode == SearchMode::Literal
            || (request.scope == ContextScope::IndexedDocuments
                && (request.documents.mode != SearchMode::Lexical
                    || request.documents.filters.include_historical)))
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "host selection requires current document context; indexed-documents supports lexical mode only",
        ));
    }
    Ok(())
}
fn normalize_request(request: &ContextRequest) -> Result<ContextRequest> {
    if request.scope == ContextScope::IndexedDocuments
        && (request.target != ContextTarget::Documents
            || request.graph.is_some()
            || !matches!(
                request.documents.mode,
                SearchMode::Literal
                    | SearchMode::Lexical
                    | SearchMode::Semantic
                    | SearchMode::Hybrid
            )
            || (request.documents.filters.include_historical
                && !matches!(
                    request.documents.mode,
                    SearchMode::Literal | SearchMode::Lexical
                ))
            || request.documents.filters.include_proposed)
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "indexed-documents supports eligible document context without graph expansion; historical inclusion requires literal or lexical mode",
        ));
    }
    if request.scope == ContextScope::IndexedEvidence
        && (request.target != ContextTarget::Documents
            || request.graph.is_some()
            || request.documents.mode != SearchMode::Lexical
            || !request.documents.filters.kinds.is_empty()
            || !request.documents.filters.authored_statuses.is_empty()
            || !request.documents.filters.tags.is_empty()
            || request.documents.filters.include_historical
            || request.documents.filters.include_proposed)
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "indexed-evidence supports current captured-source documents in lexical mode with source and path filters only",
        ));
    }
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
        || b.max_bytes > 16 * 1024
        || b.max_tokens == 0
        || b.max_tokens > 4096
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
    normalized.documents.filters.include_historical = matches!(
        request.scope,
        ContextScope::Historical | ContextScope::Snapshot
    ) || (request.scope
        == ContextScope::IndexedDocuments
        && request.documents.filters.include_historical);
    normalized.documents.filters.include_proposed = false;
    if let Some(g) = &mut normalized.graph {
        g.filters.include_historical = matches!(
            request.scope,
            ContextScope::Historical | ContextScope::Snapshot
        );
        g.filters.include_proposed = false;
    }
    Ok(normalized)
}
// One fallible lookup per admitted owner, reused by every selected window.
pub(super) struct DocumentOwner {
    document: DocumentRow,
    canonical: Option<RecordRow>,
    pub(super) allowed: bool,
}
/// Packing can merge several windows from one selected owner. Reuse its
/// authenticated cached bytes rather than charging another SQL body decode for
/// each attempted merge. Strict graph passages retain their original lookup.
struct OwnerCatalog<'a> {
    reader: &'a dyn QueryCatalog,
    owners: &'a [DocumentOwner],
}
impl QueryCatalog for OwnerCatalog<'_> {
    fn normalized_layout(&self) -> bool {
        self.reader.normalized_layout()
    }
    fn publication_id(&self) -> Option<&str> {
        self.reader.publication_id()
    }
    fn connection(&self) -> &rusqlite::Connection {
        self.reader.connection()
    }
    fn check_query_budget(&self) -> Result<()> {
        self.reader.check_query_budget()
    }
    fn snapshot(&self) -> &ReadSnapshot {
        self.reader.snapshot()
    }
    fn vault_id(&self) -> &RecordId {
        self.reader.vault_id()
    }
    fn verification(&self) -> &SnapshotVerification {
        self.reader.verification()
    }
    fn record(&self, id: &RecordId) -> Result<Option<RecordRow>> {
        if let Some(row) = self
            .owners
            .iter()
            .filter_map(|owner| owner.canonical.as_ref())
            .find(|row| row.record.id() == id)
        {
            return Ok(Some(row.clone()));
        }
        self.reader.record(id)
    }
    fn document(&self, path: &VaultRelativePath) -> Result<Option<DocumentRow>> {
        if let Some(owner) = self
            .owners
            .iter()
            .find(|owner| &owner.document.path == path)
        {
            return Ok(Some(owner.document.clone()));
        }
        self.reader.document(path)
    }
    fn diagnostics(&self, paths: &BTreeSet<VaultRelativePath>) -> Result<Vec<CatalogDiagnostic>> {
        self.reader.diagnostics(paths)
    }
    fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
        self.reader.dependency_fingerprint()
    }
    fn query_scope(&self) -> &'static str {
        self.reader.query_scope()
    }
    fn decode_document(&self, row: &rusqlite::Row<'_>, column: usize) -> Result<DocumentRow> {
        self.reader.decode_document(row, column)
    }
}
pub(super) fn document_owner(
    reader: &dyn QueryCatalog,
    hit: &SearchHit,
    request: &ContextRequest,
) -> Result<DocumentOwner> {
    let d = reader.document(&hit.locator.path)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::FreshnessConflict,
            "context hit path absent from pinned catalog",
        )
    })?;
    if d.hash != hit.locator.observed_hash {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "context hit hash differs from pinned projection",
        ));
    }
    let filters_match =
        super::filters::matches_catalog_document(reader, &d, &request.documents.filters)?;
    let source_matches = d.owner_revision.is_none()
        || request.documents.filters.source_ids.is_empty()
        || d.source_id
            .as_ref()
            .is_some_and(|id| request.documents.filters.source_ids.contains(id));
    let canonical = d
        .record_id
        .as_ref()
        .map(|id| reader.record(id))
        .transpose()?
        .flatten();
    let current = request.scope == ContextScope::Current
        || (request.scope == ContextScope::IndexedDocuments
            && !request.documents.filters.include_historical);
    let allowed = if d.owner_revision.is_some() {
        d.eligibility != Eligibility::Invalid && (!current || d.eligibility == Eligibility::Current)
    } else {
        canonical.as_ref().is_some_and(|r| match r.record.kind() {
            RecordKind::Page => {
                r.authored_status.as_deref() != Some("draft")
                    && r.eligibility != Eligibility::Invalid
                    && (!current
                        || r.eligibility == Eligibility::Current
                            && r.authored_status.as_deref() == Some("reviewed"))
            }
            RecordKind::Entity => {
                matches!(r.description_eligibility, Some(Eligibility::Current))
                    || (!current
                        && matches!(
                            r.description_eligibility,
                            Some(Eligibility::Historical | Eligibility::Stale)
                        ))
            }
            _ => false,
        })
    };
    Ok(DocumentOwner {
        document: d,
        canonical,
        allowed: allowed && filters_match && source_matches,
    })
}
pub(super) fn document_passage(
    reader: &dyn QueryCatalog,
    owner: &DocumentOwner,
    hit: &SearchHit,
    excerpt: &SearchExcerpt,
    request: &ContextRequest,
    rank: usize,
) -> Result<Option<ContextPassage>> {
    let d = &owner.document;
    let canonical = owner.canonical.as_ref();
    if !owner.allowed || excerpt.span.is_empty() {
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
        Some(RecordRef {
            vault_id: reader.vault_id().clone(),
            record_id: row.record.id().clone(),
            expected_kind: row.record.kind(),
        })
    } else {
        d.owner_revision.as_ref().map(|id| RecordRef {
            vault_id: reader.vault_id().clone(),
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

#[cfg(test)]
pub(super) fn passage_for_span_for_test(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hit: &SearchHit,
    span: ByteSpan,
    rank: usize,
) -> Result<Option<ContextPassage>> {
    let owner = document_owner(reader, hit, request)?;
    let excerpt = SearchExcerpt {
        text: span.slice(&owner.document.raw_text)?.to_owned(),
        span,
        matched_spans: vec![],
        label: hit.excerpt.label,
        citation: None,
    };
    document_passage(reader, &owner, hit, &excerpt, request, rank)
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
pub(super) struct Packet {
    pub(super) passages: Vec<ContextPassage>,
    pub(super) bundle: Option<EvidenceBundle>,
    pub(super) navigation: Option<NavigationEdge>,
    pub(super) key: String,
    pub(super) score: f64,
    /// The source proposal's position before byte-span keys are constructed.
    /// Used only for host packet admission; automatic utility stays unchanged.
    pub(super) selection_ordinal: Option<usize>,
    pub(super) selection: Option<super::context_selection::SelectionCandidate>,
    pub(super) unit_score: Option<f64>,
    /// Exact representative before child focusing and parent deduplication.
    pub(super) unit_origin: Option<(ByteSpan, f64)>,
    pub(super) fallback: Option<ContextPassage>,
    pub(super) unit_clipped: bool,
}

fn sort_packets(packets: &mut [Packet], preserve_selection_order: bool) {
    packets.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| {
                if preserve_selection_order {
                    a.selection_ordinal
                        .unwrap_or(usize::MAX)
                        .cmp(&b.selection_ordinal.unwrap_or(usize::MAX))
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then(a.key.cmp(&b.key))
    });
}

// Opt-in lineage for development/test replay. It contains only work already
// performed by assembly, has no production or selector-input representation,
// and adds no source reads. Each traced pool has the existing bounded size.
#[cfg(test)]
std::thread_local! {
    static CANDIDATE_ORDERING_TRACE: std::cell::RefCell<Option<Vec<serde_json::Value>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
pub(crate) fn with_candidate_ordering_trace<T>(
    run: impl FnOnce() -> T,
) -> (T, Vec<serde_json::Value>) {
    struct ClearTrace;
    impl Drop for ClearTrace {
        fn drop(&mut self) {
            CANDIDATE_ORDERING_TRACE.with(|trace| {
                trace.borrow_mut().take();
            });
        }
    }
    CANDIDATE_ORDERING_TRACE.with(|trace| {
        assert!(trace.borrow().is_none(), "ordering trace cannot be nested");
        *trace.borrow_mut() = Some(Vec::new());
    });
    let _clear = ClearTrace;
    let result = run();
    let trace = CANDIDATE_ORDERING_TRACE.with(|trace| trace.borrow_mut().take().unwrap());
    (result, trace)
}

#[cfg(test)]
pub(super) fn record_candidate_ordering_trace(
    stage: &str,
    rows: impl FnOnce() -> serde_json::Value,
) {
    CANDIDATE_ORDERING_TRACE.with(|trace| {
        if let Some(trace) = trace.borrow_mut().as_mut() {
            assert!(trace.len() < 16, "bounded ordering trace stage count");
            trace.push(serde_json::json!({"stage": stage, "rows": rows()}));
            check_lineage_bounds(trace);
        }
    });
}

#[cfg(test)]
fn check_lineage_bounds(trace: &[serde_json::Value]) {
    let rows = trace
        .iter()
        .map(|stage| {
            stage["rows"]
                .as_array()
                .map(Vec::len)
                .or_else(|| stage["rows"]["proposals"].as_array().map(Vec::len))
                .unwrap_or(1)
        })
        .sum::<usize>();
    assert!(
        trace.len() <= 16 && rows <= 4096,
        "bounded lineage envelopes/rows"
    );
    assert!(
        serde_json::to_vec(trace).unwrap().len() <= 2 * 1024 * 1024,
        "bounded compact lineage; complete pretty envelope checked by leaf"
    );
}

// Append only values already computed by the production path. The existing
// collector owns these development-only rows; no new observer runtime exists.
#[cfg(test)]
pub(super) fn record_lineage_event(stage: &str, row: impl FnOnce() -> serde_json::Value) {
    CANDIDATE_ORDERING_TRACE.with(|trace| {
        if let Some(trace) = trace.borrow_mut().as_mut() {
            let position = trace.iter().position(|entry| entry["stage"] == stage);
            let position = position.unwrap_or_else(|| {
                assert!(trace.len() < 16, "bounded ordering trace stage count");
                trace.push(serde_json::json!({"stage": stage, "rows": []}));
                trace.len() - 1
            });
            trace[position]["rows"].as_array_mut().unwrap().push(row());
            check_lineage_bounds(trace);
        }
    });
}

// Diagnostic stable ID: production packet key + immutable owner + source span.
// The key distinguishes the associated child fallback from its parent.
#[cfg(test)]
pub(crate) fn lineage_proposal_id(
    key: &str,
    path: &VaultRelativePath,
    hash: &Blake3Hash,
    span: ByteSpan,
) -> Blake3Hash {
    Blake3Hash::digest(serde_json::to_vec(&(key, path, hash, span)).unwrap())
}

#[cfg(test)]
fn packet_lineage_identity(packet: &Packet) -> serde_json::Value {
    serde_json::json!({"key": packet.key, "proposals": packet.passages.iter().map(|p| {
        serde_json::json!({"proposal_id": lineage_proposal_id(&packet.key, &p.locator.path, &p.locator.observed_hash, p.span),
            "owner": p.locator.path, "owner_hash": p.locator.observed_hash, "span": p.span})
    }).collect::<Vec<_>>()})
}

#[cfg(test)]
fn passage_ordering_row(passage: &ContextPassage) -> serde_json::Value {
    serde_json::json!({
        "owner": bundles::owner(passage), "locator": passage.locator,
        "span": passage.span, "text_hash": Blake3Hash::digest(passage.text.as_bytes()),
        "citations": passage.citations,
    })
}

#[cfg(test)]
fn packet_ordering_rows(packets: &[Packet], authenticate: bool) -> Vec<serde_json::Value> {
    packets
        .iter()
        .map(|packet| {
            let passages = packet.passages.iter().map(|passage| {
                let mut row = passage_ordering_row(passage);
                if authenticate {
                    row["authenticated_passage"] = serde_json::json!(passage);
                }
                row
            }).collect::<Vec<_>>();
            serde_json::json!({
                "identity": packet_lineage_identity(packet),
                "key": packet.key, "ordinal": packet.selection_ordinal, "score": packet.score,
                "unit_score": packet.unit_score, "fallback": packet.fallback, "unit_clipped": packet.unit_clipped,
                "fallback_identity": packet.fallback.as_ref().map(|p| serde_json::json!({
                    "proposal_id": lineage_proposal_id(&format!("{}:child", packet.key), &p.locator.path, &p.locator.observed_hash, p.span),
                    "key": format!("{}:child", packet.key), "owner": p.locator.path, "owner_hash": p.locator.observed_hash, "span": p.span,
                })),
                "passages": passages,
                "lexical_candidate": packet.selection.as_ref().map(|candidate| serde_json::json!({
                    "owner_index": candidate.owner_index, "span": candidate.span,
                    "covered_terms": candidate.covered_terms, "local_relevance": candidate.local_relevance,
                    "seed_overlap": candidate.seed_overlap, "clipped": candidate.clipped,
                    "semantic_affinity": candidate.semantic_affinity,
                })),
            })
        })
        .collect()
}

#[cfg(test)]
fn card_ordering_rows(cards: &[SelectionCard]) -> serde_json::Value {
    serde_json::json!(
        cards
            .iter()
            .map(|card| serde_json::json!({
                "id": card.id, "passage": passage_ordering_row(&card.passage),
                "rendered_bytes": card.rendered_bytes,
                "authenticated_card": card,
            }))
            .collect::<Vec<_>>()
    )
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
    reader: &dyn QueryCatalog,
    citation: &CitationRef,
) -> Result<crate::sources::CitationState> {
    use crate::sources::CitationState;
    let (source_id, revision) = match citation {
        CitationRef::Source(r) => (&r.source_id, &r.source_revision),
        CitationRef::Assertion(r) => (&r.source_id, &r.source_revision),
    };
    let source = reader
        .record(source_id)?
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
            .record(&r.evidence_id)?
            .ok_or_else(|| WikiError::invalid("citation evidence absent from pinned projection"))?;
        let assertion = reader.record(&r.assertion_id)?.ok_or_else(|| {
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
// A direct-source overlap component is one emitted quote. Rebind each unique
// source revision to that exact merged range instead of spending the output
// budget repeating every candidate's overlapping quote. Graph evidence keeps
// its assertion-bound references and contributing stances unchanged.
fn compact_direct_citations(passages: &mut [ContextPassage]) {
    for passage in passages {
        if !passage.contributors.is_empty()
            || passage
                .citations
                .iter()
                .any(|c| !matches!(c, CitationRef::Source(_)))
        {
            continue;
        }
        let mut seen = std::collections::BTreeSet::new();
        passage.citations.retain_mut(|citation| {
            let CitationRef::Source(reference) = citation else {
                unreachable!()
            };
            if !seen.insert((
                reference.source_id.clone(),
                reference.source_revision.clone(),
            )) {
                return false;
            }
            reference.span = passage.span;
            reference.quote_hash = Blake3Hash::digest(passage.text.as_bytes());
            true
        });
    }
}

fn render(
    reader: &dyn QueryCatalog,
    scope: ContextScope,
    passages: &[ContextPassage],
    bundles: &[EvidenceBundle],
    navigation: &[NavigationEdge],
) -> Result<(String, usize)> {
    #[cfg(test)]
    CONTEXT_RENDER_COUNTS.with(|counter| -> Result<()> {
        if let Some(mut state) = counter.get() {
            if state.0.calls == state.1 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "experimental context render cap",
                ));
            }
            state.0.calls += 1;
            counter.set(Some(state));
        }
        Ok(())
    })?;
    let mut text = match scope {
        ContextScope::Snapshot => "[context index_snapshot; unverified]\n\n".to_owned(),
        ContextScope::IndexedEvidence => format!(
            "[context indexed_evidence; discovery generation {}; captured sources only; selected canonical bytes verified; global membership, identity uniqueness and completeness not verified]\n\n",
            reader.snapshot().generation
        ),
        ContextScope::IndexedDocuments => format!(
            "[context indexed_documents; discovery generation {}; selected document dependencies verified; global membership, identity uniqueness and completeness not verified]\n\n",
            reader.snapshot().generation
        ),
        _ => String::new(),
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
            .map(|step| -> Result<_> {
                let row = reader
                    .record(&step.assertion.record_id)?
                    .ok_or_else(|| WikiError::invalid("assertion path record missing"))?;
                Ok(format!(
                    "{}: {:?}, status {:?}",
                    step.assertion.record_id, row.eligibility, row.authored_status
                ))
            })
            .collect::<Result<Vec<_>>>()?
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
    #[cfg(test)]
    CONTEXT_RENDER_COUNTS.with(|counter| {
        if let Some(mut state) = counter.get() {
            state.0.bytes += text.len() as u64;
            counter.set(Some(state));
        }
    });
    Ok((text, graph_bytes))
}

#[cfg(test)]
#[derive(Clone, Copy, Default, serde::Serialize)]
pub(crate) struct ContextRenderCountsForTest {
    pub calls: usize,
    pub bytes: u64,
}

#[cfg(test)]
std::thread_local! {
    static CONTEXT_RENDER_COUNTS: std::cell::Cell<Option<(ContextRenderCountsForTest, usize)>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_context_render_counts_for_test<T>(
    max_calls: usize,
    f: impl FnOnce() -> T,
) -> (T, ContextRenderCountsForTest) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CONTEXT_RENDER_COUNTS.with(|counter| counter.set(None));
        }
    }
    CONTEXT_RENDER_COUNTS.with(|counter| {
        assert!(counter.get().is_none(), "render accounting cannot nest");
        counter.set(Some((ContextRenderCountsForTest::default(), max_calls)));
    });
    let _reset = Reset;
    let result = f();
    let counts = CONTEXT_RENDER_COUNTS.with(|counter| counter.get().unwrap().0);
    (result, counts)
}

#[cfg(test)]
pub(crate) fn render_documents_for_test(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    passages: &[ContextPassage],
) -> Result<String> {
    render(reader, request.scope, passages, &[], &[]).map(|(text, _)| text)
}

/// The document allocator's exact admission authority. Callers supply original
/// proposal membership; coalesced output is never used to remove an origin.
pub(super) struct DocumentTrial {
    pub passages: Vec<ContextPassage>,
    pub text: String,
    pub counts: BTreeMap<String, usize>,
    pub reason: Option<&'static str>,
}
pub(super) fn document_trial(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    current: &[ContextPassage],
    additions: &[ContextPassage],
) -> Result<DocumentTrial> {
    if request.target != ContextTarget::Documents
        || request.graph.is_some()
        || !matches!(
            request.documents.mode,
            SearchMode::Lexical | SearchMode::Semantic | SearchMode::Hybrid
        )
        || !matches!(
            request.scope,
            ContextScope::IndexedDocuments | ContextScope::Snapshot
        )
        || current
            .iter()
            .chain(additions)
            .any(|p| !p.contributors.is_empty())
    {
        return Err(WikiError::invalid(
            "document trial requires document-only passages",
        ));
    }
    let mut next = current.to_vec();
    for candidate in additions {
        let mut merged = false;
        for existing in &mut next {
            if bundles::merge(existing, candidate, reader)? {
                merged = true;
                break;
            }
        }
        if !merged {
            next.push(candidate.clone());
        }
    }
    bundles::coalesce(&mut next, reader)?;
    compact_direct_citations(&mut next);
    let (text, _) = render(reader, request.scope, &next, &[], &[])?;
    let mut counts = BTreeMap::new();
    for passage in &next {
        *counts.entry(bundles::owner(passage)).or_insert(0usize) += 1;
    }
    let available_bytes = request
        .budget
        .max_bytes
        .checked_sub(request.budget.instruction_bytes)
        .and_then(|n| n.checked_sub(request.budget.output_bytes))
        .ok_or_else(|| WikiError::invalid("document trial byte reservation exceeds budget"))?;
    let available_tokens = request
        .budget
        .max_tokens
        .checked_sub(request.budget.instruction_tokens)
        .and_then(|n| n.checked_sub(request.budget.output_tokens))
        .ok_or_else(|| WikiError::invalid("document trial token reservation exceeds budget"))?;
    let reason = if next
        .iter()
        .any(|p| p.text.len() > request.documents.limits.excerpt_bytes)
    {
        Some("merged_passage_exceeds_excerpt_bound")
    } else if counts.values().any(|n| *n > 4) {
        Some("document_passage_cap")
    } else if text.len() > available_bytes || text.len().div_ceil(4) > available_tokens {
        Some("required_bundle_or_passage_does_not_fit")
    } else {
        None
    };
    Ok(DocumentTrial {
        passages: next,
        text,
        counts,
        reason,
    })
}

#[cfg(test)]
pub(crate) fn admit_document_for_test(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    current: &[ContextPassage],
    candidate: &ContextPassage,
) -> Result<std::result::Result<(Vec<ContextPassage>, String), &'static str>> {
    let trial = document_trial(reader, request, current, std::slice::from_ref(candidate))?;
    Ok(match trial.reason {
        Some(reason) => Err(reason),
        None => Ok((trial.passages, trial.text)),
    })
}
pub fn assemble(
    reader: &ReaderSnapshot,
    request: &ContextRequest,
    hits: &HitSet,
    graph: Option<&GraphResult>,
) -> Result<ContextDraft> {
    assemble_inner(
        reader,
        Some(reader),
        request,
        hits,
        graph,
        None,
        &ContextSelectionSignals::default(),
        &SelectionAction::Automatic,
    )
}

/// Source-aware context selection is separate from discovery excerpts. All new
/// spans are authenticated against the same pinned owner and final source proof.
pub(crate) fn assemble_for_query(
    reader: &ReaderSnapshot,
    request: &ContextRequest,
    hits: &HitSet,
    graph: Option<&GraphResult>,
    query: &str,
    signals: &ContextSelectionSignals,
    selection: &SelectionAction,
) -> Result<ContextDraft> {
    assemble_inner(
        reader,
        Some(reader),
        request,
        hits,
        graph,
        Some(query),
        signals,
        selection,
    )
}

/// Bounded document context shares selection and packing without a partial
/// projection or strict graph access. IndexedDocuments callers must supply the
/// closed authenticated catalog from the selected verification coordinator.
pub(crate) fn assemble_bounded_documents_for_query(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &HitSet,
    query: &str,
) -> Result<ContextDraft> {
    assemble_bounded_documents_with_selection_for_query(
        reader,
        request,
        hits,
        query,
        &SelectionAction::Automatic,
    )
}

pub(crate) fn assemble_bounded_documents_with_selection_for_query(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &HitSet,
    query: &str,
    selection: &SelectionAction,
) -> Result<ContextDraft> {
    validate_bounded_document_assembly(request, selection)?;
    assemble_inner(
        reader,
        None,
        request,
        hits,
        None,
        Some(query),
        &ContextSelectionSignals::default(),
        selection,
    )
}

fn validate_bounded_document_assembly(
    request: &ContextRequest,
    selection: &SelectionAction,
) -> Result<()> {
    validate_selection_action(request, selection)?;
    if !matches!(
        request.scope,
        ContextScope::Snapshot | ContextScope::IndexedDocuments
    ) || request.target != ContextTarget::Documents
        || (request.scope == ContextScope::Snapshot
            && request.documents.mode != SearchMode::Lexical)
        || request.graph.is_some()
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "bounded document assembly requires lexical snapshot or authenticated indexed-documents context",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn assemble_bounded_documents_with_signals_for_test(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &HitSet,
    query: &str,
    signals: &ContextSelectionSignals,
    selection: &SelectionAction,
) -> Result<ContextDraft> {
    assemble_bounded_documents_with_signals(reader, request, hits, query, signals, selection)
}

/// The indexed semantic coordinator supplies authenticated documents and exact
/// vector/input cues; selection and packing retain the same allocation rules.
pub(crate) fn assemble_bounded_documents_with_signals(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &HitSet,
    query: &str,
    signals: &ContextSelectionSignals,
    selection: &SelectionAction,
) -> Result<ContextDraft> {
    validate_bounded_document_assembly(request, selection)?;
    assemble_inner(
        reader,
        None,
        request,
        hits,
        None,
        Some(query),
        signals,
        selection,
    )
}

fn assemble_inner(
    reader: &dyn QueryCatalog,
    strict_reader: Option<&ReaderSnapshot>,
    request: &ContextRequest,
    hits: &HitSet,
    graph: Option<&GraphResult>,
    query: Option<&str>,
    signals: &ContextSelectionSignals,
    selection_action: &SelectionAction,
) -> Result<ContextDraft> {
    assemble_inner_with_evidence(
        reader,
        strict_reader,
        request,
        hits,
        graph,
        query,
        signals,
        selection_action,
        None,
    )
}

pub(crate) fn assemble_bounded_documents_with_evidence(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &HitSet,
    query: &str,
    signals: &ContextSelectionSignals,
    evidence_sets: Option<super::context_evidence::Inputs>,
) -> Result<ContextDraft> {
    validate_bounded_document_assembly(request, &SelectionAction::Automatic)?;
    assemble_inner_with_evidence(
        reader,
        None,
        request,
        hits,
        None,
        Some(query),
        signals,
        &SelectionAction::Automatic,
        evidence_sets,
    )
}

fn assemble_inner_with_evidence(
    reader: &dyn QueryCatalog,
    strict_reader: Option<&ReaderSnapshot>,
    request: &ContextRequest,
    hits: &HitSet,
    graph: Option<&GraphResult>,
    query: Option<&str>,
    signals: &ContextSelectionSignals,
    selection_action: &SelectionAction,
    evidence_sets: Option<super::context_evidence::Inputs>,
) -> Result<ContextDraft> {
    let request = normalize_request(request)?;
    if request.scope == ContextScope::IndexedEvidence
        || (request.scope == ContextScope::IndexedDocuments && strict_reader.is_some())
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "indexed scopes require their selected canonical verification coordinator",
        ));
    }
    let request = &request;
    validate_selection_action(request, selection_action)?;
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
    let dependency_fingerprint = reader.dependency_fingerprint()?;
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
    let document_owners = if request.target != ContextTarget::Graph {
        hits.hits
            .iter()
            .map(|hit| document_owner(reader, hit, request))
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    let mut packets = Vec::new();
    let mut omissions = Vec::new();
    let mut direct: BTreeMap<String, usize> = BTreeMap::new();
    let mut graph_ranks: BTreeMap<String, usize> = BTreeMap::new();
    let source_aware = query.is_some() && request.documents.mode != SearchMode::Literal;
    let mut term_weights = Vec::new();
    let mut selection_warnings = signals.warnings.clone();
    if request.target != ContextTarget::Graph && source_aware {
        let mut owners = Vec::new();
        for (i, hit) in hits.hits.iter().enumerate() {
            // Authenticate identity, hash, filters and context eligibility before
            // source-wide expansion. An invalid supplied hit must not gain trust.
            if document_passage(
                reader,
                &document_owners[i],
                hit,
                &hit.excerpt,
                request,
                i + 1,
            )?
            .is_none()
            {
                omissions.push(ContextOmission {
                    record_id: hit.locator.record.as_ref().map(|r| r.record_id.clone()),
                    path: Some(hit.locator.path.clone()),
                    reason: "discovery_only_or_empty".into(),
                    count: 1,
                });
                continue;
            }
            let document = &document_owners[i].document;
            let anchors = std::iter::once(&hit.excerpt)
                .chain(hit.secondary_excerpts.iter().take(1))
                .map(|e| e.span)
                .collect::<Vec<_>>();
            owners.push((i, document, anchors));
        }
        let documents = owners
            .iter()
            .map(
                |(i, document, anchors)| super::context_selection::SelectionDocument {
                    owner_index: *i,
                    document,
                    seed_spans: anchors,
                },
            )
            .collect::<Vec<_>>();
        if signals.semantic.is_empty() || !signals.semantic_complete {
            let selection = super::context_selection::select_candidates_with_semantics(
                reader,
                query.expect("source-aware query"),
                &documents,
                request.documents.limits.excerpt_bytes,
                &[],
            )?;
            term_weights = selection.term_weights;
            for omission in selection.omissions {
                let hit = &hits.hits[omission.owner_index];
                omissions.push(ContextOmission {
                    record_id: hit.locator.record.as_ref().map(|r| r.record_id.clone()),
                    path: Some(hit.locator.path.clone()),
                    reason: omission.reason.into(),
                    count: 1,
                });
            }
            selection_warnings.push(format!(
            "source-aware context inspected {} bytes in {} blocks; query overlap guides passage selection, not answer completeness",
            selection.scanned_bytes, selection.scanned_blocks,
        ));
            for (selection_ordinal, candidate) in selection.candidates.into_iter().enumerate() {
                let hit = &hits.hits[candidate.owner_index];
                let excerpt = SearchExcerpt {
                    text: String::new(),
                    span: candidate.span,
                    matched_spans: vec![],
                    label: hit.excerpt.label,
                    citation: None,
                };
                if let Some(p) = document_passage(
                    reader,
                    &document_owners[candidate.owner_index],
                    hit,
                    &excerpt,
                    request,
                    candidate.owner_index + 1,
                )? {
                    direct
                        .entry(bundles::owner(&p))
                        .and_modify(|r| *r = (*r).min(candidate.owner_index + 1))
                        .or_insert(candidate.owner_index + 1);
                    packets.push(Packet {
                        key: format!(
                            "document:{}:{:020}:{:020}",
                            p.locator.path,
                            p.span.start(),
                            p.span.end()
                        ),
                        passages: vec![p],
                        bundle: None,
                        navigation: None,
                        score: 0.,
                        selection_ordinal: Some(selection_ordinal),
                        selection: Some(candidate),
                        unit_score: None,
                        unit_origin: None,
                        fallback: None,
                        unit_clipped: false,
                    });
                }
            }
        } else {
            let unit_documents = owners
                .iter()
                .map(|(index, document, _)| super::context_units::UnitDocument {
                    owner_index: *index,
                    document,
                })
                .collect::<Vec<_>>();
            let selected = super::context_units::select_units(
                reader,
                query.expect("source-aware query"),
                &unit_documents,
                &signals.semantic,
                request.documents.limits.excerpt_bytes,
                request.documents.limits.candidates,
            )?;
            #[cfg(test)]
            record_candidate_ordering_trace("unit_lineage", || {
                serde_json::json!(selected.candidates.iter().map(|candidate| {
                    let owner = &document_owners[candidate.owner_index].document;
                    serde_json::json!({
                        "owner_index": candidate.owner_index,
                        "path": owner.path, "hash": owner.hash,
                        "parent_span": candidate.parent_span, "child_span": candidate.child_span,
                        "score": candidate.score, "rank_contributions": candidate.rank_contributions,
                        "clipped": candidate.clipped,
                    })
                }).collect::<Vec<_>>())
            });
            for omission in selected.omissions {
                let hit = &hits.hits[omission.owner_index];
                omissions.push(ContextOmission {
                    record_id: hit
                        .locator
                        .record
                        .as_ref()
                        .map(|record| record.record_id.clone()),
                    path: Some(hit.locator.path.clone()),
                    reason: omission.reason.into(),
                    count: 1,
                });
            }
            selection_warnings.push(format!("context retained ranked evidence units and mapped unique structural parents over {} source bytes and {} blocks; retrieval rank does not establish answer completeness", selected.scanned_bytes, selected.scanned_blocks));
            for (selection_ordinal, candidate) in selected.candidates.into_iter().enumerate() {
                let hit = &hits.hits[candidate.owner_index];
                let passage = |span| -> Result<Option<ContextPassage>> {
                    let excerpt = SearchExcerpt {
                        text: String::new(),
                        span,
                        matched_spans: vec![],
                        label: hit.excerpt.label,
                        citation: None,
                    };
                    let mut passage = document_passage(
                        reader,
                        &document_owners[candidate.owner_index],
                        hit,
                        &excerpt,
                        request,
                        candidate.owner_index + 1,
                    )?;
                    if let Some(passage) = &mut passage {
                        passage
                            .rank_contributions
                            .extend(candidate.rank_contributions.clone());
                    }
                    Ok(passage)
                };
                if let Some(parent) = passage(candidate.parent_span)? {
                    let fallback = if candidate.child_span != candidate.parent_span {
                        passage(candidate.child_span)?
                    } else {
                        None
                    };
                    direct
                        .entry(bundles::owner(&parent))
                        .and_modify(|rank| *rank = (*rank).min(candidate.owner_index + 1))
                        .or_insert(candidate.owner_index + 1);
                    packets.push(Packet {
                        key: format!(
                            "unit:{}:{:020}:{:020}",
                            parent.locator.path,
                            parent.span.start(),
                            parent.span.end()
                        ),
                        passages: vec![parent],
                        bundle: None,
                        navigation: None,
                        score: 0.0,
                        selection_ordinal: Some(selection_ordinal),
                        selection: None,
                        unit_score: Some(candidate.score),
                        unit_origin: Some((candidate.origin_span, candidate.origin_cosine)),
                        fallback,
                        unit_clipped: candidate.clipped,
                    });
                }
            }
        }
    } else if request.target != ContextTarget::Graph {
        for (i, hit) in hits.hits.iter().enumerate() {
            for (passage_index, excerpt) in std::iter::once(&hit.excerpt)
                .chain(hit.secondary_excerpts.iter().take(1))
                .enumerate()
            {
                if let Some(p) =
                    document_passage(reader, &document_owners[i], hit, excerpt, request, i + 1)?
                {
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
                        selection_ordinal: None,
                        selection: None,
                        unit_score: None,
                        unit_origin: None,
                        fallback: None,
                        unit_clipped: false,
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
        let reader = strict_reader.ok_or_else(|| {
            WikiError::new(
                ErrorCode::Usage,
                "graph context requires a strict catalog reader",
            )
        })?;
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
                    selection_ordinal: None,
                    selection: None,
                    unit_score: None,
                    unit_origin: None,
                    fallback: None,
                    unit_clipped: false,
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
                    selection_ordinal: None,
                    selection: None,
                    unit_score: None,
                    unit_origin: None,
                    fallback: None,
                    unit_clipped: false,
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
            if packet.unit_score.is_some() {
                for rank in ranks {
                    if !p.rank_contributions.contains(&rank) {
                        p.rank_contributions.push(rank);
                    }
                }
            } else {
                p.rank_contributions = ranks;
            }
        }
        if let Some(bundle) = &mut packet.bundle {
            bundle.rank_contributions = best;
        }
    }
    pack(
        &OwnerCatalog {
            reader,
            owners: &document_owners,
        },
        request,
        PackingInput {
            packets,
            omissions,
            term_weights,
            selection_warnings,
            source_aware,
            query,
            signals,
            selection_action,
            hits,
            graph,
            dependency_fingerprint,
            evidence_sets,
        },
    )
}

pub(super) struct PackingInput<'a> {
    pub packets: Vec<Packet>,
    pub omissions: Vec<ContextOmission>,
    pub term_weights: Vec<u64>,
    pub selection_warnings: Vec<String>,
    pub source_aware: bool,
    pub query: Option<&'a str>,
    pub signals: &'a ContextSelectionSignals,
    pub selection_action: &'a SelectionAction,
    pub hits: &'a HitSet,
    pub graph: Option<&'a GraphResult>,
    pub dependency_fingerprint: Blake3Hash,
    pub evidence_sets: Option<super::context_evidence::Inputs>,
}

/// Existing adaptive packet priority, shared with the bounded diagnostic.
pub(super) fn packet_utility(
    packet: &Packet,
    covered_terms: &[bool],
    term_weights: &[u64],
    total_weight: f64,
    rendered_cost: usize,
    best_affinity: f64,
) -> f64 {
    if let Some(score) = packet.unit_score {
        return score;
    }
    let Some(candidate) = &packet.selection else {
        return packet.score;
    };
    let novel = candidate
        .covered_terms
        .iter()
        .filter(|&&t| !covered_terms[t])
        .map(|&t| term_weights[t])
        .sum::<u64>() as f64
        / total_weight;
    let local = candidate.local_relevance as f64 / total_weight;
    // Include the actual standalone reference/header cost. Tiny
    // fragments must not gain priority merely by being short. Final
    // admission still measures the fully coalesced rendered result.
    let density = candidate.span.len() as f64 / rendered_cost.max(1) as f64;
    let relevance = if let Some(affinity) = candidate.semantic_affinity {
        // A monotone preference within this cached query/space, not a
        // calibrated relevance probability. Lexical coverage breaks
        // coarse unit ties without overruling semantic location.
        let relative = if best_affinity > 0.0 {
            affinity.max(0.0) / best_affinity
        } else {
            0.0
        };
        (0.05 + relative.powi(4)) * (1.0 + 0.25 * novel + 0.5 * local)
    } else {
        (0.25 + 2.0 * novel + 0.5 * local + if candidate.seed_overlap { 0.05 } else { 0.0 })
            * if best_affinity > 0.0 { 0.15 } else { 1.0 }
    };
    packet.score * relevance * density * if candidate.clipped { 0.7 } else { 1.0 }
}

pub(super) fn pack(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    input: PackingInput<'_>,
) -> Result<ContextDraft> {
    let PackingInput {
        mut packets,
        mut omissions,
        term_weights,
        mut selection_warnings,
        source_aware,
        query,
        signals,
        selection_action,
        hits,
        graph,
        dependency_fingerprint,
        evidence_sets,
    } = input;
    let preserve_selection_order = !matches!(selection_action, SelectionAction::Automatic);
    #[cfg(test)]
    record_candidate_ordering_trace("candidate_pool", || {
        let mut legacy_order = (0..packets.len()).collect::<Vec<_>>();
        legacy_order.sort_by(|&a, &b| {
            packets[b]
                .score
                .total_cmp(&packets[a].score)
                .then(packets[a].key.cmp(&packets[b].key))
        });
        serde_json::json!({
            "proposals": packet_ordering_rows(&packets, true),
            "legacy_order": legacy_order.into_iter().map(|index| &packets[index].key).collect::<Vec<_>>(),
            "preserve_selection_order": preserve_selection_order,
            "term_weights": term_weights,
        })
    });
    sort_packets(&mut packets, preserve_selection_order);
    #[cfg(test)]
    record_candidate_ordering_trace("sorted_packets", || {
        serde_json::json!(packet_ordering_rows(&packets, false))
    });
    let reserved_bytes = request.budget.instruction_bytes + request.budget.output_bytes;
    let reserved_tokens = request.budget.instruction_tokens + request.budget.output_tokens;
    let available_bytes = request.budget.max_bytes - reserved_bytes;
    let available_tokens = request.budget.max_tokens - reserved_tokens;
    let mut passages: Vec<ContextPassage> = Vec::new();
    #[cfg(test)]
    let mut accepted_lineage_ids = Vec::new();
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
    if matches!(selection_action, SelectionAction::Prepare | SelectionAction::Apply(_)) {
        let cards = packets
            .iter()
            .enumerate()
            .map(|(index, packet)| -> Result<_> {
                let passage = packet
                    .passages
                    .first()
                    .ok_or_else(|| WikiError::invalid("selection candidate has no passage"))?;
                if packet.passages.len() != 1
                    || packet.bundle.is_some()
                    || packet.navigation.is_some()
                {
                    return Err(WikiError::invalid(
                        "selection candidate is not a document passage",
                    ));
                }
                let title = reader
                    .document(&passage.locator.path)?
                    .ok_or_else(|| WikiError::invalid("selection owner unavailable"))?
                    .title;
                let (rendered, _) = render(reader, request.scope, &packet.passages, &[], &[])?;
                Ok(SelectionCard {
                    id: format!("c{index:04}"),
                    title,
                    passage: passage.clone(),
                    child_span: None,
                    rendered_bytes: rendered.len(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let cards = context_selection_packet::interleave_by_owner(cards);
        #[cfg(test)]
        record_candidate_ordering_trace("pre_card_cap", || card_ordering_rows(&cards));
        let packet = context_selection_packet::build_packet(
            serde_json::json!({
                "query": query.expect("selection requires query"), "request": request,
                "snapshot": reader.snapshot(), "dependency_fingerprint": dependency_fingerprint,
                "max_passages_per_owner": 4,
            }),
            cards,
        )?;
        #[cfg(test)]
        record_candidate_ordering_trace("displayed_cards", || {
            serde_json::json!({
                "fingerprint": packet.fingerprint,
                "candidate_count": packet.candidate_count,
                "omitted_candidates": packet.omitted_candidates,
                "input_bytes": packet.input_bytes,
                "cards": card_ordering_rows(&packet.cards),
            })
        });
        if matches!(selection_action, SelectionAction::Prepare) {
            let mut warnings = hits.warnings.clone();
            warnings.extend(signals.warnings.iter().cloned());
            warnings.extend(selection_warnings);
            warnings.push("candidate packet for one host selector invocation; not final answer context; only selector_input is the model task, and its byte/4 count excludes unavailable harness context and model reasoning".into());
            let truncated =
                hits.truncated || !omissions.is_empty() || packet.omitted_candidates > 0;
            return Ok(ContextDraft {
                text: String::new(),
                passages: vec![],
                bundles: vec![],
                omissions: aggregate_omissions(omissions),
                usage: ContextUsage {
                    rendered_bytes: 0,
                    estimated_tokens: 0,
                    token_accounting: TokenAccounting::EstimatedUtf8BytesDiv4Ceil,
                    reserved_bytes,
                    reserved_tokens,
                    graph_bytes: 0,
                    graph_estimated_tokens: 0,
                    verification_bytes: 0,
                    verification_files: 0,
                    verification_entries: 0,
                },
                snapshot: reader.snapshot().clone(),
                dependency_fingerprint,
                truncated,
                warnings,
                selection_packet: Some(packet),
            });
        }
        if let SelectionAction::Apply(reply) = selection_action {
            let ids = context_selection_packet::validate_reply(&packet, reply)?;
            let mut by_id = packets
                .into_iter()
                .enumerate()
                .map(|(index, packet)| (format!("c{index:04}"), packet))
                .collect::<BTreeMap<_, _>>();
            packets = ids
                .iter()
                .enumerate()
                .map(|(rank, id)| {
                    let mut packet = by_id.remove(id).expect("validated packet ID");
                    packet.fallback = None;
                    packet.unit_score = Some(1.0 / (60.0 + (rank + 1) as f64));
                    for passage in packet.passages.iter_mut().chain(packet.fallback.iter_mut()) {
                        passage.rank_contributions.push(RankContribution {
                            channel: "host_selection".into(),
                            rank: rank + 1,
                            score: None,
                        });
                    }
                    packet
                })
                .collect();
            selection_warnings.push(format!("host selected {} of {} supplied candidates; model selection is not proof of completeness; local citations, freshness and final packing remain authoritative", ids.len(), packet.candidate_count));
            if !by_id.is_empty() || packet.omitted_candidates > 0 {
                omissions.push(ContextOmission {
                    record_id: None,
                    path: None,
                    reason: "not_selected_by_host_or_packet_input_cap".into(),
                    count: by_id.len(),
                });
            }
        }
    }
    if let Some(evidence_sets) = evidence_sets {
        if signals.semantic_complete && packets.iter().all(|p| p.unit_origin.is_some()) {
            let arm = evidence_sets.arm;
            match super::context_evidence::allocate(
                reader,
                request,
                &packets,
                query.expect("evidence-set requires source-aware query"),
                evidence_sets,
            )? {
                super::evidence_set_selection::Outcome::Fallback(reason) => {
                    selection_warnings.push(format!(
                        "evidence-set {:?} fallback {:?}; existing allocation retained",
                        arm, reason
                    ));
                }
                super::evidence_set_selection::Outcome::Selected {
                    choices,
                    state,
                    statistics,
                    ..
                } => {
                    if let Some(state) = state {
                        passages = state.passages;
                        text = state.text;
                    }
                    for (origin, packet) in packets.iter().enumerate() {
                        if let Some(choice) = choices.iter().find(|choice| choice.origin == origin)
                        {
                            if choice.variant == super::evidence_set_selection::Variant::Core
                                && (packet.unit_clipped || packet.fallback.is_some())
                            {
                                selection_warnings.push("a bounded evidence-unit child was selected because its complete structural parent did not fit; inspect its cited source for omitted text".into());
                            }
                        } else {
                            omissions.push(ContextOmission {
                                record_id: packet.passages[0]
                                    .locator
                                    .record
                                    .as_ref()
                                    .map(|r| r.record_id.clone()),
                                path: Some(packet.passages[0].locator.path.clone()),
                                reason: "not_selected_by_evidence_set".into(),
                                count: 1,
                            });
                        }
                    }
                    selection_warnings.push(format!("evidence-set {:?}: {} origins, {} exact rendering trials, representation coverage {:.6}; selection is not proof of answer completeness",
                        arm, choices.len(), statistics.trials(), statistics.objective_value));
                    packets.clear();
                }
            }
        }
    }
    let mut covered_terms = vec![false; term_weights.len()];
    let total_weight = term_weights.iter().copied().sum::<u64>().max(1) as f64;
    let rendered_costs = packets
        .iter()
        .filter(|packet| packet.selection.is_some())
        .map(|packet| -> Result<_> {
            let (rendered, _) = render(reader, request.scope, &packet.passages, &[], &[])?;
            Ok((packet.key.clone(), rendered.len().max(1)))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let best_affinity = packets
        .iter()
        .filter_map(|packet| {
            packet
                .selection
                .as_ref()
                .and_then(|candidate| candidate.semantic_affinity)
        })
        .fold(0.0f64, f64::max);
    while !packets.is_empty() {
        let utility = |packet: &Packet| {
            packet_utility(
                packet,
                &covered_terms,
                &term_weights,
                total_weight,
                rendered_costs.get(&packet.key).copied().unwrap_or(1),
                best_affinity,
            )
        };
        let best = (0..packets.len())
            .max_by(|&a, &b| {
                utility(&packets[a])
                    .total_cmp(&utility(&packets[b]))
                    .then_with(|| packets[b].key.cmp(&packets[a].key))
            })
            .expect("nonempty packets");
        let mut packet = packets.remove(best);
        #[cfg(test)]
        let lineage_identity = packet_lineage_identity(&packet);
        if (packet.selection.is_some() || packet.unit_score.is_some())
            && packet.passages.iter().all(|p| {
                passages.iter().any(|old| {
                    old.locator.path == p.locator.path
                        && old.span.start() <= p.span.start()
                        && old.span.end() >= p.span.end()
                })
            })
        {
            #[cfg(test)]
            record_lineage_event("packing_trials", || {
                serde_json::json!({
                    "identity": lineage_identity, "outcome": "contained_in_accepted_passage",
                    "accepted_proposal_ids_before": accepted_lineage_ids,
                    "actual_render_cost": null, "accepted": passages.iter().map(passage_ordering_row).collect::<Vec<_>>()
                })
            });
            continue;
        }
        let id = packet
            .bundle
            .as_ref()
            .map(|b| b.assertion.record_id.clone());
        let (
            next,
            next_bundles,
            next_navigation,
            rendered,
            next_graph,
            _bytes,
            _tokens,
            _graph_tokens,
            _passage_cap,
            _counts,
            reason,
        ) = if source_aware
            && request.scope == ContextScope::IndexedDocuments
            && request.target == ContextTarget::Documents
            && packet.bundle.is_none()
            && packet.navigation.is_none()
            && bundles.is_empty()
            && navigation.is_empty()
            && (packet.selection.is_some() || packet.unit_score.is_some())
        {
            let trial = document_trial(reader, request, &passages, &packet.passages)?;
            let bytes = trial.text.len();
            (
                trial.passages,
                Vec::new(),
                Vec::new(),
                trial.text,
                0,
                bytes,
                bytes.div_ceil(4),
                0,
                4,
                trial.counts,
                trial.reason,
            )
        } else {
            let mut next = passages.clone();
            let mut next_bundles = bundles.clone();
            let mut next_navigation = navigation.clone();
            if let Some(edge) = &packet.navigation {
                next_navigation.push(edge.clone());
            }
            let mut indices = Vec::new();
            for p in packet.passages.iter().cloned() {
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
            if source_aware {
                compact_direct_citations(&mut next);
            }
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
            if let Some(mut bundle) = packet.bundle.clone() {
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
            let passage_cap = if source_aware && request.target != ContextTarget::Graph {
                4
            } else {
                2
            };
            let reason = if (packet.selection.is_some() || packet.unit_score.is_some())
                && next.iter().any(|p| {
                    p.contributors.is_empty()
                        && p.text.len() > request.documents.limits.excerpt_bytes
                }) {
                Some("merged_passage_exceeds_excerpt_bound")
            } else if counts.values().any(|n| *n > passage_cap) {
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
            (
                next,
                next_bundles,
                next_navigation,
                rendered,
                next_graph,
                bytes,
                tokens,
                graph_tokens,
                passage_cap,
                counts,
                reason,
            )
        };
        #[cfg(test)]
        record_lineage_event("packing_trials", || {
            serde_json::json!({
                "identity": lineage_identity, "actual_render_cost": {"bytes": _bytes, "estimated_tokens": _tokens,
                    "graph_bytes": next_graph, "graph_estimated_tokens": _graph_tokens},
                "accepted_proposal_ids_before": accepted_lineage_ids,
                "caps": {"bytes": available_bytes, "estimated_tokens": available_tokens,
                    "excerpt_bytes": request.documents.limits.excerpt_bytes, "per_owner": _passage_cap},
                "owner_counts": _counts.iter().map(|(owner, count)| serde_json::json!({"owner": owner, "count": count})).collect::<Vec<_>>(),
                "cap_reason": reason, "outcome": if reason.is_none() {"accepted"} else if packet.fallback.is_some() {"enqueue_child_fallback"} else {"omitted"},
                "fallback_identity": packet.fallback.as_ref().map(|p| serde_json::json!({
                    "proposal_id": lineage_proposal_id(&format!("{}:child", packet.key), &p.locator.path, &p.locator.observed_hash, p.span),
                    "key": format!("{}:child", packet.key), "owner": p.locator.path, "owner_hash": p.locator.observed_hash, "span": p.span})),
                "coalesced_trial": next.iter().map(passage_ordering_row).collect::<Vec<_>>()
            })
        });
        if let Some(reason) = reason {
            if let Some(child) = packet.fallback.take() {
                // Keep one child fallback associated with its ranked unit;
                // it is not another independently voting retrieval result.
                packets.push(Packet {
                    passages: vec![child],
                    bundle: None,
                    navigation: None,
                    key: format!("{}:child", packet.key),
                    score: packet.score,
                    selection_ordinal: packet.selection_ordinal,
                    selection: None,
                    unit_score: packet.unit_score,
                    unit_origin: packet.unit_origin,
                    fallback: None,
                    unit_clipped: true,
                });
                continue;
            }
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
        if let Some(candidate) = &packet.selection {
            for &term in &candidate.covered_terms {
                covered_terms[term] = true;
            }
            if candidate.clipped {
                selection_warnings.push("a bounded source window was selected instead of a complete structural block; inspect its cited source for omitted text".into());
            }
        }
        if packet.unit_clipped {
            selection_warnings.push("a bounded evidence-unit child was selected because its complete structural parent did not fit; inspect its cited source for omitted text".into());
        }
        #[cfg(test)]
        accepted_lineage_ids.extend(
            lineage_identity["proposals"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["proposal_id"].clone()),
        );
        passages = next;
        bundles = next_bundles;
        navigation = next_navigation;
        text = rendered;
        graph_bytes = next_graph;
    }
    #[cfg(test)]
    record_lineage_event("final_accepted", || {
        serde_json::json!({
            "accepted_proposal_ids": accepted_lineage_ids,
            "passages": passages.iter().map(passage_ordering_row).collect::<Vec<_>>(),
        })
    });
    let mut warnings = vec![
        "tokens are UTF-8 byte estimates (ceil(bytes/4)); no exact tokenizer accounting".into(),
    ];
    warnings.extend(selection_warnings);
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
        omissions: aggregate_omissions(omissions),
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
        selection_packet: None,
    })
}

fn aggregate_omissions(omissions: Vec<ContextOmission>) -> Vec<ContextOmission> {
    let mut indices: BTreeMap<_, usize> = BTreeMap::new();
    let mut aggregated: Vec<ContextOmission> = Vec::new();
    for omission in omissions {
        let key = (
            omission.record_id.clone(),
            omission.path.clone(),
            omission.reason.clone(),
        );
        if let Some(&index) = indices.get(&key) {
            aggregated[index].count += omission.count;
        } else {
            indices.insert(key, aggregated.len());
            aggregated.push(omission);
        }
    }
    aggregated
}

#[cfg(test)]
#[path = "snapshot_context_tests.rs"]
mod snapshot_context_tests;

//! Shared retrieval contracts, owned by the orchestrator.
use crate::{catalog::SnapshotVerification, domain::*};
use serde::{Deserialize, Serialize, ser::SerializeStruct};

/// Application input bound, independent of a space's embedding input budget.
pub const MAX_QUERY_BYTES: usize = 16 * 1024;
/// Maximum whitespace-separated phrases in a lexical OR expression.
pub const MAX_LEXICAL_TERMS: usize = 256;
/// Separate work bound after tokenization/deduplication during passage selection.
pub(crate) const MAX_CONTEXT_QUERY_TERMS: usize = 256;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Literal,
    #[default]
    Lexical,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchFilters {
    pub kinds: Vec<RecordKind>,
    pub tags: Vec<String>,
    pub source_ids: Vec<RecordId>,
    pub path_prefix: Option<String>,
    pub authored_statuses: Vec<String>,
    pub include_proposed: bool,
    pub include_historical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchLimits {
    pub hits: usize,
    pub candidates: usize,
    pub excerpt_bytes: usize,
}
impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            hits: 10,
            candidates: 80,
            excerpt_bytes: 240,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueryPlan {
    pub mode: SearchMode,
    pub filters: SearchFilters,
    pub limits: SearchLimits,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcerptLabel {
    NoteText,
    CapturedSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchExcerpt {
    pub text: String,
    pub span: ByteSpan,
    pub matched_spans: Vec<ByteSpan>,
    pub label: ExcerptLabel,
    pub citation: Option<CitationRef>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalReason {
    ExactId,
    ExactTitle,
    ExactAlias,
    Lexical,
    Literal,
    Identity,
    Semantic,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankContribution {
    pub channel: String,
    pub rank: usize,
    pub score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub locator: DocumentLocator,
    pub title: String,
    pub kind: Option<RecordKind>,
    pub authored_status: Option<String>,
    pub eligibility: Eligibility,
    pub identity_eligibility: Option<Eligibility>,
    pub excerpt: SearchExcerpt,
    /// At most one additional focused passage from the same dense owner.
    pub secondary_excerpts: Vec<SearchExcerpt>,
    pub reasons: Vec<RetrievalReason>,
    pub rank_contributions: Vec<RankContribution>,
    pub source_id: Option<RecordId>,
    pub owner_revision: Option<RecordId>,
}
impl Serialize for SearchHit {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut value = s.serialize_struct("SearchHit", 14)?;
        value.serialize_field("record_ref", &self.locator.record)?;
        value.serialize_field("path", &self.locator.path)?;
        value.serialize_field("locator", &self.locator)?;
        value.serialize_field("title", &self.title)?;
        value.serialize_field("kind", &self.kind)?;
        value.serialize_field("authored_status", &self.authored_status)?;
        value.serialize_field("eligibility", &self.eligibility)?;
        value.serialize_field("identity_eligibility", &self.identity_eligibility)?;
        value.serialize_field("excerpt", &self.excerpt)?;
        value.serialize_field("secondary_excerpts", &self.secondary_excerpts)?;
        value.serialize_field("reasons", &self.reasons)?;
        value.serialize_field("rank_contributions", &self.rank_contributions)?;
        value.serialize_field("source_id", &self.source_id)?;
        value.serialize_field("owner_revision", &self.owner_revision)?;
        value.end()
    }
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HitSet {
    pub network_used: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<crate::graph::GraphResult>,
    pub hits: Vec<SearchHit>,
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub candidate_count: usize,
    pub omitted_candidates: usize,
    pub snapshot: ReadSnapshot,
    pub verification: SnapshotVerification,
    pub dependency_fingerprint: Blake3Hash,
    pub warnings: Vec<String>,
}

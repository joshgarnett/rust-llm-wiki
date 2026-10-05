//! Canonical graph discovery. Traversal never constructs a new assertion.
use crate::{
    catalog::SnapshotVerification,
    domain::*,
    retrieval::{RankContribution, SearchFilters},
    sources::EvidenceStance,
};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphStrategy {
    Entity,
    Relationship,
    #[default]
    Combined,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphSeedMode {
    #[default]
    Lexical,
    Semantic,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphLimits {
    pub candidates: usize,
    pub seeds: usize,
    pub depth: usize,
    pub incident_per_seed: usize,
    pub assertions: usize,
    pub hits: usize,
    pub excerpt_bytes: usize,
    pub support_per_assertion: usize,
    pub contradictions_per_assertion: usize,
}
impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            candidates: 80,
            seeds: 12,
            depth: 1,
            incident_per_seed: 16,
            assertions: 128,
            hits: 10,
            excerpt_bytes: 240,
            support_per_assertion: 2,
            contradictions_per_assertion: 1,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphPlan {
    pub strategy: GraphStrategy,
    pub seed_mode: GraphSeedMode,
    pub filters: SearchFilters,
    pub limits: GraphLimits,
    pub include_navigation: bool,
    pub cursor: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphSeed {
    pub record_ref: RecordRef,
    pub locator: DocumentLocator,
    pub title: String,
    pub kind: RecordKind,
    pub eligibility: Eligibility,
    pub identity_eligibility: Option<Eligibility>,
    pub rank_contributions: Vec<RankContribution>,
    pub rrf_score: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphEntity {
    pub record_ref: RecordRef,
    pub locator: DocumentLocator,
    pub title: String,
    pub entity_type: String,
    pub aliases: Vec<String>,
    pub eligibility: Eligibility,
    pub identity_eligibility: Option<Eligibility>,
    pub description_eligibility: Option<Eligibility>,
    pub description: Option<String>,
    pub description_truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphObject {
    Entity { record_ref: RecordRef },
    Literal { literal_type: String, value: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphQualifiers {
    pub negated: bool,
    pub modality: String,
    pub property: Option<String>,
    pub unit: Option<String>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraversalDirection {
    Outgoing,
    Incoming,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphPathStep {
    pub assertion: RecordRef,
    pub subject: RecordRef,
    pub predicate: String,
    pub object: GraphObject,
    pub qualifiers: GraphQualifiers,
    pub traversal: TraversalDirection,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphEvidence {
    pub record_ref: RecordRef,
    pub locator: DocumentLocator,
    pub stance: EvidenceStance,
    pub eligibility: Eligibility,
    pub authored_status: Option<String>,
    pub source: SourceSpanRef,
    pub citation: Option<CitationRef>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphAssertion {
    pub record_ref: RecordRef,
    pub locator: DocumentLocator,
    pub title: String,
    pub subject: RecordRef,
    pub predicate: String,
    pub object: GraphObject,
    pub qualifiers: GraphQualifiers,
    pub authored_status: Option<String>,
    pub eligibility: Eligibility,
    pub disputed: bool,
    pub seed_ids: Vec<RecordId>,
    pub path: Vec<GraphPathStep>,
    pub hop: usize,
    pub direct_seed: bool,
    /// Position in the fused, tiered seed order; independent of per-channel ranks.
    pub direct_seed_rank: Option<usize>,
    pub rank_contributions: Vec<RankContribution>,
    pub rrf_score: f64,
    pub support: Vec<GraphEvidence>,
    pub contradictions: Vec<GraphEvidence>,
    /// Current accepted assertions with matching qualifiers and opposite negation.
    pub opposing_assertions: Vec<RecordRef>,
    pub omitted_opposing_assertions: usize,
    pub omitted_support: usize,
    pub omitted_contradictions: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavigationReason {
    PageLink,
    Provenance,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NavigationEdge {
    pub from: DocumentLocator,
    pub to: DocumentLocator,
    pub reason: NavigationReason,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GraphCoverage {
    pub entity_candidates: usize,
    pub assertion_candidates: usize,
    pub omitted_candidates: usize,
    pub omitted_seeds: usize,
    pub visited_entities: usize,
    pub visited_assertions: usize,
    pub omitted_incident_assertions: usize,
    pub omitted_assertions: usize,
    pub omitted_navigation: usize,
    pub depth_limited: bool,
    /// Sentinel-bounded discovery cannot report exact total omissions.
    pub omissions_are_lower_bounds: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphResult {
    pub network_used: bool,
    pub seeds: Vec<GraphSeed>,
    /// Seeds and endpoints of displayed assertions only; auxiliary identity data.
    pub entities: Vec<GraphEntity>,
    pub assertions: Vec<GraphAssertion>,
    pub navigation: Vec<NavigationEdge>,
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub coverage: GraphCoverage,
    pub snapshot: ReadSnapshot,
    pub verification: SnapshotVerification,
    pub dependency_fingerprint: Blake3Hash,
    pub warnings: Vec<String>,
}

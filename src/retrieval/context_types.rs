//! Context output can claim verification only after a private final proof.
use crate::{
    catalog::SnapshotVerification,
    domain::*,
    graph::{GraphObject, GraphPathStep, GraphPlan, GraphQualifiers},
    retrieval::{ExcerptLabel, QueryPlan, RankContribution},
    sources::EvidenceStance,
};
use serde::{Deserialize, Serialize};

/// Local ranking hints derived from the retained embedding space. These are
/// never evidence, citation authority, or a completeness/confidence estimate.
#[derive(Debug, Clone)]
pub(crate) struct ContextSemanticCue {
    pub owner: VaultRelativePath,
    pub observed_hash: Blake3Hash,
    pub span: ByteSpan,
    pub cosine: f64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ContextSelectionSignals {
    pub semantic: Vec<ContextSemanticCue>,
    pub semantic_complete: bool,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScope {
    #[default]
    Current,
    Historical,
    Snapshot,
    IndexedEvidence,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextTarget {
    #[default]
    Documents,
    Graph,
    Combined,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextBudget {
    pub max_bytes: usize,
    pub max_tokens: usize,
    pub instruction_bytes: usize,
    pub instruction_tokens: usize,
    pub output_bytes: usize,
    pub output_tokens: usize,
}
impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_bytes: 12000,
            max_tokens: 3000,
            instruction_bytes: 0,
            instruction_tokens: 0,
            output_bytes: 0,
            output_tokens: 0,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VerificationBudget {
    pub max_bytes: usize,
    pub max_files: usize,
    pub max_entries: usize,
    pub max_elapsed_ms: u64,
}
impl Default for VerificationBudget {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_files: 4096,
            max_entries: 16384,
            max_elapsed_ms: 2000,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextRequest {
    pub scope: ContextScope,
    pub target: ContextTarget,
    pub documents: QueryPlan,
    pub graph: Option<GraphPlan>,
    pub budget: ContextBudget,
    pub verification_budget: VerificationBudget,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceContribution {
    pub reference: EvidenceRef,
    pub stance: EvidenceStance,
    pub eligibility: Eligibility,
    pub authored_status: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextPassage {
    pub locator: DocumentLocator,
    pub text: String,
    pub span: ByteSpan,
    pub label: ExcerptLabel,
    pub eligibility: Eligibility,
    pub citations: Vec<CitationRef>,
    pub contributors: Vec<EvidenceContribution>,
    pub rank_contributions: Vec<RankContribution>,
    pub support_group: Option<Blake3Hash>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceBundle {
    pub assertion: RecordRef,
    pub subject: RecordRef,
    pub predicate: String,
    pub object: GraphObject,
    pub qualifiers: GraphQualifiers,
    pub authored_status: Option<String>,
    pub eligibility: Eligibility,
    pub disputed: bool,
    pub path: Vec<GraphPathStep>,
    pub passage_indices: Vec<usize>,
    pub omitted_support: usize,
    pub omitted_contradictions: usize,
    pub rank_contributions: Vec<RankContribution>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextOmission {
    pub record_id: Option<RecordId>,
    pub path: Option<VaultRelativePath>,
    pub reason: String,
    pub count: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenAccounting {
    EstimatedUtf8BytesDiv4Ceil,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextUsage {
    pub rendered_bytes: usize,
    pub estimated_tokens: usize,
    pub token_accounting: TokenAccounting,
    pub reserved_bytes: usize,
    pub reserved_tokens: usize,
    pub graph_bytes: usize,
    pub graph_estimated_tokens: usize,
    pub verification_bytes: usize,
    pub verification_files: usize,
    pub verification_entries: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextResult {
    pub network_used: bool,
    /// Complete budgeted text authority, including headers, references and qualifiers.
    pub(crate) text: String,
    pub(crate) passages: Vec<ContextPassage>,
    pub(crate) bundles: Vec<EvidenceBundle>,
    pub(crate) omissions: Vec<ContextOmission>,
    pub(crate) usage: ContextUsage,
    pub(crate) snapshot: ReadSnapshot,
    pub(crate) verification: SnapshotVerification,
    pub(crate) dependency_fingerprint: Blake3Hash,
    pub(crate) truncated: bool,
    pub(crate) warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) selection_packet: Option<super::context_selection_packet::SelectionPacket>,
}

impl ContextResult {
    pub fn selection_packet(&self) -> Option<&super::context_selection_packet::SelectionPacket> {
        self.selection_packet.as_ref()
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
    pub fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }
    pub fn verification(&self) -> &SnapshotVerification {
        &self.verification
    }
    pub fn dependency_fingerprint(&self) -> &Blake3Hash {
        &self.dependency_fingerprint
    }
    pub fn truncated(&self) -> bool {
        self.truncated
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}
#[derive(Clone, Default)]
pub struct ContextOptions {
    pub fault: Option<std::sync::Arc<dyn ContextFault>>,
    pub selection: super::context_selection_packet::SelectionAction,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCheckpoint {
    BeforeFinalVerification { attempt: usize },
}
pub trait ContextFault: Send + Sync {
    fn check(&self, checkpoint: ContextCheckpoint) -> Result<()>;
}

//! Complete evidence-review contracts, owned by the orchestrator.
//! D42 and the technical contracts govern validation and acceptance.
use crate::{
    changes::{ChangeStatus, PreparedChange, ReadDependency, ValidationInput},
    domain::{Blake3Hash, ByteSpan, DocumentLocator, RecordId, VaultRelativePath},
    graph::ExpectedRecord,
    sources::EvidenceStance,
    vault::ExpectedState,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const GRAPH_REVIEW_SCHEMA: &str = "lwiki.graph-review.v1";
pub const GRAPH_REVIEW_RECEIPT_SCHEMA: &str = "lwiki.graph-review-receipt.v1";
pub const GRAPH_REVIEW_FENCE: &str = "lwiki-graph-review-v1";
pub const MAX_REVIEW_BYTES: usize = 256 * 1024;
pub const MAX_REVIEWS: usize = 16;
pub const MAX_REVIEW_EVIDENCE: usize = 256;
pub const MAX_REVIEW_SUPERSEDES: usize = 128;
pub const MAX_REVIEW_REASON_BYTES: usize = 4096;
pub const MAX_REVIEW_RECEIPT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_REVIEW_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REVIEW_CAPTURE_FILES: usize = 4096;
pub const MAX_REVIEW_RETAINED_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_REVIEW_HISTORY_HOPS: usize = 64;
pub const MAX_REVIEW_OPERATIONS: usize = 672;
// Exact bounded keyset: reviewed assertions/new decisions, original/successor evidence,
// all evidence source/revision IDs, declared predecessor decisions. No unused entries.
pub const MAX_REVIEW_RECORD_PATHS: usize = 1184;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub schema: String,
    pub decisions: Vec<AssertionReview>,
    // Omitted defaults empty; explicit null fails Vec deserialization AND strict decoder.
    // Normalized serialization always includes this field, including [].
    #[serde(default)]
    pub supersedes: Vec<ExpectedRecord>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionReview {
    pub assertion_id: RecordId,
    pub expected_hash: Blake3Hash,
    pub decision: ReviewDecision,
    pub reason: String,
    pub evidence_checks: Vec<EvidenceCheck>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Accept,
    Reject,
}
impl ReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
        }
    }
    pub fn assertion_status(self) -> &'static str {
        match self {
            Self::Accept => "accepted",
            Self::Reject => "rejected",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCheck {
    pub evidence_id: RecordId,
    pub expected_hash: Blake3Hash,
    pub assessment: EvidenceAssessment,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAssessment {
    Supports,
    Contradicts,
    Insufficient,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewedAssertionStatus {
    Proposed,
    Accepted,
    Rejected,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewedEvidenceStatus {
    Active,
    Retracted,
}

// Explicit full normalized proposition; no free-form Value authorization.
// Shape/registry/qualifiers are checked exactly as CanonicalRecord does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewProposition {
    pub subject_id: RecordId,
    pub predicate: String,
    pub object: ReviewObject,
    pub negated: bool,
    pub modality: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum ReviewObject {
    Entity {
        entity_id: RecordId,
    },
    Literal {
        literal_type: String,
        literal_value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAllocations {
    // Exactly one Decision for each normalized assertion review.
    pub decisions: BTreeMap<RecordId, RecordId>,
    // Exactly changed-stance evidence checks, keyed by original evidence ID.
    // Insufficient and unchanged assessments never allocate successor IDs.
    pub successors: BTreeMap<RecordId, RecordId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAssertionProof {
    pub assertion_id: RecordId,
    pub before_status: ReviewedAssertionStatus,
    pub proposition: ReviewProposition,
    // Exact complete active membership before review, equal request check IDs/hashes.
    pub before_active_evidence: BTreeMap<RecordId, Blake3Hash>,
    pub governing_decision_id: RecordId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSourceProof {
    pub source_id: RecordId,
    pub revision_id: RecordId,
    // Immutable revision envelope exact bytes, independent of mutable source head/status.
    pub revision_hash: Blake3Hash,
    pub original_path: VaultRelativePath,
    pub original_hash: Blake3Hash,
    pub content_path: VaultRelativePath,
    pub content_hash: Blake3Hash,
    pub span: ByteSpan,
    pub quote_hash: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewEvidenceProof {
    pub evidence_id: RecordId,
    pub assertion_id: RecordId,
    pub before_stance: EvidenceStance,
    pub assessment: EvidenceAssessment,
    pub after_status: ReviewedEvidenceStatus,
    pub source: ReviewSourceProof,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_id: Option<RecordId>,
    // Semantic fields except status/stance/identity/navigation; strict whitelist construction.
    // Author body exact bytes are separate; hash alone never authorizes replacement bytes.
    pub invariant_fields_hash: Blake3Hash,
    pub body_hash: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSuccessorTemplate {
    // Exactly changed-stance predecessors. UTF-8 note captured before the review,
    // digest must equal that check's expected_hash; parse/hash/source/body proofs all checked.
    // This bounded durable snapshot lets creation bytes be regenerated after author/path edits.
    // It is NOT a changes manifest or evidence of committed filesystem application.
    // Receipt2MiB bounds apply before cloning, so large complete reviews may refuse.
    pub predecessor_note: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPredecessorProof {
    pub decision_id: RecordId,
    pub expected_hash: Blake3Hash,
    pub action: ReviewDecision,
    pub input_ids: Vec<RecordId>,
    pub output_ids: Vec<RecordId>,
    // Exact entire nonempty assertion scope is input_ids union output_ids.
    // Before is active, after superseded; whole raw status-only write proven via engine witness.
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSupersession {
    pub predecessor_id: RecordId,
    pub successor_id: RecordId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewWriteProof {
    pub target: VaultRelativePath,
    pub before: ExpectedState,
    pub after: ExpectedState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReceiptV1 {
    pub schema: String,
    pub task_id: RecordId,
    pub request: ReviewRequest,
    pub request_hash: Blake3Hash,
    // One RFC3339 UTC creation time for reproducible new review Decision bytes.
    pub created_at: String,
    pub allocations: ReviewAllocations,
    pub record_paths: BTreeMap<RecordId, VaultRelativePath>,
    pub assertion_proofs: Vec<ReviewAssertionProof>,
    pub evidence_proofs: Vec<ReviewEvidenceProof>,
    pub successor_templates: BTreeMap<RecordId, ReviewSuccessorTemplate>,
    pub predecessors: Vec<ReviewPredecessorProof>,
    // Exactly predecessor->each new Decision covering its scope; sorted unique edges.
    pub supersessions: Vec<ReviewSupersession>,
    // Non-Decision writes only. New/edited Decision bytes are independently exact in
    // retained manifests; new Decision bytes regenerate from receipt without self-hash.
    pub operations: Vec<ReviewWriteProof>,
}

#[derive(Debug, Clone)]
pub struct ValidatedReview {
    pub(crate) request: ReviewRequest,
    pub(crate) request_hash: Blake3Hash,
    pub(crate) task_id: RecordId,
    pub(crate) dependencies: Vec<ReadDependency>,
    pub(crate) restored_receipt: Option<ReviewReceiptV1>,
    pub(crate) input: ValidationInput,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewSummary {
    pub accept_assertions: usize,
    pub reject_assertions: usize,
    pub unchanged_evidence: usize,
    pub retract_evidence: usize,
    pub successor_evidence: usize,
    pub supersede_decisions: usize,
    pub create_decisions: usize,
}
#[derive(Debug, Clone)]
pub struct ReviewPlan {
    pub(crate) validated: ValidatedReview,
    pub summary: ReviewSummary,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOutcomeDisposition {
    RetainedChange,
    CanonicalRestored,
}
#[derive(Debug, Clone, Serialize)]
pub struct ReviewOutcome {
    pub decisions: Vec<DocumentLocator>,
    pub prepared: Option<PreparedChange>,
    pub status: Option<ChangeStatus>,
    pub disposition: ReviewOutcomeDisposition,
    pub allocations: ReviewAllocations,
    pub summary: ReviewSummary,
    pub reused: bool,
}
#[derive(Debug, Clone)]
pub struct VerifiedReviewReceipt {
    pub(crate) receipt: ReviewReceiptV1,
    pub(crate) dependencies: Vec<ReadDependency>,
    pub(crate) decision_locators: Vec<DocumentLocator>,
}
impl ValidatedReview {
    pub fn request(&self) -> &ReviewRequest {
        &self.request
    }
    pub fn request_hash(&self) -> &Blake3Hash {
        &self.request_hash
    }
    pub fn task_id(&self) -> &RecordId {
        &self.task_id
    }
    pub fn dependencies(&self) -> &[ReadDependency] {
        &self.dependencies
    }
}
impl ReviewPlan {
    pub fn validated(&self) -> &ValidatedReview {
        &self.validated
    }
}
impl VerifiedReviewReceipt {
    pub fn receipt(&self) -> &ReviewReceiptV1 {
        &self.receipt
    }
    pub fn dependencies(&self) -> &[ReadDependency] {
        &self.dependencies
    }
    pub fn decisions(&self) -> &[DocumentLocator] {
        &self.decision_locators
    }
}

// Leaf-owned sealed proof structs/functions to be declared in review.rs, not shared DTOs:
// normalize(ReviewRequest)->Result<ReviewRequest> (pub(crate), root app pure preflight)
// validate_review(&SourceView, &[u8])->Result<ValidatedReview>
// plan_review(&ValidatedReview)->Result<ReviewPlan>
// stage_review(&ChangeEngine,&WriterPermit,&ValidatedReview)->Result<ReviewOutcome>
// load_review_receipt(&SourceView,&RecordId)->Result<Option<VerifiedReviewReceipt>>
// verify_review_overlay(&VaultFs,&ValidationInput,Option<&RetainedGraphInput>)
//   ->Result<Option<VerifiedReviewOverlay>>; getter accepted_assertions(), supersession_edges().
// verify_review_policy(&BTreeMap<VaultRelativePath,ParsedNote>)
//   ->Result<Option<VerifiedReviewPolicy>>; getter supersession_edges().
// relevant_decision_ids(&BTreeMap<VaultRelativePath,ParsedNote>)->BTreeSet<RecordId>
// verify_review_inverse_overlay(&VaultFs,&ValidationInput,&RetainedGraphInverseInput)
//   ->Result<VerifiedReviewInverse>; getter restored_accepted().
// verify_review_committed_anchor(...,&RetainedGraphInverseInput) is crate-private,
// accepts only genuine engine seal, never a public historical bool/untrusted DTO flag.
// Explicit history hook proposal: verify_review_evolution(&SourceView,&RecordId,
//   &ReviewReceiptV1)->Result<Vec<ReadDependency>>. It proves current explicit successors,
// statuses/propositions/Decision scopes via bounded P13/P12/P04 chains without calling
// import loaders/Catalog/project or granting fresh acceptance permission.
// All Option fields: strict bounded JSON rejects explicit null when present.

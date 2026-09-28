//! Explicit mention-resolution interfaces, owned by the orchestrator.
use crate::{
    changes::{ChangeStatus, PreparedChange, ReadDependency},
    domain::{Blake3Hash, DocumentLocator, RecordId, VaultRelativePath},
    graph::{MentionBinding, PacketLocalId, VerifiedExtractionArtifact},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RESOLUTION_SCHEMA: &str = "lwiki.graph-resolution.v1";
pub const RESOLUTION_RECEIPT_SCHEMA: &str = "lwiki.graph-resolution-receipt.v1";
pub const MAX_RESOLUTION_BYTES: usize = 256 * 1024;
pub const MAX_RESOLUTION_MAPPINGS: usize = 64;
pub const MAX_RESOLUTION_REASON_BYTES: usize = 4096;
pub const MAX_RESOLUTION_RECEIPT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionRequest {
    pub schema: String,
    pub extraction_id: RecordId,
    pub expected_hash: Blake3Hash,
    pub mappings: Vec<ResolutionMapping>,
}

// Each arm owns all its fields. Do not flatten an operation into a permissive DTO.
// Preserve the exact PascalCase operation spelling in the published contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum ResolutionMapping {
    BindMention {
        mention_id: PacketLocalId,
        reason: String,
        entity_id: RecordId,
        expected_entity_hash: Blake3Hash,
    },
    CreateEntity {
        mention_id: PacketLocalId,
        reason: String,
        title: String,
        entity_type: String,
    },
    RejectMention {
        mention_id: PacketLocalId,
        reason: String,
    },
}

// Sealed: strict bounded parser + verified canonical artifact/receipt constructors only.
// No public Deserialize and no unchecked constructor.
#[derive(Debug, Clone)]
pub struct ValidatedResolution {
    pub(crate) request: ResolutionRequest,
    pub(crate) request_hash: Blake3Hash,
    pub(crate) task_id: RecordId,
    pub(crate) extraction: VerifiedExtractionArtifact,
    pub(crate) dependencies: Vec<ReadDependency>,
    pub(crate) restored_receipt: Option<ResolutionReceiptV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolutionSummary {
    pub pending_mentions: usize,
    pub resolved_mentions: usize,
    pub rejected_mentions: usize,
    pub create_entities: usize,
    pub create_decisions: usize,
    // Reserved existing assertion IDs are visible; no decision/entity ID allocation yet.
    pub materialize_assertions: Vec<PacketLocalId>,
}

#[derive(Debug, Clone)]
pub struct ResolutionPlan {
    pub(crate) validated: ValidatedResolution,
    pub summary: ResolutionSummary,
}
impl ResolutionPlan {
    pub fn validated(&self) -> &ValidatedResolution {
        &self.validated
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionAllocations {
    pub decisions: BTreeMap<PacketLocalId, RecordId>,
    // Exactly CreateEntity mappings, never BindMention/RejectMention mappings.
    pub entities: BTreeMap<PacketLocalId, RecordId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingTransition {
    pub before: MentionBinding,
    pub after: MentionBinding,
}

// Bounded durable batch receipt, copied identically into each batch decision body.
// Fence: lwiki-graph-resolution-v1. Root freezes the exact receipt schema.
// request.mappings is normalized into unique mention-ID order before hashing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionReceiptV1 {
    pub schema: String,
    pub task_id: RecordId,
    pub request: ResolutionRequest,
    pub request_hash: Blake3Hash,
    // Canonical sorted JSON hash of original artifact with bindings/materialized removed.
    pub immutable_extraction_hash: Blake3Hash,
    pub allocations: ResolutionAllocations,
    /// Complete semantic state before this batch, independent of author prose.
    /// Whole-note hashes below remain historical claims unless retained bytes
    /// exist; these maps never recreate a missing manifest or before-image.
    pub prior_bindings: BTreeMap<PacketLocalId, MentionBinding>,
    pub prior_materialized_assertions: Vec<PacketLocalId>,
    /// Original paths used by this batch's creation/quotation operation bytes.
    /// Current identity authority resolves IDs independently of these paths.
    pub record_paths: BTreeMap<RecordId, VaultRelativePath>,
    // Exactly this batch's changed mention keys; complete current map remains in artifact.
    pub transitions: BTreeMap<PacketLocalId, BindingTransition>,
    // New local assertion IDs only, preserving the P10 reserved assertion/evidence IDs.
    pub materialized_assertions: Vec<PacketLocalId>,
    // Exact canonical writes other than decision receipt notes; see recursion rule in plan.
    pub operations: Vec<ResolutionWriteProof>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionWriteProof {
    pub target: VaultRelativePath,
    pub before: crate::vault::ExpectedState,
    pub after: crate::vault::ExpectedState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionDisposition {
    RetainedChange,
    CanonicalRestored,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolutionOutcome {
    pub extraction: DocumentLocator,
    pub prepared: Option<PreparedChange>,
    pub status: Option<ChangeStatus>,
    pub disposition: ResolutionDisposition,
    pub allocations: ResolutionAllocations,
    pub summary: ResolutionSummary,
    pub reused: bool,
}

impl ValidatedResolution {
    pub fn request(&self) -> &ResolutionRequest {
        &self.request
    }
    pub fn request_hash(&self) -> &Blake3Hash {
        &self.request_hash
    }
    pub fn task_id(&self) -> &RecordId {
        &self.task_id
    }
}
#[derive(Debug, Clone)]
pub struct VerifiedResolutionReceipt {
    pub(crate) receipt: ResolutionReceiptV1,
    pub(crate) extraction: VerifiedExtractionArtifact,
    pub(crate) dependencies: Vec<ReadDependency>,
    pub(crate) decision_locators: Vec<DocumentLocator>,
}
impl VerifiedResolutionReceipt {
    pub fn receipt(&self) -> &ResolutionReceiptV1 {
        &self.receipt
    }
    pub fn decision_locators(&self) -> &[DocumentLocator] {
        &self.decision_locators
    }
}

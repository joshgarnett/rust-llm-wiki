//! Explicit exhaustive entity-decision contracts, owned by the orchestrator.
use crate::{
    changes::{ChangeStatus, PreparedChange, ReadDependency, ValidationInput},
    domain::{Blake3Hash, DocumentLocator, RecordId, VaultRelativePath},
    graph::{MentionBinding, PacketLocalId},
    vault::ExpectedState,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ENTITY_DECISIONS_SCHEMA: &str = "lwiki.entity-decisions.v1";
pub const ENTITY_DECISION_RECEIPT_SCHEMA: &str = "lwiki.entity-decision-receipt.v1";
pub const ENTITY_DECISION_FENCE: &str = "lwiki-entity-decisions-v1";
pub const MAX_ENTITY_DECISION_BYTES: usize = 256 * 1024;
pub const MAX_ENTITY_DECISIONS: usize = 16;
pub const MAX_ENTITY_REMAPS: usize = 1024;
pub const MAX_ENTITY_EXPECTED_RECORDS: usize = 4096;
pub const MAX_ENTITY_DECISION_RECEIPT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_AUTHORIZED_EVOLUTION_HOPS: usize = 64;

// Root reuses local-ID grammar with a semantic scoped wrapper; no global ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NewEntityKey(PacketLocalId);
impl NewEntityKey {
    pub fn new(value: impl Into<String>) -> crate::domain::Result<Self> {
        PacketLocalId::new(value).map(Self)
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl std::fmt::Display for NewEntityKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityDecisionRequest {
    pub schema: String,
    pub decisions: Vec<EntityDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum EntityDecision {
    MergeEntities {
        reason: String,
        expected_records: Vec<ExpectedRecord>,
        remaps: Vec<EntityRemap>,
        // Absorbed active entities, excluding the separately guarded retained target.
        source_ids: Vec<RecordId>,
        target_id: RecordId,
    },
    SplitEntity {
        reason: String,
        expected_records: Vec<ExpectedRecord>,
        remaps: Vec<EntityRemap>,
        source_id: RecordId,
        new_entities: Vec<NewEntity>,
    },
    AddAlias {
        reason: String,
        expected_records: Vec<ExpectedRecord>,
        remaps: Vec<EntityRemap>, // Must be empty.
        entity_id: RecordId,
        alias: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedRecord {
    pub record_id: RecordId,
    pub hash: Blake3Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewEntity {
    pub key: NewEntityKey,
    pub title: String,
    pub entity_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionEntityField {
    SubjectId,
    ObjectId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum RemapTarget {
    ExistingEntity { entity_id: RecordId },
    NewEntity { key: NewEntityKey },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum EntityRemap {
    Assertion {
        assertion_id: RecordId,
        field: AssertionEntityField,
        old_entity_id: RecordId,
        target: RemapTarget,
    },
    Mention {
        extraction_id: RecordId,
        mention_id: PacketLocalId,
        old_entity_id: RecordId,
        target: RemapTarget,
        // Same-entity carry-forward only for replacing whole shared predecessor.
    },
}

// No Deserialize/unchecked constructor. Bounded full canonical capture + proofs.
#[derive(Debug, Clone)]
pub struct ValidatedEntityDecisions {
    pub(crate) request: EntityDecisionRequest,
    pub(crate) request_hash: Blake3Hash,
    pub(crate) task_id: RecordId,
    pub(crate) dependencies: Vec<ReadDependency>,
    pub(crate) restored_receipt: Option<EntityDecisionReceiptV1>,
    pub(crate) input: ValidationInput, // Verified bounded closed canonical capture.
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntityDecisionSummary {
    pub merge_entities: usize,
    pub split_entities: usize,
    pub add_aliases: usize,
    pub create_entities: usize,
    pub create_decisions: usize,
    pub remap_assertion_fields: usize,
    pub remap_mentions: usize,
}

#[derive(Debug, Clone)]
pub struct EntityDecisionPlan {
    pub(crate) validated: ValidatedEntityDecisions,
    pub summary: EntityDecisionSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MentionDecisionAllocation {
    pub extraction_id: RecordId,
    pub mention_id: PacketLocalId,
    pub predecessor_id: RecordId,
    pub decision_id: RecordId,
}

// Exactly one per normalized operation ordinal, ordered like request.decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityDecisionAllocation {
    pub decision_id: RecordId,
    pub entities: BTreeMap<NewEntityKey, RecordId>, // Exactly SplitEntity keys.
    pub mention_decisions: Vec<MentionDecisionAllocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MentionRemapProof {
    pub mention_id: PacketLocalId,
    pub before: MentionBinding,
    pub after: MentionBinding,
    pub governing_decision_id: RecordId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionRemapProof {
    pub extraction_id: RecordId,
    pub immutable_extraction_hash: Blake3Hash,
    pub prior_bindings: BTreeMap<PacketLocalId, MentionBinding>,
    pub prior_materialized_assertions: Vec<PacketLocalId>,
    pub transitions: Vec<MentionRemapProof>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionRemapProof {
    pub assertion_id: RecordId,
    pub field: AssertionEntityField,
    pub before_entity_id: RecordId,
    pub after_entity_id: RecordId,
    // Canonical proposition excluding only subject/entity object; preserves qualifiers.
    pub invariant_proposition_hash: Blake3Hash,
    pub governing_decision_id: RecordId,
    // Endpoint changes reset accepted to proposed; other statuses are retained.
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityDecisionWriteProof {
    pub target: VaultRelativePath,
    pub before: ExpectedState,
    pub after: ExpectedState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Exact obsolete main or complete-scope acceptance authority (D37).
pub struct MainDecisionSupersession {
    pub predecessor_id: RecordId,
    pub successor_id: RecordId,
    // ExpectedRecord binds actual old note bytes; exact semantic status transition
    // is verified separately because all Decision after-hashes are nonrecursive.
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityDecisionReceiptV1 {
    pub schema: String,
    pub task_id: RecordId,
    pub request: EntityDecisionRequest,
    pub request_hash: Blake3Hash,
    pub allocations: Vec<EntityDecisionAllocation>,
    pub record_paths: BTreeMap<RecordId, VaultRelativePath>,
    pub extraction_proofs: Vec<ExtractionRemapProof>,
    pub assertion_proofs: Vec<AssertionRemapProof>,
    pub main_supersessions: Vec<MainDecisionSupersession>,
    // Exact canonical writes excluding ALL receipt-bearing decision note hashes.
    // Actual retained manifest independently binds all decision payloads.
    pub operations: Vec<EntityDecisionWriteProof>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityDecisionDisposition {
    RetainedChange,
    CanonicalRestored,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityDecisionOutcome {
    pub decisions: Vec<DocumentLocator>,
    pub prepared: Option<PreparedChange>,
    pub status: Option<ChangeStatus>,
    pub disposition: EntityDecisionDisposition,
    pub allocations: Vec<EntityDecisionAllocation>,
    pub summary: EntityDecisionSummary,
    pub reused: bool,
}

#[derive(Debug, Clone)]
pub struct VerifiedEntityDecisionReceipt {
    pub(crate) receipt: EntityDecisionReceiptV1,
    pub(crate) dependencies: Vec<ReadDependency>,
    pub(crate) decision_locators: Vec<DocumentLocator>,
}

impl EntityDecisionPlan {
    pub fn validated(&self) -> &ValidatedEntityDecisions {
        &self.validated
    }
}
impl ValidatedEntityDecisions {
    pub fn request(&self) -> &EntityDecisionRequest {
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
impl VerifiedEntityDecisionReceipt {
    pub fn receipt(&self) -> &EntityDecisionReceiptV1 {
        &self.receipt
    }
    pub fn dependencies(&self) -> &[ReadDependency] {
        &self.dependencies
    }
    pub fn decisions(&self) -> &[DocumentLocator] {
        &self.decision_locators
    }
}

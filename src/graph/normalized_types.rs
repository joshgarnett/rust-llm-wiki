//! Versioned canonical authority for selected graph operations.
//!
//! Canonical commitments describe semantic origins. They never manufacture a
//! lost retained Change, its before-images, or its publication proof.
use crate::domain::{Blake3Hash, RecordId, Result, VaultRelativePath, WikiError};
use serde::{Deserialize, Serialize};

pub(crate) const GRAPH_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphOperationFamily {
    PacketPersist,
    GraphImport,
    MentionResolve,
    AssertionReview,
}

/// The semantic hash includes original allocations, guards and templates, but
/// excludes recursive hashes of Decision notes containing this commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalGraphOriginV2 {
    pub version: u32,
    pub vault_id: RecordId,
    pub family: GraphOperationFamily,
    pub scope_id: RecordId,
    pub semantic_hash: Blake3Hash,
}

impl CanonicalGraphOriginV2 {
    pub(crate) fn for_task(
        vault_id: RecordId,
        family: GraphOperationFamily,
        task_id: &RecordId,
        semantic_hash: Blake3Hash,
    ) -> Result<Self> {
        let scope_id = Self::scope(&vault_id, family, task_id)?;
        Ok(Self {
            version: GRAPH_PROTOCOL_VERSION,
            vault_id,
            family,
            scope_id,
            semantic_hash,
        })
    }

    pub(crate) fn validate_task(
        &self,
        expected_vault: &RecordId,
        family: GraphOperationFamily,
        task_id: &RecordId,
        semantic_hash: &Blake3Hash,
    ) -> Result<()> {
        if self.version != GRAPH_PROTOCOL_VERSION
            || &self.vault_id != expected_vault
            || self.family != family
            || self.scope_id != Self::scope(&self.vault_id, family, task_id)?
            || &self.semantic_hash != semantic_hash
        {
            return Err(WikiError::invalid(
                "canonical graph origin differs from its task",
            ));
        }
        Ok(())
    }

    fn scope(
        vault_id: &RecordId,
        family: GraphOperationFamily,
        task_id: &RecordId,
    ) -> Result<RecordId> {
        let bytes = super::packet::canonical_json(&(
            "lwiki.normalized-graph-origin-scope.v2",
            vault_id,
            family,
            task_id,
        ))?;
        RecordId::new(format!("graph_ng2_{}", Blake3Hash::digest(bytes).hex()))
    }
}

pub(crate) const REVIEW_CARRIER_SCHEMA_V2: &str = "lwiki.graph-review-carrier.v2";
pub(crate) const REVIEW_REFERENCE_SCHEMA_V2: &str = "lwiki.graph-review-reference.v2";
pub(crate) const REVIEW_CARRIER_FENCE_V2: &str = "lwiki-graph-review-carrier-v2";
pub(crate) const REVIEW_REFERENCE_FENCE_V2: &str = "lwiki-graph-review-reference-v2";

/// Exactly one existing allocated Decision carries this task's immutable
/// payload. Its status may evolve without changing the payload commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewCarrierV2 {
    pub schema: String,
    pub origin: CanonicalGraphOriginV2,
    pub carrier_id: RecordId,
    pub carrier_path: VaultRelativePath,
    pub receipt: super::review_types::ReviewReceiptV1,
}

/// The payload hash is BLAKE3 of canonical JSON for ReviewCarrierV2, never a
/// hash of the carrier's mutable envelope or of reference-containing children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewReferenceV2 {
    pub schema: String,
    pub task_id: RecordId,
    pub origin_scope_id: RecordId,
    pub carrier_id: RecordId,
    pub carrier_path: VaultRelativePath,
    pub payload_hash: Blake3Hash,
}

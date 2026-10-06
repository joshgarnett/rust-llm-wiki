//! Rebuildable unit identities, distinct from canonical proofs and paid inputs.
use super::{
    render::{RenderedUnit, TargetKind},
    spaces::{EmbeddingSettings, RENDER_VERSION, SEGMENT_VERSION},
};
use crate::domain::*;
use serde::{Deserialize, Serialize};

pub(crate) const INVENTORY_VERSION: u32 = 1;

/// Embedding spaces deliberately omit segmentation geometry. Inventory policies
/// bind every render setting as well as parser and renderer versions.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct RenderPolicyId(pub(crate) Blake3Hash);

impl RenderPolicyId {
    pub(crate) fn for_settings(parser: &Blake3Hash, settings: &EmbeddingSettings) -> Result<Self> {
        settings.validate()?;
        Ok(Self(Blake3Hash::digest(
            crate::graph::packet::canonical_json(&(
                "lwiki-unit-inventory-policy",
                INVENTORY_VERSION,
                parser,
                RENDER_VERSION,
                SEGMENT_VERSION,
                settings,
            ))?,
        )))
    }
    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// A catalog rebuild changes this identity even if the epoch is unchanged.
pub(crate) fn catalog_incarnation(
    vault_id: &RecordId,
    snapshot: &ReadSnapshot,
) -> Result<Blake3Hash> {
    let SnapshotBinding::PublishedEpoch { publication } = &snapshot.binding else {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "unit inventory requires a published normalized catalog",
        ));
    };
    publication.validate()?;
    Ok(Blake3Hash::digest(crate::graph::packet::canonical_json(
        &(
            "lwiki-unit-inventory-catalog",
            vault_id,
            &publication.file_id,
        ),
    )?))
}

/// Compact cached discovery data. It contains neither corpus text nor a proof
/// fingerprint, and therefore cannot authorize a remote input or a citation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnitDescriptor {
    pub(crate) policy: RenderPolicyId,
    pub(crate) owner: VaultRelativePath,
    pub(crate) target: TargetKind,
    pub(crate) target_id: Option<RecordId>,
    pub(crate) source_hash: Blake3Hash,
    pub(crate) source_span: Option<ByteSpan>,
    pub(crate) unit_id: Blake3Hash,
    pub(crate) input_hash: Blake3Hash,
}
impl UnitDescriptor {
    pub(crate) fn from_rendered(policy: &RenderPolicyId, unit: &RenderedUnit) -> Self {
        Self {
            policy: policy.clone(),
            owner: unit.owner.clone(),
            target: unit.target,
            target_id: unit.target_id.clone(),
            source_hash: unit.source_hash.clone(),
            source_span: unit.source_span,
            unit_id: unit.unit_id.clone(),
            input_hash: unit.input_hash.clone(),
        }
    }
    pub(crate) fn matches_rendered(&self, unit: &RenderedUnit) -> bool {
        self.owner == unit.owner
            && self.target == unit.target
            && self.target_id == unit.target_id
            && self.source_hash == unit.source_hash
            && self.source_span == unit.source_span
            && self.unit_id == unit.unit_id
            && self.input_hash == unit.input_hash
    }
}

/// Readiness is scoped to an owner version, never to a snapshot-wide JSON copy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnitOwnerBinding {
    pub(crate) incarnation: Blake3Hash,
    pub(crate) policy: RenderPolicyId,
    pub(crate) owner: VaultRelativePath,
    pub(crate) render_token: Blake3Hash,
    pub(crate) proof_version: u64,
    pub(crate) modified_seq: u64,
    pub(crate) tombstone: bool,
    pub(crate) unit_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnitOwnerCursor {
    pub(crate) modified_seq: u64,
    pub(crate) owner: VaultRelativePath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnitInventoryState {
    pub(crate) policy: RenderPolicyId,
    pub(crate) complete: bool,
    pub(crate) after_owner: Option<VaultRelativePath>,
    pub(crate) through_seq: u64,
    pub(crate) unit_count: usize,
}

/// A continuation is valid only for the same physical catalog and render policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparationCursor {
    pub(crate) version: u32,
    pub(crate) incarnation: Blake3Hash,
    pub(crate) policy: RenderPolicyId,
    pub(crate) since_seq: u64,
    pub(crate) through_seq: u64,
    pub(crate) after: Option<UnitOwnerCursor>,
    pub(crate) complete: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_policy_changes_with_geometry_and_parser() {
        let parser = Blake3Hash::digest("parser");
        let settings = EmbeddingSettings::default();
        let original = RenderPolicyId::for_settings(&parser, &settings).unwrap();
        let mut segmented = settings.clone();
        segmented.quality_target_bytes = Some(1000);
        assert_ne!(
            original,
            RenderPolicyId::for_settings(&parser, &segmented).unwrap()
        );
        segmented = settings.clone();
        segmented.max_input_bytes = 9000;
        assert_ne!(
            original,
            RenderPolicyId::for_settings(&parser, &segmented).unwrap()
        );
        assert_ne!(
            original,
            RenderPolicyId::for_settings(&Blake3Hash::digest("new parser"), &settings).unwrap()
        );
        assert_eq!(
            original,
            RenderPolicyId::for_settings(&parser, &settings).unwrap()
        );
    }

    #[test]
    fn preparation_namespace_survives_epoch_but_changes_on_rebuild() {
        let vault = RecordId::new("vault_inventory").unwrap();
        let parser = Blake3Hash::digest("parser");
        let first = ReadSnapshot::published(
            1,
            parser.clone(),
            "a".repeat(32),
            Blake3Hash::digest("publication1"),
        )
        .unwrap();
        let updated = ReadSnapshot::published(
            2,
            parser.clone(),
            "a".repeat(32),
            Blake3Hash::digest("publication2"),
        )
        .unwrap();
        let rebuilt = ReadSnapshot::published(
            2,
            parser,
            "b".repeat(32),
            Blake3Hash::digest("publication2"),
        )
        .unwrap();
        assert_eq!(
            catalog_incarnation(&vault, &first).unwrap(),
            catalog_incarnation(&vault, &updated).unwrap()
        );
        assert_ne!(
            catalog_incarnation(&vault, &first).unwrap(),
            catalog_incarnation(&vault, &rebuilt).unwrap()
        );
    }
}

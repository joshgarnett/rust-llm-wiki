//! Exact authenticated GraphDecide reversal; no generic proposition authority.
use super::{decision_types::MAX_AUTHORIZED_EVOLUTION_HOPS, packet, remap};
use crate::{
    changes::{OperationRole, RetainedGraphInverseInput, ValidationInput},
    domain::{Blake3Hash, RecordId, RecordKind, Result},
    records::parse_note,
    sources::SourceView,
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub struct VerifiedInverseRemap {
    assertions: BTreeSet<RecordId>,
    accepted: BTreeSet<RecordId>,
}
impl VerifiedInverseRemap {
    pub fn authorized_assertions(&self) -> &BTreeSet<RecordId> {
        &self.assertions
    }
    pub fn restored_accepted(&self) -> &BTreeSet<RecordId> {
        &self.accepted
    }
}
fn state(bytes: Option<&[u8]>) -> ExpectedState {
    bytes.map_or(ExpectedState::Absent, |b| {
        ExpectedState::Hash(Blake3Hash::digest(b))
    })
}
fn bounded(input: &ValidationInput) -> Result<()> {
    if input.documents.len() > 4096
        || input.overlay.len() > crate::changes::prepare::MAX_OPS
        || input
            .documents
            .iter()
            .try_fold(0usize, |n, d| n.checked_add(d.bytes.len()))
            .is_none_or(|n| n > packet::SOURCE_CAP)
        || input
            .overlay
            .iter()
            .filter_map(|o| o.bytes.as_ref())
            .try_fold(0usize, |n, b| n.checked_add(b.len()))
            .is_none_or(|n| n > packet::SOURCE_CAP)
    {
        return Err(remap::bad(
            "inverse captured documents/overlay exceed ceiling",
        ));
    }
    Ok(())
}

pub fn verify_inverse_overlay(
    fs: &VaultFs,
    input: &ValidationInput,
    witness: &RetainedGraphInverseInput,
) -> Result<VerifiedInverseRemap> {
    bounded(input)?;
    if !(1..=MAX_AUTHORIZED_EVOLUTION_HOPS).contains(&witness.inversion_depth()) {
        return Err(remap::bad("inverse lineage exceeds bounded depth"));
    }
    if witness.inversion_depth() == 1
        && (witness.parent_change_id() != witness.anchor().change_id()
            || witness.parent_manifest_hash() != witness.anchor().manifest_hash())
    {
        return Err(remap::bad(
            "direct inverse parent differs from committed anchor",
        ));
    }
    let authorized = remap::verify_committed_anchor(fs, input, witness)?;
    let current = SourceView::from_closed_input(
        fs,
        &ValidationInput {
            vault_id: input.vault_id.clone(),
            documents: input.documents.clone(),
            overlay: vec![],
        },
    )?;
    let final_view = SourceView::from_closed_input(fs, input)?;
    let actual = input
        .documents
        .iter()
        .map(|d| (&d.path, d.bytes.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let overlay = input
        .overlay
        .iter()
        .map(|o| (&o.path, o.bytes.as_deref()))
        .collect::<BTreeMap<_, _>>();
    let mutable = witness
        .parent_operations()
        .iter()
        .filter(|op| op.role() != OperationRole::ImmutableAsset)
        .collect::<Vec<_>>();
    let expected_paths = mutable.iter().map(|op| op.path()).collect::<BTreeSet<_>>();
    if expected_paths.len() != mutable.len()
        || overlay.len() != input.overlay.len()
        || overlay.keys().copied().collect::<BTreeSet<_>>() != expected_paths
    {
        return Err(remap::bad("inverse exact mutable write membership differs"));
    }
    let mut deleted_entities = BTreeSet::new();
    let mut restored_accepted = BTreeSet::new();
    for op in mutable {
        if state(op.before_bytes()) != *op.before() || state(op.after_bytes()) != *op.after() {
            return Err(remap::bad("inverse authenticated parent bytes/hash differ"));
        }
        let observed = state(actual.get(op.path()).copied());
        if observed != *op.before() && observed != *op.after() {
            return Err(remap::conflict(
                "inverse parent target has unfamiliar current bytes",
            ));
        }
        let final_bytes = overlay
            .get(op.path())
            .copied()
            .ok_or_else(|| remap::bad("inverse target absent from overlay"))?;
        if final_bytes != op.before_bytes() {
            return Err(remap::bad(
                "inverse target is not exact authenticated parent reversal",
            ));
        }
        if op.before_bytes().is_none()
            && let Some(bytes) = op.after_bytes()
            && let Some(record) = parse_note(bytes).canonical
            && record.kind() == RecordKind::Entity
        {
            deleted_entities.insert(record.id().clone());
        }
        if let Some(before_bytes) = op.before_bytes()
            && let Some(before) = parse_note(before_bytes).canonical
            && before.kind() == RecordKind::Assertion
            && before.string("wiki_status") == Some("accepted")
        {
            let after = op
                .after_bytes()
                .map(parse_note)
                .and_then(|n| n.canonical)
                .ok_or_else(|| remap::bad("restored accepted assertion parent after missing"))?;
            if after.id() != before.id()
                || !authorized.contains(before.id())
                || after.string("wiki_status") != Some("proposed")
            {
                return Err(remap::bad(
                    "inverse accepted restoration lacks exact remap authority",
                ));
            }
            // This set is based on authenticated parent bytes, even when a
            // recovery scan already contains the restored accepted bytes.
            restored_accepted.insert(before.id().clone());
        }
    }
    // Only references explicitly reversed by this exact overlay may disappear.
    // Actual extra assertions/mention mappings cannot be hidden by the overlay.
    let actual_refs = remap::reference_set(&current.notes, &deleted_entities)?;
    for id in actual_refs.values() {
        let (path, _) = remap::find(&current.notes, id)?;
        if !expected_paths.contains(path) {
            return Err(remap::conflict(
                "new inbound reference prevents deleting split-created identity",
            ));
        }
    }
    if !remap::reference_set(&final_view.notes, &deleted_entities)?.is_empty() {
        return Err(remap::conflict(
            "inverse leaves reference to deleted split-created identity",
        ));
    }
    Ok(VerifiedInverseRemap {
        assertions: authorized,
        accepted: restored_accepted,
    })
}

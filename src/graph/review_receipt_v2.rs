//! One authenticated payload per allocated review family. Legacy v1 notes keep
//! their original renderer, copy agreement and semantic verification contracts.
use super::*;
use crate::graph::normalized_types::*;

// The receipt retains its existing limit. This allows only the bounded wrapper
// and canonical Decision envelope in addition to it, not a larger receipt.
const MAX_CARRIER_BYTES: usize = MAX_REVIEW_RECEIPT_BYTES + 262144;
const MAX_REFERENCE_BYTES: usize = 16384;

pub(crate) struct CollectedReviewReceipts {
    pub(crate) receipts: BTreeMap<RecordId, ReviewReceiptV1>,
    pub(crate) origins: BTreeMap<RecordId, CanonicalGraphOriginV2>,
}
impl CollectedReviewReceipts {
    pub(crate) fn bind_vault(&self, vault_id: &RecordId) -> Result<()> {
        if self
            .origins
            .values()
            .any(|origin| &origin.vault_id != vault_id)
        {
            return Err(bad("review origin belongs to another vault"));
        }
        Ok(())
    }
}

fn fences(n: &ParsedNote) -> Vec<String> {
    std::str::from_utf8(n.body())
        .map(|body| {
            Parser::new(body)
                .filter_map(|event| match event {
                    Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                        if matches!(
                            info.as_ref(),
                            GRAPH_REVIEW_FENCE
                                | REVIEW_CARRIER_FENCE_V2
                                | REVIEW_REFERENCE_FENCE_V2
                        ) =>
                    {
                        Some(info.to_string())
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn witness_count(n: &ParsedNote) -> usize {
    fences(n).len()
}

pub(super) fn carrier(
    receipt: &ReviewReceiptV1,
    origin: &CanonicalGraphOriginV2,
) -> Result<ReviewCarrierV2> {
    shape(receipt)?;
    origin.validate_task(
        &origin.vault_id, // Self-consistency only; production caller binds actual Vault.
        GraphOperationFamily::AssertionReview,
        &receipt.task_id,
        &Blake3Hash::digest(packet::canonical_json(receipt)?),
    )?;
    let carrier_id = receipt
        .allocations
        .decisions
        .values()
        .min()
        .ok_or_else(|| bad("review carrier allocation absent"))?
        .clone();
    Ok(ReviewCarrierV2 {
        schema: REVIEW_CARRIER_SCHEMA_V2.into(),
        origin: origin.clone(),
        carrier_path: receipt.record_paths[&carrier_id].clone(),
        carrier_id,
        receipt: receipt.clone(),
    })
}

fn reference(carrier: &ReviewCarrierV2, payload_hash: &Blake3Hash) -> ReviewReferenceV2 {
    ReviewReferenceV2 {
        schema: REVIEW_REFERENCE_SCHEMA_V2.into(),
        task_id: carrier.receipt.task_id.clone(),
        origin_scope_id: carrier.origin.scope_id.clone(),
        carrier_id: carrier.carrier_id.clone(),
        carrier_path: carrier.carrier_path.clone(),
        payload_hash: payload_hash.clone(),
    }
}

/// Render a single Decision. Bulk planners should use render_decisions_v2 so
/// payload validation, serialization and hashing happen once for the family.
pub(crate) fn render_decision_v2(
    receipt: &ReviewReceiptV1,
    origin: &CanonicalGraphOriginV2,
    review: &AssertionReview,
) -> Result<Vec<u8>> {
    if !receipt.request.decisions.contains(review) {
        return Err(bad("review renderer decision not in immutable request"));
    }
    let c = carrier(receipt, origin)?;
    let bytes = packet::canonical_json(&c)?;
    let proof = if receipt.allocations.decisions[&review.assertion_id] == c.carrier_id {
        packet::render_fence(&c, REVIEW_CARRIER_FENCE_V2, MAX_CARRIER_BYTES)?
    } else {
        packet::render_fence(
            &reference(&c, &Blake3Hash::digest(bytes)),
            REVIEW_REFERENCE_FENCE_V2,
            MAX_REFERENCE_BYTES,
        )?
    };
    decision_with_proof(receipt, review, &proof)
}

pub(crate) fn render_decisions_v2(
    receipt: &ReviewReceiptV1,
    origin: &CanonicalGraphOriginV2,
) -> Result<BTreeMap<RecordId, Vec<u8>>> {
    let c = carrier(receipt, origin)?;
    render_carrier_decisions(&c)
}
pub(super) fn render_carrier_decisions(c: &ReviewCarrierV2) -> Result<BTreeMap<RecordId, Vec<u8>>> {
    let receipt = &c.receipt;
    let hash = Blake3Hash::digest(packet::canonical_json(&c)?);
    let carrier_proof = packet::render_fence(&c, REVIEW_CARRIER_FENCE_V2, MAX_CARRIER_BYTES)?;
    let reference_proof = packet::render_fence(
        &reference(&c, &hash),
        REVIEW_REFERENCE_FENCE_V2,
        MAX_REFERENCE_BYTES,
    )?;
    receipt
        .request
        .decisions
        .iter()
        .map(|d| {
            let id = &receipt.allocations.decisions[&d.assertion_id];
            Ok((
                id.clone(),
                decision_with_proof(
                    receipt,
                    d,
                    if id == &c.carrier_id {
                        &carrier_proof
                    } else {
                        &reference_proof
                    },
                )?,
            ))
        })
        .collect()
}

struct Family {
    carrier: ReviewCarrierV2,
    hash: Blake3Hash,
    members: BTreeSet<RecordId>,
}

/// Authenticate all v2 families before exposing their v1 semantic payloads to
/// the existing policy/evolution validators. Callers with a Vault must also
/// bind every returned origin.vault_id to that Vault's actual identity.
pub(crate) fn collect_review_receipts_v2(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    scope: &mut ReceiptBudget,
) -> Result<CollectedReviewReceipts> {
    scope.require(super::super::policy_inputs::PolicyInputKey::ReviewReceiptCandidates)?;
    let mut legacy = BTreeMap::<RecordId, ReviewReceiptV1>::new();
    let mut families = BTreeMap::<RecordId, Family>::new();
    let mut references = Vec::new();
    let mut v2_ids = BTreeSet::new();
    for (path, note) in notes {
        scope.step()?;
        let witnesses = fences(note);
        if witnesses.is_empty() {
            continue;
        }
        scope.admit(path, note)?;
        if witnesses.len() != 1 {
            return Err(bad("review note contains multiple proof witnesses"));
        }
        if note.raw.len() > MAX_CARRIER_BYTES {
            return Err(super::super::receipt_budget::exhausted());
        }
        let enclosing = record(note)?;
        if enclosing.kind() != RecordKind::Decision
            || !matches!(enclosing.string("wiki_action"), Some("accept" | "reject"))
        {
            return Err(bad("relevant review receipt in wrong record/action"));
        }
        match witnesses[0].as_str() {
            GRAPH_REVIEW_FENCE => {
                let proof = receipt(note)?;
                shape(&proof)?;
                if !proof
                    .allocations
                    .decisions
                    .values()
                    .any(|id| id == enclosing.id())
                {
                    return Err(bad("review receipt unallocated enclosing Decision"));
                }
                if let Some(old) = legacy.get(&proof.task_id) {
                    if old != &proof {
                        return Err(bad("review receipt copies disagree"));
                    }
                } else {
                    legacy.insert(proof.task_id.clone(), proof);
                }
            }
            REVIEW_CARRIER_FENCE_V2 => {
                let c: ReviewCarrierV2 = packet::decode(
                    packet::fenced_json(note, REVIEW_CARRIER_FENCE_V2, MAX_CARRIER_BYTES)?,
                    MAX_CARRIER_BYTES,
                )?;
                shape(&c.receipt)?;
                c.origin.validate_task(
                    &c.origin.vault_id, // Self-consistency only; bind_vault is separate.
                    GraphOperationFamily::AssertionReview,
                    &c.receipt.task_id,
                    &Blake3Hash::digest(packet::canonical_json(&c.receipt)?),
                )?;
                if c.schema != REVIEW_CARRIER_SCHEMA_V2
                    || c.receipt.allocations.decisions.values().min() != Some(&c.carrier_id)
                    || enclosing.id() != &c.carrier_id
                    || c.receipt.record_paths.get(&c.carrier_id) != Some(path)
                    || &c.carrier_path != path
                    || !v2_ids.insert(enclosing.id().clone())
                {
                    return Err(bad(
                        "review carrier schema/allocation/path/identity differs",
                    ));
                }
                let hash = Blake3Hash::digest(packet::canonical_json(&c)?);
                let members = BTreeSet::from([c.carrier_id.clone()]);
                if families
                    .insert(
                        c.origin.scope_id.clone(),
                        Family {
                            carrier: c,
                            hash,
                            members,
                        },
                    )
                    .is_some()
                {
                    return Err(bad("duplicate review carrier scope"));
                }
            }
            REVIEW_REFERENCE_FENCE_V2 => {
                let reference: ReviewReferenceV2 = packet::decode(
                    packet::fenced_json(note, REVIEW_REFERENCE_FENCE_V2, MAX_REFERENCE_BYTES)?,
                    MAX_REFERENCE_BYTES,
                )?;
                if reference.schema != REVIEW_REFERENCE_SCHEMA_V2
                    || !v2_ids.insert(enclosing.id().clone())
                {
                    return Err(bad("review reference schema/duplicate identity differs"));
                }
                references.push((path, enclosing.id(), reference));
            }
            _ => unreachable!("filtered proof fence"),
        }
    }
    for (path, id, reference) in references {
        scope.step()?;
        let family = families
            .get_mut(&reference.origin_scope_id)
            .ok_or_else(|| bad("review reference carrier missing"))?;
        let c = &family.carrier;
        if reference.task_id != c.receipt.task_id
            || reference.carrier_id != c.carrier_id
            || reference.carrier_path != c.carrier_path
            || reference.payload_hash != family.hash
            || id == &c.carrier_id
            || !c
                .receipt
                .allocations
                .decisions
                .values()
                .any(|allocated| allocated == id)
            || c.receipt.record_paths.get(id) != Some(path)
            || !family.members.insert(id.clone())
        {
            return Err(bad(
                "review reference task/scope/hash/path/allocation differs",
            ));
        }
    }
    let mut origins = BTreeMap::new();
    for (_, family) in families {
        scope.step()?;
        let c = family.carrier;
        let allocated = c
            .receipt
            .allocations
            .decisions
            .values()
            .cloned()
            .collect::<BTreeSet<_>>();
        if family.members != allocated {
            return Err(bad("review allocated reference family incomplete"));
        }
        // Identity discovery must include non-proof notes, too. This prevents a
        // duplicate canonical ID without a fence from escaping authentication.
        for id in &allocated {
            let (actual, _) = scope.find(notes, id)?;
            if c.receipt.record_paths.get(id) != Some(actual) {
                return Err(bad("review allocated Decision actual path differs"));
            }
        }
        if legacy.contains_key(&c.receipt.task_id) || origins.contains_key(&c.receipt.task_id) {
            return Err(bad("review task has multiple receipt protocols/scopes"));
        }
        origins.insert(c.receipt.task_id.clone(), c.origin);
        legacy.insert(c.receipt.task_id.clone(), c.receipt);
    }
    Ok(CollectedReviewReceipts {
        receipts: legacy,
        origins,
    })
}

#[cfg(test)]
#[path = "review_receipt_v2_tests.rs"]
mod tests;

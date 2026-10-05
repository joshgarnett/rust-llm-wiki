//! Receipt-policy recomputation over explicit, indexed input certificates.
//! A missing certificate suspends evaluation; it is never a policy diagnostic.
use super::{
    policy_delta::{PolicyDelta, PolicyMembershipUpdate, PolicyReplacement},
    policy_facts::PolicyKind,
    query::QuerySnapshot,
};
use crate::{
    domain::{Blake3Hash, ErrorCode, Result, VaultRelativePath, WikiError},
    graph::policy_inputs::{self, PolicyAttempt, PolicyInputKey, PolicyWork},
    records::ParsedNote,
};
use std::collections::{BTreeMap, BTreeSet};

/// The enclosing sealed write admission owns one meter and its before-images.
/// Implementations authenticate expected hashes and record every loaded file
/// for the same final recheck and retained manifest as other write inputs.
pub(super) trait PolicyInputAccess {
    fn note(&mut self, path: &VaultRelativePath, expected: &Blake3Hash) -> Result<ParsedNote>;
    fn work(&mut self, work: PolicyWork) -> Result<()>;
}

fn corrupt(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}

/// `before` contains the authenticated prior notes for replaced owners; absent
/// owners have no entry. The caller validates identity/path/write authority.
/// Neither this helper nor its public row delta grants canonical write access.
pub(super) fn project_policy(
    reader: &QuerySnapshot,
    before: &BTreeMap<VaultRelativePath, ParsedNote>,
    overlay: &BTreeMap<VaultRelativePath, ParsedNote>,
    access: &mut dyn PolicyInputAccess,
) -> Result<PolicyDelta> {
    project_policy_inner(reader, before, overlay, &BTreeSet::new(), access, false)
}
pub(super) fn project_policy_for_move(
    reader: &QuerySnapshot,
    before: &BTreeMap<VaultRelativePath, ParsedNote>,
    overlay: &BTreeMap<VaultRelativePath, ParsedNote>,
    removed: &BTreeSet<VaultRelativePath>,
    access: &mut dyn PolicyInputAccess,
) -> Result<PolicyDelta> {
    if removed.len() != 1 {
        return Err(corrupt("Page move must retire exactly one policy owner"));
    }
    project_policy_inner(reader, before, overlay, removed, access, true)
}
fn project_policy_inner(
    reader: &QuerySnapshot,
    before: &BTreeMap<VaultRelativePath, ParsedNote>,
    overlay: &BTreeMap<VaultRelativePath, ParsedNote>,
    removed: &BTreeSet<VaultRelativePath>,
    access: &mut dyn PolicyInputAccess,
    page_move: bool,
) -> Result<PolicyDelta> {
    reader.require_policy_layout()?;
    let ceiling = if page_move {
        super::normalized_delta::MAX_ROWS
    } else {
        16
    };
    if overlay.is_empty()
        || overlay.len().saturating_add(removed.len()) > ceiling
        || !removed.is_disjoint(&overlay.keys().cloned().collect())
        || removed.iter().any(|p| !before.contains_key(p))
        || before
            .keys()
            .any(|p| !overlay.contains_key(p) && !removed.contains(p))
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            format!(
                "policy overlay requires 1–{ceiling} explicit note owners with a closed before scope"
            ),
        ));
    }
    let paths: BTreeSet<_> = overlay.keys().chain(removed.iter()).cloned().collect();
    let mut changed = BTreeSet::new();
    for (path, note) in before.iter().chain(overlay) {
        access.work(PolicyWork { steps: 1, bytes: 0 })?;
        changed.extend(policy_inputs::policy_membership_keys(path, note)?);
    }
    let mut delta = PolicyDelta {
        retired_owners: removed.iter().cloned().collect(),
        memberships: Vec::new(),
        replacements: Vec::new(),
    };
    for (path, note) in overlay {
        delta.memberships.push(PolicyMembershipUpdate {
            path: path.clone(),
            hash: note.source_hash.clone(),
            keys: policy_inputs::policy_membership_keys(path, note)?,
        });
    }
    let affected = reader.policy_affected(&changed, &paths)?;
    // An unchanged complete trace proves preservation relative to this indexed
    // publication. Unrelated external edits remain the explicit sync contract.
    if affected.is_empty() {
        return Ok(delta);
    }
    let mut notes = overlay.clone();
    let mut certificates = BTreeSet::new();
    for kind in affected {
        loop {
            access.work(PolicyWork { steps: 1, bytes: 0 })?;
            let need = match kind {
                PolicyKind::Remap => {
                    let mut meter = |work| access.work(work);
                    match policy_inputs::attempt_decision_policy_metered(
                        &notes,
                        &certificates,
                        &mut meter,
                    )? {
                        PolicyAttempt::Need(key) => key,
                        PolicyAttempt::Complete { result, trace } => {
                            delta
                                .replacements
                                .push(PolicyReplacement::from_remap(&result, trace)?);
                            break;
                        }
                    }
                }
                PolicyKind::Review => {
                    let mut meter = |work| access.work(work);
                    match policy_inputs::attempt_review_policy_metered(
                        &notes,
                        &certificates,
                        &mut meter,
                    )? {
                        PolicyAttempt::Need(key) => key,
                        PolicyAttempt::Complete { result, trace } => {
                            delta
                                .replacements
                                .push(PolicyReplacement::from_review(&result, trace)?);
                            break;
                        }
                    }
                }
            };
            if certificates.contains(&need) {
                return Err(corrupt(
                    "receipt evaluator repeated a certified input request",
                ));
            }
            if certificates.len() >= 4096 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "policy input certificate allowance exhausted",
                ));
            }
            load_certificate(reader, &need, &paths, &mut notes, access, page_move)?;
            // Certify only after the complete ordered SQL result and all named
            // witnesses were admitted. An empty result is explicit evidence.
            certificates.insert(need);
        }
    }
    Ok(delta)
}

fn load_certificate(
    reader: &QuerySnapshot,
    key: &PolicyInputKey,
    replaced: &BTreeSet<VaultRelativePath>,
    notes: &mut BTreeMap<VaultRelativePath, ParsedNote>,
    access: &mut dyn PolicyInputAccess,
    page_move: bool,
) -> Result<()> {
    let members = if page_move {
        reader.policy_members_for_page_move(key, replaced)?
    } else {
        reader.policy_members(key, replaced)?
    };
    let mut expected = BTreeMap::new();
    for (path, hash) in members {
        access.work(PolicyWork { steps: 1, bytes: 0 })?;
        if replaced.contains(&path) || expected.insert(path.clone(), hash.clone()).is_some() {
            return Err(corrupt(
                "policy membership includes a replaced or duplicate owner",
            ));
        }
        if let Some(note) = notes.get(&path) {
            if note.source_hash != hash {
                return Err(corrupt("policy memberships disagree on file hash"));
            }
        } else {
            if notes.len() >= 4096 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "policy note boundary exceeds row allowance",
                ));
            }
            let note = access.note(&path, &hash)?;
            if note.source_hash != hash {
                return Err(corrupt("policy witness differs from expected hash"));
            }
            notes.insert(path.clone(), note);
        }
        if !policy_inputs::policy_membership_keys(&path, &notes[&path])?.contains(key) {
            return Err(corrupt(
                "policy membership disagrees with canonical predicate",
            ));
        }
    }
    // Previously loaded witnesses must not introduce an unlisted member into
    // the certificate. Overlay members are admitted by their replacement scope.
    for (path, note) in notes.iter() {
        access.work(PolicyWork { steps: 1, bytes: 0 })?;
        if !replaced.contains(path)
            && policy_inputs::policy_membership_keys(path, note)?.contains(key)
            && expected.get(path) != Some(&note.source_hash)
        {
            return Err(corrupt(
                "policy certificate omits an already observed member",
            ));
        }
    }
    Ok(())
}

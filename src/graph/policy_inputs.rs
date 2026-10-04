//! Complete syntactic membership certificates for replaying receipt policy.
use super::{receipt_budget::ReceiptBudget, remap, review};
use crate::{domain::*, records::ParsedNote};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum PolicyInputKey {
    CanonicalIdentity(RecordId),
    RemapReceiptCandidates,
    ReviewReceiptCandidates,
    Extractions,
    AssertionEndpoint(RecordId),
    AssertionCandidates,
    ScopedAuthority(RecordId),
    ScopedAuthorityCandidates,
    ActiveDecisionOutput(RecordId),
    ActiveDecisionCandidates,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PolicyInputTrace {
    pub keys: BTreeSet<PolicyInputKey>,
    pub paths: BTreeMap<VaultRelativePath, Blake3Hash>,
}
/// Incremental evaluator work charged before scans and exact-byte hashing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PolicyWork {
    pub steps: usize,
    pub bytes: usize,
}
pub(crate) enum PolicyAttempt<T> {
    Need(PolicyInputKey),
    Complete {
        result: Result<T>,
        trace: PolicyInputTrace,
    },
}
fn attempt<T>(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    certificates: Option<&BTreeSet<PolicyInputKey>>,
    verify: impl FnOnce(&BTreeMap<VaultRelativePath, ParsedNote>, &mut ReceiptBudget) -> Result<T>,
) -> Result<PolicyAttempt<T>> {
    attempt_metered(notes, certificates, None, verify)
}
fn attempt_metered<T>(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    certificates: Option<&BTreeSet<PolicyInputKey>>,
    meter: Option<&mut dyn FnMut(PolicyWork) -> Result<()>>,
    verify: impl FnOnce(&BTreeMap<VaultRelativePath, ParsedNote>, &mut ReceiptBudget) -> Result<T>,
) -> Result<PolicyAttempt<T>> {
    let mut budget = ReceiptBudget::certified(certificates.cloned());
    if let Some(meter) = meter {
        budget = budget.with_meter(meter);
    }
    let result = verify(notes, &mut budget);
    let (pending, trace) = budget.into_trace();
    if let Some(key) = pending {
        return Ok(PolicyAttempt::Need(key));
    }
    if result
        .as_ref()
        .err()
        .is_some_and(|e| e.code == ErrorCode::BudgetExceeded)
    {
        return Err(result.err().unwrap());
    }
    Ok(PolicyAttempt::Complete { result, trace })
}
pub(crate) fn attempt_decision_policy(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    certificates: &BTreeSet<PolicyInputKey>,
) -> Result<PolicyAttempt<Option<remap::VerifiedDecisionPolicy>>> {
    attempt(
        notes,
        Some(certificates),
        remap::verify_decision_policy_scoped,
    )
}
pub(crate) fn attempt_review_policy(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    certificates: &BTreeSet<PolicyInputKey>,
) -> Result<PolicyAttempt<Option<review::VerifiedReviewPolicy>>> {
    attempt(
        notes,
        Some(certificates),
        review::verify_review_policy_scoped,
    )
}
pub(crate) fn evaluate_decision_policy(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
) -> Result<PolicyAttempt<Option<remap::VerifiedDecisionPolicy>>> {
    attempt(notes, None, remap::verify_decision_policy_scoped)
}
pub(crate) fn evaluate_review_policy(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
) -> Result<PolicyAttempt<Option<review::VerifiedReviewPolicy>>> {
    attempt(notes, None, review::verify_review_policy_scoped)
}

/// The driver supplies one shared meter across fresh attempts. It must check
/// its monotonic deadline and return BudgetExceeded on cumulative exhaustion.
pub(crate) fn attempt_decision_policy_metered(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    certificates: &BTreeSet<PolicyInputKey>,
    meter: &mut dyn FnMut(PolicyWork) -> Result<()>,
) -> Result<PolicyAttempt<Option<remap::VerifiedDecisionPolicy>>> {
    attempt_metered(
        notes,
        Some(certificates),
        Some(meter),
        remap::verify_decision_policy_scoped,
    )
}
pub(crate) fn attempt_review_policy_metered(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    certificates: &BTreeSet<PolicyInputKey>,
    meter: &mut dyn FnMut(PolicyWork) -> Result<()>,
) -> Result<PolicyAttempt<Option<review::VerifiedReviewPolicy>>> {
    attempt_metered(
        notes,
        Some(certificates),
        Some(meter),
        review::verify_review_policy_scoped,
    )
}

/// Pure conservative prefilters. Broad keys retain errors encountered before
/// endpoint/output matching; no eligibility or fence decoder is consulted.
pub(crate) fn policy_membership_keys(
    _path: &VaultRelativePath,
    note: &ParsedNote,
) -> Result<BTreeSet<PolicyInputKey>> {
    use PolicyInputKey::*;
    let mut keys = BTreeSet::new();
    if review::has_fence(note) {
        keys.insert(ReviewReceiptCandidates);
    }
    // Raw malformed identity claims are retained as witnesses; find still
    // filters canonical claims, so they cannot fabricate a canonical record.
    if let Some(id) = note
        .fields
        .as_ref()
        .and_then(|f| f.get("wiki_id"))
        .and_then(|v| v.as_str())
        .and_then(|s| RecordId::new(s).ok())
    {
        keys.insert(CanonicalIdentity(id));
    }
    for field in ["wiki_subject_id", "wiki_object_id"] {
        if let Some(id) = note
            .fields
            .as_ref()
            .and_then(|f| f.get(field))
            .and_then(|v| v.as_str())
            .and_then(|s| RecordId::new(s).ok())
        {
            keys.insert(AssertionEndpoint(id));
        }
    }
    if let Some(r) = &note.canonical {
        keys.insert(CanonicalIdentity(r.id().clone()));
        if r.kind() == RecordKind::Extraction {
            keys.insert(Extractions);
        }
        if r.kind() == RecordKind::Assertion {
            keys.insert(AssertionCandidates);
        }
        if r.kind() == RecordKind::Decision {
            if matches!(
                r.string("wiki_action"),
                Some("merge" | "split" | "add_alias" | "bind_mention")
            ) {
                keys.insert(RemapReceiptCandidates);
            }
            if r.string("wiki_status") == Some("active") {
                keys.insert(ActiveDecisionCandidates);
                let scoped = matches!(
                    r.string("wiki_action"),
                    Some("bind_mention" | "create_entity")
                );
                if scoped {
                    keys.insert(ScopedAuthorityCandidates);
                }
                if let Ok(outputs) = remap::id_list(r, "wiki_output_ids") {
                    if scoped && outputs.len() == 1 {
                        keys.insert(ScopedAuthority(outputs[0].clone()));
                    }
                    keys.extend(outputs.into_iter().map(ActiveDecisionOutput));
                }
            }
        }
    }
    Ok(keys)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_map_requires_complete_candidate_membership() {
        assert!(matches!(
            attempt_decision_policy(&BTreeMap::new(), &BTreeSet::new()).unwrap(),
            PolicyAttempt::Need(PolicyInputKey::RemapReceiptCandidates)
        ));
        assert!(matches!(
            attempt_review_policy(&BTreeMap::new(), &BTreeSet::new()).unwrap(),
            PolicyAttempt::Need(PolicyInputKey::ReviewReceiptCandidates)
        ));
    }
    #[test]
    fn certified_empty_domain_completes_with_trace() {
        let keys = BTreeSet::from([PolicyInputKey::RemapReceiptCandidates]);
        match attempt_decision_policy(&BTreeMap::new(), &keys).unwrap() {
            PolicyAttempt::Complete { result, trace } => {
                assert!(result.unwrap().is_none());
                assert_eq!(trace.keys, keys);
            }
            PolicyAttempt::Need(_) => panic!("complete empty domain requested another key"),
        }
    }
    #[test]
    fn missing_identity_requires_certificate_before_validation() {
        let id = RecordId::new("entity_missing").unwrap();
        let mut budget = ReceiptBudget::certified(Some(BTreeSet::new()));
        assert!(budget.find(&BTreeMap::new(), &id).is_err());
        assert_eq!(
            budget.into_trace().0,
            Some(PolicyInputKey::CanonicalIdentity(id.clone()))
        );
        let mut budget =
            ReceiptBudget::certified(Some(BTreeSet::from([PolicyInputKey::CanonicalIdentity(
                id,
            )])));
        assert!(
            budget
                .find(&BTreeMap::new(), &RecordId::new("entity_missing").unwrap())
                .unwrap_err()
                .message
                .contains("missing canonical record")
        );
        let (pending, trace) = budget.into_trace();
        assert!(pending.is_none());
        assert_eq!(trace.keys.len(), 1);
    }
    #[test]
    fn duplicate_identity_traces_both_witnesses() {
        use super::super::receipt_budget::tests::{note, path};
        let n = note(
            "page",
            "page_duplicate",
            serde_json::json!({"wiki_status":"reviewed"}),
            b"Page",
        );
        let notes = BTreeMap::from([(path("pages/a.md"), n.clone()), (path("pages/b.md"), n)]);
        let mut budget = ReceiptBudget::certified(None);
        assert!(
            budget
                .find(&notes, &RecordId::new("page_duplicate").unwrap())
                .unwrap_err()
                .message
                .contains("duplicate canonical ID")
        );
        assert_eq!(budget.into_trace().1.paths.len(), 2);
    }
    #[test]
    fn page_review_fence_is_candidate_and_failed_read_is_traced() {
        use super::super::receipt_budget::tests::{note, path};
        let n = note(
            "page",
            "page_review",
            serde_json::json!({"wiki_status":"reviewed"}),
            format!(
                "```{}\n{{}}\n```\n",
                super::super::review_types::GRAPH_REVIEW_FENCE
            )
            .as_bytes(),
        );
        let p = path("pages/review.md");
        assert!(
            policy_membership_keys(&p, &n)
                .unwrap()
                .contains(&PolicyInputKey::ReviewReceiptCandidates)
        );
        let notes = BTreeMap::from([(p.clone(), n)]);
        let certs = BTreeSet::from([PolicyInputKey::ReviewReceiptCandidates]);
        match attempt_review_policy(&notes, &certs).unwrap() {
            PolicyAttempt::Complete { result, trace } => {
                assert!(result.is_err());
                assert!(trace.paths.contains_key(&p));
            }
            PolicyAttempt::Need(_) => {
                panic!("wrong-record error must retain traced validation result")
            }
        }
    }
    #[test]
    fn cumulative_meter_stops_inside_candidate_scan() {
        use super::super::receipt_budget::tests::{note, path};
        let notes = BTreeMap::from([(
            path("pages/a.md"),
            note(
                "page",
                "page_a",
                serde_json::json!({"wiki_status":"reviewed"}),
                b"a",
            ),
        )]);
        let certificates = BTreeSet::from([PolicyInputKey::RemapReceiptCandidates]);
        let mut steps = 0;
        let mut meter = |work: PolicyWork| {
            steps += work.steps;
            if steps >= 2 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "test work exhausted",
                ));
            }
            Ok(())
        };
        match attempt_decision_policy_metered(&notes, &certificates, &mut meter) {
            Err(error) => assert_eq!(error.code, ErrorCode::BudgetExceeded),
            Ok(_) => panic!("candidate scan did not consult cumulative meter"),
        }
        assert_eq!(steps, 2);
    }
    #[test]
    fn observation_hashing_is_opt_in_and_deduplicated() {
        use super::super::receipt_budget::tests::{note, path};
        let n = note(
            "page",
            "page_a",
            serde_json::json!({"wiki_status":"reviewed"}),
            b"a",
        );
        let p = path("pages/a.md");
        let mut legacy = ReceiptBudget::default();
        legacy.admit(&p, &n).unwrap();
        assert!(legacy.into_trace().1.paths.is_empty());
        let mut bytes = 0;
        let mut meter = |work: PolicyWork| {
            bytes += work.bytes;
            Ok(())
        };
        let mut traced = ReceiptBudget::certified(None).with_meter(&mut meter);
        traced.observe(&p, &n).unwrap();
        traced.admit(&p, &n).unwrap();
        traced.observe(&p, &n).unwrap();
        assert_eq!(traced.into_trace().1.paths.len(), 1);
        assert_eq!(bytes, n.raw.len());
    }
}

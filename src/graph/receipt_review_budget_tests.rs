use super::*;
use crate::graph::receipt_budget::tests::{evaluate, id, note, path, unrelated};
fn fixture() -> (BTreeMap<VaultRelativePath, ParsedNote>, ReviewReceiptV1) {
    let assertion = note(
        "assertion",
        "assertion_review_budget",
        json!({"wiki_status":"rejected","wiki_subject_id":"entity_review_budget","wiki_object_id":"entity_review_budget","wiki_predicate":"uses"}),
        b"Authored proposition.",
    );
    let entity = note(
        "entity",
        "entity_review_budget",
        json!({"wiki_status":"active","wiki_entity_type":"concept"}),
        b"Identity.",
    );
    let request = normalize(ReviewRequest {
        schema: GRAPH_REVIEW_SCHEMA.into(),
        decisions: vec![AssertionReview {
            assertion_id: id("assertion_review_budget"),
            expected_hash: Blake3Hash::digest(b"prior assertion"),
            decision: ReviewDecision::Reject,
            reason: "Explicit rejected fixture".into(),
            evidence_checks: vec![],
        }],
        supersedes: vec![],
    })
    .unwrap();
    let (task_id, request_hash) = identity(&request).unwrap();
    let receipt = ReviewReceiptV1 {
        schema: GRAPH_REVIEW_RECEIPT_SCHEMA.into(),
        task_id,
        request,
        request_hash,
        created_at: "2026-10-03T00:00:00Z".into(),
        allocations: ReviewAllocations {
            decisions: BTreeMap::from([(
                id("assertion_review_budget"),
                id("decision_review_budget"),
            )]),
            successors: BTreeMap::new(),
        },
        record_paths: BTreeMap::from([
            (id("assertion_review_budget"), path("assertion.md")),
            (id("decision_review_budget"), path("decision.md")),
        ]),
        assertion_proofs: vec![ReviewAssertionProof {
            assertion_id: id("assertion_review_budget"),
            before_status: ReviewedAssertionStatus::Proposed,
            proposition: proposition(assertion.canonical.as_ref().unwrap()).unwrap(),
            before_active_evidence: BTreeMap::new(),
            governing_decision_id: id("decision_review_budget"),
        }],
        evidence_proofs: vec![],
        successor_templates: BTreeMap::new(),
        predecessors: vec![],
        supersessions: vec![],
        operations: vec![],
    };
    shape(&receipt).unwrap();
    let decision = parse_note(&decision_bytes(&receipt, &receipt.request.decisions[0]).unwrap());
    (
        BTreeMap::from([
            (path("assertion.md"), assertion),
            (path("decision.md"), decision),
            (path("entity.md"), entity),
        ]),
        receipt,
    )
}
#[test]
fn real_review_policy_and_eligibility_ignore_4100_unrelated_pages() {
    let (mut notes, _) = fixture();
    let before = verify_review_policy(&notes).unwrap().unwrap();
    let expected = evaluate(&notes, true)
        .unwrap()
        .remove(&id("decision_review_budget"))
        .unwrap();
    let proof_bytes =
        notes[&path("assertion.md")].raw.len() + notes[&path("decision.md")].raw.len();
    unrelated(&mut notes);
    let mut scope = ReceiptBudget::limited(2, proof_bytes);
    let after = verify_review_policy_scoped(&notes, &mut scope)
        .unwrap()
        .unwrap();
    assert_eq!(before.supersession_edges(), after.supersession_edges());
    assert_eq!(scope.usage(), (2, proof_bytes));
    for normalized in [false, true] {
        assert_eq!(
            evaluate(&notes, normalized).unwrap()[&id("decision_review_budget")],
            expected
        );
    }
    assert_eq!(
        verify_review_policy_scoped(&notes, &mut ReceiptBudget::limited(2, proof_bytes - 1))
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
}
#[test]
fn review_discovery_preserves_malformed_and_contradictory_copies() {
    let (mut notes, mut receipt) = fixture();
    let original = notes[&path("decision.md")].clone();
    notes.insert(path("copy.md"), original);
    // All copies are charged even though equal task receipts deduplicate.
    assert_eq!(
        receipts_scoped(&notes, &mut ReceiptBudget::limited(1, usize::MAX))
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(receipts(&notes).unwrap().len(), 1);
    receipt.request.decisions[0].reason = "Changed rationale".into();
    let (task, hash) = identity(&receipt.request).unwrap();
    assert_eq!(task, receipt.task_id);
    receipt.request_hash = hash;
    notes.insert(
        path("copy.md"),
        parse_note(&decision_bytes(&receipt, &receipt.request.decisions[0]).unwrap()),
    );
    assert!(
        receipts(&notes)
            .unwrap_err()
            .message
            .contains("copies disagree")
    );
    let malformed = note(
        "page",
        "page_wrong_receipt",
        json!({"wiki_status":"reviewed"}),
        format!("```{GRAPH_REVIEW_FENCE}\nnot JSON\n```\n").as_bytes(),
    );
    let mut only = BTreeMap::from([(path("wrong.md"), malformed)]);
    let expected = verify_review_policy(&only).unwrap_err();
    unrelated(&mut only);
    let actual = verify_review_policy(&only).unwrap_err();
    assert_eq!(actual.code, expected.code);
    assert_eq!(actual.message, expected.message);
}
#[test]
fn review_receipt_exhaustion_never_becomes_a_cached_semantic_failure() {
    let (mut notes, _) = fixture();
    let receipt = notes[&path("decision.md")].clone();
    for n in 0..4096 {
        notes.insert(path(&format!("copies/{n}.md")), receipt.clone());
    }
    for normalized in [false, true] {
        assert_eq!(
            evaluate(&notes, normalized).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }
}

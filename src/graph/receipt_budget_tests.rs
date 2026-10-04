use super::*;
use crate::{
    catalog::{RecordRow, eligibility},
    changes::{ScanDocument, ValidationInput},
    domain::{Blake3Hash, CanonicalRecord, Eligibility},
    graph::{decision_types::*, packet, remap},
    records::parse_note,
    sources::SourceView,
    vault::{VaultFs, VaultRoot},
};
use serde_json::{Value, json};
pub(crate) fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
pub(crate) fn path(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
pub(crate) fn note(kind: &str, name: &str, extra: Value, body: &[u8]) -> ParsedNote {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_kind".into(), json!(kind)),
        ("wiki_id".into(), json!(name)),
        ("title".into(), json!(name)),
    ]);
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    CanonicalRecord::new(fields.clone()).unwrap();
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields {
        bytes.extend(format!("{key}: {value}\n").as_bytes());
    }
    bytes.extend(b"---\n");
    bytes.extend(body);
    parse_note(&bytes)
}
pub(crate) fn unrelated(notes: &mut BTreeMap<VaultRelativePath, ParsedNote>) {
    for n in 0..4100 {
        notes.insert(
            path(&format!("pages/unrelated_{n}.md")),
            note(
                "page",
                &format!("page_unrelated_{n}"),
                json!({"wiki_status":"reviewed"}),
                b"Unrelated ordinary Markdown.",
            ),
        );
    }
}
fn receipt_note(receipt: &EntityDecisionReceiptV1) -> ParsedNote {
    note(
        "decision",
        "decision_alias",
        json!({"wiki_status":"active","wiki_action":"add_alias","wiki_input_ids":["entity_receipt"],"wiki_output_ids":["entity_receipt"],"wiki_created_at":"2026-10-03T00:00:00Z"}),
        &packet::render_fence(
            receipt,
            ENTITY_DECISION_FENCE,
            MAX_ENTITY_DECISION_RECEIPT_BYTES,
        )
        .unwrap(),
    )
}
fn fixture() -> (
    BTreeMap<VaultRelativePath, ParsedNote>,
    EntityDecisionReceiptV1,
) {
    let entity = note(
        "entity",
        "entity_receipt",
        json!({"wiki_status":"active","wiki_entity_type":"concept","aliases":["Known alias"]}),
        b"Explicit identity.",
    );
    let request = remap::normalize(EntityDecisionRequest {
        schema: ENTITY_DECISIONS_SCHEMA.into(),
        decisions: vec![EntityDecision::AddAlias {
            reason: "Explicit alias fixture".into(),
            expected_records: vec![ExpectedRecord {
                record_id: id("entity_receipt"),
                hash: Blake3Hash::digest(b"before"),
            }],
            remaps: vec![],
            entity_id: id("entity_receipt"),
            alias: "Known alias".into(),
        }],
    })
    .unwrap();
    let (task_id, request_hash) = remap::identity(&request).unwrap();
    let receipt = EntityDecisionReceiptV1 {
        schema: ENTITY_DECISION_RECEIPT_SCHEMA.into(),
        task_id,
        request,
        request_hash,
        allocations: vec![EntityDecisionAllocation {
            decision_id: id("decision_alias"),
            entities: BTreeMap::new(),
            mention_decisions: vec![],
        }],
        record_paths: BTreeMap::from([
            (id("entity_receipt"), path("entities/entity.md")),
            (id("decision_alias"), path("decisions/alias.md")),
        ]),
        extraction_proofs: vec![],
        assertion_proofs: vec![],
        main_supersessions: vec![],
        operations: vec![],
    };
    remap::shape(&receipt).unwrap();
    (
        BTreeMap::from([
            (path("entities/entity.md"), entity),
            (path("decisions/alias.md"), receipt_note(&receipt)),
        ]),
        receipt,
    )
}
pub(crate) fn evaluate(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    normalized: bool,
) -> Result<BTreeMap<RecordId, RecordRow>> {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("WIKI.md"),b"---\nwiki_schema: '1'\nwiki_kind: vault\nwiki_id: vault_budget\ntitle: Budget fixture\n---\n").unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let input = ValidationInput {
        vault_id: id("vault_budget"),
        documents: notes
            .iter()
            .map(|(path, note)| ScanDocument {
                path: path.clone(),
                hash: note.source_hash.clone(),
                bytes: note.raw.clone(),
            })
            .collect(),
        overlay: vec![],
    };
    let view = SourceView::from_closed_input(&fs, &input)?;
    let mut rows = notes
        .iter()
        .filter_map(|(path, note)| {
            note.canonical.as_ref().map(|record| {
                (
                    record.id().clone(),
                    RecordRow {
                        record: record.clone(),
                        path: path.clone(),
                        hash: note.source_hash.clone(),
                        authored_status: record.string("wiki_status").map(str::to_owned),
                        eligibility: Eligibility::Current,
                        reasons: vec![],
                        identity_eligibility: None,
                        description_eligibility: None,
                        disputed: false,
                        dependencies: vec![],
                    },
                )
            })
        })
        .collect();
    if normalized {
        eligibility::compute_normalized(&view, notes, &mut rows, &mut vec![])?;
    } else {
        eligibility::compute(&view, notes, &mut rows, &mut vec![])?;
    }
    Ok(rows)
}
#[test]
fn real_remap_policy_and_eligibility_ignore_4100_unrelated_pages() {
    let (mut notes, _) = fixture();
    let before = remap::verify_decision_policy(&notes).unwrap().unwrap();
    let before_row = evaluate(&notes, true)
        .unwrap()
        .remove(&id("decision_alias"))
        .unwrap();
    let proof_bytes: usize = notes.values().map(|n| n.raw.len()).sum();
    unrelated(&mut notes);
    let mut budget = ReceiptBudget::limited(2, proof_bytes);
    let after = remap::verify_decision_policy_scoped(&notes, &mut budget)
        .unwrap()
        .unwrap();
    assert_eq!(before.supersession_edges(), after.supersession_edges());
    assert!(after.historical_alias_authority(&id("decision_alias")));
    assert_eq!(budget.usage(), (2, proof_bytes));
    for normalized in [false, true] {
        assert_eq!(
            evaluate(&notes, normalized).unwrap()[&id("decision_alias")],
            before_row
        );
    }
    assert_eq!(
        remap::verify_decision_policy_scoped(
            &notes,
            &mut ReceiptBudget::limited(2, proof_bytes - 1)
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
}
#[test]
fn no_receipts_and_malformed_receipts_keep_results_after_unrelated_growth() {
    let mut notes = BTreeMap::new();
    unrelated(&mut notes);
    assert!(
        remap::verify_decision_policy_scoped(&notes, &mut ReceiptBudget::limited(0, 0))
            .unwrap()
            .is_none()
    );
    let malformed = note(
        "decision",
        "decision_bad",
        json!({"wiki_status":"active","wiki_action":"add_alias","wiki_input_ids":["entity_receipt"],"wiki_output_ids":["entity_receipt"],"wiki_created_at":"2026-10-03T00:00:00Z"}),
        format!("```{ENTITY_DECISION_FENCE}\nnot JSON\n```\n").as_bytes(),
    );
    let small = BTreeMap::from([(path("bad.md"), malformed.clone())]);
    let expected = remap::verify_decision_policy(&small).unwrap_err();
    notes.insert(path("bad.md"), malformed);
    let actual = remap::verify_decision_policy(&notes).unwrap_err();
    assert_eq!(actual.code, expected.code);
    assert_eq!(actual.message, expected.message);
}
#[test]
fn actual_receipt_copies_count_and_contradictions_are_not_filtered() {
    let (mut notes, receipt) = fixture();
    notes.insert(path("decisions/copy.md"), receipt_note(&receipt));
    // Both physical receipt notes consume allowance before ID duplicate checks.
    let error =
        remap::verify_decision_policy_scoped(&notes, &mut ReceiptBudget::limited(1, usize::MAX))
            .unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert!(
        remap::verify_decision_policy(&notes)
            .unwrap_err()
            .message
            .contains("duplicate canonical ID")
    );
    let mut changed = receipt.clone();
    let EntityDecision::AddAlias { reason, .. } = &mut changed.request.decisions[0] else {
        unreachable!()
    };
    *reason = "Different rationale".into();
    let (task, hash) = remap::identity(&changed.request).unwrap();
    assert_eq!(task, changed.task_id);
    changed.request_hash = hash;
    notes.insert(path("decisions/copy.md"), receipt_note(&changed));
    assert!(
        remap::verify_decision_policy(&notes)
            .unwrap_err()
            .message
            .contains("contradictory")
    );
}
#[test]
fn relevant_count_exhaustion_is_an_operation_error_in_both_eligibility_modes() {
    let (mut notes, receipt) = fixture();
    let canonical = receipt_note(&receipt);
    for n in 0..4096 {
        notes.insert(path(&format!("decisions/copy_{n}.md")), canonical.clone());
    }
    for normalized in [false, true] {
        assert_eq!(
            evaluate(&notes, normalized).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }
}
#[test]
fn repeated_proof_reads_charge_once_and_rejected_admission_does_not_mutate_usage() {
    let (notes, _) = fixture();
    let proof = &notes[&path("entities/entity.md")];
    let mut budget = ReceiptBudget::limited(1, proof.raw.len());
    budget.find(&notes, &id("entity_receipt")).unwrap();
    budget.find(&notes, &id("entity_receipt")).unwrap();
    assert_eq!(budget.usage(), (1, proof.raw.len()));
    assert_eq!(
        budget.find(&notes, &id("decision_alias")).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(budget.usage(), (1, proof.raw.len()));
}

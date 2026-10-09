use super::*;
use crate::{
    domain::{Blake3Hash, RecordId, Result},
    graph::{
        decision_types::EntityDecision,
        policy_inputs::{self, PolicyAttempt, PolicyInputTrace},
        receipt_budget::tests::{fixture, note, path, receipt_note},
        remap,
    },
};
use serde_json::json;
use std::collections::BTreeMap;

fn bind(body: &[u8]) -> ParsedNote {
    note(
        "decision",
        "decision_probe_bind",
        json!({"wiki_status":"active","wiki_action":"bind_mention",
            "wiki_extraction_id":"extraction_probe","wiki_mention_ids":["m_probe"],
            "wiki_input_ids":["extraction_probe"],"wiki_output_ids":["entity_probe"],
            "wiki_created_at":"2026-10-03T00:00:00Z"}),
        body,
    )
}
fn evaluate(
    selected: RemapInputProbeMode,
    notes: &BTreeMap<crate::domain::VaultRelativePath, ParsedNote>,
) -> (
    Result<Option<remap::VerifiedDecisionPolicy>>,
    PolicyInputTrace,
) {
    with_mode(
        selected,
        || match policy_inputs::evaluate_decision_policy(notes).unwrap() {
            PolicyAttempt::Complete { result, trace } => (result, trace),
            PolicyAttempt::Need(_) => panic!("complete notes need no certificate"),
        },
    )
}

#[test]
fn probe_scope_restores_on_return_and_unwind() {
    assert_eq!(mode(), RemapInputProbeMode::Current);
    with_mode(RemapInputProbeMode::Precise, || {
        assert_eq!(mode(), RemapInputProbeMode::Precise);
    });
    let failure = std::panic::catch_unwind(|| {
        with_mode(RemapInputProbeMode::Precise, || panic!("scope probe"));
    });
    assert!(failure.is_err());
    assert_eq!(mode(), RemapInputProbeMode::Current);
}

#[test]
fn resolution_only_bind_is_excluded_and_fence_gain_loss_is_syntactic() {
    let ordinary = bind(b"```lwiki-resolution-v1\nnot JSON\n```\n");
    assert_eq!(classify(&ordinary), RemapInputWitness::Excluded);
    let fenced = bind(format!("```{ENTITY_DECISION_FENCE}\nnot JSON\n```\n").as_bytes());
    assert_eq!(classify(&fenced), RemapInputWitness::Fence);
    assert_eq!(classify(&ordinary), RemapInputWitness::Excluded);
    let p = path("decisions/bind.md");
    let notes = BTreeMap::from([(p.clone(), ordinary)]);
    for selected in [RemapInputProbeMode::Current, RemapInputProbeMode::Precise] {
        let (result, trace) = evaluate(selected, &notes);
        assert!(result.unwrap().is_none());
        assert_eq!(
            trace.paths.contains_key(&p),
            selected == RemapInputProbeMode::Current
        );
    }
}

#[test]
fn marker_absent_legacy_non_utf8_preserves_error_and_trace() {
    // parse_note rejects whole-file nonUTF8. Preserve the adversarial internal
    // canonical-note invariant explicitly rather than claiming public parsing.
    let mut n = bind(b"ordinary body\n");
    n.raw.push(0xff);
    n.source_hash = Blake3Hash::digest(&n.raw);
    assert_eq!(classify(&n), RemapInputWitness::LegacySyntaxError);
    let p = path("decisions/nonutf8.md");
    let notes = BTreeMap::from([(p.clone(), n)]);
    let (current, old_trace) = evaluate(RemapInputProbeMode::Current, &notes);
    let (precise, trace) = evaluate(RemapInputProbeMode::Precise, &notes);
    let current = current.unwrap_err();
    let precise = precise.unwrap_err();
    assert_eq!(precise.code, current.code);
    assert_eq!(precise.message, current.message);
    assert!(precise.message.contains("decision body not UTF8"));
    assert_eq!(trace, old_trace);
    assert!(trace.paths.contains_key(&p));
}

#[test]
fn malformed_json_and_multiple_fences_are_observed_failures() {
    let (_, receipt) = fixture();
    let valid = receipt_note(&receipt);
    let twice = [valid.body(), b"\n", valid.body()].concat();
    for body in [
        format!("```{ENTITY_DECISION_FENCE}\nnot JSON\n```\n").into_bytes(),
        twice,
    ] {
        let p = path("decisions/bad.md");
        let notes = BTreeMap::from([(p.clone(), bind(&body))]);
        assert_eq!(classify(&notes[&p]), RemapInputWitness::Fence);
        let (current, _) = evaluate(RemapInputProbeMode::Current, &notes);
        let (precise, trace) = evaluate(RemapInputProbeMode::Precise, &notes);
        let current = current.unwrap_err();
        let precise = precise.unwrap_err();
        assert_eq!(precise.code, current.code);
        assert_eq!(precise.message, current.message);
        assert!(trace.paths.contains_key(&p));
    }
}

#[test]
fn raw_fence_wrong_kind_action_and_noncanonical_are_strengthened_failures() {
    let (base, receipt) = fixture();
    let valid = receipt_note(&receipt);
    let wrong_kind = note(
        "page",
        "page_probe",
        json!({"wiki_status":"reviewed"}),
        valid.body(),
    );
    let wrong_action = note(
        "decision",
        "decision_alias",
        json!({"wiki_status":"active","wiki_action":"merge",
            "wiki_input_ids":["entity_receipt"],"wiki_output_ids":["entity_receipt"],
            "wiki_created_at":"2026-10-03T00:00:00Z"}),
        valid.body(),
    );
    let noncanonical = crate::records::parse_note(valid.body());
    for (witness, current_success) in [
        (wrong_kind, true),
        (wrong_action, false),
        (noncanonical, true),
        (bind(valid.body()), true),
    ] {
        assert_eq!(classify(&witness), RemapInputWitness::Fence);
        let p = path("decisions/unallocated.md");
        let mut notes = base.clone();
        notes.insert(p.clone(), witness);
        assert_eq!(
            evaluate(RemapInputProbeMode::Current, &notes).0.is_ok(),
            current_success,
            "record the actual baseline instead of claiming equivalence"
        );
        let (result, trace) = evaluate(RemapInputProbeMode::Precise, &notes);
        assert!(result.unwrap_err().message.contains("receipt witness"));
        assert!(trace.paths.contains_key(&p));
    }
}

#[test]
fn duplicate_allocated_identity_keeps_both_witnesses_and_failure() {
    let (base, receipt) = fixture();
    assert!(
        evaluate(RemapInputProbeMode::Precise, &base)
            .0
            .unwrap()
            .is_some()
    );
    let allocated = &receipt.allocations[0].decision_id;
    let original = receipt.record_paths[allocated].clone();
    let mut notes = base.clone();
    // Retain a genuine receipt copy with its same allocated identity. Removing
    // the original member's fence cannot turn its receipt family into absence.
    notes.insert(path("decisions/copy.md"), receipt_note(&receipt));
    let (result, trace) = evaluate(RemapInputProbeMode::Precise, &notes);
    assert!(
        result
            .unwrap_err()
            .message
            .contains("duplicate canonical ID")
    );
    assert!(trace.paths.contains_key(&original));
    assert!(trace.paths.contains_key(&path("decisions/copy.md")));
    assert_eq!(allocated, &RecordId::new("decision_alias").unwrap());
}

#[test]
fn one_missing_fence_in_a_valid_two_allocation_alias_family_is_not_filtered() {
    let (mut notes, mut receipt) = fixture();
    // Extend the existing valid AddAlias fixture with a disjoint second alias
    // allocation, so another valid receipt witness survives without duplicate ID.
    let other = RecordId::new("entity_receipt_other").unwrap();
    let other_decision = RecordId::new("decision_alias_other").unwrap();
    let mut op = receipt.request.decisions[0].clone();
    let EntityDecision::AddAlias {
        entity_id,
        expected_records,
        ..
    } = &mut op
    else {
        unreachable!()
    };
    *entity_id = other.clone();
    expected_records[0].record_id = other.clone();
    receipt.request.decisions.push(op);
    receipt.request = remap::normalize(receipt.request).unwrap();
    (receipt.task_id, receipt.request_hash) = remap::identity(&receipt.request).unwrap();
    let mut allocation = receipt.allocations[0].clone();
    allocation.decision_id = other_decision.clone();
    receipt.allocations.push(allocation);
    receipt
        .record_paths
        .insert(other.clone(), path("entities/other.md"));
    receipt
        .record_paths
        .insert(other_decision.clone(), path("decisions/other.md"));
    remap::shape(&receipt).unwrap();
    notes.insert(
        path("entities/other.md"),
        note(
            "entity",
            other.as_str(),
            json!({"wiki_status":"active","wiki_entity_type":"concept","aliases":["Known alias"]}),
            b"Explicit other identity.\n",
        ),
    );
    let main = receipt_note(&receipt);
    notes.insert(path("decisions/alias.md"), main.clone());
    notes.insert(
        path("decisions/other.md"),
        note(
            "decision",
            other_decision.as_str(),
            json!({"wiki_status":"active","wiki_action":"add_alias",
            "wiki_input_ids":[other],"wiki_output_ids":[other],
            "wiki_created_at":"2026-10-03T00:00:00Z"}),
            main.body(),
        ),
    );
    for selected in [RemapInputProbeMode::Current, RemapInputProbeMode::Precise] {
        assert!(evaluate(selected, &notes).0.unwrap().is_some());
    }
    let replacement = note(
        "decision",
        "decision_alias",
        json!({"wiki_status":"active","wiki_action":"add_alias",
            "wiki_input_ids":["entity_receipt"],"wiki_output_ids":["entity_receipt"],
            "wiki_created_at":"2026-10-03T00:00:00Z"}),
        b"removed receipt fence\n",
    );
    notes.insert(path("decisions/alias.md"), replacement);
    let (result, trace) = evaluate(RemapInputProbeMode::Precise, &notes);
    assert!(
        result
            .unwrap_err()
            .message
            .contains("requires one actual extraction artifact fence")
    );
    assert!(trace.paths.contains_key(&path("decisions/alias.md")));
    assert!(trace.paths.contains_key(&path("decisions/other.md")));
}

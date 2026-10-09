use super::*;
use crate::graph::ExpectedRecord;
use crate::graph::receipt_budget::tests::{id, note, path};

fn fixture(
    count: usize,
    evidence_per_assertion: usize,
) -> (
    BTreeMap<VaultRelativePath, ParsedNote>,
    ReviewReceiptV1,
    CanonicalGraphOriginV2,
) {
    let mut notes = BTreeMap::new();
    let mut decisions = vec![];
    let mut allocations = BTreeMap::new();
    let mut paths = BTreeMap::new();
    let mut assertions = vec![];
    let mut evidence = vec![];
    for a in 0..count {
        let aid = id(&format!("assertion_shared_{a:02}"));
        let did = id(&format!("decision_shared_{a:02}"));
        let ap = path(&format!("assertion_{a:02}.md"));
        let assertion = note(
            "assertion",
            aid.as_str(),
            json!({
                "wiki_status":"rejected", "wiki_subject_id":"entity_shared",
                "wiki_object_id":"entity_shared", "wiki_predicate":"uses"
            }),
            b"Authored proposition.\n",
        );
        let mut checks = vec![];
        for e in 0..evidence_per_assertion {
            let eid = id(&format!("evidence_shared_{a:02}_{e:02}"));
            let hash = Blake3Hash::digest(format!("Evidence {a}/{e}"));
            checks.push(EvidenceCheck {
                evidence_id: eid.clone(),
                expected_hash: hash.clone(),
                assessment: EvidenceAssessment::Supports,
            });
            paths.insert(eid.clone(), path(&format!("evidence_{a:02}_{e:02}.md")));
            evidence.push(ReviewEvidenceProof {
                evidence_id: eid,
                assertion_id: aid.clone(),
                before_stance: EvidenceStance::Supports,
                assessment: EvidenceAssessment::Supports,
                after_status: ReviewedEvidenceStatus::Active,
                source: ReviewSourceProof {
                    source_id: id("source_shared"),
                    revision_id: id("revision_shared"),
                    revision_hash: hash.clone(),
                    original_path: path("sources/shared/original.txt"),
                    original_hash: hash.clone(),
                    content_path: path("sources/shared/content.txt"),
                    content_hash: hash.clone(),
                    span: ByteSpan::new(0, 1).unwrap(),
                    quote_hash: hash.clone(),
                },
                extraction_id: None,
                invariant_fields_hash: hash.clone(),
                body_hash: hash,
            });
        }
        assertions.push(ReviewAssertionProof {
            assertion_id: aid.clone(),
            before_status: ReviewedAssertionStatus::Proposed,
            proposition: proposition(record(&assertion).unwrap()).unwrap(),
            before_active_evidence: checks
                .iter()
                .map(|c| (c.evidence_id.clone(), c.expected_hash.clone()))
                .collect(),
            governing_decision_id: did.clone(),
        });
        decisions.push(AssertionReview {
            assertion_id: aid.clone(),
            expected_hash: Blake3Hash::digest(format!("Prior assertion {a}")),
            decision: ReviewDecision::Reject,
            reason: format!("Complete explicit review {a}"),
            evidence_checks: checks,
        });
        allocations.insert(aid.clone(), did.clone());
        paths.insert(aid, ap.clone());
        paths.insert(did, path(&format!("decision_{a:02}.md")));
        notes.insert(ap, assertion);
    }
    if !evidence.is_empty() {
        paths.insert(id("source_shared"), path("source.md"));
        paths.insert(id("revision_shared"), path("revision.md"));
    }
    let request = normalize(ReviewRequest {
        schema: GRAPH_REVIEW_SCHEMA.into(),
        decisions,
        supersedes: vec![],
    })
    .unwrap();
    let (task_id, request_hash) = identity(&request).unwrap();
    let receipt = ReviewReceiptV1 {
        schema: GRAPH_REVIEW_RECEIPT_SCHEMA.into(),
        task_id,
        request,
        request_hash,
        created_at: "2026-10-09T00:00:00Z".into(),
        allocations: ReviewAllocations {
            decisions: allocations,
            successors: BTreeMap::new(),
        },
        record_paths: paths,
        assertion_proofs: assertions,
        evidence_proofs: evidence,
        successor_templates: BTreeMap::new(),
        predecessors: vec![],
        supersessions: vec![],
        operations: vec![],
    };
    shape(&receipt).unwrap();
    let origin = origin(&receipt);
    install(&mut notes, &receipt, &origin);
    (notes, receipt, origin)
}
fn origin(receipt: &ReviewReceiptV1) -> CanonicalGraphOriginV2 {
    CanonicalGraphOriginV2::for_task(
        id("vault_shared"),
        GraphOperationFamily::AssertionReview,
        &receipt.task_id,
        Blake3Hash::digest(packet::canonical_json(receipt).unwrap()),
    )
    .unwrap()
}
fn install(
    notes: &mut BTreeMap<VaultRelativePath, ParsedNote>,
    receipt: &ReviewReceiptV1,
    origin: &CanonicalGraphOriginV2,
) {
    for (id, bytes) in render_decisions_v2(receipt, origin).unwrap() {
        notes.insert(receipt.record_paths[&id].clone(), parse_note(&bytes));
    }
}
fn collect(notes: &BTreeMap<VaultRelativePath, ParsedNote>) -> Result<CollectedReviewReceipts> {
    collect_review_receipts_v2(notes, &mut ReceiptBudget::default())
}
fn replace_reference(
    notes: &mut BTreeMap<VaultRelativePath, ParsedNote>,
    receipt: &ReviewReceiptV1,
    origin: &CanonicalGraphOriginV2,
    mutate: impl FnOnce(&mut ReviewReferenceV2),
) {
    let c = carrier(receipt, origin).unwrap();
    let mut r = reference(&c, &Blake3Hash::digest(packet::canonical_json(&c).unwrap()));
    mutate(&mut r);
    let d = receipt
        .request
        .decisions
        .iter()
        .find(|d| receipt.allocations.decisions[&d.assertion_id] != c.carrier_id)
        .unwrap();
    let proof = packet::render_fence(&r, REVIEW_REFERENCE_FENCE_V2, MAX_REFERENCE_BYTES).unwrap();
    notes.insert(
        receipt.record_paths[&receipt.allocations.decisions[&d.assertion_id]].clone(),
        parse_note(&decision_with_proof(receipt, d, &proof).unwrap()),
    );
}

#[test]
fn shared_family_preserves_v1_semantics_and_full_16_by_256_payload() {
    let (v2, receipt, origin) = fixture(16, 16);
    let collected = collect(&v2).unwrap();
    assert_eq!(collected.receipts[&receipt.task_id], receipt);
    assert_eq!(receipt.evidence_proofs.len(), 256);
    collected.bind_vault(&id("vault_shared")).unwrap();
    assert!(collected.bind_vault(&id("vault_foreign")).is_err());
    let mut v1 = v2.clone();
    let mut bytes_v1 = 0;
    let mut bytes_v2 = 0;
    let mut carriers = 0;
    for d in &receipt.request.decisions {
        let p = &receipt.record_paths[&receipt.allocations.decisions[&d.assertion_id]];
        let old = decision_bytes(&receipt, d).unwrap();
        let shared = render_decision_v2(&receipt, &origin, d).unwrap();
        assert_eq!(v2[p].raw, shared);
        bytes_v1 += old.len();
        bytes_v2 += shared.len();
        carriers += usize::from(fences(&v2[p])[0] == REVIEW_CARRIER_FENCE_V2);
        v1.insert(p.clone(), parse_note(&old));
    }
    assert_eq!(carriers, 1);
    assert!(bytes_v2 * 8 < bytes_v1, "v1={bytes_v1}, v2={bytes_v2}");
    assert_eq!(
        verify_review_policy(&v1)
            .unwrap()
            .unwrap()
            .supersession_edges(),
        verify_review_policy_for_vault(&v2, &id("vault_shared"))
            .unwrap()
            .unwrap()
            .supersession_edges()
    );
    assert_eq!(collect(&v1).unwrap().receipts, collected.receipts);
    // The authenticated payload and semantic inputs are charged once, and each
    // actual tiny reference is still observed/admitted, never manufactured.
    let bytes: usize = v2.values().map(|n| n.raw.len()).sum();
    let mut scope = ReceiptBudget::limited(v2.len(), bytes);
    scope.bind_vault(&id("vault_shared")).unwrap();
    verify_review_policy_scoped(&v2, &mut scope).unwrap();
    assert_eq!(scope.usage(), (v2.len(), bytes));
}

#[test]
fn missing_duplicate_wrong_path_and_unallocated_carriers_fail() {
    let (notes, receipt, _) = fixture(2, 0);
    let carrier_id = receipt.allocations.decisions.values().min().unwrap();
    let carrier_path = &receipt.record_paths[carrier_id];
    let reference_path = receipt
        .allocations
        .decisions
        .values()
        .find(|id| *id != carrier_id)
        .map(|id| &receipt.record_paths[id])
        .unwrap();
    for removed in [carrier_path, reference_path] {
        let mut bad = notes.clone();
        bad.remove(removed);
        assert!(collect(&bad).is_err());
    }
    for copied in [carrier_path, reference_path] {
        let mut bad = notes.clone();
        bad.insert(path("duplicate.md"), notes[copied].clone());
        assert!(collect(&bad).is_err());
        let mut bad = notes.clone();
        let n = bad.remove(copied).unwrap();
        bad.insert(path("moved.md"), n);
        assert!(collect(&bad).is_err());
    }
    let mut bad = notes.clone();
    let n = &notes[carrier_path];
    // Deliberate raw identity tamper bypasses the ordinary editor's correct
    // refusal to change identity without an explicit migration.
    let mut fields = record(n).unwrap().fields().clone();
    fields.insert("wiki_id".into(), json!("decision_unallocated"));
    let edited =
        crate::sources::revision::record_bytes(CanonicalRecord::new(fields).unwrap(), n.body())
            .unwrap();
    bad.insert(carrier_path.clone(), parse_note(&edited));
    assert!(collect(&bad).is_err());
    // Duplicate identity with an ordinary, fence-free Decision also conflicts.
    let mut bad = notes.clone();
    let d = &receipt.request.decisions[1];
    bad.insert(
        path("plain_duplicate.md"),
        parse_note(&decision_with_proof(&receipt, d, b"").unwrap()),
    );
    assert!(collect(&bad).is_err());
}

#[test]
fn reference_authenticates_hash_scope_task_carrier_and_path() {
    let (notes, receipt, origin) = fixture(2, 0);
    let mutations: Vec<Box<dyn FnOnce(&mut ReviewReferenceV2)>> = vec![
        Box::new(|r| r.payload_hash = Blake3Hash::digest(b"wrong payload")),
        Box::new(|r| r.origin_scope_id = id("graph_wrong_scope")),
        Box::new(|r| r.task_id = id("review_wrong_task")),
        Box::new(|r| r.carrier_id = id("decision_wrong_carrier")),
        Box::new(|r| r.carrier_path = path("wrong.md")),
        Box::new(|r| r.schema = "wrong.schema".into()),
    ];
    for mutation in mutations {
        let mut bad = notes.clone();
        replace_reference(&mut bad, &receipt, &origin, mutation);
        assert!(collect(&bad).is_err());
    }
    let mut invalid = origin.clone();
    invalid.family = GraphOperationFamily::MentionResolve;
    assert!(render_decisions_v2(&receipt, &invalid).is_err());
    invalid = origin.clone();
    invalid.semantic_hash = Blake3Hash::digest(b"different semantics");
    assert!(render_decisions_v2(&receipt, &invalid).is_err());
}

#[test]
fn carrier_authenticates_semantic_origin_and_payload_kind() {
    let (notes, receipt, origin) = fixture(2, 0);
    let mutations: Vec<Box<dyn FnOnce(&mut ReviewCarrierV2)>> = vec![
        Box::new(|c| c.schema = REVIEW_REFERENCE_SCHEMA_V2.into()),
        Box::new(|c| c.origin.version = 1),
        Box::new(|c| c.origin.family = GraphOperationFamily::MentionResolve),
        Box::new(|c| c.origin.scope_id = id("graph_wrong_scope")),
        Box::new(|c| c.origin.semantic_hash = Blake3Hash::digest(b"wrong semantics")),
        Box::new(|c| c.carrier_id = id("decision_unallocated")),
        Box::new(|c| c.carrier_path = path("wrong.md")),
        // Semantically valid v1 bytes with an unchanged origin must fail, too.
        Box::new(|c| c.receipt.created_at = "2026-10-10T00:00:00Z".into()),
    ];
    for mutate in mutations {
        let mut c = carrier(&receipt, &origin).unwrap();
        mutate(&mut c);
        let mut bad = notes.clone();
        let d = receipt
            .request
            .decisions
            .iter()
            .find(|d| {
                &receipt.allocations.decisions[&d.assertion_id]
                    == receipt.allocations.decisions.values().min().unwrap()
            })
            .unwrap();
        let proof = packet::render_fence(&c, REVIEW_CARRIER_FENCE_V2, MAX_CARRIER_BYTES).unwrap();
        bad.insert(
            receipt.record_paths[&receipt.allocations.decisions[&d.assertion_id]].clone(),
            parse_note(&decision_with_proof(&receipt, d, &proof).unwrap()),
        );
        assert!(collect(&bad).is_err());
    }
    // A valid Decision carrying the other wire kind is not accepted as a ref.
    let c = carrier(&receipt, &origin).unwrap();
    let d = &receipt.request.decisions[1];
    let proof = packet::render_fence(&c, REVIEW_REFERENCE_FENCE_V2, MAX_CARRIER_BYTES).unwrap();
    let mut bad = notes;
    bad.insert(
        receipt.record_paths[&receipt.allocations.decisions[&d.assertion_id]].clone(),
        parse_note(&decision_with_proof(&receipt, d, &proof).unwrap()),
    );
    assert!(collect(&bad).is_err());
}

#[test]
fn malformed_wrong_kind_and_multiple_fences_are_global_witnesses() {
    for fence in [
        GRAPH_REVIEW_FENCE,
        REVIEW_CARRIER_FENCE_V2,
        REVIEW_REFERENCE_FENCE_V2,
    ] {
        for kind in ["page", "decision"] {
            let n = note(
                kind,
                "record_malformed",
                if kind == "decision" {
                    json!({"wiki_status":"active", "wiki_action":"reject", "wiki_created_at":"2026-10-09T00:00:00Z", "wiki_input_ids":["assertion_shared_00"], "wiki_output_ids":["assertion_shared_00"]})
                } else {
                    json!({"wiki_status":"reviewed"})
                },
                format!("```{fence}\nnot JSON\n```\n").as_bytes(),
            );
            assert!(has_fence(&n));
            assert!(verify_review_policy(&BTreeMap::from([(path("malformed.md"), n)])).is_err());
        }
    }
    let (mut notes, receipt, _) = fixture(2, 0);
    let p = &receipt.record_paths[receipt.allocations.decisions.values().min().unwrap()];
    let n = &notes[p];
    let mut body = n.body().to_vec();
    body.extend(format!("\n```{REVIEW_REFERENCE_FENCE_V2}\nnot JSON\n```\n").as_bytes());
    let raw = edit_note(n, &BTreeMap::new(), Some(&body), &n.source_hash).unwrap();
    notes.insert(p.clone(), parse_note(&raw));
    assert!(collect(&notes).is_err());
}

#[test]
fn two_review_families_supersede_and_legacy_v1_coexists() {
    let (mut notes, old, _) = fixture(2, 0);
    let mut next = old.clone();
    next.created_at = "2026-10-09T01:00:00Z".into();
    for d in &mut next.request.decisions {
        d.expected_hash = notes[&old.record_paths[&d.assertion_id]]
            .source_hash
            .clone();
        d.reason = "Second complete review".into();
    }
    next.request.supersedes = old
        .allocations
        .decisions
        .values()
        .map(|id| ExpectedRecord {
            record_id: id.clone(),
            hash: notes[&old.record_paths[id]].source_hash.clone(),
        })
        .collect();
    next.request = normalize(next.request).unwrap();
    (next.task_id, next.request_hash) = identity(&next.request).unwrap();
    next.predecessors = old
        .allocations
        .decisions
        .values()
        .map(|id| {
            let d = old
                .request
                .decisions
                .iter()
                .find(|d| &old.allocations.decisions[&d.assertion_id] == id)
                .unwrap();
            ReviewPredecessorProof {
                decision_id: id.clone(),
                expected_hash: notes[&old.record_paths[id]].source_hash.clone(),
                action: ReviewDecision::Reject,
                input_ids: vec![d.assertion_id.clone()],
                output_ids: vec![d.assertion_id.clone()],
            }
        })
        .collect();
    for (aid, did) in &mut next.allocations.decisions {
        *did = id(&format!("decision_second_{}", aid.as_str()));
        next.record_paths
            .insert(did.clone(), path(&format!("{did}.md")));
    }
    for proof in &mut next.assertion_proofs {
        proof.before_status = ReviewedAssertionStatus::Rejected;
        proof.governing_decision_id = next.allocations.decisions[&proof.assertion_id].clone();
    }
    next.supersessions = next
        .predecessors
        .iter()
        .map(|p| ReviewSupersession {
            predecessor_id: p.decision_id.clone(),
            successor_id: next.allocations.decisions[&p.input_ids[0]].clone(),
        })
        .collect();
    shape(&next).unwrap();
    install(&mut notes, &next, &origin(&next));
    for id in old.allocations.decisions.values() {
        let p = &old.record_paths[id];
        let bytes = set_status(&notes[p], "superseded").unwrap();
        notes.insert(p.clone(), parse_note(&bytes));
    }
    assert_eq!(collect(&notes).unwrap().receipts.len(), 2);
    assert_eq!(
        verify_review_policy_for_vault(&notes, &id("vault_shared"))
            .unwrap()
            .unwrap()
            .supersession_edges()
            .len(),
        2
    );
    // A superseded carrier's envelope hash changes; references still commit to
    // its unchanged payload, rather than recursively to mutable note bytes.
    for d in &old.request.decisions {
        let p = &old.record_paths[&old.allocations.decisions[&d.assertion_id]];
        notes.insert(
            p.clone(),
            parse_note(
                &set_status(&parse_note(&decision_bytes(&old, d).unwrap()), "superseded").unwrap(),
            ),
        );
    }
    assert_eq!(collect(&notes).unwrap().origins.len(), 1);
    assert!(verify_review_policy_for_vault(&notes, &id("vault_shared")).is_ok());
    let p = &next.record_paths[next.allocations.decisions.values().min().unwrap()];
    let wrong = edit_note(
        &notes[p],
        &BTreeMap::from([("wiki_action".into(), json!("accept"))]),
        None,
        &notes[p].source_hash,
    )
    .unwrap();
    notes.insert(p.clone(), parse_note(&wrong));
    assert!(verify_review_policy_for_vault(&notes, &id("vault_shared")).is_err());
}

#[test]
fn self_consistent_foreign_origin_never_grants_review_policy() {
    let (mut notes, receipt, _) = fixture(2, 0);
    let foreign = CanonicalGraphOriginV2::for_task(
        id("vault_foreign"),
        GraphOperationFamily::AssertionReview,
        &receipt.task_id,
        Blake3Hash::digest(packet::canonical_json(&receipt).unwrap()),
    )
    .unwrap();
    install(&mut notes, &receipt, &foreign);
    // Every carrier/reference checksum agrees; the actual vault is independent.
    assert!(collect(&notes).is_ok());
    assert!(verify_review_policy(&notes).is_err());
    assert!(verify_review_policy_for_vault(&notes, &id("vault_shared")).is_err());
    notes.insert(
        path("WIKI.md"),
        note("vault", "vault_shared", json!({}), b"Vault\n"),
    );
    assert!(verify_review_policy(&notes).is_err());
    assert!(verify_review_policy_for_vault(&notes, &id("vault_foreign")).is_err());
}

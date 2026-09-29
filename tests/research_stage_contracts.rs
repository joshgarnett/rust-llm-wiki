use lwiki::{
    changes::ChangeDraft,
    domain::*,
    research::{
        frontier::{self, StageLimits},
        gaps,
        synthesis::{self, ClaimStatus},
    },
    sources::*,
    vault::{VaultFs, VaultRoot},
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs};

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn frontier_value() -> Value {
    json!({"queries":["Rust exact quote verification"],"urls":["https://example.com/source?q=1"],"reason":"Find original evidence."})
}
fn gaps_value() -> Value {
    json!({"covered_evidence_ids":[],"gaps":["Unanswered"],"next_queries":[],"next_urls":[],"stop":true})
}
fn synthesis_value(citations: &[CitationRef]) -> Value {
    json!({"sections":[{"heading":"Findings","claims":[{"text":"A deliberately unrelated model claim.","citations":citations}]}],"unanswered_questions":[],"proposed_changes":[]})
}
fn write_draft(root: &VaultRoot, draft: ChangeDraft) {
    // Disposable verification fixture only; publication/crash behavior is covered by changeset tests.
    for operation in draft.operations {
        let file = root.path().join(operation.target.as_str());
        if let Some(content) = operation.proposed {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, content).unwrap();
        }
    }
}
fn fixture() -> (tempfile::TempDir, VaultRoot, SourceStore, CitationRef) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        b"---\nwiki_schema: \"1\"\nwiki_id: vault_stage\nwiki_kind: vault\ntitle: Stage\n---\n",
    )
    .unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    let store = SourceStore::new(VaultFs::new(root.clone()));
    let quote = "Original λ source bytes.\n".as_bytes();
    let plan = store
        .plan_capture(CaptureRequest {
            title: "Source".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture-source".into(),
            original: quote.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: Some("text/plain".into()),
        })
        .unwrap();
    write_draft(&root, plan.draft.unwrap());
    let citation = CitationRef::Source(SourceSpanRef {
        source_id: plan.source_id,
        source_revision: plan.revision_id,
        span: ByteSpan::new(0, quote.len() as u64).unwrap(),
        quote_hash: Blake3Hash::digest(quote),
    });
    (temp, root, store, citation)
}

#[test]
fn research_stage_schemas_limits_unknown_refs_and_invented_paths() {
    let limits = StageLimits::default();
    let original = frontier_value();
    assert!(frontier::validate(&bytes(&original), &limits, &[]).is_ok());
    for key in [
        "path",
        "commands",
        "limits",
        "apply",
        "accepted_status",
        "schema",
    ] {
        let mut value = original.clone();
        value[key] = json!("invented");
        assert!(
            frontier::validate(&bytes(&value), &limits, &[]).is_err(),
            "accepted {key}"
        );
    }
    for key in ["queries", "urls", "reason"] {
        let mut value = original.clone();
        value.as_object_mut().unwrap().remove(key);
        assert!(frontier::validate(&bytes(&value), &limits, &[]).is_err());
    }
    let reduced = StageLimits {
        max_queries: 0,
        ..limits.clone()
    };
    assert!(frontier::validate(&bytes(&original), &reduced, &[]).is_err());
    let reduced = StageLimits {
        max_bytes: 8,
        ..limits.clone()
    };
    assert_eq!(
        frontier::validate(&bytes(&original), &reduced, &[])
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    let reduced = StageLimits {
        max_nodes: 3,
        ..limits.clone()
    };
    assert!(frontier::validate(&bytes(&original), &reduced, &[]).is_err());
    let reduced = StageLimits {
        max_depth: 1,
        ..limits.clone()
    };
    assert!(frontier::validate(&bytes(&original), &reduced, &[]).is_err());
    let reduced = StageLimits {
        max_text_bytes: 3,
        ..limits.clone()
    };
    assert!(frontier::validate(&bytes(&original), &reduced, &[]).is_err());
    let mut value = gaps_value();
    for key in ["path", "commands", "limits", "apply", "accepted_status"] {
        let mut bad = value.clone();
        bad[key] = json!("invented");
        assert!(gaps::validate(&bytes(&bad), &limits, &BTreeSet::new(), &[]).is_err());
    }
    for key in [
        "covered_evidence_ids",
        "gaps",
        "next_queries",
        "next_urls",
        "stop",
    ] {
        let mut bad = value.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(gaps::validate(&bytes(&bad), &limits, &BTreeSet::new(), &[]).is_err());
    }
    value["covered_evidence_ids"] = json!(["evidence_unknown"]);
    assert!(gaps::validate(&bytes(&value), &limits, &BTreeSet::new(), &[]).is_err());
    let current = BTreeSet::from([RecordId::new("evidence_known").unwrap()]);
    value["covered_evidence_ids"] = json!(["evidence_known"]);
    assert!(gaps::validate(&bytes(&value), &limits, &current, &[]).is_ok());
    value["covered_evidence_ids"] = json!(["evidence_known", "evidence_known"]);
    assert!(gaps::validate(&bytes(&value), &limits, &current, &[]).is_err());
    assert!(
        frontier::validate(
            b"{\"queries\":[],\"queries\":[],\"urls\":[],\"reason\":\"ok\"}",
            &limits,
            &[]
        )
        .is_err()
    );
    assert!(
        frontier::validate(
            b"{\"queries\":[],\"urls\":[],\"reason\":\"ok\"} true",
            &limits,
            &[]
        )
        .is_err()
    );
    assert!(frontier::validate(b"\xff", &limits, &[]).is_err());
}

#[test]
fn frontier_public_urls_brave_bounds_and_caller_exclusions() {
    let limits = StageLimits::default();
    for url in [
        "file:///tmp/secret",
        "http://127.0.0.1/",
        "http://[::1]/",
        "http://localhost/",
        "https://user:pass@example.com/",
        "https://host.internal/",
    ] {
        let mut value = frontier_value();
        value["urls"] = json!([url]);
        assert!(
            frontier::validate(&bytes(&value), &limits, &[]).is_err(),
            "accepted {url}"
        );
    }
    for query in [
        "x".repeat(601),
        "x ".repeat(76),
        "a\nb".into(),
        "   ".into(),
    ] {
        let mut value = frontier_value();
        value["queries"] = json!([query]);
        assert!(frontier::validate(&bytes(&value), &limits, &[]).is_err());
    }
    assert!(frontier::validate(&bytes(&frontier_value()), &limits, &["RUST".into()]).is_err());
    assert!(
        frontier::validate(&bytes(&frontier_value()), &limits, &["EXAMPLE.COM".into()]).is_err()
    );
    let mut value = frontier_value();
    value["urls"] = json!(["https://example.com/a#one", "https://example.com/a#two"]);
    assert!(frontier::validate(&bytes(&value), &limits, &[]).is_err());
    value["urls"] = json!(["https://example.com/a?q=one", "https://example.com/a?q=two"]);
    assert!(frontier::validate(&bytes(&value), &limits, &[]).is_ok());
}

#[test]
fn synthesis_citation_bytes_verified_unsupported_claims_not_accepted() {
    let (_temp, _root, store, citation) = fixture();
    let limits = StageLimits::default();
    let view = store.view().unwrap();
    let value = synthesis_value(std::slice::from_ref(&citation));
    let validated = synthesis::validate(
        &bytes(&value),
        &limits,
        std::slice::from_ref(&citation),
        &[],
        &view,
    )
    .unwrap();
    assert_eq!(validated.verified_citations.len(), 1);
    assert!(!validated.verified_citations[0].dependencies.is_empty());
    assert!(validated.claim_assessments[0].provenance_verified);
    assert_eq!(
        validated.claim_assessments[0].status,
        ClaimStatus::Unassessed
    );
    assert!(synthesis::validate(&bytes(&value), &limits, &[], &[], &view).is_err());
    let value = synthesis_value(&[]);
    let validated = synthesis::validate(&bytes(&value), &limits, &[], &[], &view).unwrap();
    assert!(!validated.claim_assessments[0].provenance_verified);
    assert_eq!(
        validated.claim_assessments[0].status,
        ClaimStatus::Unassessed
    );
    assert_eq!(validated.gaps.len(), 1);
    let serialized = serde_json::to_value(&validated).unwrap();
    assert_eq!(serialized["claim_assessments"][0]["status"], "unassessed");
}

#[test]
fn synthesis_nested_reference_keys_and_authorized_page_proposals() {
    let (_temp, _root, store, citation) = fixture();
    let limits = StageLimits::default();
    let view = store.view().unwrap();
    let original = synthesis_value(std::slice::from_ref(&citation));
    for path in [vec!["kind"], vec!["reference"], vec!["reference", "span"]] {
        let mut value = original.clone();
        let cite = &mut value["sections"][0]["claims"][0]["citations"][0];
        let target = if path.len() == 1 && path[0] == "kind" {
            cite
        } else if path.len() == 1 {
            &mut cite["reference"]
        } else {
            &mut cite["reference"]["span"]
        };
        target["path"] = json!("/tmp/invented");
        assert!(
            synthesis::validate(
                &bytes(&value),
                &limits,
                std::slice::from_ref(&citation),
                &[],
                &view
            )
            .is_err()
        );
    }
    for location in ["heading", "claim"] {
        let mut value = original.clone();
        if location == "heading" {
            value["sections"][0]["target_path"] = json!("arbitrary.md");
        } else {
            value["sections"][0]["claims"][0]["accepted_status"] = json!("accepted");
        }
        assert!(
            synthesis::validate(
                &bytes(&value),
                &limits,
                std::slice::from_ref(&citation),
                &[],
                &view
            )
            .is_err()
        );
    }
    let record = RecordRef {
        vault_id: RecordId::new("vault_stage").unwrap(),
        record_id: RecordId::new("page_existing").unwrap(),
        expected_kind: RecordKind::Page,
    };
    let mut value = synthesis_value(&[]);
    value["proposed_changes"] = json!([{"kind":"update_page","record":record,"body":"Proposed prose","citations":[citation]}]);
    assert!(
        synthesis::validate(
            &bytes(&value),
            &limits,
            std::slice::from_ref(&citation),
            std::slice::from_ref(&record),
            &view
        )
        .is_ok()
    );
    assert!(
        synthesis::validate(
            &bytes(&value),
            &limits,
            std::slice::from_ref(&citation),
            &[],
            &view
        )
        .is_err()
    );
    value["proposed_changes"][0]["record"]["target_path"] = json!("arbitrary.md");
    assert!(
        synthesis::validate(
            &bytes(&value),
            &limits,
            std::slice::from_ref(&citation),
            std::slice::from_ref(&record),
            &view
        )
        .is_err()
    );
    value["proposed_changes"][0]["record"]
        .as_object_mut()
        .unwrap()
        .remove("target_path");
    let non_page = RecordRef {
        expected_kind: RecordKind::Source,
        ..record.clone()
    };
    value["proposed_changes"][0]["record"] = json!(non_page);
    assert!(
        synthesis::validate(
            &bytes(&value),
            &limits,
            std::slice::from_ref(&citation),
            &[non_page],
            &view
        )
        .is_err()
    );
    value["proposed_changes"] = json!([{"kind":"create_page","title":"Proposed page","body":"Unassessed prose","citations":[]}]);
    assert!(synthesis::validate(&bytes(&value), &limits, &[], &[], &view).is_ok());
    for field in [
        "target_path",
        "commands",
        "limits",
        "apply",
        "accepted_status",
    ] {
        let mut bad = value.clone();
        bad["proposed_changes"][0][field] = json!("arbitrary");
        assert!(synthesis::validate(&bytes(&bad), &limits, &[], &[], &view).is_err());
    }
}

#[test]
fn synthesis_rechecks_utf8_hash_ownership_and_withdrawn_source() {
    let (_temp, root, store, citation) = fixture();
    let limits = StageLimits::default();
    let source = match &citation {
        CitationRef::Source(s) => s,
        _ => unreachable!(),
    };
    let mut wrong_hash = source.clone();
    wrong_hash.quote_hash = Blake3Hash::digest(b"different");
    let mut split = source.clone();
    split.span = ByteSpan::new(9, 10).unwrap();
    split.quote_hash = Blake3Hash::digest([0xce]);
    let mut owner = source.clone();
    owner.source_id = RecordId::new("source_unknown").unwrap();
    for bad in [wrong_hash, split, owner] {
        let bad = CitationRef::Source(bad);
        let value = synthesis_value(std::slice::from_ref(&bad));
        assert!(
            synthesis::validate(
                &bytes(&value),
                &limits,
                std::slice::from_ref(&bad),
                &[],
                &store.view().unwrap()
            )
            .is_err()
        );
    }
    let content = root.path().join(format!(
        "sources/{}/revisions/{}/content.md",
        source.source_id, source.source_revision
    ));
    let original = fs::read(&content).unwrap();
    fs::write(&content, b"Edited source bytes.\n").unwrap();
    assert!(
        synthesis::validate(
            &bytes(&synthesis_value(std::slice::from_ref(&citation))),
            &limits,
            std::slice::from_ref(&citation),
            &[],
            &store.view().unwrap()
        )
        .is_err()
    );
    fs::write(content, original).unwrap();
    let plan = store
        .plan_withdraw(&source.source_id, "Fixture withdrawn")
        .unwrap();
    write_draft(&root, plan.draft.unwrap());
    assert!(
        synthesis::validate(
            &bytes(&synthesis_value(std::slice::from_ref(&citation))),
            &limits,
            std::slice::from_ref(&citation),
            &[],
            &store.view().unwrap()
        )
        .is_err()
    );
}

#[test]
fn synthesis_total_claim_citation_item_caps_and_gap_continuation() {
    let (_temp, _root, store, citation) = fixture();
    let limits = StageLimits::default();
    let view = store.view().unwrap();
    let value = synthesis_value(std::slice::from_ref(&citation));
    for reduced in [
        StageLimits {
            max_sections: 0,
            ..limits.clone()
        },
        StageLimits {
            max_claims: 0,
            ..limits.clone()
        },
        StageLimits {
            max_citations: 0,
            ..limits.clone()
        },
        StageLimits {
            max_heading_bytes: 1,
            ..limits.clone()
        },
    ] {
        assert!(
            synthesis::validate(
                &bytes(&value),
                &reduced,
                std::slice::from_ref(&citation),
                &[],
                &view
            )
            .is_err()
        );
    }
    let mut value = synthesis_value(&[]);
    value["sections"][0]["claims"] =
        json!([{"text":"one","citations":[]},{"text":"two","citations":[]}]);
    value["sections"]
        .as_array_mut()
        .unwrap()
        .push(json!({"heading":"Next","claims":[{"text":"three","citations":[]}]}));
    assert!(
        synthesis::validate(
            &bytes(&value),
            &StageLimits {
                max_claims: 2,
                ..limits.clone()
            },
            &[],
            &[],
            &view
        )
        .is_err()
    );
    let mut value = gaps_value();
    value["next_queries"] = json!(["next"]);
    assert!(gaps::validate(&bytes(&value), &limits, &BTreeSet::new(), &[]).is_err());
    value["stop"] = json!(false);
    assert!(gaps::validate(&bytes(&value), &limits, &BTreeSet::new(), &[]).is_ok());
    value["next_queries"] = json!(["private exclusion"]);
    assert!(
        gaps::validate(
            &bytes(&value),
            &limits,
            &BTreeSet::new(),
            &["exclusion".into()]
        )
        .is_err()
    );
}

#[test]
fn canonical_stage_schemas_match_shapes_and_reject_unknown_nested_fields() {
    let (_temp, _root, _store, citation) = fixture();
    for (schema, value) in [
        (frontier::schema(), frontier_value()),
        (gaps::schema(), gaps_value()),
        (
            synthesis::schema(),
            synthesis_value(std::slice::from_ref(&citation)),
        ),
    ] {
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&value));
        let mut bad = value.clone();
        bad["apply"] = json!(true);
        assert!(!validator.is_valid(&bad));
    }
    let schema = synthesis::schema();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let mut value = synthesis_value(std::slice::from_ref(&citation));
    value["sections"][0]["claims"][0]["citations"][0]["reference"]["path"] = json!("arbitrary.md");
    assert!(!validator.is_valid(&value));
    let evidence = CitationRef::Assertion(EvidenceRef {
        evidence_id: RecordId::new("evidence_known").unwrap(),
        assertion_id: RecordId::new("assertion_known").unwrap(),
        source_id: RecordId::new("source_known").unwrap(),
        source_revision: RecordId::new("revision_known").unwrap(),
        span: ByteSpan::new(0, 4).unwrap(),
        quote_hash: Blake3Hash::digest(b"text"),
    });
    assert!(validator.is_valid(&synthesis_value(&[evidence])));
}

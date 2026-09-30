use lwiki::domain::*;
use serde_json::{Value, json};

fn record(kind: &str, extra: Value) -> Value {
    let mut value = json!({"wiki_schema":"1", "wiki_id":format!("{kind}_fixture"), "wiki_kind":kind, "title":"Fixture 🦀"});
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    value
}
fn hash() -> String {
    Blake3Hash::digest("fixture evidence 🦀\r\n").to_string()
}
fn assertion(extra: Value) -> Value {
    let mut value = record(
        "assertion",
        json!({"wiki_status":"accepted", "wiki_subject_id":"entity_alex_1", "wiki_predicate":"uses", "wiki_object_id":"entity_component"}),
    );
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    value
}
fn canonical(value: Value) -> CanonicalRecord {
    CanonicalRecord::from_value(value).unwrap()
}
fn invalid(value: Value) {
    let error = CanonicalRecord::from_value(value.clone()).unwrap_err();
    assert_eq!(error.code, ErrorCode::RecordInvalid, "{value}");
    assert_eq!(error.exit_code(), 9);
    assert!(serde_json::from_value::<CanonicalRecord>(value).is_err());
}
fn examples() -> Vec<Value> {
    vec![
        record("vault", json!({"wiki_profile":"offline"})),
        record(
            "page",
            json!({"wiki_status":"reviewed", "aliases":["Architecture"], "tags":["rust"], "description":"A checked fixture", "wiki_depends_on_ids":["assertion_fixture"]}),
        ),
        record(
            "entity",
            json!({"wiki_status":"active", "wiki_entity_type":"person"}),
        ),
        assertion(
            json!({"wiki_negated":true, "wiki_modality":"possible", "wiki_valid_from":"2026-01-01", "wiki_valid_until":"2027-01-01", "wiki_subject":"[[knowledge/entities/alex.md|Alex]]", "wiki_object":"[[knowledge/entities/component.md#Details]]", "wiki_evidence":["[[knowledge/evidence/support.md]]"]}),
        ),
        record(
            "evidence",
            json!({"wiki_status":"active", "wiki_assertion_id":"assertion_fixture", "wiki_source_id":"source_fixture", "wiki_source_revision":"revision_fixture", "wiki_stance":"supports", "wiki_locator_kind":"utf8-bytes", "wiki_span_start":0, "wiki_span_end":25, "wiki_quote_hash":hash(), "wiki_source":"[[sources/fixture.md]]", "wiki_revision":"[[sources/revisions/fixture/revision.md]]"}),
        ),
        record(
            "source",
            json!({"wiki_status":"active", "wiki_origin_kind":"local-file", "wiki_origin":"fixture.txt", "wiki_current_revision":"revision_fixture", "wiki_revisions":["revision_old", "revision_fixture"]}),
        ),
        record(
            "revision",
            json!({"wiki_source_id":"source_fixture", "wiki_captured_at":"2026-09-28T00:00:00Z", "wiki_original_path":"original/fixture.txt", "wiki_original_hash":hash(), "wiki_extractor":"utf8", "wiki_extractor_fingerprint":hash(), "wiki_extraction_status":"complete", "wiki_content_path":"content.txt", "wiki_content_hash":hash(), "wiki_media_type":"text/plain", "wiki_published_at":"2026-09-01T00:00:00+00:00"}),
        ),
        record(
            "extraction_packet",
            json!({"wiki_id":RecordId::packet(&Blake3Hash::digest("packet")).to_string(), "wiki_source_id":"source_fixture", "wiki_source_revision":"revision_fixture", "wiki_packet_fingerprint":Blake3Hash::digest("packet").to_string(), "wiki_output_schema":"lwiki-extraction-v1", "wiki_created_at":"2026-09-28T00:00:00Z"}),
        ),
        record(
            "extraction",
            json!({"wiki_status":"completed", "wiki_packet_id":"packet_fixture", "wiki_input_hash":hash(), "wiki_extractor_fingerprint":hash(), "wiki_executor":"agent", "wiki_source_ids":["source_one", "source_two"], "wiki_source_revision_ids":["revision_two", "revision_old", "revision_one"], "wiki_completed_at":"2026-09-28T00:00:00Z", "wiki_model":"mock", "wiki_model_revision":"fixture"}),
        ),
        record(
            "decision",
            json!({"wiki_status":"active", "wiki_action":"accept", "wiki_input_ids":["assertion_fixture"], "wiki_output_ids":["assertion_fixture"], "wiki_created_at":"2026-09-28T00:00:00Z"}),
        ),
        record(
            "change",
            json!({"wiki_status":"prepared", "wiki_created_at":"2026-09-28T00:00:00Z", "wiki_manifest_hash":hash()}),
        ),
        record(
            "run",
            json!({"wiki_status":"planned", "wiki_created_at":"2026-09-28T00:00:00Z", "wiki_checkpoint_event_id":"run_event_fixture"}),
        ),
        record(
            "run_event",
            json!({"wiki_run_id":"run_fixture", "wiki_sequence":0, "wiki_event_type":"checkpoint", "wiki_occurred_at":"2026-09-28T00:00:00z", "wiki_request_id":"request_fixture"}),
        ),
    ]
}

#[test]
fn id_hash_and_path_newtypes() {
    for id in ["A", "alpha-1.2_test", "entity_alex", &"a".repeat(128)] {
        let typed = RecordId::new(id).unwrap();
        assert_eq!(typed.to_string(), id);
        assert_eq!(
            serde_json::from_value::<RecordId>(json!(id)).unwrap(),
            typed
        );
    }
    for id in [
        "",
        "_bad",
        "contains space",
        "é",
        "../bad",
        &"a".repeat(129),
    ] {
        assert!(RecordId::new(id).is_err(), "{id}");
        assert!(serde_json::from_value::<RecordId>(json!(id)).is_err());
    }
    for kind in [RecordKind::Page, RecordKind::Revision, RecordKind::RunEvent] {
        let generated = RecordId::generate(kind).unwrap();
        let uuid = generated
            .as_str()
            .strip_prefix(&format!("{kind}_"))
            .unwrap();
        assert_eq!(uuid::Uuid::parse_str(uuid).unwrap().get_version_num(), 7);
        assert_eq!(uuid, uuid.to_lowercase());
    }
    assert!(RecordId::generate(RecordKind::ExtractionPacket).is_err());
    let digest = Blake3Hash::digest(b"abc");
    assert_eq!(
        digest.as_str(),
        "blake3:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
    assert_eq!(digest.as_str().parse::<Blake3Hash>().unwrap(), digest);
    assert_eq!(
        RecordId::packet(&digest).as_str(),
        format!("packet_{}", digest.hex())
    );
    for bad in [
        "blake3:abc",
        &digest.to_string().to_uppercase(),
        &format!("sha256:{}", digest.hex()),
    ] {
        assert!(Blake3Hash::new(bad).is_err());
        assert!(serde_json::from_value::<Blake3Hash>(json!(bad)).is_err());
    }
    for path in [
        "knowledge/people/Alex.md",
        "sources/revision-1/content.txt",
        "Unicode/日本語.md",
        ".wiki/state/lock",
    ] {
        assert_eq!(VaultRelativePath::new(path).unwrap().as_str(), path);
    }
    for path in [
        "",
        "/absolute",
        "../escape",
        "a/../b",
        "a/./b",
        "a//b",
        "a/",
        "C:/file",
        "a\\b",
        "a\0b",
        "NUL.md",
        "com1.txt",
        "LPT9",
        "LPT¹.log",
        "a/trailing.",
        "a/trailing ",
        "bad?name",
    ] {
        assert!(VaultRelativePath::new(path).is_err(), "{path:?}");
        assert!(serde_json::from_value::<VaultRelativePath>(json!(path)).is_err());
    }
    assert!(ByteSpan::new(4, 3).is_err());
    assert!(serde_json::from_value::<ByteSpan>(json!({"start":4,"end":3})).is_err());
    assert!(serde_json::from_value::<ByteSpan>(json!({"start":-1,"end":3})).is_err());
    assert_eq!(ByteSpan::new(1, 5).unwrap().slice("a🦀b").unwrap(), "🦀");
    assert!(ByteSpan::new(1, 3).unwrap().slice("a🦀b").is_err());
    assert!(ByteSpan::new(1, 99).unwrap().slice("abc").is_err());
    assert!(ByteSpan::new(1, 1).unwrap().is_empty());
    let families = [
        (ErrorCode::Internal, 1),
        (ErrorCode::Usage, 2),
        (ErrorCode::ConfigInvalid, 2),
        (ErrorCode::VaultNotFound, 3),
        (ErrorCode::RecordNotFound, 3),
        (ErrorCode::ContentConflict, 4),
        (ErrorCode::LockTimeout, 4),
        (ErrorCode::CursorStale, 4),
        (ErrorCode::FreshnessConflict, 4),
        (ErrorCode::RecoveryRequired, 5),
        (ErrorCode::IndexCorrupt, 5),
        (ErrorCode::SourceIntegrity, 5),
        (ErrorCode::CapabilityUnavailable, 6),
        (ErrorCode::OfflineUnavailable, 6),
        (ErrorCode::ProfileUntrusted, 6),
        (ErrorCode::BudgetExceeded, 7),
        (ErrorCode::ProviderAuth, 8),
        (ErrorCode::ProviderRateLimit, 8),
        (ErrorCode::ProviderResponse, 8),
        (ErrorCode::ProviderUnavailable, 8),
        (ErrorCode::RecordInvalid, 9),
        (ErrorCode::ReferenceAmbiguous, 9),
        (ErrorCode::ExtractionInvalid, 9),
        (ErrorCode::Cancelled, 130),
    ];
    for (code, exit) in families {
        assert_eq!(code.exit_code(), exit);
        assert_eq!(serde_json::to_value(code).unwrap(), code.to_string());
        let error = WikiError::new(code, "fixture");
        assert_eq!(error.exit_code(), exit);
        assert_eq!(
            serde_json::from_value::<WikiError>(serde_json::to_value(&error).unwrap()).unwrap(),
            error
        );
    }
}

#[test]
fn contracts_valid_examples() {
    let published: Value = serde_json::from_str(include_str!("../schemas/record-v1.json")).unwrap();
    let schema = jsonschema::options()
        .should_validate_formats(true)
        .build(&published)
        .unwrap();
    for mut example in examples() {
        example["user_metadata"] = json!({"nested":[1,true,{"label":"preserve"}]});
        schema
            .validate(&example)
            .unwrap_or_else(|e| panic!("{}: {e}", example["wiki_kind"]));
        let valid = canonical(example.clone());
        assert_eq!(serde_json::to_value(&valid).unwrap(), example);
        assert_eq!(
            serde_json::from_value::<CanonicalRecord>(example).unwrap(),
            valid
        );
    }
    let alias = record(
        "decision",
        json!({"wiki_status":"active", "wiki_action":"add_alias", "wiki_input_ids":["entity_fixture"], "wiki_output_ids":["entity_fixture"], "wiki_created_at":"2026-09-28T00:00:00Z"}),
    );
    schema.validate(&alias).unwrap();
    canonical(alias.clone());
    let mut mention_alias = alias.clone();
    mention_alias["wiki_mention_ids"] = json!(["packet-local mention"]);
    invalid(mention_alias.clone());
    mention_alias["wiki_extraction_id"] = json!("extraction_fixture");
    canonical(mention_alias);
    // These are already canonical, accepted fixtures. Import proposals have a
    // different wire schema and may never be mistaken for canonical frontmatter.
    assert_eq!(
        canonical(assertion(json!({}))).string("wiki_status"),
        Some("accepted")
    );
    assert_eq!(
        canonical(assertion(json!({"wiki_status":"proposed"}))).string("wiki_status"),
        Some("proposed")
    );
    invalid(json!({"schema":"lwiki-extraction-v1", "mentions":[], "assertions":[]}));
    for status in ["unsupported", "failed"] {
        let mut revision = examples().remove(6);
        revision["wiki_extraction_status"] = json!(status);
        revision
            .as_object_mut()
            .unwrap()
            .remove("wiki_content_path");
        revision
            .as_object_mut()
            .unwrap()
            .remove("wiki_content_hash");
        canonical(revision);
    }
    canonical(record(
        "decision",
        json!({"wiki_status":"active", "wiki_action":"bind_mention", "wiki_input_ids":[], "wiki_output_ids":["entity_fixture"], "wiki_created_at":"2026-09-28T00:00:00Z", "wiki_extraction_id":"extraction_fixture", "wiki_mention_ids":["mention_1"]}),
    ));
}

#[test]
fn contracts_invalid_types_and_references() {
    for base in examples() {
        for (key, value) in base.as_object().unwrap() {
            let mut wrong = base.clone();
            wrong[key] = if value.is_string() {
                json!(17)
            } else {
                json!("wrong scalar")
            };
            invalid(wrong);
        }
        let mut unknown = base.clone();
        unknown["wiki_future_semantics"] = json!("cannot activate");
        invalid(unknown);
        if base.get("wiki_status").is_some() {
            let mut status = base.clone();
            status["wiki_status"] = json!("invented");
            invalid(status);
        }
    }
    for (key, bad) in [
        ("wiki_schema", json!(1)),
        ("wiki_schema", json!("2")),
        ("wiki_kind", json!("unknown")),
        ("wiki_id", json!("with spaces")),
        ("wiki_subject_id", json!("[[entity/Alex.md]]")),
        ("wiki_subject", json!("entity/Alex.md")),
        ("wiki_subject", json!("[[../outside.md]]")),
        ("wiki_object_id", json!("revision:1")),
        ("wiki_evidence", json!(["evidence_one"])),
        ("wiki_depends_on_ids", json!(["assertion good"])),
        ("aliases", json!(["ok", 1])),
        ("tags", json!("single")),
        ("description", json!({"invalid":"nested"})),
    ] {
        invalid(assertion(json!({key:bad})));
    }
    let mut evidence = examples().remove(4);
    evidence["wiki_span_end"] = json!(-1);
    invalid(evidence.clone());
    evidence["wiki_span_start"] = json!(5);
    evidence["wiki_span_end"] = json!(4);
    invalid(evidence.clone());
    evidence["wiki_span_end"] = json!(10);
    evidence["wiki_span_start"] = json!(10);
    invalid(evidence.clone());
    evidence["wiki_span_start"] = json!(0);
    evidence["wiki_source_revision"] = json!("2026-09-28T00:00:00Z");
    invalid(evidence);
    let mut revision = examples().remove(6);
    for (field, bad) in [
        ("wiki_original_path", "../escape"),
        ("wiki_captured_at", "2026-09-28"),
        ("wiki_captured_at", "2026-09-28T01:00:00+01:00"),
        ("wiki_captured_at", "2026-09-28T00:00:00-00:00"),
        ("wiki_captured_at", "2026-02-30T00:00:00Z"),
    ] {
        let mut wrong = revision.clone();
        wrong[field] = json!(bad);
        invalid(wrong);
    }
    revision
        .as_object_mut()
        .unwrap()
        .remove("wiki_content_hash");
    invalid(revision);
    let mut packet = examples().remove(7);
    packet["wiki_id"] = json!("packet_wrong");
    invalid(packet);
    let mut extraction = examples().remove(8);
    extraction["wiki_source_ids"] = json!(["source_one", "source_one"]);
    invalid(extraction);
    let mut decision = examples().remove(9);
    decision["wiki_action"] = json!("bind_mention");
    invalid(decision);
    invalid(record(
        "page",
        json!({"wiki_status":"draft", "wiki_negated":true}),
    ));
}

#[test]
fn predicate_literal_and_qualifier_matrix() {
    for predicate in [
        "is_a",
        "part_of",
        "depends_on",
        "maintains",
        "works_for",
        "uses",
        "plans_to_use",
        "located_in",
    ] {
        for modality in ["asserted", "possible", "planned"] {
            for negated in [false, true] {
                canonical(assertion(
                    json!({"wiki_predicate":predicate,"wiki_modality":modality,"wiki_negated":negated}),
                ));
            }
        }
        let mut wrong = assertion(
            json!({"wiki_predicate":predicate,"wiki_literal_type":"string","wiki_literal_value":"value"}),
        );
        wrong.as_object_mut().unwrap().remove("wiki_object_id");
        invalid(wrong);
    }
    for (kind, values) in [
        ("string", vec!["", "Unicode 🦀"]),
        (
            "decimal",
            vec!["0", "-0", "-17.000001", "123456789012345678901234567890"],
        ),
        ("boolean", vec!["true", "false"]),
        ("date", vec!["2024-02-29", "2026-09-28"]),
    ] {
        for value in values {
            let mut literal = assertion(
                json!({"wiki_predicate":"has_property", "wiki_property":"score", "wiki_literal_type":kind, "wiki_literal_value":value}),
            );
            literal.as_object_mut().unwrap().remove("wiki_object_id");
            canonical(literal.clone());
            literal["wiki_unit"] = json!("points");
            if kind == "decimal" {
                canonical(literal);
            } else {
                invalid(literal);
            }
        }
    }
    for bad in [
        "", "+1", "01", "-01", ".1", "1.", "1e2", "NaN", "inf", " 1", "1 ", "1.2.3", "--1",
    ] {
        let mut literal = assertion(
            json!({"wiki_predicate":"has_property", "wiki_property":"score", "wiki_literal_type":"decimal", "wiki_literal_value":bad}),
        );
        literal.as_object_mut().unwrap().remove("wiki_object_id");
        invalid(literal);
    }
    for extra in [
        json!({"wiki_predicate":"unknown"}),
        json!({"wiki_property":"extra"}),
        json!({"wiki_unit":"kg"}),
        json!({"wiki_modality":"likely"}),
        json!({"wiki_negated":"false"}),
        json!({"wiki_confidence":0.9}),
        json!({"wiki_valid_from":"2026-02-30"}),
        json!({"wiki_valid_from":"2026-09-28","wiki_valid_until":"2026-09-28"}),
        json!({"wiki_valid_from":"2026-09-28","wiki_valid_until":"2026-09-27"}),
        json!({"wiki_literal_type":"string","wiki_literal_value":"ambiguous"}),
        json!({"wiki_predicate":"has_property"}),
    ] {
        invalid(assertion(extra));
    }
    for (kind, bad) in [
        ("boolean", "TRUE"),
        ("boolean", "0"),
        ("date", "2026-02-29"),
        ("date", "2026-9-01"),
        ("date", "2026-01-01T00:00:00Z"),
    ] {
        let mut literal = assertion(
            json!({"wiki_predicate":"has_property","wiki_property":"value","wiki_literal_type":kind,"wiki_literal_value":bad}),
        );
        literal.as_object_mut().unwrap().remove("wiki_object_id");
        invalid(literal);
    }
    let mut incomplete = assertion(
        json!({"wiki_predicate":"has_property","wiki_property":"value","wiki_literal_type":"string"}),
    );
    incomplete.as_object_mut().unwrap().remove("wiki_object_id");
    invalid(incomplete);
}

#[test]
fn storage_layout_two_is_reserved_for_vault_markers() {
    let marker = record("vault", json!({"wiki_schema":"2"}));
    let parsed = canonical(marker.clone());
    assert_eq!(parsed.kind(), RecordKind::Vault);
    let bytes = b"---\nwiki_schema: \"2\"\nwiki_id: vault_fixture\nwiki_kind: vault\ntitle: Layout two\n---\nStorage layout 2\n";
    assert_eq!(
        lwiki::records::parse_note(bytes).status,
        lwiki::records::ParseStatus::Valid
    );
    invalid(record(
        "page",
        json!({"wiki_schema":"2","wiki_status":"reviewed"}),
    ));
    invalid(record("vault", json!({"wiki_schema":"3"})));
}

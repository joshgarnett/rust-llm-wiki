use lwiki::output::Envelope;
use serde_json::{Value, json};
use std::process::Command;

fn validator(text: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(text).unwrap();
    assert!(jsonschema::meta::is_valid(&schema));
    jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap()
}

#[test]
fn output_envelope_matches_published_schema() {
    let schema = validator(include_str!("../schemas/output-v1.json"));
    for envelope in [
        Envelope::success("search", json!({"hits": [], "next_cursor": null})),
        Envelope::failure("search", "USAGE", "Invalid option".into()),
    ] {
        schema
            .validate(&serde_json::to_value(envelope).unwrap())
            .unwrap();
    }
    let mut invalid = serde_json::to_value(Envelope::success("search", Value::Null)).unwrap();
    invalid["schema_version"] = json!(2);
    assert!(!schema.is_valid(&invalid));
}

#[test]
fn cli_only_advertises_implemented_commands_and_clean_json() {
    let binary = env!("CARGO_BIN_EXE_lwiki");
    let output = Command::new(binary)
        .args(["--json", "--offline", "--dry-run", "capabilities"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    validator(include_str!("../schemas/output-v1.json"))
        .validate(&envelope)
        .unwrap();
    assert_eq!(
        envelope["data"]["commands"],
        json!(["capabilities", "schema"])
    );
    assert_eq!(envelope["meta"]["network_used"], false);
    for name in ["output", "record"] {
        let output = Command::new(binary)
            .args(["--json", "schema", name])
            .output()
            .unwrap();
        assert!(output.status.success());
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(jsonschema::meta::is_valid(&envelope["data"]));
    }
    let output = Command::new(binary)
        .args(["--json", "not-implemented"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "USAGE");
    assert_eq!(envelope["ok"], false);
    let output = Command::new(binary)
        .args(["--json", "schema", "unknown"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "CAPABILITY_UNAVAILABLE");
}

#[test]
fn published_record_schema_validates_types_and_qualifiers() {
    let schema = validator(include_str!("../schemas/record-v1.json"));
    let mut assertion = json!({
        "wiki_schema": "1", "wiki_id": "assertion-example", "wiki_kind": "assertion",
        "title": "Measured mass", "wiki_status": "accepted", "wiki_subject_id": "entity-example",
        "wiki_predicate": "has_property", "wiki_property": "mass", "wiki_literal_type": "decimal",
        "wiki_literal_value": "-12.50", "wiki_unit": "kg", "wiki_negated": false,
        "wiki_modality": "asserted", "wiki_valid_from": "2024-02-29"
    });
    schema.validate(&assertion).unwrap();
    lwiki::domain::CanonicalRecord::from_value(assertion.clone()).unwrap();
    for invalid in [json!(12.50), json!("NaN"), json!("1e3"), json!("01")] {
        assertion["wiki_literal_value"] = invalid;
        assert!(!schema.is_valid(&assertion));
        assert!(lwiki::domain::CanonicalRecord::from_value(assertion.clone()).is_err());
    }
    assertion["wiki_literal_value"] = json!("12");
    assertion["wiki_valid_from"] = json!("2023-02-29");
    assert!(!schema.is_valid(&assertion));
    assert!(lwiki::domain::CanonicalRecord::from_value(assertion.clone()).is_err());
    assertion["wiki_valid_from"] = json!("2024-02-29");
    assertion["wiki_object_id"] = json!("other");
    assert!(!schema.is_valid(&assertion));
    assert!(lwiki::domain::CanonicalRecord::from_value(assertion).is_err());
}

#[test]
fn canonical_fixture_schema_hashes_and_utf8_spans_agree() {
    use lwiki::domain::{Blake3Hash, ByteSpan, CanonicalRecord};
    use std::{collections::BTreeSet, fs, path::Path};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bootstrap");
    assert!(root.join("vault/WIKI.md").is_file());
    assert!(
        fs::read_dir(root.join("vault"))
            .unwrap()
            .any(|entry| entry.unwrap().file_name() == "WIKI.md")
    );
    let expected: Value =
        serde_json::from_slice(&fs::read(root.join("expected.json")).unwrap()).unwrap();
    let schema = validator(include_str!("../schemas/record-v1.json"));
    let mut ids = BTreeSet::new();
    for item in expected["records"].as_array().unwrap() {
        schema
            .validate(&item["fields"])
            .unwrap_or_else(|err| panic!("{}: {err}", item["path"]));
        let record = CanonicalRecord::from_value(item["fields"].clone()).unwrap();
        assert!(ids.insert(record.id().to_string()));
        assert_eq!(record.id().as_str(), item["id"].as_str().unwrap());
    }
    for revision in expected["revisions"].as_array().unwrap() {
        for role in ["original", "content"] {
            let path = revision[format!("{role}_path")].as_str().unwrap();
            let bytes = fs::read(root.join("vault").join(path)).unwrap();
            assert_eq!(
                Blake3Hash::digest(&bytes).as_str(),
                revision[format!("{role}_hash")].as_str().unwrap()
            );
        }
    }
    for evidence in expected["evidence"].as_array().unwrap() {
        let revision = expected["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == evidence["revision_id"])
            .unwrap();
        assert_eq!(revision["source_id"], evidence["source_id"]);
        let bytes = fs::read(
            root.join("vault")
                .join(revision["content_path"].as_str().unwrap()),
        )
        .unwrap();
        let content = std::str::from_utf8(&bytes).unwrap();
        let span = ByteSpan::new(
            evidence["span_start"].as_u64().unwrap(),
            evidence["span_end"].as_u64().unwrap(),
        )
        .unwrap();
        let quote = span.slice(content).unwrap();
        assert_eq!(quote, evidence["quote"].as_str().unwrap());
        assert_eq!(
            Blake3Hash::digest(quote).as_str(),
            evidence["quote_hash"].as_str().unwrap()
        );
    }
}

#[test]
fn evidence_link_schema_rejects_plain_strings() {
    let schema = validator(include_str!("../schemas/record-v1.json"));
    let mut record = json!({"wiki_schema":"1", "wiki_id":"assertion-example", "wiki_kind":"assertion", "title":"Example", "wiki_status":"proposed", "wiki_subject_id":"entity-a", "wiki_predicate":"uses", "wiki_object_id":"entity-b", "wiki_evidence":["[[knowledge/evidence/example.md]]"]});
    assert!(schema.is_valid(&record));
    lwiki::domain::CanonicalRecord::from_value(record.clone()).unwrap();
    record["wiki_evidence"] = json!(["plain text"]);
    assert!(!schema.is_valid(&record));
    assert!(lwiki::domain::CanonicalRecord::from_value(record).is_err());
}

#[cfg(unix)]
#[test]
fn invalid_utf8_arguments_return_clean_usage_envelope() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let output = Command::new(env!("CARGO_BIN_EXE_lwiki"))
        .arg("schema")
        .arg(OsString::from_vec(vec![0xff]))
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "USAGE");
    validator(include_str!("../schemas/output-v1.json"))
        .validate(&envelope)
        .unwrap();
}

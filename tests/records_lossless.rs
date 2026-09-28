use lwiki::{
    domain::{Blake3Hash, ErrorCode, RecordId, RecordKind, VaultRelativePath},
    records::{edit::edit_note, links::*, parse::*},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn base(extra: &str) -> Vec<u8> {
    format!("---\nwiki_schema: \"1\"\nwiki_id: page_00000000-0000-7000-8000-000000000001\nwiki_kind: page\ntitle: Title\nwiki_status: draft\n{extra}---\nBody 🦀\n").into_bytes()
}
fn changes(key: &str, value: Value) -> BTreeMap<String, Value> {
    [(key.to_owned(), value)].into()
}

#[test]
fn frontmatter_preserves_unknown_nested_comments_quotes_bom_crlf() {
    let raw = include_bytes!("fixtures/p01/lossless-crlf.md");
    let note = parse_note(raw);
    assert_eq!(note.status, ParseStatus::Valid, "{:?}", note.diagnostics);
    assert_eq!(
        edit_note(&note, &BTreeMap::new(), None, &note.source_hash).unwrap(),
        raw
    );
    let output = edit_note(
        &note,
        &changes("title", json!("New 🦀 title")),
        None,
        &note.source_hash,
    )
    .unwrap();
    let expected = String::from_utf8(raw.to_vec())
        .unwrap()
        .replace("'旧 title'", "\"New 🦀 title\"");
    assert_eq!(output, expected.as_bytes());
    assert_eq!(parse_note(&output).body(), note.body());
    let output = edit_note(
        &note,
        &changes("description", json!("Appended")),
        Some(b"Explicit body\r\n"),
        &note.source_hash,
    )
    .unwrap();
    let expected = String::from_utf8(raw.to_vec()).unwrap().replace(
        "---\r\n# Body",
        "description: \"Appended\"\r\n---\r\n# Body",
    );
    let closing = expected.find("---\r\n# Body").unwrap() + 5;
    assert_eq!(
        output,
        [&expected.as_bytes()[..closing], b"Explicit body\r\n"].concat()
    );
}

#[test]
fn reject_duplicate_keys_anchors_tags_and_wrong_types() {
    for extra in [
        "title: Again\n",
        "unknown: {one: 1, one: 2}\n",
        "unknown: [{deep: {x: 1, x: 2}}]\n",
        "unknown: {é: 1, \"\\u00e9\": 2}\n",
        "unknown: &anchor plain\n",
        "unknown: &anchor [one]\n",
        "unknown: &anchor {one: two}\n",
        "unknown: *missing\n",
        "unknown: !!str plain\n",
        "unknown: !custom {one: two}\n",
        "unknown: {'<<': {one: two}}\n",
        "aliases: [one, 3]\n",
        "tags: false\n",
        "description: 12\n",
        "description: Null\n",
        "description: NULL\n",
        "wiki_future: thing\n",
    ] {
        let raw = base(extra);
        let note = parse_note(&raw);
        assert_eq!(
            note.status,
            ParseStatus::Invalid,
            "{extra} {:?}",
            note.diagnostics
        );
        assert!(note.canonical.is_none());
        assert_eq!(note.literal_text().unwrap().as_bytes(), raw);
        assert!(
            edit_note(
                &note,
                &changes("title", json!("X")),
                None,
                &note.source_hash
            )
            .is_err()
        );
    }
    for (from, to) in [
        ("wiki_schema: \"1\"", "wiki_schema: 1"),
        ("title: Title", "title: true"),
        ("wiki_id: page_", "wiki_id: -page_"),
    ] {
        let raw = String::from_utf8(base("")).unwrap().replace(from, to);
        assert_eq!(parse_note(raw.as_bytes()).status, ParseStatus::Invalid);
    }
}

#[test]
fn unsupported_schema_is_structured_read_only() {
    let raw = String::from_utf8(base("unknown: {nested: yes}\n"))
        .unwrap()
        .replace("wiki_schema: \"1\"", "wiki_schema: \"2\"");
    let note = parse_note(raw.as_bytes());
    assert_eq!(note.status, ParseStatus::UnsupportedSchema);
    assert!(note.fields.is_some());
    assert!(note.canonical.is_none());
    assert_eq!(note.body(), "Body 🦀\n".as_bytes());
    assert_eq!(note.literal_text(), Some(raw.as_str()));
    assert!(
        edit_note(
            &note,
            &changes("title", json!("New")),
            None,
            &note.source_hash
        )
        .is_err()
    );
}

#[test]
fn numeric_syntax_never_falls_back_to_string_and_spans_remain_exact() {
    for numeric in [
        "0x8000000000000000",
        "0o1000000000000000000000",
        "0xffffffffffffffff",
        "0x10000000000000000",
        "0o2000000000000000000000",
        "18446744073709551616",
        "-9223372036854775809",
    ] {
        let raw = String::from_utf8(base(""))
            .unwrap()
            .replace("title: Title", &format!("title: {numeric}"));
        assert_eq!(
            parse_note(raw.as_bytes()).status,
            ParseStatus::Invalid,
            "{numeric}"
        );
        let quoted = raw.replace(
            &format!("title: {numeric}"),
            &format!("title: \"{numeric}\""),
        );
        let note = parse_note(quoted.as_bytes());
        assert_eq!(note.status, ParseStatus::Valid, "{numeric}");
        assert_eq!(note.canonical.unwrap().title(), numeric);
    }
    let fixture = include_str!("fixtures/bootstrap/vault/knowledge/evidence/forward_short.md");
    for (start, end) in [
        ("9223372036854775808", "18446744073709551615"),
        ("0x8000000000000000", "0xffffffffffffffff"),
        ("0o1000000000000000000000", "0o1777777777777777777777"),
    ] {
        let raw = fixture
            .replace("wiki_span_start: 28", &format!("wiki_span_start: {start}"))
            .replace("wiki_span_end: 54", &format!("wiki_span_end: {end}"));
        let note = parse_note(raw.as_bytes());
        assert_eq!(
            note.status,
            ParseStatus::Valid,
            "{start} {end} {:?}",
            note.diagnostics
        );
        let fields = note.fields.unwrap();
        assert_eq!(fields["wiki_span_start"].as_u64(), Some(1u64 << 63));
        assert_eq!(fields["wiki_span_end"].as_u64(), Some(u64::MAX));
    }
    for end in [
        "18446744073709551616",
        "0x10000000000000000",
        "0o2000000000000000000000",
        "\"18446744073709551615\"",
        "18446744073709551615.0",
    ] {
        let raw = fixture.replace("wiki_span_end: 54", &format!("wiki_span_end: {end}"));
        assert_eq!(
            parse_note(raw.as_bytes()).status,
            ParseStatus::Invalid,
            "{end}"
        );
    }
}

fn entry(n: u8, path: &str, aliases: &[&str]) -> RegistryEntry {
    RegistryEntry {
        id: RecordId::new(format!("entity_00000000-0000-7000-8000-{n:012x}")).unwrap(),
        kind: RecordKind::Entity,
        path: VaultRelativePath::new(path).unwrap(),
        aliases: aliases.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn links_ignore_code_and_preserve_ambiguity() {
    let body = "[[one#Heading|label]] [ordinary](folder/two.md#^block) `[[inline]] [code](x)`\n\n```text\n[[fenced]] [code](y)\n```\n\n    [[indented]] [code](z)\n\n[[alias]] \\[[escaped]]\n";
    let links = extract_links(body);
    assert_eq!(
        links
            .iter()
            .map(|l| l.destination.as_str())
            .collect::<Vec<_>>(),
        ["one#Heading", "folder/two.md#^block", "alias"]
    );
    for link in &links {
        assert!(body.is_char_boundary(link.range.start));
        assert!(body.is_char_boundary(link.range.end));
    }
    let registry = vec![
        entry(1, "a/one.md", &["alias"]),
        entry(2, "b/one.md", &["alias"]),
        entry(3, "folder/two.md", &["Unique"]),
    ];
    assert!(matches!(
        resolve_untyped(&registry, "one#Heading"),
        LinkResolution::Ambiguous { .. }
    ));
    assert!(matches!(
        resolve_untyped(&registry, "alias"),
        LinkResolution::Ambiguous { .. }
    ));
    assert!(matches!(
        resolve_untyped(&registry, "a/one.md#Heading"),
        LinkResolution::Resolved {
            fragment: Some(_),
            ..
        }
    ));
    assert!(matches!(
        resolve_untyped(&registry, "Unique"),
        LinkResolution::Resolved { .. }
    ));
    assert!(matches!(
        resolve_untyped(&registry, "https://example.invalid"),
        LinkResolution::External
    ));
    assert_eq!(
        resolve_untyped(&registry, "#Heading"),
        LinkResolution::Missing
    );
    assert!(matches!(
        resolve_typed(
            &registry,
            &registry[0].id,
            RecordKind::Entity,
            Some("[[b/one.md]]")
        ),
        LinkResolution::CompanionConflict { .. }
    ));
    assert!(matches!(
        resolve_typed(
            &registry,
            &registry[0].id,
            RecordKind::Entity,
            Some("[[old.md]]")
        ),
        LinkResolution::Resolved {
            companion_stale: true,
            ..
        }
    ));
    assert_eq!(
        resolve_typed(&registry, &registry[0].id, RecordKind::Source, None),
        LinkResolution::WrongKind {
            actual: RecordKind::Entity
        }
    );
    let mut copied = registry.clone();
    let mut copy = registry[0].clone();
    copy.path = VaultRelativePath::new("copy.md").unwrap();
    copied.push(copy);
    assert!(matches!(
        resolve_untyped(&copied, "a/one.md"),
        LinkResolution::Ambiguous { .. }
    ));
    assert!(matches!(
        resolve_typed(&copied, &registry[0].id, RecordKind::Entity, None),
        LinkResolution::Ambiguous { .. }
    ));
}

#[test]
fn delimiter_bounds_and_unsafe_edits() {
    for raw in [
        b"text\n---\ntitle: x\n---\n".as_slice(),
        b" ---\n",
        b"--- \n",
    ] {
        assert_eq!(parse_note(raw).status, ParseStatus::NoFrontmatter);
    }
    assert_eq!(
        parse_note(b"---\ntitle: x\n--- \n").status,
        ParseStatus::Invalid
    );
    assert_eq!(parse_note(b"---\n---").status, ParseStatus::Invalid);
    assert_eq!(parse_note(&[0xff]).status, ParseStatus::Invalid);
    let raw = base("unknown: [[[[[[[[[[42]]]]]]]]]]\n");
    assert_eq!(
        parse_note_with_limits(
            &raw,
            ParseLimits {
                max_envelope_bytes: 4096,
                max_depth: 4
            }
        )
        .status,
        ParseStatus::Invalid
    );
    let raw = base(&format!(
        "unknown: {}0{}\n",
        "[".repeat(15000),
        "]".repeat(15000)
    ));
    assert_eq!(parse_note(&raw).status, ParseStatus::Invalid);
    let note = parse_note_with_limits(
        &base(""),
        ParseLimits {
            max_envelope_bytes: 10,
            max_depth: 4,
        },
    );
    assert_eq!(note.status, ParseStatus::Invalid);
    let raw = String::from_utf8(base(""))
        .unwrap()
        .replace("title: Title", "title: |\n  Multi\n  Line");
    let note = parse_note(raw.as_bytes());
    assert_eq!(note.status, ParseStatus::Valid);
    assert!(
        edit_note(
            &note,
            &changes("title", json!("Replacement")),
            None,
            &note.source_hash
        )
        .is_err()
    );
    assert!(edit_note(&note, &BTreeMap::new(), Some(b"body"), &note.source_hash).is_ok());
    let note = parse_note(&base(""));
    assert_eq!(
        edit_note(
            &note,
            &changes("title", json!("X")),
            None,
            &Blake3Hash::digest(b"other")
        )
        .unwrap_err()
        .code,
        ErrorCode::ContentConflict
    );
    for key in ["wiki_id", "wiki_kind", "wiki_schema", "unknown"] {
        assert!(edit_note(&note, &changes(key, json!("X")), None, &note.source_hash).is_err());
    }
    assert_eq!(parser_fingerprint(), parser_fingerprint());
}

#[test]
fn bootstrap_canonical_bytes_match_manifest() {
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/bootstrap/expected.json")).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bootstrap");
    for fixture in expected["records"].as_array().unwrap() {
        let path = format!("vault/{}", fixture["path"].as_str().unwrap());
        let raw = std::fs::read(root.join(&path)).unwrap();
        let note = parse_note(&raw);
        assert_eq!(
            note.status,
            ParseStatus::Valid,
            "{} {:?}",
            fixture["path"],
            note.diagnostics
        );
        let record = note.canonical.as_ref().unwrap();
        assert_eq!(record.id().as_str(), fixture["id"].as_str().unwrap());
        assert_eq!(record.kind().as_str(), fixture["kind"].as_str().unwrap());
        assert_eq!(
            serde_json::to_value(record.fields()).unwrap(),
            fixture["fields"]
        );
        let hashed = expected["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == path)
            .unwrap();
        assert_eq!(note.source_hash.as_str(), hashed["hash"].as_str().unwrap());
        assert_eq!(
            edit_note(&note, &BTreeMap::new(), None, &note.source_hash).is_ok(),
            !matches!(
                record.kind(),
                RecordKind::Revision | RecordKind::ExtractionPacket | RecordKind::RunEvent
            )
        );
    }
}

#[test]
fn scalar_comment_ranges_and_inline_list_edits() {
    for old in [
        "'can''t # comment'",
        "\"escaped \\\" # value\"",
        "plain#inside",
        "plain",
        "\"🦀 title\"",
    ] {
        let raw = String::from_utf8(base(""))
            .unwrap()
            .replace("title: Title", &format!("title: {old}  # actual comment"));
        let note = parse_note(raw.as_bytes());
        assert_eq!(note.status, ParseStatus::Valid, "{:?}", note.diagnostics);
        let output = edit_note(
            &note,
            &changes("title", json!("Changed")),
            None,
            &note.source_hash,
        )
        .unwrap();
        assert_eq!(output, raw.replace(old, "\"Changed\"").as_bytes());
    }
    let raw = base("aliases: ['# one', \"two\"]  # retained\n");
    let note = parse_note(&raw);
    let output = edit_note(
        &note,
        &changes("aliases", json!(["new"])),
        None,
        &note.source_hash,
    )
    .unwrap();
    assert_eq!(
        output,
        String::from_utf8(raw)
            .unwrap()
            .replace("['# one', \"two\"]", "[\"new\"]")
            .as_bytes()
    );
    let raw = base("'description': '旧 description' # retained\n");
    let note = parse_note(&raw);
    let output = edit_note(
        &note,
        &changes("description", json!("Changed")),
        None,
        &note.source_hash,
    )
    .unwrap();
    assert_eq!(
        output,
        String::from_utf8(raw)
            .unwrap()
            .replace("'旧 description'", "\"Changed\"")
            .as_bytes()
    );
    let raw = base("unknown: \"🦀 before edit marker\"\naliases: [one]\n");
    let note = parse_note(&raw);
    let output = edit_note(
        &note,
        &changes("aliases", json!(["two"])),
        None,
        &note.source_hash,
    )
    .unwrap();
    assert_eq!(
        output,
        String::from_utf8(raw)
            .unwrap()
            .replace("[one]", "[\"two\"]")
            .as_bytes()
    );
}

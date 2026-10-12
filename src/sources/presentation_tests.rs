use super::*;
use crate::sources::revision::common;

fn source(head: &RecordId, revisions: &[&RecordId]) -> CanonicalRecord {
    let id = RecordId::new("source_notes").unwrap();
    let mut fields = common(&id, RecordKind::Source, "Planning notes");
    fields.extend(BTreeMap::from([
        ("wiki_status".into(), "active".into()),
        ("wiki_origin_kind".into(), "local-file".into()),
        ("wiki_origin".into(), "/notes/Planning.md".into()),
        ("wiki_current_revision".into(), head.as_str().into()),
        ("wiki_revisions".into(), serde_json::json!(revisions)),
    ]));
    CanonicalRecord::new(fields).unwrap()
}
fn revision(id: &RecordId, text: &[u8]) -> CanonicalRecord {
    let mut fields = common(id, RecordKind::Revision, "Planning notes");
    fields.extend(BTreeMap::from([
        ("wiki_source_id".into(), "source_notes".into()),
        ("wiki_captured_at".into(), "2026-10-11T00:00:00Z".into()),
        ("wiki_original_path".into(), "original.bin".into()),
        (
            "wiki_original_hash".into(),
            Blake3Hash::digest(text).to_string().into(),
        ),
        ("wiki_extractor".into(), "utf8-preserve-v1".into()),
        (
            "wiki_extractor_fingerprint".into(),
            Blake3Hash::digest(b"utf8-preserve-v1").to_string().into(),
        ),
        ("wiki_extraction_status".into(), "complete".into()),
        ("wiki_content_path".into(), "content.md".into()),
        (
            "wiki_content_hash".into(),
            Blake3Hash::digest(text).to_string().into(),
        ),
    ]));
    CanonicalRecord::new(fields).unwrap()
}

#[test]
fn cited_current_becomes_one_full_historical_copy_without_changing_route() {
    let old = RecordId::new("revision_old").unwrap();
    let new = RecordId::new("revision_new").unwrap();
    let old_text = b"# Planning\n\nMira approves.\n\n## Condition\nOnly after legal review.\n";
    let new_text = b"# Planning\n\nMira and Jules approve.\n";
    let old_record = revision(&old, old_text);
    let new_record = revision(&new, new_text);
    let before_source = source(&old, &[&old]);
    let path = allocate_path(
        before_source.title(),
        before_source.id(),
        None,
        &BTreeSet::new(),
    )
    .unwrap();
    let captures = BTreeMap::from([(
        old.clone(),
        AuthenticatedCapture {
            revision: &old_record,
            content: Some(old_text),
        },
    )]);
    let first = plan_source_presentation(SourcePresentationInput {
        source: &before_source,
        source_body: b"Author kept this note.\n",
        previous: None,
        captures: &captures,
        requested: &BTreeSet::from([old.clone()]),
        allocated_current: &path,
        allocated_saved: &BTreeMap::new(),
        observed_views: &BTreeMap::from([(path.clone(), None)]),
    })
    .unwrap();
    assert_eq!(first.writes.len(), 1);
    assert!(first.metadata.published[&old].is_none());
    let previous_route = first.navigation[&old].capture_anchor.clone();
    let saved = allocate_path(
        before_source.title(),
        &old,
        Some("2026-10-11"),
        &BTreeSet::new(),
    )
    .unwrap();
    let after_source = source(&new, &[&old, &new]);
    let captures = BTreeMap::from([
        (
            old.clone(),
            AuthenticatedCapture {
                revision: &old_record,
                content: Some(old_text),
            },
        ),
        (
            new.clone(),
            AuthenticatedCapture {
                revision: &new_record,
                content: Some(new_text),
            },
        ),
    ]);
    let observed = BTreeMap::from([
        (path.clone(), first.writes[0].proposed.clone()),
        (saved.clone(), None),
    ]);
    let second = plan_source_presentation(SourcePresentationInput {
        source: &after_source,
        source_body: b"Author kept this note.\n",
        previous: Some(&first.metadata),
        captures: &captures,
        requested: &BTreeSet::new(),
        allocated_current: &path,
        allocated_saved: &BTreeMap::from([(old.clone(), saved.clone())]),
        observed_views: &observed,
    })
    .unwrap();
    assert_eq!(second.writes.len(), 2);
    assert_eq!(second.navigation[&old].capture_anchor, previous_route);
    let historical = second
        .writes
        .iter()
        .find(|op| op.target == saved)
        .unwrap()
        .proposed
        .as_ref()
        .unwrap();
    assert!(historical.ends_with(old_text));
    let current = second
        .writes
        .iter()
        .find(|op| op.target == path)
        .unwrap()
        .proposed
        .as_ref()
        .unwrap();
    assert!(current.ends_with(new_text));
    assert!(
        std::str::from_utf8(current)
            .unwrap()
            .contains(saved.as_str())
    );
}

#[test]
fn original_properties_are_fenced_without_losing_bytes() {
    let original =
        b"\xef\xbb\xbf---\r\ntitle: '[[other]]'\r\nnote: '````'\r\n---\r\n# Body\r\nText.\r\n";
    let mut output = String::new();
    append_original(&mut output, original).unwrap();
    assert!(output.contains("Original note properties"));
    assert!(output.contains("`````text\n"));
    assert!(output.contains("\u{feff}---\r\ntitle: '[[other]]'\r\nnote: '````'\r\n---\r\n"));
    assert!(output.ends_with("# Body\r\nText.\r\n"));
}

#[test]
fn edited_generated_copy_refuses_with_specific_preserving_error() {
    let path = VaultRelativePath::new("sources/views/Planning-abc.md").unwrap();
    let expected = Blake3Hash::digest(b"generated");
    let observed = BTreeMap::from([(path.clone(), Some(b"my edit".to_vec()))]);
    let error = checked_write(
        &path,
        b"new".to_vec(),
        Some(&expected),
        &observed,
        &mut vec![],
        &mut vec![],
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert_eq!(error.details["reason"], "saved_note_modified");
    assert_eq!(observed[&path].as_deref(), Some(b"my edit".as_slice()));
}

#[test]
fn identity_suffix_handles_case_collisions_and_titles_never_control_identity() {
    let id = RecordId::new("source_first").unwrap();
    let initial = allocate_path("CON: Plan / α", &id, None, &BTreeSet::new()).unwrap();
    let blocked = BTreeSet::from([initial.as_str().to_uppercase()]);
    let next = allocate_path("CON: Plan / α", &id, None, &blocked).unwrap();
    assert_ne!(initial, next);
    assert!(next.as_str().contains("CON Plan α"));
    assert!(!next.as_str().contains(':'));
}

#[test]
fn corrupt_capture_cannot_produce_navigation() {
    let id = RecordId::new("revision_old").unwrap();
    let record = revision(&id, b"true text");
    let source = source(&id, &[&id]);
    let path = allocate_path(source.title(), source.id(), None, &BTreeSet::new()).unwrap();
    let captures = BTreeMap::from([(
        id,
        AuthenticatedCapture {
            revision: &record,
            content: Some(b"wrong text"),
        },
    )]);
    let result = plan_source_presentation(SourcePresentationInput {
        source: &source,
        source_body: b"",
        previous: None,
        captures: &captures,
        requested: &BTreeSet::new(),
        allocated_current: &path,
        allocated_saved: &BTreeMap::new(),
        observed_views: &BTreeMap::from([(path.clone(), None)]),
    });
    assert!(matches!(result, Err(error) if error.code == ErrorCode::SourceIntegrity));
}

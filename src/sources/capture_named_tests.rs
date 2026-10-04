use super::{
    CaptureAllocation, CaptureRequest, ExtractionInput, SourceCaptureState, SourceOrigin,
    SourcePlan, SourceStore,
    capture::{extract, revision_writes, revision_writes_at},
};
use crate::{
    domain::{Blake3Hash, CanonicalRecord, ErrorCode, RecordId, RecordKind, VaultRelativePath},
    records::parse_note,
    vault::{ExpectedState, VaultFs, VaultRoot},
};
use std::fs;

fn store() -> (tempfile::TempDir, SourceStore) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    (temp, SourceStore::new(handle))
}

fn allocation() -> CaptureAllocation {
    CaptureAllocation {
        // Kind belongs to the record envelope, not an opaque ID's prefix.
        source_id: RecordId::new("revision_named-source").unwrap(),
        revision_id: RecordId::new("source_named-revision").unwrap(),
        captured_at: "2024-02-29T09:08:07.1200+00:00".into(),
    }
}

fn request(original: Vec<u8>, extraction: ExtractionInput) -> CaptureRequest {
    CaptureRequest {
        title: "Quoted \"capture\": 茶".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "/input base/quoted \"file\".txt".into(),
        original,
        extraction,
        media_type: Some("application/test; charset=UTF-8".into()),
    }
}

fn bytes<'a>(plan: &'a SourcePlan, suffix: &str) -> &'a [u8] {
    plan.draft
        .as_ref()
        .unwrap()
        .operations
        .iter()
        .find(|write| write.target.as_str().ends_with(suffix))
        .unwrap()
        .proposed
        .as_deref()
        .unwrap()
}

fn record(plan: &SourcePlan, suffix: &str) -> CanonicalRecord {
    let parsed = parse_note(bytes(plan, suffix));
    assert!(parsed.body().is_empty());
    parsed.canonical.unwrap()
}

fn assert_same_plan(left: &SourcePlan, right: &SourcePlan) {
    assert_eq!(left.source_id, right.source_id);
    assert_eq!(left.revision_id, right.revision_id);
    assert_eq!(left.capture_state, right.capture_state);
    assert_eq!(left.reused, right.reused);
    assert_eq!(left.invalidation, right.invalidation);
    assert!(left.dependencies.is_empty() && right.dependencies.is_empty());
    let left = left.draft.as_ref().unwrap();
    let right = right.draft.as_ref().unwrap();
    assert_eq!(left.title, right.title);
    assert_eq!(left.origin, right.origin);
    assert_eq!(left.inverse_of, right.inverse_of);
    assert_eq!(left.allocated_ids, right.allocated_ids);
    assert!(left.read_preconditions.is_empty() && right.read_preconditions.is_empty());
    assert_eq!(left.operations.len(), right.operations.len());
    for (left, right) in left.operations.iter().zip(&right.operations) {
        assert_eq!(left.target, right.target);
        assert_eq!(left.expected, right.expected);
        assert_eq!(left.proposed, right.proposed);
        assert_eq!(left.apply_after, right.apply_after);
    }
}

#[test]
fn named_capture_repeats_exact_bytes_time_opaque_ids_and_dependencies() {
    let (temp, store) = store();
    let allocation = allocation();
    let request = request(
        b"first\r\n\nlast \xf0\x9f\x8d\xb5".to_vec(),
        ExtractionInput::Utf8Preserve,
    );
    let first = store
        .plan_capture_named(request.clone(), &allocation)
        .unwrap();
    let repeated = store
        .plan_capture_named(request.clone(), &allocation)
        .unwrap();
    assert_same_plan(&first, &repeated);
    assert_eq!(first.capture_state, Some(SourceCaptureState::Complete));
    assert_eq!(bytes(&first, "/original.bin"), request.original);
    assert_eq!(bytes(&first, "/content.md"), request.original);

    let source = record(&first, "/source.md");
    assert_eq!(source.kind(), RecordKind::Source);
    assert_eq!(source.id(), &allocation.source_id);
    assert_eq!(source.title(), request.title);
    assert_eq!(source.string("wiki_origin"), Some(request.origin.as_str()));
    assert_eq!(source.string("wiki_origin_kind"), Some("local-file"));
    assert_eq!(source.string("wiki_status"), Some("active"));
    assert_eq!(
        source.string("wiki_current_revision"),
        Some(allocation.revision_id.as_str())
    );
    assert_eq!(
        source.fields()["wiki_revisions"],
        serde_json::json!([allocation.revision_id.as_str()])
    );
    let revision = record(&first, "/revision.md");
    assert_eq!(revision.kind(), RecordKind::Revision);
    assert_eq!(revision.id(), &allocation.revision_id);
    assert_eq!(
        revision.string("wiki_source_id"),
        Some(allocation.source_id.as_str())
    );
    assert_eq!(
        revision.string("wiki_captured_at"),
        Some(allocation.captured_at.as_str())
    );
    assert_eq!(
        revision.string("wiki_media_type"),
        request.media_type.as_deref()
    );
    let expected_hash = Blake3Hash::digest(&request.original);
    assert_eq!(
        revision.string("wiki_original_hash"),
        Some(expected_hash.as_str())
    );
    assert_eq!(
        revision.string("wiki_content_hash"),
        Some(expected_hash.as_str())
    );

    let draft = first.draft.as_ref().unwrap();
    let parent = format!(
        "sources/{}/revisions/{}",
        allocation.source_id, allocation.revision_id
    );
    let targets: Vec<_> = draft
        .operations
        .iter()
        .map(|write| write.target.as_str())
        .collect();
    assert_eq!(
        targets,
        vec![
            format!("{parent}/original.bin"),
            format!("{parent}/content.md"),
            format!("{parent}/revision.md"),
            format!("sources/{}/source.md", allocation.source_id)
        ]
    );
    assert!(
        draft
            .operations
            .iter()
            .all(|write| write.expected == ExpectedState::Absent)
    );
    assert!(
        draft.operations[0].apply_after.is_empty() && draft.operations[1].apply_after.is_empty()
    );
    assert_eq!(
        draft.operations[2].apply_after,
        draft.operations[..2]
            .iter()
            .map(|write| write.target.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        draft.operations[3].apply_after,
        draft.operations[..3]
            .iter()
            .map(|write| write.target.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        source.string("wiki_revision"),
        Some(format!("[[{parent}/revision.md]]").as_str())
    );
    assert_eq!(
        revision.string("wiki_source"),
        Some(format!("[[sources/{}/source.md]]", allocation.source_id).as_str())
    );
    assert_eq!(draft.allocated_ids["source"], allocation.source_id);
    assert_eq!(draft.allocated_ids["revision"], allocation.revision_id);
    assert!(!first.reused);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn named_capture_preserves_empty_unsupported_and_supplied_extraction() {
    let (_temp, store) = store();
    let allocation = allocation();
    let fingerprint = Blake3Hash::digest(b"frozen external extractor");
    let cases = [
        (
            vec![],
            ExtractionInput::Utf8Preserve,
            Some(vec![]),
            SourceCaptureState::Empty,
        ),
        (
            vec![0xff, 0x00],
            ExtractionInput::Utf8Preserve,
            None,
            SourceCaptureState::Unsupported,
        ),
        (
            b"original text".to_vec(),
            ExtractionInput::Unsupported {
                extractor: "unsupported-format-v1".into(),
                fingerprint: fingerprint.clone(),
            },
            None,
            SourceCaptureState::Unsupported,
        ),
        (
            vec![0xff, 0x00],
            ExtractionInput::Supplied {
                extractor: "external-v1".into(),
                fingerprint: fingerprint.clone(),
                content: b"different extracted\r\ntext".to_vec(),
            },
            Some(b"different extracted\r\ntext".to_vec()),
            SourceCaptureState::Complete,
        ),
        (
            b"nonempty original".to_vec(),
            ExtractionInput::Supplied {
                extractor: "external-v1".into(),
                fingerprint: fingerprint.clone(),
                content: vec![],
            },
            Some(vec![]),
            SourceCaptureState::Empty,
        ),
    ];
    for (original, extraction, content, state) in cases {
        let mut request = request(original.clone(), extraction);
        request.origin_kind = SourceOrigin::Url;
        request.origin = "https://example.invalid/input?a=1#frozen".into();
        let plan = store
            .plan_capture_named(request.clone(), &allocation)
            .unwrap();
        assert_eq!(plan.capture_state, Some(state));
        assert_eq!(bytes(&plan, "/original.bin"), original);
        let revision = record(&plan, "/revision.md");
        assert_eq!(
            revision.string("wiki_original_hash"),
            Some(Blake3Hash::digest(&original).as_str())
        );
        assert_eq!(
            record(&plan, "/source.md").string("wiki_origin"),
            Some(request.origin.as_str())
        );
        assert_eq!(
            record(&plan, "/source.md").string("wiki_origin_kind"),
            Some("url")
        );
        match content {
            Some(content) => {
                assert_eq!(bytes(&plan, "/content.md"), content);
                assert_eq!(
                    revision.string("wiki_content_hash"),
                    Some(Blake3Hash::digest(&content).as_str())
                );
                assert_eq!(revision.string("wiki_extraction_status"), Some("complete"));
                assert_eq!(plan.draft.as_ref().unwrap().operations.len(), 4);
            }
            None => {
                assert_eq!(
                    revision.string("wiki_extraction_status"),
                    Some("unsupported")
                );
                assert!(
                    revision.string("wiki_content_path").is_none()
                        && revision.string("wiki_content_hash").is_none()
                );
                assert_eq!(plan.draft.as_ref().unwrap().operations.len(), 3);
            }
        }
        if !matches!(request.extraction, ExtractionInput::Utf8Preserve) {
            assert_eq!(
                revision.string("wiki_extractor_fingerprint"),
                Some(fingerprint.as_str())
            );
        }
    }
}

#[test]
fn named_capture_refuses_invalid_time_overlapping_ids_and_wire_ids() {
    let (temp, store) = store();
    let request = request(b"bytes".to_vec(), ExtractionInput::Utf8Preserve);
    for invalid in [
        "",
        "2024-02-30T09:08:07Z",
        "2024-02-29T25:08:07Z",
        "2024-02-29T09:08:07",
        "now",
    ] {
        let mut allocation = allocation();
        allocation.captured_at = invalid.into();
        assert_eq!(
            store
                .plan_capture_named(request.clone(), &allocation)
                .unwrap_err()
                .message,
            "captured_at must be RFC3339"
        );
    }
    // Valid RFC3339 alone does not satisfy the generated revision's UTC field
    // contract. Keep a separate rejection assertion after lexical parsing.
    for non_utc in [
        "2024-02-29T09:08:07.1200+05:30",
        "2024-02-29T09:08:07-04:00",
    ] {
        let mut allocation = allocation();
        allocation.captured_at = non_utc.into();
        let error = store
            .plan_capture_named(request.clone(), &allocation)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RecordInvalid);
        assert_eq!(error.message, "wiki_captured_at must use UTC");
        assert_eq!(error.details["field"], "wiki_captured_at");
    }
    let mut overlapping = allocation();
    overlapping.revision_id = overlapping.source_id.clone();
    assert_eq!(
        store
            .plan_capture_named(request, &overlapping)
            .unwrap_err()
            .message,
        "capture source and revision IDs must differ"
    );
    for field in ["source_id", "revision_id"] {
        let mut value = serde_json::to_value(allocation()).unwrap();
        value[field] = "../unsafe".into();
        assert!(serde_json::from_value::<CaptureAllocation>(value).is_err());
    }
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn named_and_standard_capture_preserve_invalid_extraction_refusals() {
    let (_temp, store) = store();
    for extraction in [
        ExtractionInput::Supplied {
            extractor: "external-v1".into(),
            fingerprint: Blake3Hash::digest(b"external-v1"),
            content: vec![0xff],
        },
        ExtractionInput::Unsupported {
            extractor: String::new(),
            fingerprint: Blake3Hash::digest(b"empty extractor"),
        },
    ] {
        let request = request(vec![0xff], extraction);
        let normal = store.plan_capture(request.clone()).unwrap_err();
        let named = store
            .plan_capture_named(request, &allocation())
            .unwrap_err();
        assert_eq!(normal.code, named.code);
        assert_eq!(normal.message, named.message);
    }
}

#[test]
fn standard_capture_reproduces_through_frozen_named_allocation() {
    let (_temp, store) = store();
    let request = request(b"ordinary bytes\n".to_vec(), ExtractionInput::Utf8Preserve);
    let normal = store.plan_capture(request.clone()).unwrap();
    let revision = record(&normal, "/revision.md");
    let allocation = CaptureAllocation {
        source_id: normal.source_id.clone(),
        revision_id: normal.revision_id.clone(),
        captured_at: revision.string("wiki_captured_at").unwrap().into(),
    };
    assert!(
        normal.source_id.as_str().len() == 39
            && normal
                .source_id
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
    );
    let named = store
        .plan_capture_named(request.clone(), &allocation)
        .unwrap();
    assert_same_plan(&normal, &named);
    let another = store.plan_capture(request).unwrap();
    assert_ne!(normal.source_id, another.source_id);
    assert_ne!(normal.revision_id, another.revision_id);
}

#[test]
fn ordinary_revision_clock_wrapper_matches_fixed_timestamp_generator() {
    let allocation = allocation();
    let request = request(b"refresh bytes".to_vec(), ExtractionInput::Utf8Preserve);
    let extraction = extract(&request).unwrap();
    let source_path =
        VaultRelativePath::new(format!("sources/{}/source.md", allocation.source_id)).unwrap();
    let ordinary = revision_writes(
        &allocation.source_id,
        &allocation.revision_id,
        &source_path,
        &request,
        &extraction,
    )
    .unwrap();
    let parsed = parse_note(ordinary.last().unwrap().proposed.as_deref().unwrap());
    let revision = parsed.canonical.unwrap();
    let frozen = revision_writes_at(
        &allocation.source_id,
        &allocation.revision_id,
        &source_path,
        &request,
        &extraction,
        revision.string("wiki_captured_at").unwrap(),
    )
    .unwrap();
    for (ordinary, frozen) in ordinary.iter().zip(&frozen) {
        assert_eq!(ordinary.target, frozen.target);
        assert_eq!(ordinary.proposed, frozen.proposed);
        assert_eq!(ordinary.expected, frozen.expected);
        assert_eq!(ordinary.apply_after, frozen.apply_after);
    }
    assert_eq!(ordinary.len(), frozen.len());
}

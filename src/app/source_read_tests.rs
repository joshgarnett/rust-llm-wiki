use super::*;
use crate::{
    app::{OperationOptions, SourceCaptureRequest, offline::init},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{VaultFs, VaultRoot},
};
use std::{fs, path::PathBuf};

fn options() -> OperationOptions {
    OperationOptions {
        offline: true,
        ..Default::default()
    }
}
fn fixture(normalized: bool) -> (tempfile::TempDir, OfflineApp, PathBuf, RecordId) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("wiki");
    init(&root, "Direct source read", options()).unwrap();
    let app =
        OfflineApp::new(VaultFs::new(VaultRoot::explicit(&root).unwrap()), options()).unwrap();
    if normalized {
        app.index_rebuild_normalized().unwrap();
    }
    let original = temp.path().join("My é's note.md");
    fs::write(&original, "éAlpha\nExact condition remains.\n").unwrap();
    let captured = app
        .capture_sources(SourceCaptureRequest {
            paths: vec![original.clone()],
            files_from: None,
        })
        .unwrap();
    let id = captured.items[0].source_id.clone().unwrap();
    (temp, app, original, id)
}
fn request(id: &RecordId, revision: Option<RecordId>, max_bytes: usize) -> SourceReadRequest {
    SourceReadRequest {
        source_id: id.clone(),
        revision_id: revision,
        range: None,
        from: None,
        max_bytes,
    }
}

#[test]
fn source_id_read_returns_exact_current_then_history_without_writer_changes() {
    for normalized in [false, true] {
        let (_temp, app, original, id) = fixture(normalized);
        let lock = app.fs.root().path().join(".wiki/state/writer.lock");
        let before = fs::read(&lock).unwrap();
        let first = app.read_source(request(&id, None, 1024)).unwrap();
        assert_eq!(first.read.body, "éAlpha\nExact condition remains.\n");
        assert_eq!(
            first.read.source_citation.as_ref().unwrap().eligibility,
            Eligibility::Current
        );
        assert_eq!(
            first.read.source_citation.as_ref().unwrap().citation,
            CitationRef::Source(SourceSpanRef {
                source_id: id.clone(),
                source_revision: first.revision_id.clone(),
                span: first.read.range,
                quote_hash: Blake3Hash::digest(first.read.body.as_bytes())
            })
        );
        assert_eq!(fs::read(&lock).unwrap(), before);
        fs::write(&original, "New current condition.\n").unwrap();
        app.capture_sources(SourceCaptureRequest {
            paths: vec![original],
            files_from: None,
        })
        .unwrap();
        let current = app.read_source(request(&id, None, 1024)).unwrap();
        assert_eq!(current.read.body, "New current condition.\n");
        let history = app
            .read_source(request(&id, Some(first.revision_id), 1024))
            .unwrap();
        assert_eq!(history.read.body, "éAlpha\nExact condition remains.\n");
        assert_eq!(
            history.read.source_citation.as_ref().unwrap().eligibility,
            Eligibility::Historical
        );
    }
}

#[test]
fn truncation_cites_only_returned_utf8_bytes_and_continuation_is_exact() {
    for normalized in [false, true] {
        let (_temp, app, _original, id) = fixture(normalized);
        let first = app.read_source(request(&id, None, 3)).unwrap();
        assert_eq!(first.read.body, "éA");
        assert!(first.read.truncated);
        let CitationRef::Source(reference) = &first.read.source_citation.as_ref().unwrap().citation
        else {
            panic!("source citation expected")
        };
        assert_eq!(reference.span, ByteSpan::new(0, 3).unwrap());
        assert_eq!(reference.quote_hash, Blake3Hash::digest("éA".as_bytes()));
        let mut continuation = request(&id, None, 1024);
        continuation.from = Some(first.read.continuation.unwrap().start());
        assert_eq!(
            app.read_source(continuation).unwrap().read.body,
            "lpha\nExact condition remains.\n"
        );
        assert_eq!(
            app.read_source(request(&id, None, 1)).unwrap_err().code,
            ErrorCode::Usage
        );
    }
}

#[test]
fn exact_foreign_revision_refuses_instead_of_reading_another_source() {
    let (_temp, app, _original, id) = fixture(false);
    let other = app
        .source_add(CaptureRequest {
            title: "Other".into(),
            origin_kind: SourceOrigin::AgentReport,
            origin: "agent report".into(),
            original: b"other".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let revision = other.allocated_ids["revision"].clone();
    assert_eq!(
        app.read_source(request(&id, Some(revision), 1024))
            .unwrap_err()
            .code,
        ErrorCode::SourceIntegrity
    );
}

#[test]
fn empty_and_unsupported_capture_never_return_a_fabricated_citation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("wiki");
    init(&root, "Empty sources", options()).unwrap();
    let app =
        OfflineApp::new(VaultFs::new(VaultRoot::explicit(&root).unwrap()), options()).unwrap();
    let empty = temp.path().join("empty.txt");
    fs::write(&empty, []).unwrap();
    let binary = temp.path().join("image.bin");
    fs::write(&binary, [0, 255]).unwrap();
    let outcome = app
        .capture_sources(SourceCaptureRequest {
            paths: vec![empty, binary],
            files_from: None,
        })
        .unwrap();
    assert_eq!(outcome.totals.captured, 2);
    assert!(outcome.items.iter().all(|item| item.citation.is_none()));
    let empty = outcome
        .items
        .iter()
        .find(|item| item.capture_state == Some(SourceCaptureState::Empty))
        .unwrap();
    let empty_read = app
        .read_source(request(empty.source_id.as_ref().unwrap(), None, 1024))
        .unwrap();
    assert_eq!(empty_read.capture_state, SourceCaptureState::Empty);
    assert!(empty_read.read.body.is_empty());
    assert!(empty_read.read.source_citation.is_none());
    let binary = outcome
        .items
        .iter()
        .find(|item| item.capture_state == Some(SourceCaptureState::Unsupported))
        .unwrap();
    assert_eq!(
        app.read_source(request(binary.source_id.as_ref().unwrap(), None, 1024))
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
}

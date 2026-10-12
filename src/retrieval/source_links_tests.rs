//! Public disposable fixtures plus focused cumulative accounting checks.
use super::*;
use crate::{
    app::{
        OfflineApp, OperationOptions, PageSourceRefs, SourceCaptureRequest, SourceReadRequest,
        offline::init,
    },
    catalog::Catalog,
    domain::{ByteSpan, SourceSpanRef},
    retrieval::{
        ContextBudget, ContextCheckpoint, ContextFault, ContextOptions, ContextRequest,
        ContextScope, SearchMode, VerificationBudget, verification,
    },
    vault::{VaultFs, VaultRoot},
};
use std::{fs, path::PathBuf, sync::Arc};
const ORIGINAL: &str = "navigationfixture: the cobalt key opens the survey cabinet.\n";
fn fixture(normalized: bool) -> (tempfile::TempDir, OfflineApp, PathBuf, RecordId) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Wiki é space #100% (draft)");
    let options = OperationOptions {
        offline: true,
        ..Default::default()
    };
    init(&root, "Readable navigation", options).unwrap();
    let app = OfflineApp::new(VaultFs::new(VaultRoot::explicit(&root).unwrap()), options).unwrap();
    if normalized {
        app.index_rebuild_normalized().unwrap();
    }
    let original = temp.path().join("Café [manual] #100% (édition).md");
    fs::write(&original, ORIGINAL).unwrap();
    let captured = app
        .capture_sources(SourceCaptureRequest {
            paths: vec![original.clone()],
            files_from: None,
            markdown_only: false,
        })
        .unwrap();
    let id = captured.items[0].source_id.clone().unwrap();
    (temp, app, original, id)
}
fn catalog(app: &OfflineApp) -> Catalog {
    Catalog::new(app.fs().clone(), app.vault_id().clone())
}
fn read(
    app: &OfflineApp,
    id: &RecordId,
    revision: Option<RecordId>,
) -> crate::app::SourceReadOutcome {
    app.read_source(SourceReadRequest {
        source_id: id.clone(),
        revision_id: revision,
        range: None,
        from: None,
        max_bytes: 1024,
    })
    .unwrap()
}
// Inspect declared metadata through canonical identity resolution for either
// catalog layout. The legacy-only projection helper below is for resolver tests.
fn declared_presentation(app: &OfflineApp, id: &RecordId) -> SourcePresentation {
    let view = crate::sources::SourceView::from_fs_bounded(app.fs(), 1024 * 1024, 128).unwrap();
    let (_, note) = view.resolve(id, RecordKind::Source, None).unwrap();
    let source = note.canonical.as_ref().unwrap();
    let presentation = SourcePresentation::from_record(source).unwrap().unwrap();
    presentation.validate_owner(source).unwrap();
    presentation
}
fn target(link: &SourceReadLink) -> PathBuf {
    let path = link
        .markdown
        .rsplit_once("](<")
        .unwrap()
        .1
        .strip_suffix(">)")
        .unwrap();
    let mut bytes = Vec::new();
    let encoded = path.as_bytes();
    let mut i = 0;
    while i < encoded.len() {
        if encoded[i] == b'%' {
            bytes.push(u8::from_str_radix(&path[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            bytes.push(encoded[i]);
            i += 1;
        }
    }
    PathBuf::from(String::from_utf8(bytes).unwrap())
}
fn request(scope: ContextScope) -> ContextRequest {
    let mut request = ContextRequest {
        scope,
        budget: ContextBudget {
            max_bytes: 6000,
            max_tokens: 1500,
            ..Default::default()
        },
        ..Default::default()
    };
    request.documents.mode = SearchMode::Lexical;
    request.verification_budget.max_elapsed_ms = 30_000;
    request
}
#[test]
fn public_current_history_withdrawn_links_open_owned_files_and_keep_exact_refs() {
    for normalized in [false, true] {
        let (_temp, app, original, id) = fixture(normalized);
        let first = read(&app, &id, None);
        let link = first.source_link.as_ref().expect("current saved copy link");
        assert_eq!(link.status, SourceReadLinkStatus::Current);
        assert_eq!(link.source_id, id);
        assert_eq!(link.revision_id, first.revision_id);
        assert!(!link.markdown.contains("file://"));
        let encoded_path = link.markdown.rsplit_once("](<").unwrap().1;
        assert!(!encoded_path.contains('#'));
        for escaped in ["%20", "%23", "%25", "%28", "%29", "%C3%A9"] {
            assert!(encoded_path.contains(escaped), "{link:?}");
        }
        assert!(link.markdown.contains("\\[manual\\]"));
        let current = target(link);
        assert!(current.is_absolute());
        assert!(fs::read_to_string(&current).unwrap().contains(ORIGINAL));
        assert_eq!(
            first.read.source_citation.as_ref().unwrap().citation,
            CitationRef::Source(SourceSpanRef {
                source_id: id.clone(),
                source_revision: first.revision_id.clone(),
                span: ByteSpan::new(0, ORIGINAL.len() as u64).unwrap(),
                quote_hash: Blake3Hash::digest(ORIGINAL)
            })
        );
        // Capture alone does not publish an older saved copy. A Page citation
        // publishes this exact revision through the ordinary public write route.
        let citation = &first.read.source_citation.as_ref().unwrap().citation;
        let refs = PageSourceRefs::from_json_slice(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": "1", "citations": [citation]
            }))
            .unwrap(),
        )
        .unwrap();
        app.page_initialize_with_source_refs(
            Some(VaultRelativePath::new("pages/navigation.md").unwrap()),
            None,
            "Cabinet instructions".into(),
            "The cobalt key opens the survey cabinet.\n".into(),
            refs,
        )
        .unwrap();
        let presentation = declared_presentation(&app, &id);
        assert!(
            presentation.published.contains_key(&first.revision_id),
            "Page citation publishes the exact revision (normalized={normalized})"
        );
        fs::write(
            &original,
            "navigationfixture: the silver key replaces the cobalt key.\n",
        )
        .unwrap();
        app.capture_sources(SourceCaptureRequest {
            paths: vec![original],
            files_from: None,
            markdown_only: false,
        })
        .unwrap();
        let presentation = declared_presentation(&app, &id);
        let saved = presentation.published[&first.revision_id]
            .as_ref()
            .expect("refresh materializes the published older copy");
        let declared_saved_path = app.fs().root().resolve(&saved.path).unwrap();
        let saved_bytes = fs::read(&declared_saved_path).unwrap();
        assert_eq!(Blake3Hash::digest(&saved_bytes), saved.generated_hash);
        let history = read(&app, &id, Some(first.revision_id.clone()));
        assert_eq!(
            history.read.source_citation.as_ref().unwrap().citation,
            *citation,
            "history preserves the exact citation"
        );
        let old_link = history
            .source_link
            .as_ref()
            .expect("published older copy link");
        assert_eq!(old_link.status, SourceReadLinkStatus::Historical);
        assert_ne!(target(old_link), current);
        assert_eq!(
            target(old_link),
            fs::canonicalize(declared_saved_path).unwrap()
        );
        assert!(
            fs::read_to_string(target(old_link))
                .unwrap()
                .contains(ORIGINAL)
        );
        fs::remove_file(&current).unwrap();
        let history = read(&app, &id, Some(first.revision_id.clone()));
        assert_eq!(
            history.source_link.as_ref().unwrap().markdown,
            old_link.markdown
        );
        // Withdrawal uses a fresh untouched fixture after the missing-view case.
        let (_temp2, withdrawn, _original2, id2) = fixture(normalized);
        withdrawn
            .source_withdraw(id2.clone(), "Fixture retirement")
            .unwrap();
        let output = read(&withdrawn, &id2, None);
        assert_eq!(
            output.source_link.as_ref().unwrap().status,
            SourceReadLinkStatus::Withdrawn
        );
        assert_eq!(
            output.read.source_citation.as_ref().unwrap().eligibility,
            crate::domain::Eligibility::Withdrawn
        );
    }
}
#[test]
fn missing_edited_oversized_optional_views_preserve_evidence_and_do_not_write() {
    for normalized in [false, true] {
        let (_temp, app, _original, id) = fixture(normalized);
        let first = read(&app, &id, None);
        let path = target(first.source_link.as_ref().unwrap());
        let original = fs::read(&path).unwrap();
        let lock = app.fs.root().path().join(".wiki/state/writer.lock");
        let lock_before = fs::read(&lock).unwrap();
        for content in [
            Some(b"Edited saved display".to_vec()),
            Some(vec![b'x'; MAX_FILE_BYTES + 1]),
            None,
        ] {
            if let Some(content) = content {
                fs::write(&path, &content).unwrap();
            } else {
                fs::remove_file(&path).unwrap();
            }
            let output = read(&app, &id, None);
            assert!(output.source_link.is_none());
            assert_eq!(output.read.body, first.read.body);
            assert_eq!(
                serde_json::to_value(&output.read.source_citation).unwrap(),
                serde_json::to_value(&first.read.source_citation).unwrap()
            );
            assert_eq!(fs::read(&lock).unwrap(), lock_before);
        }
        fs::write(&path, original).unwrap();
        assert!(read(&app, &id, None).source_link.is_some());
    }
}
struct EditView(PathBuf);
impl ContextFault for EditView {
    fn check(&self, _: ContextCheckpoint) -> crate::domain::Result<()> {
        fs::write(&self.0, "Edited after packing").unwrap();
        Ok(())
    }
}
#[test]
fn exact_public_context_bounds_and_final_edit_keep_the_same_passages() {
    for normalized in [false, true] {
        let (_temp, app, _original, id) = fixture(normalized);
        let path = target(read(&app, &id, None).source_link.as_ref().unwrap());
        let catalog = catalog(&app);
        let scope = if normalized {
            ContextScope::IndexedDocuments
        } else {
            ContextScope::Current
        };
        let request = request(scope);
        let first = verification::context(&catalog, None, "navigationfixture", &request).unwrap();
        assert!(!first.passages().is_empty());
        assert!(first.passages().iter().any(|p| !p.source_links.is_empty()));
        assert!(first.text().contains("Source: ["));
        assert_eq!(first.usage().rendered_bytes, first.text().len());
        assert!(first.text().len() <= 6000);
        assert!(first.usage().estimated_tokens <= 1500);
        let changed = verification::context_with_options(
            &catalog,
            None,
            "navigationfixture",
            &request,
            &ContextOptions {
                fault: Some(Arc::new(EditView(path))),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(changed.passages().len(), first.passages().len());
        for (before, after) in first.passages().iter().zip(changed.passages()) {
            assert_eq!(before.citations, after.citations);
            assert_eq!(before.span, after.span);
            assert_eq!(before.text, after.text);
            assert!(after.source_links.is_empty());
        }
        assert!(!changed.text().contains("Source: ["));
        assert_eq!(changed.usage().rendered_bytes, changed.text().len());
        assert!(changed.text().len() < first.text().len());
    }
}
#[test]
fn markdown_encoding_is_exact_and_does_not_double_encode_percent() {
    #[cfg(unix)]
    {
        let link =
            markdown_link("A [title]\\x\n", Path::new("/tmp/Wiki é #100% (v)/note.md")).unwrap();
        assert_eq!(
            link,
            "[A \\[title\\]\\\\x ](</tmp/Wiki%20%C3%A9%20%23100%25%20%28v%29/note.md>)"
        );
    }
    assert!(markdown_link("title", Path::new("relative/note.md")).is_none());
}
#[test]
fn optional_reads_reserve_mandatory_resources_and_count_failed_work() {
    let (_temp, app, _original, id) = fixture(false);
    let output = read(&app, &id, None);
    let full = target(output.source_link.as_ref().unwrap());
    let relative = VaultRelativePath::new(
        full.strip_prefix(app.fs.root().path())
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let size = fs::metadata(&full).unwrap().len() as usize;
    let budget = VerificationBudget {
        max_bytes: size * 3,
        max_files: 100,
        max_entries: 1000,
        max_elapsed_ms: 30_000,
    };
    let mut meter = Meter::new(&budget);
    assert!(
        meter
            .read_optional(&catalog(&app), &relative, size - 1, (size * 2, 0, 0), 1000)
            .is_none()
    );
    assert_eq!(meter.work().0, 0);
    assert!(meter.work().1 > 0);
    assert!(meter.work().2 > 0);
    assert!(
        meter
            .read_optional(&catalog(&app), &relative, size, (size * 2, 0, 0), 1000)
            .is_some()
    );
    assert_eq!(meter.work().0, size);
    assert!(
        meter
            .read_optional(&catalog(&app), &relative, size, (size * 2, 0, 0), 1000)
            .is_none()
    );
    assert_eq!(meter.work().0, size);
}

fn authenticated_rows(
    app: &OfflineApp,
) -> (
    BTreeMap<RecordId, crate::catalog::RecordRow>,
    Vec<DocumentRow>,
) {
    let budget =
        crate::catalog::scan::ReadScanBudget::new(crate::catalog::scan::ReadScanLimits::default())
            .unwrap();
    let reader = catalog(app)
        .read_only_snapshot_with_budget(&budget)
        .unwrap();
    (
        reader.projection().records.clone(),
        reader.projection().documents.clone(),
    )
}
#[test]
fn declared_revision_owner_and_content_hash_are_required() {
    let (_temp, app, _original, id) = fixture(false);
    let first = read(&app, &id, None);
    let (records, documents) = authenticated_rows(&app);
    let original = records[&first.revision_id].clone();
    for (field, value) in [
        ("wiki_source_id", "source_foreign".to_owned()),
        (
            "wiki_content_hash",
            Blake3Hash::digest(b"different").to_string(),
        ),
    ] {
        let mut modified = records.clone();
        let mut revision = original.clone();
        let mut fields = revision.record.fields().clone();
        fields.insert(field.into(), value.into());
        revision.record = crate::domain::CanonicalRecord::new(fields).unwrap();
        modified.insert(first.revision_id.clone(), revision);
        let mut meter = Meter::new(&VerificationBudget {
            max_elapsed_ms: 30_000,
            ..Default::default()
        });
        let proof =
            SourceLinkProof::observe(&catalog(&app), &modified, &documents, &mut meter, (0, 0, 0));
        assert!(proof.links.0.is_empty());
        assert_eq!(meter.work(), (0, 0, 0));
    }
}

// Synthetic authenticated-row inputs isolate exact optional caps. Public workflows
// above establish the canonical proof and actual generated-display behavior.
fn synthetic_rows(
    app: &OfflineApp,
    count: usize,
    view_size: usize,
) -> (
    BTreeMap<RecordId, crate::catalog::RecordRow>,
    Vec<DocumentRow>,
) {
    let (base, documents) = authenticated_rows(app);
    let document = documents.iter().find(|d| d.source_id.is_some()).unwrap();
    let source = &base[document.source_id.as_ref().unwrap()];
    let revision = &base[document.owner_revision.as_ref().unwrap()];
    let mut records = BTreeMap::new();
    let mut copies = Vec::new();
    for index in 0..count {
        let source_id = RecordId::new(format!("source_navigation_{index}")).unwrap();
        let revision_id = RecordId::new(format!("revision_navigation_{index}")).unwrap();
        let path = crate::sources::presentation::allocate_path(
            "Cap fixture",
            &source_id,
            None,
            &BTreeSet::new(),
        )
        .unwrap();
        let bytes = vec![b'x'; view_size];
        fs::write(app.fs.root().resolve(&path).unwrap(), &bytes).unwrap();
        let mut metadata = SourcePresentation::from_record(&source.record)
            .unwrap()
            .unwrap();
        metadata.source_id = source_id.clone();
        metadata.current.path = path;
        metadata.current.revision_id = revision_id.clone();
        metadata.current.generated_hash = Blake3Hash::digest(&bytes);
        metadata.published.clear();
        let mut fields = source.record.fields().clone();
        fields.insert("wiki_id".into(), source_id.as_str().into());
        fields.insert("wiki_current_revision".into(), revision_id.as_str().into());
        fields.insert("wiki_revisions".into(), serde_json::json!([revision_id]));
        fields.insert(
            "wiki_presentation".into(),
            metadata.encode().unwrap().into(),
        );
        let mut source = source.clone();
        source.record = crate::domain::CanonicalRecord::new(fields).unwrap();
        let mut fields = revision.record.fields().clone();
        fields.insert("wiki_id".into(), revision_id.as_str().into());
        fields.insert("wiki_source_id".into(), source_id.as_str().into());
        let mut revision = revision.clone();
        revision.record = crate::domain::CanonicalRecord::new(fields).unwrap();
        let mut document = document.clone();
        document.source_id = Some(source_id.clone());
        document.owner_revision = Some(revision_id.clone());
        records.insert(source_id, source);
        records.insert(revision_id, revision);
        copies.push(document);
    }
    (records, copies)
}
#[test]
fn optional_revision_and_cumulative_initial_final_byte_caps_are_exact() {
    let (_temp, app, _original, _id) = fixture(false);
    let (records, documents) = synthetic_rows(&app, MAX_REVISIONS + 1, 16);
    let budget = VerificationBudget {
        max_elapsed_ms: 30_000,
        ..Default::default()
    };
    let mut meter = Meter::new(&budget);
    let mut proof =
        SourceLinkProof::observe(&catalog(&app), &records, &documents, &mut meter, (0, 0, 0));
    assert_eq!(proof.links.0.len(), MAX_REVISIONS);
    assert_eq!(proof.bytes, MAX_REVISIONS * 16);
    assert!(!proof.recheck(&catalog(&app), &mut meter));
    assert_eq!(proof.bytes, MAX_REVISIONS * 16 * 2);
    let (records, documents) = synthetic_rows(&app, 5, MAX_FILE_BYTES);
    let mut meter = Meter::new(&budget);
    let mut proof =
        SourceLinkProof::observe(&catalog(&app), &records, &documents, &mut meter, (0, 0, 0));
    assert_eq!(proof.bytes, MAX_TOTAL_BYTES / 2);
    assert_eq!(proof.links.0.len(), 2);
    assert!(!proof.recheck(&catalog(&app), &mut meter));
    assert_eq!(proof.links.0.len(), 2);
    assert_eq!(proof.bytes, MAX_TOTAL_BYTES);
    let (records, documents) = synthetic_rows(&app, 1, MAX_FILE_BYTES + 1);
    let mut meter = Meter::new(&budget);
    let proof =
        SourceLinkProof::observe(&catalog(&app), &records, &documents, &mut meter, (0, 0, 0));
    assert!(proof.links.0.is_empty());
    assert_eq!(proof.bytes, 0);
}
#[test]
fn host_cards_are_evidence_only_apply_counts_links_and_snapshot_omits_them() {
    use crate::retrieval::context_selection_packet::{SelectionAction, SelectionReply};
    let (_temp, app, _original, _id) = fixture(true);
    let catalog = catalog(&app);
    let request = request(ContextScope::IndexedDocuments);
    let prepared = verification::context_with_options(
        &catalog,
        None,
        "navigationfixture",
        &request,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            ..Default::default()
        },
    )
    .unwrap();
    let packet = prepared.selection_packet().unwrap();
    assert!(!packet.cards.is_empty());
    assert!(
        packet
            .cards
            .iter()
            .all(|card| card.passage.source_links.is_empty())
    );
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: packet.cards.iter().map(|card| card.id.clone()).collect(),
    };
    let output = verification::context_with_options(
        &catalog,
        None,
        "navigationfixture",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(output.text().contains("Source: ["));
    assert!(output.text().len() <= 6000);
    let mut snapshot = request.clone();
    snapshot.scope = ContextScope::Snapshot;
    let output = verification::context(&catalog, None, "navigationfixture", &snapshot).unwrap();
    assert!(output.passages().iter().all(|p| p.source_links.is_empty()));
    assert!(!output.text().contains("Source: ["));
}
#[test]
fn exact_packing_boundary_includes_navigation_before_admission() {
    let (_temp, app, _original, _id) = fixture(true);
    let catalog = catalog(&app);
    let mut request = request(ContextScope::IndexedDocuments);
    let first = verification::context(&catalog, None, "navigationfixture", &request).unwrap();
    let exact = first.text().len();
    assert_eq!(first.passages().len(), 1);
    request.budget.max_bytes = exact;
    request.budget.max_tokens = exact.div_ceil(4);
    let packed = verification::context(&catalog, None, "navigationfixture", &request).unwrap();
    assert_eq!(packed.text(), first.text());
    request.budget.max_bytes = exact - 1;
    let clipped = verification::context(&catalog, None, "navigationfixture", &request).unwrap();
    assert!(clipped.passages().is_empty());
    assert!(clipped.text().len() < exact);
}
#[cfg(unix)]
#[test]
fn symlink_optional_view_is_omitted_without_reading_external_bytes() {
    use std::os::unix::fs::symlink;
    let (temp, app, _original, id) = fixture(false);
    let first = read(&app, &id, None);
    let path = target(first.source_link.as_ref().unwrap());
    let external = temp.path().join("outside.md");
    fs::write(&external, fs::read(&path).unwrap()).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&external, &path).unwrap();
    let output = read(&app, &id, None);
    assert!(output.source_link.is_none());
    assert_eq!(output.read.body, ORIGINAL);
    assert_eq!(
        fs::read_to_string(external).unwrap(),
        fs::read_to_string(path).unwrap()
    );
}
#[test]
fn unpublished_history_omits_only_navigation() {
    let (_temp, app, original, id) = fixture(false);
    let first = read(&app, &id, None);
    fs::write(&original, "navigationfixture: a later original.\n").unwrap();
    app.capture_sources(SourceCaptureRequest {
        paths: vec![original],
        files_from: None,
        markdown_only: false,
    })
    .unwrap();
    let (mut records, documents) = authenticated_rows(&app);
    let source = records.get_mut(&id).unwrap();
    let mut metadata = SourcePresentation::from_record(&source.record)
        .unwrap()
        .unwrap();
    metadata.published.remove(&first.revision_id);
    let mut fields = source.record.fields().clone();
    fields.insert(
        "wiki_presentation".into(),
        metadata.encode().unwrap().into(),
    );
    source.record = crate::domain::CanonicalRecord::new(fields).unwrap();
    let historical = documents
        .iter()
        .find(|d| d.owner_revision.as_ref() == Some(&first.revision_id))
        .unwrap();
    let mut meter = Meter::new(&VerificationBudget {
        max_elapsed_ms: 30_000,
        ..Default::default()
    });
    let proof = SourceLinkProof::observe(
        &catalog(&app),
        &records,
        [historical],
        &mut meter,
        (0, 0, 0),
    );
    assert!(proof.links.0.is_empty());
    assert_eq!(meter.work(), (0, 0, 0));
    let actual = read(&app, &id, Some(first.revision_id));
    assert_eq!(actual.read.body, ORIGINAL);
    assert!(actual.read.source_citation.is_some());
}

#[test]
fn windows_prefix_normalization_and_encoding_are_portable() {
    use std::{ffi::OsStr, path::Prefix};
    let tail = r"\Wiki é #100% (draft)\sources\views\note.md";
    let normal = windows_destination(Prefix::Disk(b'C'), tail).unwrap();
    let verbatim = windows_destination(Prefix::VerbatimDisk(b'C'), tail).unwrap();
    assert_eq!(normal, verbatim);
    assert_eq!(
        encode_destination(&normal),
        "C:/Wiki%20%C3%A9%20%23100%25%20%28draft%29/sources/views/note.md"
    );
    let server = OsStr::new("server é");
    let share = OsStr::new("my share");
    let normal = windows_destination(Prefix::UNC(server, share), tail).unwrap();
    let verbatim = windows_destination(Prefix::VerbatimUNC(server, share), tail).unwrap();
    assert_eq!(normal, verbatim);
    assert_eq!(
        encode_destination(&normal),
        "//server%20%C3%A9/my%20share/Wiki%20%C3%A9%20%23100%25%20%28draft%29/sources/views/note.md"
    );
    for prefix in [
        Prefix::DeviceNS(OsStr::new("pipe")),
        Prefix::Verbatim(OsStr::new("GLOBALROOT")),
        Prefix::Disk(b'?'),
    ] {
        assert!(windows_destination(prefix, tail).is_none());
    }
    assert!(windows_destination(Prefix::Disk(b'C'), "relative\\note.md").is_none());
}

#[test]
fn prepare_reply_survives_view_edits_and_tight_optional_omission() {
    use crate::retrieval::context_selection_packet::{SelectionAction, SelectionReply};
    let (_temp, app, original, id) = fixture(true);
    let catalog = catalog(&app);
    let view = target(read(&app, &id, None).source_link.as_ref().unwrap());
    let generated = fs::read(&view).unwrap();
    let mut request = request(ContextScope::IndexedDocuments);
    let options = ContextOptions {
        selection: SelectionAction::Prepare,
        ..Default::default()
    };
    let initial =
        verification::context_with_options(&catalog, None, "navigationfixture", &request, &options)
            .unwrap();
    let original_request = request.clone();
    let original_packet = initial.selection_packet().unwrap();
    let original_reply = SelectionReply {
        packet_fingerprint: original_packet.fingerprint.clone(),
        ordered_ids: original_packet
            .cards
            .iter()
            .map(|card| card.id.clone())
            .collect(),
    };
    // Prepare counts mandatory evidence only. Reuse those actual bounds for both
    // commands, keeping their request binding exactly the same.
    request.verification_budget.max_bytes = initial.usage().verification_bytes;
    request.verification_budget.max_files = initial.usage().verification_files;
    request.verification_budget.max_entries = initial.usage().verification_entries;
    let prepared =
        verification::context_with_options(&catalog, None, "navigationfixture", &request, &options)
            .unwrap();
    let packet = prepared.selection_packet().unwrap();
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: packet.cards.iter().map(|card| card.id.clone()).collect(),
    };
    for optional_bytes in [Some(generated.clone()), Some(b"edited view".to_vec()), None] {
        if let Some(bytes) = optional_bytes {
            fs::write(&view, bytes).unwrap();
        } else {
            fs::remove_file(&view).unwrap();
        }
        let repeated = verification::context_with_options(
            &catalog,
            None,
            "navigationfixture",
            &request,
            &options,
        )
        .unwrap();
        assert_eq!(repeated.selection_packet().unwrap(), packet);
        let applied = verification::context_with_options(
            &catalog,
            None,
            "navigationfixture",
            &request,
            &ContextOptions {
                selection: SelectionAction::Apply(reply.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!applied.passages().is_empty());
        assert!(
            applied
                .passages()
                .iter()
                .all(|passage| passage.source_links.is_empty())
        );
        assert_eq!(
            applied.passages()[0].citations,
            packet.cards[0].passage.citations
        );
        assert_eq!(
            applied.usage().verification_bytes,
            prepared.usage().verification_bytes
        );
    }
    fs::write(&view, generated).unwrap();
    fs::write(
        &original,
        "navigationfixture: changed immutable captured evidence.\n",
    )
    .unwrap();
    // A genuine Source/head change retains ordinary strict packet rejection.
    // Public refresh uses the original configured path, independently of views.
    app.capture_sources(SourceCaptureRequest {
        paths: vec![original],
        files_from: None,
        markdown_only: false,
    })
    .unwrap();
    assert!(
        verification::context_with_options(
            &catalog,
            None,
            "navigationfixture",
            &original_request,
            &ContextOptions {
                selection: SelectionAction::Apply(original_reply),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn public_original_range_apply_emits_verified_links_and_final_removal() {
    use crate::retrieval::context_selection_packet::{
        OriginalRange, OriginalSelectionReply, OriginalSelectionRequest, OriginalSelectionVersion,
        SelectionAction,
    };
    let (_temp, app, _original, id) = fixture(true);
    let current = read(&app, &id, None);
    let view = target(current.source_link.as_ref().unwrap());
    let catalog = catalog(&app);
    let request = request(ContextScope::IndexedDocuments);
    let originals = OriginalSelectionRequest {
        paths: vec![current.read.path.clone()],
        max_input_bytes: 96 * 1024,
    };
    let prepared = verification::context_with_options(
        &catalog,
        None,
        "navigationfixture",
        &request,
        &ContextOptions {
            selection: SelectionAction::PrepareOriginals(originals.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let packet = prepared.selection_packet().unwrap();
    let reply = OriginalSelectionReply {
        version: OriginalSelectionVersion::V1,
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ranges: vec![OriginalRange {
            original_id: packet.originals[0].id.clone(),
            span: ByteSpan::new(0, ORIGINAL.len() as u64).unwrap(),
        }],
    };
    let options = ContextOptions {
        selection: SelectionAction::ApplyOriginals {
            request: originals,
            reply,
        },
        ..Default::default()
    };
    let applied =
        verification::context_with_options(&catalog, None, "navigationfixture", &request, &options)
            .unwrap();
    assert_eq!(applied.passages().len(), 1);
    assert_eq!(applied.passages()[0].text, ORIGINAL);
    assert_eq!(applied.passages()[0].source_links.len(), 1);
    assert!(applied.text().contains("Source: ["));
    let changed = verification::context_with_options(
        &catalog,
        None,
        "navigationfixture",
        &request,
        &ContextOptions {
            fault: Some(Arc::new(EditView(view))),
            ..options
        },
    )
    .unwrap();
    assert_eq!(
        changed.passages()[0].citations,
        applied.passages()[0].citations
    );
    assert_eq!(changed.passages()[0].span, applied.passages()[0].span);
    assert!(changed.passages()[0].source_links.is_empty());
}

#[allow(
    clippy::duplicate_mod,
    reason = "Reuse the existing isolated accounted embedding fixture; no provider/network transport is used."
)]
#[path = "../../tests/fixtures/p17/common.rs"]
mod embedding_common;
#[test]
fn cached_mock_hybrid_and_evidence_set_routes_emit_exact_verified_navigation() {
    use std::sync::atomic::Ordering;
    let fixture = embedding_common::Fixture::new();
    fixture.app.index_rebuild_normalized().unwrap();
    let original = fixture.temp.path().join("Hybrid é #100% [cabinet].md");
    fs::write(&original, ORIGINAL).unwrap();
    let captured = fixture
        .app
        .capture_sources(SourceCaptureRequest {
            paths: vec![original],
            files_from: None,
            markdown_only: true,
        })
        .unwrap();
    let source_id = captured.items[0].source_id.clone().unwrap();
    let responses = Arc::new(embedding_common::Responses::new());
    let dispatcher = embedding_common::dispatcher(&fixture.fs, responses.clone());
    let runtime = embedding_common::runtime(&fixture.service, &dispatcher);
    fixture
        .app
        .embeddings_sync(
            &crate::retrieval::spaces::EmbeddingSettings::default(),
            &runtime,
        )
        .unwrap();
    let calls = responses.calls.load(Ordering::SeqCst);
    assert!(calls > 0, "mock corpus preparation must actually execute");
    fixture.query_seed(&fixture.spec(), "navigationfixture", vec![1.0, 0.0]);
    let offline = fixture.offline();
    let mut request = request(ContextScope::IndexedDocuments);
    request.documents.mode = SearchMode::Hybrid;
    request.documents.filters.source_ids = vec![source_id.clone()];
    for policy in ["B", "F0"] {
        let context = crate::retrieval::context_evidence::with_policy_for_test(policy, || {
            offline
                .semantic_context("navigationfixture", &request, None, true, false)
                .unwrap()
        });
        assert!(!context.network_used);
        assert!(!context.passages().is_empty());
        assert_eq!(context.usage().rendered_bytes, context.text().len());
        assert!(context.text().len() <= 6000 && context.usage().estimated_tokens <= 1500);
        for passage in context.passages() {
            assert_eq!(passage.source_links.len(), 1);
            assert_eq!(passage.source_links[0].source_id, source_id);
            assert!(
                fs::read_to_string(target(&passage.source_links[0]))
                    .unwrap()
                    .contains(ORIGINAL)
            );
            for citation in &passage.citations {
                let CitationRef::Source(reference) = citation else {
                    panic!("source citation required")
                };
                assert_eq!(reference.span.slice(ORIGINAL).unwrap(), passage.text);
                assert_eq!(
                    reference.quote_hash,
                    Blake3Hash::digest(passage.text.as_bytes())
                );
            }
        }
    }
    assert_eq!(
        responses.calls.load(Ordering::SeqCst),
        calls,
        "cached lookup must make no additional mock requests"
    );
}

#[test]
fn public_cli_dispatch_json_and_human_outputs_use_current_observation_and_links() {
    use crate::cli::{
        arguments::{Arguments, OutputFormat},
        dispatch::{execute, present},
    };
    use clap::Parser;
    let (_temp, app, _original, id) = fixture(true);
    let root = app.fs().root().path().to_str().unwrap();
    for command in [
        vec!["source", "read", id.as_str()],
        vec![
            "context",
            "navigationfixture",
            "--mode",
            "lexical",
            "--max-bytes",
            "6000",
            "--max-tokens",
            "1500",
        ],
    ] {
        let mut argv = vec!["lwiki", "--no-user-config", "--offline", "--wiki", root];
        argv.extend(command);
        let args = Arguments::try_parse_from(argv).unwrap();
        let (envelope, code) = execute(&args);
        assert_eq!(code, 0, "{}", serde_json::to_string(&envelope).unwrap());
        assert!(envelope.ok);
        let mut json = Vec::new();
        present(&envelope, OutputFormat::Json, &mut json).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(json["meta"]["network_used"], false);
        assert!(
            json["meta"]["index_generation"].is_null(),
            "ordinary schema-3 current observation"
        );
        let mut human = Vec::new();
        present(&envelope, OutputFormat::Human, &mut human).unwrap();
        let human = String::from_utf8(human).unwrap();
        assert!(human.contains("Source: ["));
        assert!(human.contains("current saved copy"));
        assert!(human.contains("cobalt key"));
    }
}

#[test]
fn public_unpublished_history_and_malformed_canonical_keep_existing_semantics() {
    use crate::records::{edit_note, parse_note};
    let (_temp, app, original, id) = fixture(false);
    let first = read(&app, &id, None);
    fs::write(&original, "navigationfixture: updated original.\n").unwrap();
    app.capture_sources(SourceCaptureRequest {
        paths: vec![original],
        files_from: None,
        markdown_only: false,
    })
    .unwrap();
    let (rows, _) = authenticated_rows(&app);
    let source = &rows[&id];
    let path = app.fs().root().resolve(&source.path).unwrap();
    let bytes = fs::read(&path).unwrap();
    let note = parse_note(&bytes);
    let mut metadata = SourcePresentation::from_record(&source.record)
        .unwrap()
        .unwrap();
    metadata.published.remove(&first.revision_id);
    let bytes = edit_note(
        &note,
        &BTreeMap::from([(
            "wiki_presentation".into(),
            metadata.encode().unwrap().into(),
        )]),
        None,
        &note.source_hash,
    )
    .unwrap();
    fs::write(&path, &bytes).unwrap();
    let old = read(&app, &id, Some(first.revision_id.clone()));
    assert!(old.source_link.is_none());
    assert_eq!(old.read.body, ORIGINAL);
    assert_eq!(
        old.read.source_citation.as_ref().unwrap().citation,
        first.read.source_citation.as_ref().unwrap().citation
    );
    assert_eq!(
        old.read.source_citation.as_ref().unwrap().eligibility,
        crate::domain::Eligibility::Historical
    );
    assert_eq!(
        first.read.source_citation.as_ref().unwrap().eligibility,
        crate::domain::Eligibility::Current
    );
    // Invalid canonical presentation metadata retains its mandatory parse error.
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        text.lines()
            .any(|line| line.starts_with("wiki_presentation:"))
    );
    let malformed = text
        .lines()
        .map(|line| {
            if line.starts_with("wiki_presentation:") {
                "wiki_presentation: '{}'"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(parse_note(malformed.as_bytes()).canonical.is_none());
    fs::write(path, malformed).unwrap();
    assert!(
        app.read_source(SourceReadRequest {
            source_id: id,
            revision_id: Some(first.revision_id),
            range: None,
            from: None,
            max_bytes: 1024
        })
        .is_err()
    );
}

#[test]
fn multi_source_long_root_apply_counts_navigation_near_context_cap() {
    use crate::retrieval::context_selection_packet::{SelectionAction, SelectionReply};
    let temp = tempfile::tempdir().unwrap();
    // Keep the long escaped target while avoiding a prohibited trailing space.
    let long_root = "Long wiki é #100% (links) ".repeat(7);
    let root = temp.path().join(long_root.trim_end());
    let options = OperationOptions {
        offline: true,
        ..Default::default()
    };
    init(&root, "Long target navigation", options).unwrap();
    let app = OfflineApp::new(VaultFs::new(VaultRoot::explicit(root).unwrap()), options).unwrap();
    app.index_rebuild_normalized().unwrap();
    let mut paths = Vec::new();
    for index in 0..4 {
        let path = temp.path().join(format!(
            "Note {index} é [north cabinet] #100% ({}).md",
            "saved notes ".repeat(5)
        ));
        let body = format!(
            "navigationfixture {index}: condition {index} requires the cobalt key. {}\n",
            "The cabinet remains closed until the survey is complete. ".repeat(18)
        );
        fs::write(&path, body).unwrap();
        paths.push(path);
    }
    app.capture_sources(SourceCaptureRequest {
        paths,
        files_from: None,
        markdown_only: true,
    })
    .unwrap();
    let catalog = catalog(&app);
    let mut request = request(ContextScope::IndexedDocuments);
    request.documents.limits.excerpt_bytes = 1400;
    let prepared = verification::context_with_options(
        &catalog,
        None,
        "navigationfixture",
        &request,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            ..Default::default()
        },
    )
    .unwrap();
    let packet = prepared.selection_packet().unwrap();
    assert!(packet.cards.len() >= 4);
    assert!(
        packet
            .cards
            .iter()
            .all(|card| card.passage.source_links.is_empty())
    );
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: packet.cards.iter().map(|card| card.id.clone()).collect(),
    };
    let applied = verification::context_with_options(
        &catalog,
        None,
        "navigationfixture",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(applied.passages().len() >= 2);
    assert!(applied.text().len() >= 3000);
    assert!(applied.text().len() <= 6000 && applied.usage().estimated_tokens <= 1500);
    assert_eq!(applied.usage().rendered_bytes, applied.text().len());
    for passage in applied.passages() {
        assert!(!passage.source_links.is_empty());
        let link = &passage.source_links[0];
        assert!(link.markdown.len() >= 300);
        assert!(
            fs::read_to_string(target(link))
                .unwrap()
                .contains(&passage.text)
        );
    }
}

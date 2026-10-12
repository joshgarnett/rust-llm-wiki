use super::*;
use crate::{
    app::{OperationOptions, offline::init},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{VaultFs, VaultRoot},
};
use std::{
    fs,
    sync::{Arc, Barrier},
};

fn options() -> OperationOptions {
    OperationOptions {
        offline: true,
        ..Default::default()
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    app: OfflineApp,
    notes: PathBuf,
}
impl Fixture {
    fn new(normalized: bool, count: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("wiki");
        init(&root, "Capture tests", options()).unwrap();
        let app =
            OfflineApp::new(VaultFs::new(VaultRoot::explicit(&root).unwrap()), options()).unwrap();
        if normalized {
            app.index_rebuild_normalized().unwrap();
        }
        let notes = temp.path().join("Notes é's folder");
        fs::create_dir(&notes).unwrap();
        for index in 0..count {
            fs::write(
                notes.join(format!("note {index:03}.md")),
                format!("# Note {index}\nFact {index} remains exact.\n"),
            )
            .unwrap();
        }
        Self {
            _temp: temp,
            app,
            notes,
        }
    }
    fn capture(&self) -> SourceCaptureOutcome {
        self.app
            .capture_sources(SourceCaptureRequest {
                paths: vec![self.notes.clone()],
                files_from: None,
                markdown_only: false,
            })
            .unwrap()
    }
}

#[test]
fn markdown_only_explicit_list_and_folder_preview_capture_repeat_preserve_originals() {
    for normalized in [false, true] {
        for mode in 0..3 {
            let fixture = Fixture::new(normalized, 0);
            let originals = [
                ("One.MD", "# One\nOriginal first fact.\n"),
                ("Two.MaRkDoWn", "# Two\nOriginal second fact.\n"),
                ("Three.TXT", "An excluded text file.\n"),
                ("Four.bin", "An excluded attachment.\n"),
            ];
            for (name, bytes) in originals {
                fs::write(fixture.notes.join(name), bytes).unwrap();
            }
            let list = fixture._temp.path().join("files.txt");
            fs::write(
                &list,
                originals
                    .iter()
                    .map(|(name, _)| format!("{}/{name}\n", fixture.notes.display()))
                    .collect::<String>(),
            )
            .unwrap();
            let request = SourceCaptureRequest {
                paths: match mode {
                    0 => originals
                        .iter()
                        .map(|(name, _)| fixture.notes.join(name))
                        .collect(),
                    1 => vec![],
                    _ => vec![fixture.notes.clone()],
                },
                files_from: (mode == 1).then_some(list),
                markdown_only: true,
            };
            let preview_app = OfflineApp::new(
                fixture.app.fs.clone(),
                OperationOptions {
                    dry_run: true,
                    ..options()
                },
            )
            .unwrap();
            let lock = fixture.app.fs.root().path().join(".wiki/state/writer.lock");
            let before_lock = fs::read(&lock).unwrap();
            let before_changes = fixture.app.engine().unwrap().change_ids().unwrap();
            let preview = preview_app.capture_sources(request.clone()).unwrap();
            assert!(!preview.partial, "{:#?}", preview.items);
            assert!(preview.dry_run);
            assert_eq!(preview.totals.planned, 2);
            assert_eq!(preview.totals.skipped, 2);
            assert_eq!(preview.groups_committed, 0);
            assert!(preview.items.iter().all(|item| {
                item.source_id.is_none() && item.revision_id.is_none() && item.citation.is_none()
            }));
            assert_eq!(fs::read(lock).unwrap(), before_lock);
            assert_eq!(
                fixture.app.engine().unwrap().change_ids().unwrap(),
                before_changes
            );
            let first = fixture.app.capture_sources(request.clone()).unwrap();
            assert!(!first.partial, "{:#?}", first.items);
            assert_eq!(first.totals.captured, 2);
            assert_eq!(first.totals.skipped, 2);
            assert_eq!(first.groups_committed, 1);
            assert_eq!(first.changed_owner_ids.len(), 2);
            let changes = fixture.app.engine().unwrap().change_ids().unwrap();
            let second = fixture.app.capture_sources(request).unwrap();
            assert!(!second.partial, "{:#?}", second.items);
            assert_eq!(second.totals.unchanged, 2);
            assert_eq!(second.totals.skipped, 2);
            assert_eq!(second.groups_committed, 0);
            assert!(second.changed_owners.is_empty());
            assert_eq!(fixture.app.engine().unwrap().change_ids().unwrap(), changes);
            let view = source_view(&fixture.app).unwrap();
            assert_eq!(origin_lookup(&view).unwrap().len(), 2);
            for item in first
                .items
                .iter()
                .filter(|item| item.action == SourceCaptureAction::Captured)
            {
                let repeated = second
                    .items
                    .iter()
                    .find(|other| other.input == item.input)
                    .unwrap();
                assert_eq!(repeated.source_id, item.source_id);
                assert_eq!(repeated.revision_id, item.revision_id);
                view.verify(
                    &item.citation.as_ref().unwrap().citation,
                    CitationScope::Current,
                )
                .unwrap();
            }
            for (name, bytes) in originals {
                assert_eq!(
                    fs::read(fixture.notes.join(name)).unwrap(),
                    bytes.as_bytes()
                );
            }
        }
    }
}

#[test]
fn markdown_only_missing_original_check_ignores_remembered_non_markdown_files() {
    for normalized in [false, true] {
        let fixture = Fixture::new(normalized, 0);
        let names = ["One.MaRkDoWn", "Two.TXT", "Three.bin"];
        for name in names {
            fs::write(fixture.notes.join(name), "Exact retained original.\n").unwrap();
        }
        let initial = fixture
            .app
            .capture_sources(SourceCaptureRequest {
                paths: names.iter().map(|name| fixture.notes.join(name)).collect(),
                files_from: None,
                markdown_only: false,
            })
            .unwrap();
        assert_eq!(initial.totals.captured, 3);
        fs::remove_file(fixture.notes.join("Two.TXT")).unwrap();
        fs::remove_file(fixture.notes.join("Three.bin")).unwrap();
        let filtered_request = SourceCaptureRequest {
            paths: vec![fixture.notes.clone()],
            files_from: None,
            markdown_only: true,
        };
        let filtered = fixture
            .app
            .capture_sources(filtered_request.clone())
            .unwrap();
        assert!(!filtered.partial, "{:#?}", filtered.items);
        assert_eq!(filtered.items.len(), 1);
        assert_eq!(filtered.totals.unchanged, 1);
        assert_eq!(filtered.totals.failed, 0);
        let default = fixture.capture();
        assert!(default.partial);
        assert_eq!(default.totals.failed, 2);
        assert_eq!(default.totals.unchanged, 1);
        assert!(
            default
                .items
                .iter()
                .filter(|item| item.error.is_some())
                .all(|item| {
                    item.reason.as_deref() == Some("missing_original") && item.source_id.is_some()
                })
        );
        fs::remove_file(fixture.notes.join("One.MaRkDoWn")).unwrap();
        let missing_markdown = fixture.app.capture_sources(filtered_request).unwrap();
        assert!(missing_markdown.partial);
        assert_eq!(missing_markdown.items.len(), 1);
        assert_eq!(missing_markdown.totals.failed, 1);
        assert_eq!(
            missing_markdown.items[0].reason.as_deref(),
            Some("missing_original")
        );
        let view = source_view(&fixture.app).unwrap();
        assert_eq!(origin_lookup(&view).unwrap().len(), 3);
        for item in initial.items.iter().filter(|item| item.citation.is_some()) {
            view.verify(
                &item.citation.as_ref().unwrap().citation,
                CitationScope::Current,
            )
            .unwrap();
        }
    }
}

#[test]
fn thirty_five_notes_use_real_groups_then_unchanged_and_two_refreshes_preserve_history() {
    for normalized in [false, true] {
        let fixture = Fixture::new(normalized, 35);
        let first = fixture.capture();
        assert!(!first.partial, "{:#?}", first.items);
        assert_eq!(first.totals.captured, 35);
        assert_eq!(first.groups_committed, 5);
        assert_eq!(first.changed_owners.len(), 70);
        let identities: BTreeSet<_> = first
            .items
            .iter()
            .filter_map(|item| item.source_id.clone())
            .collect();
        assert_eq!(identities.len(), 35);
        assert!(first.items.iter().all(|item| {
            item.citation
                .as_ref()
                .is_some_and(|citation| citation.eligibility == Eligibility::Current)
        }));
        let change_count = fixture.app.engine().unwrap().change_ids().unwrap().len();
        let unchanged = fixture.capture();
        assert!(!unchanged.partial);
        assert_eq!(unchanged.totals.unchanged, 35);
        assert_eq!(unchanged.groups_committed, 0);
        assert!(unchanged.changed_owners.is_empty());
        assert_eq!(
            fixture.app.engine().unwrap().change_ids().unwrap().len(),
            change_count
        );
        fs::write(
            fixture.notes.join("note 002.md"),
            "Changed exact fact two.\n",
        )
        .unwrap();
        fs::write(
            fixture.notes.join("note 030.md"),
            "Changed exact fact thirty.\n",
        )
        .unwrap();
        let refreshed = fixture.capture();
        assert!(!refreshed.partial, "{:#?}", refreshed.items);
        assert_eq!(refreshed.totals.refreshed, 2);
        assert_eq!(refreshed.totals.unchanged, 33);
        assert_eq!(refreshed.changed_owner_ids.len(), 2);
        assert_eq!(refreshed.changed_owners.len(), 4);
        assert_eq!(
            refreshed
                .items
                .iter()
                .filter_map(|item| item.source_id.clone())
                .collect::<BTreeSet<_>>(),
            identities
        );
        let view = source_view(&fixture.app).unwrap();
        for item in first.items {
            let old = item.citation.unwrap().citation;
            let proof = view.verify(&old, CitationScope::Historical).unwrap();
            let changed = matches!(
                item.input.file_name().and_then(|s| s.to_str()),
                Some("note 002.md" | "note 030.md")
            );
            assert_eq!(
                proof.state,
                if changed {
                    crate::sources::CitationState::Historical
                } else {
                    crate::sources::CitationState::Current
                }
            );
        }
    }
}

#[test]
fn own_wiki_repeat_preserves_editor_bytes_and_accounts_for_missing_original() {
    let fixture = Fixture::new(false, 2);
    let root = fixture.app.fs.root().path();
    fs::write(root.join("Personal.md"), "A lasting personal note.\n").unwrap();
    fs::create_dir(root.join(".obsidian")).unwrap();
    fs::write(root.join(".obsidian/config.json"), "user settings").unwrap();
    let request = SourceCaptureRequest {
        paths: vec![root.to_path_buf()],
        files_from: None,
        markdown_only: false,
    };
    let first = fixture.app.capture_sources(request.clone()).unwrap();
    assert_eq!(first.totals.captured, 1);
    let second = fixture.app.capture_sources(request).unwrap();
    assert_eq!(second.totals.unchanged, 1);
    assert_eq!(second.totals.captured, 0);
    assert_eq!(
        fs::read(root.join(".obsidian/config.json")).unwrap(),
        b"user settings"
    );
    fixture.capture();
    fs::remove_file(fixture.notes.join("note 000.md")).unwrap();
    let missing = fixture.capture();
    assert_eq!(missing.totals.unchanged, 1);
    assert_eq!(missing.totals.failed, 1);
    assert!(
        missing
            .items
            .iter()
            .any(|item| item.reason.as_deref() == Some("missing_original")
                && item.source_id.is_some())
    );
}

#[test]
fn same_bytes_different_original_paths_remain_distinct_and_title_survives_refresh() {
    let fixture = Fixture::new(false, 2);
    fs::write(
        fixture.notes.join("note 001.md"),
        fs::read(fixture.notes.join("note 000.md")).unwrap(),
    )
    .unwrap();
    let first = fixture.capture();
    assert_eq!(first.totals.captured, 2);
    assert_ne!(first.items[0].source_id, first.items[1].source_id);
    let first_item = &first.items[0];
    let path = first_item.managed_path.as_ref().unwrap();
    let bytes = fixture.app.fs.read_before(path).unwrap().unwrap().bytes;
    let note = crate::records::parse_note(&bytes);
    let changed = crate::records::edit_note(
        &note,
        &BTreeMap::from([("title".into(), "My chosen source title".into())]),
        None,
        &note.source_hash,
    )
    .unwrap();
    fs::write(fixture.app.fs.root().resolve(path).unwrap(), changed).unwrap();
    fs::write(fixture.notes.join("note 000.md"), "New source text").unwrap();
    let refreshed = fixture.capture();
    assert_eq!(refreshed.items[0].title, "My chosen source title");
}

#[test]
fn preview_never_publishes_ids_citations_or_writer_state() {
    let fixture = Fixture::new(false, 3);
    let app = OfflineApp::new(
        fixture.app.fs.clone(),
        OperationOptions {
            dry_run: true,
            ..options()
        },
    )
    .unwrap();
    let lock = fixture.app.fs.root().path().join(".wiki/state/writer.lock");
    let before = fs::read(&lock).unwrap();
    let outcome = app
        .capture_sources(SourceCaptureRequest {
            paths: vec![fixture.notes.clone()],
            files_from: None,
            markdown_only: false,
        })
        .unwrap();
    assert_eq!(outcome.totals.planned, 3);
    assert!(outcome.items.iter().all(|item| item.source_id.is_none()
        && item.revision_id.is_none()
        && item.citation.is_none()));
    assert_eq!(fs::read(lock).unwrap(), before);
    assert!(
        fixture
            .app
            .engine()
            .unwrap()
            .change_ids()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ambiguous_absolute_and_relative_legacy_origins_are_never_guessed() {
    let fixture = Fixture::new(false, 1);
    let path = fixture.notes.join("note 000.md");
    let request = |origin: String| CaptureRequest {
        title: "Legacy".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin,
        original: b"legacy text".to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: None,
    };
    fixture
        .app
        .source_add(request("note 000.md".into()))
        .unwrap();
    let conflict = fixture.capture();
    assert_eq!(conflict.totals.failed, 1);
    assert_eq!(conflict.totals.captured, 0);
    fixture
        .app
        .source_add(request(
            fs::canonicalize(&path).unwrap().to_str().unwrap().into(),
        ))
        .unwrap();
    fixture
        .app
        .source_add(request(
            fs::canonicalize(&path).unwrap().to_str().unwrap().into(),
        ))
        .unwrap();
    assert_eq!(
        fixture
            .app
            .capture_sources(SourceCaptureRequest {
                paths: vec![path],
                files_from: None,
                markdown_only: false,
            })
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn concurrent_capture_of_one_new_original_allocates_one_source() {
    let fixture = Fixture::new(false, 1);
    let root = fixture.app.fs.root().path().to_path_buf();
    let notes = fixture.notes.clone();
    let barrier = Arc::new(Barrier::new(2));
    let workers = (0..2)
        .map(|_| {
            let root = root.clone();
            let notes = notes.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let app =
                    OfflineApp::new(VaultFs::new(VaultRoot::explicit(root).unwrap()), options())
                        .unwrap();
                barrier.wait();
                app.capture_sources(SourceCaptureRequest {
                    paths: vec![notes],
                    files_from: None,
                    markdown_only: false,
                })
                .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let outcomes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.totals.captured)
            .sum::<usize>(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.totals.unchanged)
            .sum::<usize>(),
        1
    );
    assert_eq!(
        outcomes[0].items[0].source_id,
        outcomes[1].items[0].source_id
    );
}

#[test]
fn returned_error_after_publication_keeps_real_committed_groups_and_replay_adds_only_remaining() {
    for normalized in [false, true] {
        let fixture = Fixture::new(normalized, 17);
        FAIL_AFTER_GROUP.with(|fault| fault.set(Some(1)));
        let partial = fixture.capture();
        assert!(partial.partial);
        assert_eq!(partial.totals.captured, 16);
        assert_eq!(partial.groups_committed, 2);
        assert_eq!(partial.totals.not_attempted, 1);
        assert_eq!(partial.changed_owner_ids.len(), 16);
        assert_eq!(partial.changed_owners.len(), 32);
        assert_eq!(
            partial
                .items
                .iter()
                .filter(|item| item.source_id.is_some())
                .count(),
            16
        );
        assert!(
            partial
                .items
                .iter()
                .filter(|item| item.action == SourceCaptureAction::Captured)
                .all(|item| item.citation.is_some())
        );
        let replay = fixture.capture();
        assert!(!replay.partial, "{:#?}", replay.items);
        assert_eq!(replay.totals.captured, 1);
        assert_eq!(replay.totals.unchanged, 16);
        assert_eq!(replay.groups_committed, 1);
        let identities = origin_lookup(&source_view(&fixture.app).unwrap()).unwrap();
        assert_eq!(identities.len(), 17);
        for committed in partial.items.iter().filter(|item| item.source_id.is_some()) {
            let current = replay
                .items
                .iter()
                .find(|item| item.input == committed.input)
                .unwrap();
            assert_eq!(current.source_id, committed.source_id);
            assert_eq!(current.revision_id, committed.revision_id);
        }
    }
}

#[test]
fn selected_source_edit_after_lookup_refuses_the_group_without_overwriting_it() {
    for normalized in [false, true] {
        let fixture = Fixture::new(normalized, 2);
        let initial = fixture.capture();
        let inputs = observe_inputs(
            std::slice::from_ref(&fixture.notes),
            None,
            fixture.app.fs.root().path(),
            false,
            CaptureInputLimits::default(),
        )
        .unwrap();
        let writer = fixture.app.writer().unwrap();
        let origins = origin_lookup(&source_view(&fixture.app).unwrap()).unwrap();
        let path = initial.items[0].managed_path.as_ref().unwrap();
        let bytes = fixture.app.fs.read_before(path).unwrap().unwrap().bytes;
        let note = crate::records::parse_note(&bytes);
        let changed = crate::records::edit_note(
            &note,
            &BTreeMap::from([("title".into(), "Concurrent author title".into())]),
            None,
            &note.source_hash,
        )
        .unwrap();
        fs::write(fixture.app.fs.root().resolve(path).unwrap(), &changed).unwrap();
        assert!(
            fixture
                .app
                .capture_group(&writer, &inputs.files, &origins, &[0, 1], true, normalized)
                .is_err()
        );
        assert_eq!(
            fixture.app.fs.read_before(path).unwrap().unwrap().bytes,
            changed
        );
        assert_eq!(fixture.app.engine().unwrap().change_ids().unwrap().len(), 1);
    }
}

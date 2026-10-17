//! Existing-folder onboarding uses disposable author files and no providers.
use super::{
    OfflineApp, OperationOptions,
    offline::{init, init_with_existing},
};
use crate::{
    domain::ErrorCode,
    vault::{VaultFs, VaultRoot},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

fn options(dry_run: bool) -> OperationOptions {
    OperationOptions {
        dry_run,
        offline: true,
        lock_timeout_ms: 100,
        ..Default::default()
    }
}

// Include directory times and symlink identities, so a rejected/preview command
// cannot quietly create state, change a lock, or replace a user's directory.
fn tree(root: &Path) -> BTreeMap<PathBuf, (String, Vec<u8>, SystemTime)> {
    fn walk(
        root: &Path,
        current: &Path,
        out: &mut BTreeMap<PathBuf, (String, Vec<u8>, SystemTime)>,
    ) {
        let metadata = fs::symlink_metadata(current).unwrap();
        let (kind, bytes) = if metadata.file_type().is_symlink() {
            (
                "symlink",
                fs::read_link(current)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
            )
        } else if metadata.is_dir() {
            ("directory", vec![])
        } else {
            ("file", fs::read(current).unwrap())
        };
        out.insert(
            current.strip_prefix(root).unwrap().to_owned(),
            (kind.into(), bytes, metadata.modified().unwrap()),
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(current).unwrap() {
                walk(root, &entry.unwrap().path(), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn author_folder(root: &Path) {
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::create_dir(root.join(".obsidian")).unwrap();
    fs::write(
        root.join("notes/café 東京.md"),
        b"\xef\xbb\xbf---\r\ncustom_property: untouched\r\n---\r\nAn exact author fact.\r\n",
    )
    .unwrap();
    fs::write(root.join(".obsidian/app.json"), b"{\"editorOwned\":true}\n").unwrap();
}

#[test]
fn new_directory_preview_and_landing_are_useful_and_sync_preserves_authorship() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("New café wiki 東京");
    let before = tree(temp.path());
    let preview = init(&root, "A useful wiki", options(true)).unwrap();
    assert!(!preview.created);
    assert!(preview.planned_directories.iter().any(|p| p == "pages"));
    assert_eq!(before, tree(temp.path()));
    let created = init(&root, "A useful wiki", options(false)).unwrap();
    assert!(created.created);
    let marker = fs::read_to_string(root.join("WIKI.md")).unwrap();
    assert!(marker.contains("[Pages](pages/)"));
    assert!(marker.contains("[Sources](sources/)"));
    assert!(marker.contains("hidden `.wiki/`"));
    assert!(marker.contains("refresh its saved copy"));
    assert_eq!(
        fs::read(root.join(".wiki/.gitignore")).unwrap(),
        b"cache/\n"
    );
    let authored = format!("{marker}\nAn owner-authored landing paragraph.\n");
    fs::write(root.join("WIKI.md"), &authored).unwrap();
    let app = OfflineApp::new(
        VaultFs::new(VaultRoot::explicit(&root).unwrap()),
        options(false),
    )
    .unwrap();
    app.index_sync(false).unwrap();
    assert_eq!(fs::read(root.join("WIKI.md")).unwrap(), authored.as_bytes());
    app.index_rebuild_normalized().unwrap();
    app.index_sync(false).unwrap();
    assert_eq!(fs::read(root.join("WIKI.md")).unwrap(), authored.as_bytes());
}

#[test]
fn existing_author_folder_requires_intent_and_preserves_editor_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Existing editor wiki");
    author_folder(&root);
    let before = tree(temp.path());
    for dry_run in [true, false] {
        let error = init(&root, "Author wiki", options(dry_run)).unwrap_err();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert!(error.message.contains("--existing"));
        assert_eq!(before, tree(temp.path()));
    }
    assert!(
        !init_with_existing(&root, "Author wiki", true, options(true))
            .unwrap()
            .created
    );
    assert_eq!(before, tree(temp.path()));
    assert!(
        init_with_existing(&root, "Author wiki", true, options(false))
            .unwrap()
            .created
    );
    for (relative, original) in before {
        if relative.starts_with("Existing editor wiki/notes")
            || relative.starts_with("Existing editor wiki/.obsidian")
        {
            assert_eq!(tree(temp.path()).get(&relative), Some(&original));
        }
    }
    let app = OfflineApp::new(
        VaultFs::new(VaultRoot::explicit(&root).unwrap()),
        options(false),
    )
    .unwrap();
    app.index_sync(false).unwrap();
    assert_eq!(
        fs::read(root.join("notes/café 東京.md")).unwrap(),
        b"\xef\xbb\xbf---\r\ncustom_property: untouched\r\n---\r\nAn exact author fact.\r\n"
    );
    assert_eq!(
        fs::read(root.join(".obsidian/app.json")).unwrap(),
        b"{\"editorOwned\":true}\n"
    );
}

#[test]
fn every_reserved_file_and_empty_directory_collision_refuses_without_writes() {
    for name in [
        "WIKI.md",
        "pages",
        "knowledge",
        "sources",
        "changes",
        ".wiki",
    ] {
        for directory in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("Author folder");
            author_folder(&root);
            let occupied = root.join(name);
            if directory {
                fs::create_dir(&occupied).unwrap();
            } else {
                fs::write(&occupied, b"author-owned occupied bytes\n").unwrap();
            }
            let before = tree(temp.path());
            for dry_run in [true, false] {
                let error =
                    init_with_existing(&root, "Collision", true, options(dry_run)).unwrap_err();
                assert_eq!(error.code, ErrorCode::ContentConflict);
                assert!(error.message.contains(occupied.to_str().unwrap()));
                assert_eq!(error.details["reason"], "managed_path_collision");
                assert_eq!(
                    before,
                    tree(temp.path()),
                    "collision {name}, directory={directory}, dry_run={dry_run}"
                );
            }
        }
    }
}

#[test]
fn portable_reserved_aliases_refuse_before_hidden_state_is_created() {
    for name in [
        "WiKi.Md",
        "PAGES",
        "Knowledge",
        "SOURCES",
        "Changes",
        ".WIKI",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Author folder");
        author_folder(&root);
        fs::create_dir(root.join(name)).unwrap();
        let before = tree(temp.path());
        for dry_run in [true, false] {
            assert!(init_with_existing(&root, "Collision", true, options(dry_run)).is_err());
            assert_eq!(before, tree(temp.path()));
        }
    }
}

#[test]
fn retained_state_is_never_reused_or_gitignore_appended_during_initialization() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Interrupted or old wiki");
    author_folder(&root);
    fs::create_dir_all(root.join(".wiki/state")).unwrap();
    fs::write(root.join(".wiki/.gitignore"), b"# author ignore\nkeep").unwrap();
    fs::write(
        root.join(".wiki/state/writer.lock"),
        b"previous diagnostic\n",
    )
    .unwrap();
    let before = tree(temp.path());
    for dry_run in [true, false] {
        assert_eq!(
            init_with_existing(&root, "No adoption", true, options(dry_run))
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        assert_eq!(before, tree(temp.path()));
    }
}

#[test]
fn missing_parent_and_invalid_titles_leave_the_filesystem_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let before = tree(temp.path());
    for dry_run in [true, false] {
        assert!(
            init(
                &temp.path().join("missing-parent/new-wiki"),
                "New",
                options(dry_run)
            )
            .is_err()
        );
        assert!(init(&temp.path().join("new-wiki"), " \n\t", options(dry_run)).is_err());
        assert_eq!(before, tree(temp.path()));
    }
}

#[cfg(unix)]
#[test]
fn root_and_dangling_managed_symlinks_refuse_without_touching_targets() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Author folder");
    author_folder(&root);
    let alias = temp.path().join("Root alias");
    symlink(&root, &alias).unwrap();
    let dangling = temp.path().join("missing external target");
    symlink(&dangling, root.join("sources")).unwrap();
    let before = tree(temp.path());
    for dry_run in [true, false] {
        assert!(init_with_existing(&alias, "Alias", true, options(dry_run)).is_err());
        assert_eq!(
            init_with_existing(&root, "Managed alias", true, options(dry_run))
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        assert_eq!(before, tree(temp.path()));
    }
}

type InitCheckpoint = Box<dyn Fn(&str, &Path) -> crate::domain::Result<()>>;

fn cut(point: &'static str) -> InitCheckpoint {
    Box::new(move |name, _| {
        if name == point {
            Err(crate::domain::WikiError::new(
                ErrorCode::Internal,
                format!("injected initialization cut at {point}"),
            ))
        } else {
            Ok(())
        }
    })
}

fn assert_capture_and_context(root: &Path) {
    use crate::{
        domain::{Blake3Hash, CitationRef},
        retrieval::{ContextRequest, ContextScope, verification},
        sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    };
    let app = OfflineApp::new(
        VaultFs::new(VaultRoot::explicit(root).unwrap()),
        options(false),
    )
    .unwrap();
    let original = b"Bootstrapneedle: the author keeps the cobalt notebook.\n";
    let captured = app
        .source_add(CaptureRequest {
            title: "An author's own note".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "own-note.md".into(),
            original: original.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    app.index_rebuild_normalized().unwrap();
    let context = verification::context(
        &app.catalog(),
        None,
        "Bootstrapneedle",
        &ContextRequest {
            scope: ContextScope::IndexedEvidence,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!context.network_used);
    let passage = context.passages.iter().find(|passage| passage.citations.iter().any(|citation|
        matches!(citation, CitationRef::Source(reference) if reference.source_id == captured.allocated_ids["source"])
    )).expect("captured author fact must be returned with a source citation");
    for citation in &passage.citations {
        if let CitationRef::Source(reference) = citation {
            let quote = &original[reference.span.start() as usize..reference.span.end() as usize];
            assert_eq!(quote, passage.text.as_bytes());
            assert_eq!(Blake3Hash::digest(quote), reference.quote_hash);
        }
    }
}

#[test]
fn every_new_root_cut_preserves_staging_and_allows_identical_retry() {
    for point in [
        "root_created",
        "marker_staged",
        "marker_published",
        "marker_synced",
        "managed_directory",
        "layout_complete",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("New wiki");
        let error = super::init_recovery::with_checkpoint(cut(point), || {
            init(&root, "New wiki", options(false))
        })
        .unwrap_err();
        let report = &error.details["initialization"];
        assert_eq!(report["applied"], true, "{point}");
        assert_eq!(report["published"], false, "{point}");
        assert_eq!(report["durable"], false, "{point}");
        assert!(!root.exists(), "{point}");
        let retained = report["retained_paths"].as_array().unwrap();
        assert!(!retained.is_empty());
        let staged_root = PathBuf::from(retained[0].as_str().unwrap());
        let staged_before = tree(&staged_root);
        init(&root, "New wiki", options(false)).unwrap();
        assert_eq!(
            staged_before,
            tree(&staged_root),
            "retry must preserve interrupted staging"
        );
        assert!(root.join("WIKI.md").is_file());
    }
}

#[test]
fn marker_publication_cuts_leave_existing_author_folder_usable_with_exact_citations() {
    for point in [
        "marker_published",
        "marker_synced",
        "managed_directory",
        "layout_complete",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Author wiki");
        author_folder(&root);
        let notes = tree(&root.join("notes"));
        let editor = tree(&root.join(".obsidian"));
        let error = super::init_recovery::with_checkpoint(cut(point), || {
            init_with_existing(&root, "Author wiki", true, options(false))
        })
        .unwrap_err();
        assert_eq!(
            error.details["initialization"]["published"], true,
            "{point}"
        );
        assert_eq!(error.details["initialization"]["applied"], true, "{point}");
        assert_eq!(
            error.details["initialization"]["durable"],
            point != "marker_published",
            "{point}"
        );
        let marker = fs::read(root.join("WIKI.md")).unwrap();
        // This is the existing-marker path taken by an ordinary setup retry.
        assert_capture_and_context(&root);
        assert_eq!(marker, fs::read(root.join("WIKI.md")).unwrap());
        assert_eq!(notes, tree(&root.join("notes")));
        assert_eq!(editor, tree(&root.join(".obsidian")));
    }
}

#[test]
fn existing_folder_before_marker_cut_resumes_owned_bootstrap_and_preserves_staging() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Author wiki");
    author_folder(&root);
    let error = super::init_recovery::with_checkpoint(cut("marker_staged"), || {
        init_with_existing(&root, "Author wiki", true, options(false))
    })
    .unwrap_err();
    assert_eq!(error.details["initialization"]["applied"], true);
    assert_eq!(error.details["initialization"]["published"], false);
    let stage = error.details["initialization"]["retained_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| PathBuf::from(value.as_str().unwrap()))
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".lwiki-marker-")
        })
        .unwrap();
    let bytes = fs::read(&stage).unwrap();
    let authority = tree(&root.join(".wiki"));
    assert!(!root.join("WIKI.md").exists());
    assert!(root.join(".wiki/state/storage/layout.json").is_file());
    assert!(!root.join(".wiki/state/writer.lock").exists());
    init_with_existing(&root, "Author wiki", true, options(false)).unwrap();
    assert_eq!(bytes, fs::read(stage).unwrap());
    assert_eq!(bytes, fs::read(root.join("WIKI.md")).unwrap());
    let current = tree(&root.join(".wiki"));
    for (path, (kind, bytes, modified)) in authority {
        if kind == "file" {
            assert_eq!(current.get(&path), Some(&(kind, bytes, modified)));
        }
    }
}

#[test]
fn destination_and_marker_publication_never_replace_concurrent_author_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("New wiki");
    let raced = root.clone();
    let error = super::init_recovery::with_checkpoint(
        Box::new(move |point, _| {
            if point == "layout_complete" {
                fs::create_dir(&raced).unwrap();
            }
            Ok(())
        }),
        || init(&root, "New wiki", options(false)),
    )
    .unwrap_err();
    assert_eq!(error.details["initialization"]["published"], false);
    assert!(root.is_dir());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    let author = temp.path().join("Author wiki");
    author_folder(&author);
    let raced = author.clone();
    let authority = std::rc::Rc::new(std::cell::RefCell::new(None));
    let observed_authority = authority.clone();
    let error = super::init_recovery::with_checkpoint(
        Box::new(move |point, _| {
            if point == "marker_staged" {
                *observed_authority.borrow_mut() = Some(tree(&raced.join(".wiki")));
                fs::write(raced.join("WIKI.md"), b"author-owned marker").unwrap();
            }
            Ok(())
        }),
        || init_with_existing(&author, "Author wiki", true, options(false)),
    )
    .unwrap_err();
    assert_eq!(error.details["initialization"]["published"], false);
    assert_eq!(
        fs::read(author.join("WIKI.md")).unwrap(),
        b"author-owned marker"
    );
    assert_eq!(
        authority.borrow().as_ref().unwrap(),
        &tree(&author.join(".wiki"))
    );
    assert!(!author.join(".wiki/state/writer.lock").exists());
}

#[test]
fn stage_substitution_and_managed_directory_races_preserve_unknown_bytes() {
    for replace_stage in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Author wiki");
        author_folder(&root);
        let error = super::init_recovery::with_checkpoint(
            Box::new(move |point, working| {
                if replace_stage && point == "marker_staged" {
                    let path = fs::read_dir(working)
                        .unwrap()
                        .map(|e| e.unwrap().path())
                        .find(|p| {
                            p.file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with(".lwiki-marker-")
                        })
                        .unwrap();
                    fs::remove_file(&path).unwrap();
                    fs::write(path, b"unknown replacement").unwrap();
                } else if !replace_stage && point == "marker_synced" {
                    fs::create_dir(working.join("knowledge")).unwrap();
                    fs::write(working.join("knowledge/author.txt"), b"unknown state").unwrap();
                }
                Ok(())
            }),
            || init_with_existing(&root, "Author wiki", true, options(false)),
        )
        .unwrap_err();
        assert_eq!(error.details["initialization"]["published"], !replace_stage);
        if replace_stage {
            let stage = fs::read_dir(&root)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".lwiki-marker-")
                })
                .unwrap();
            assert_eq!(fs::read(stage).unwrap(), b"unknown replacement");
        } else {
            assert_eq!(
                fs::read(root.join("knowledge/author.txt")).unwrap(),
                b"unknown state"
            );
            assert_eq!(fs::read_dir(root.join("knowledge")).unwrap().count(), 1);
            assert!(root.join(".wiki/state/storage/layout.json").is_file());
            assert!(!root.join("pages").exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn real_permission_failure_after_marker_is_reported_and_capture_recovers() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Author wiki");
    author_folder(&root);
    let error = super::init_recovery::with_checkpoint(
        Box::new(|point, working| {
            if point == "marker_synced" {
                fs::set_permissions(working, fs::Permissions::from_mode(0o500)).unwrap();
            }
            Ok(())
        }),
        || init_with_existing(&root, "Author wiki", true, options(false)),
    )
    .unwrap_err();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(error.details["initialization"]["applied"], true);
    assert_eq!(error.details["initialization"]["published"], true);
    assert!(root.join(".wiki/state/storage/layout.json").is_file());
    assert!(!root.join(".wiki/cache").exists());
    assert!(!root.join("pages").exists());
    assert_capture_and_context(&root);
}

#[test]
fn new_root_publication_sync_cut_reports_valid_retained_wiki() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("New wiki");
    let error = super::init_recovery::with_checkpoint(cut("root_published"), || {
        init(&root, "New wiki", options(false))
    })
    .unwrap_err();
    assert_eq!(error.details["initialization"]["published"], true);
    assert_eq!(error.details["initialization"]["durable"], false);
    assert_capture_and_context(&root);
}

#[cfg(unix)]
#[test]
fn permission_refusal_before_first_write_does_not_claim_partial_initialization() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Author wiki");
    author_folder(&root);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
    let before = tree(&root);
    let error = init_with_existing(&root, "Author wiki", true, options(false)).unwrap_err();
    assert_eq!(error.details["initialization"]["applied"], false);
    assert_eq!(error.details["initialization"]["published"], false);
    assert_eq!(before, tree(&root));
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    init_with_existing(&root, "Author wiki", true, options(false)).unwrap();
}

#[cfg(unix)]
#[test]
fn initialization_durability_retry_preserves_legacy_and_current_hardlinked_author_wikis() {
    for bootstrap in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Author wiki");
        author_folder(&root);
        if bootstrap {
            super::init_recovery::with_checkpoint(cut("marker_published"), || {
                init_with_existing(&root, "Author wiki", true, options(false))
            })
            .unwrap_err();
        } else {
            fs::write(
                root.join("WIKI.md"),
                include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
            )
            .unwrap();
        }
        // A legitimate hardlink does not give us grounds to reject the selected wiki.
        fs::hard_link(root.join("WIKI.md"), temp.path().join("marker-backup.md")).unwrap();
        let before = tree(temp.path());
        super::init_recovery::verify_init_durability(&root).unwrap();
        super::init_recovery::verify_init_durability(&root).unwrap();
        assert_eq!(before, tree(temp.path()));
        assert_eq!(root.join(".wiki").exists(), bootstrap);
    }
}

#[cfg(unix)]
#[test]
fn initialization_durability_rejects_marker_replacement_or_edit_during_sync() {
    for replace in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Author wiki");
        init(&root, "Author wiki", options(false)).unwrap();
        let replacement = fs::read(root.join("WIKI.md")).unwrap();
        let mut edited = replacement.clone();
        edited.extend_from_slice(b"\nAn author's concurrent edit.\n");
        let expected = if replace {
            replacement.clone()
        } else {
            edited.clone()
        };
        let error = super::init_recovery::with_checkpoint(
            Box::new(move |point, working| {
                if point == "durability_synced" {
                    let marker = working.join("WIKI.md");
                    if replace {
                        fs::remove_file(&marker).unwrap();
                        fs::write(marker, &replacement).unwrap();
                    } else {
                        fs::write(marker, &edited).unwrap();
                    }
                }
                Ok(())
            }),
            || super::init_recovery::verify_init_durability(&root),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert_eq!(fs::read(root.join("WIKI.md")).unwrap(), expected);
    }
}

#[cfg(unix)]
#[test]
fn initialization_durability_sync_failure_is_retryable_without_layout_writes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Author wiki");
    init(&root, "Author wiki", options(false)).unwrap();
    let before = tree(temp.path());
    let error = super::init_recovery::with_checkpoint(cut("durability_synced"), || {
        super::init_recovery::verify_init_durability(&root)
    })
    .unwrap_err();
    assert!(error.message.contains("injected initialization cut"));
    assert_eq!(before, tree(temp.path()));
    super::init_recovery::verify_init_durability(&root).unwrap();
    assert_eq!(before, tree(temp.path()));
}

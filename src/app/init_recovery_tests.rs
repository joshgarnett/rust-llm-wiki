use super::*;
use crate::app::{
    OfflineApp, OperationOptions,
    offline::{init, init_bootstrap_identity, init_with_title_intent},
};

fn options() -> OperationOptions {
    OperationOptions {
        offline: true,
        ..Default::default()
    }
}
fn interrupted(title: &str) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("Author.md"),
        b"Keep these author bytes.\r\n",
    )
    .unwrap();
    fs::create_dir(temp.path().join(".obsidian")).unwrap();
    fs::write(
        temp.path().join(".obsidian/app.json"),
        b"{\"vimMode\":true}\n",
    )
    .unwrap();
    let error = with_checkpoint(
        Box::new(|name, _| {
            if name == "bootstrap_published" {
                Err(WikiError::new(ErrorCode::Internal, "injected interruption"))
            } else {
                Ok(())
            }
        }),
        || init_with_title_intent(temp.path(), Some(title), "unused", true, options()),
    )
    .unwrap_err();
    assert!(
        !error.details["initialization"]["published"]
            .as_bool()
            .unwrap()
    );
    assert!(!temp.path().join("WIKI.md").exists());
    assert!(!temp.path().join(".wiki/state/writer.lock").exists());
    temp
}

#[test]
fn existing_folder_omitted_title_retries_exact_identity_without_touching_author_files() {
    let temp = interrupted("My \"stored\" wiki");
    let identity = init_bootstrap_identity(temp.path(), None).unwrap().unwrap();
    let preview = init_with_title_intent(
        temp.path(),
        None,
        "Different CLI default",
        true,
        OperationOptions {
            dry_run: true,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(preview.id, identity.0);
    assert!(!preview.created);
    assert!(!temp.path().join("WIKI.md").exists());
    assert!(!temp.path().join(".wiki/state/writer.lock").exists());
    let outcome =
        init_with_title_intent(temp.path(), None, "Different CLI default", true, options())
            .unwrap();
    assert_eq!(outcome.id, identity.0);
    let root = VaultRoot::explicit(temp.path()).unwrap();
    assert_eq!(
        crate::storage::layout::layout_kind(&root).unwrap(),
        crate::storage::layout::LayoutKind::ReadableSources
    );
    let marker = crate::records::parse_note(&fs::read(temp.path().join("WIKI.md")).unwrap());
    assert_eq!(marker.canonical.unwrap().title(), identity.1);
    assert_eq!(
        fs::read(temp.path().join("Author.md")).unwrap(),
        b"Keep these author bytes.\r\n"
    );
    assert_eq!(
        fs::read(temp.path().join(".obsidian/app.json")).unwrap(),
        b"{\"vimMode\":true}\n"
    );
    verify_init_durability(temp.path()).unwrap();
}

#[test]
fn explicit_changed_title_and_extra_bootstrap_entries_refuse_without_marker() {
    let temp = interrupted("First title");
    assert!(
        init_with_title_intent(temp.path(), Some("Second title"), "unused", true, options())
            .is_err()
    );
    assert!(!temp.path().join("WIKI.md").exists());
    fs::write(temp.path().join(".wiki/private-note.txt"), b"User data.\n").unwrap();
    assert!(init_with_title_intent(temp.path(), None, "unused", true, options()).is_err());
    assert_eq!(
        fs::read(temp.path().join(".wiki/private-note.txt")).unwrap(),
        b"User data.\n"
    );
    assert!(!temp.path().join("WIKI.md").exists());
}

#[test]
fn resumed_bootstrap_can_be_interrupted_again_without_changing_identity() {
    let temp = interrupted("Same identity");
    let identity = init_bootstrap_identity(temp.path(), None).unwrap().unwrap();
    with_checkpoint(
        Box::new(|name, _| {
            if name == "marker_staged" {
                Err(WikiError::new(ErrorCode::Internal, "second interruption"))
            } else {
                Ok(())
            }
        }),
        || init_with_title_intent(temp.path(), None, "other default", true, options()),
    )
    .unwrap_err();
    assert_eq!(
        init_bootstrap_identity(temp.path(), None).unwrap().unwrap(),
        identity
    );
    let outcome = init_with_title_intent(
        temp.path(),
        Some("Same identity"),
        "unused",
        true,
        options(),
    )
    .unwrap();
    assert_eq!(outcome.id, identity.0);
}

#[test]
fn concurrent_author_marker_wins_and_changed_bootstrap_refuses_publication() {
    let temp = interrupted("Concurrent");
    let error = with_checkpoint(
        Box::new(|name, root| {
            if name == "marker_staged" {
                fs::write(root.join("WIKI.md"), b"An author created this file.\n").unwrap();
            }
            Ok(())
        }),
        || init_with_title_intent(temp.path(), None, "unused", true, options()),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert_eq!(
        fs::read(temp.path().join("WIKI.md")).unwrap(),
        b"An author created this file.\n"
    );
    let temp = interrupted("Changed authority");
    with_checkpoint(
        Box::new(|name, root| {
            if name == "marker_staged" {
                fs::write(root.join(".wiki/.gitignore"), b"Edited by author.\n").unwrap();
            }
            Ok(())
        }),
        || init_with_title_intent(temp.path(), None, "unused", true, options()),
    )
    .unwrap_err();
    assert!(!temp.path().join("WIKI.md").exists());
    assert_eq!(
        fs::read(temp.path().join(".wiki/.gitignore")).unwrap(),
        b"Edited by author.\n"
    );
}

#[test]
fn new_layout_capture_is_hidden_and_saved_display_is_not_a_canonical_document() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("wiki");
    init(&path, "New readable wiki", options()).unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(&path).unwrap());
    let app = OfflineApp::new(vault.clone(), options()).unwrap();
    let added = app
        .source_add(crate::sources::CaptureRequest {
            title: "Local instructions".into(),
            origin_kind: crate::sources::SourceOrigin::LocalFile,
            origin: "Author.md".into(),
            original: b"# Instructions\nBring the blue key.\n".to_vec(),
            extraction: crate::sources::ExtractionInput::Utf8Preserve,
            media_type: Some("text/markdown".into()),
        })
        .unwrap();
    let id = &added.allocated_ids["source"];
    let logical = VaultRelativePath::new(format!("sources/{id}/source.md")).unwrap();
    assert!(!path.join(logical.as_str()).exists());
    let source = crate::records::parse_note(&vault.read_before(&logical).unwrap().unwrap().bytes)
        .canonical
        .unwrap();
    let display = crate::domain::source_presentation::SourcePresentation::from_record(&source)
        .unwrap()
        .unwrap();
    assert!(path.join(display.current.path.as_str()).is_file());
    let paths = vault.root().scan_markdown().unwrap();
    assert!(paths.contains(&logical));
    assert!(!paths.contains(&display.current.path));
    app.index_rebuild_normalized().unwrap();
}

#[test]
fn direct_init_retry_after_marker_uses_existing_discovery_without_overwriting_author_edits() {
    let temp = tempfile::tempdir().unwrap();
    let result = with_checkpoint(
        Box::new(|name, _| {
            if name == "marker_published" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "marker sync interruption",
                ))
            } else {
                Ok(())
            }
        }),
        || init_with_title_intent(temp.path(), Some("Stored title"), "unused", true, options()),
    );
    assert!(result.is_err());
    let marker = fs::read(temp.path().join("WIKI.md")).unwrap();
    let id = crate::records::parse_note(&marker)
        .canonical
        .unwrap()
        .id()
        .clone();
    let result = init_with_title_intent(temp.path(), None, "New default", true, options()).unwrap();
    assert_eq!(result.id, id);
    assert!(!result.created);
    assert_eq!(fs::read(temp.path().join("WIKI.md")).unwrap(), marker);
    let mut edited = marker.clone();
    edited.extend_from_slice(b"Author additions after initialization.\n");
    fs::write(temp.path().join("WIKI.md"), &edited).unwrap();
    assert!(init_with_title_intent(temp.path(), None, "unused", true, options()).is_err());
    assert_eq!(fs::read(temp.path().join("WIKI.md")).unwrap(), edited);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native {
    use super::*;
    use clap::Parser;
    use std::{
        collections::BTreeMap,
        path::PathBuf,
        process::{Child, Command, ExitStatus, Stdio},
        time::Instant,
    };

    const CHILD_TEST: &str = "app::init_recovery::tests::native::bootstrap_native_child";
    const TITLE: &str = "My \"persisted\" notes α";
    const WAIT: Duration = Duration::from_secs(15);

    // Control files and preferences are siblings of the disposable author folder.
    // No checkpoint signal, result or subprocess log enters the wiki inventory.
    struct OwnedChild(Child);

    impl OwnedChild {
        fn spawn(wiki: &Path, control: &Path, mode: &str) -> Self {
            Self(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        CHILD_TEST,
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("LWIKI_BOOTSTRAP_NATIVE_VAULT", wiki)
                    .env("LWIKI_BOOTSTRAP_NATIVE_CONTROL", control)
                    .env("LWIKI_BOOTSTRAP_NATIVE_MODE", mode)
                    .current_dir(control)
                    .stdin(Stdio::null())
                    .stdout(fs::File::create(control.join(format!("{mode}.stdout"))).unwrap())
                    .stderr(fs::File::create(control.join(format!("{mode}.stderr"))).unwrap())
                    .spawn()
                    .unwrap(),
            )
        }

        fn ready(&mut self, control: &Path, point: &str) {
            let started = Instant::now();
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    panic!(
                        "{point}: child exited before rendezvous: {status}; logs: {}",
                        child_logs(control, point)
                    );
                }
                if control.join("ready").is_file() {
                    let observed: serde_json::Value =
                        serde_json::from_slice(&fs::read(control.join("checkpoint.json")).unwrap())
                            .unwrap();
                    assert_eq!(observed["point"], point);
                    assert_eq!(observed["pid"], self.0.id());
                    break;
                }
                assert!(
                    started.elapsed() < WAIT,
                    "{point}: rendezvous timed out; {}",
                    child_logs(control, point)
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn finish(&mut self, control: &Path, mode: &str) -> ExitStatus {
            let started = Instant::now();
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    return status;
                }
                assert!(
                    started.elapsed() < WAIT,
                    "{mode}: child exit timed out; {}",
                    child_logs(control, mode)
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn child_logs(control: &Path, mode: &str) -> String {
        format!(
            "stdout: {}; stderr: {}",
            fs::read_to_string(control.join(format!("{mode}.stdout"))).unwrap_or_default(),
            fs::read_to_string(control.join(format!("{mode}.stderr"))).unwrap_or_default()
        )
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            // Kill and reap only the child this guard owns, including assertion
            // failures and deadlines. A previously reaped child is harmless.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    type Tree = BTreeMap<PathBuf, Option<Vec<u8>>>;

    fn tree(root: &Path) -> Tree {
        fn visit(root: &Path, directory: &Path, entries: &mut Tree) {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                assert!(
                    !kind.is_symlink(),
                    "unexpected fixture symlink: {}",
                    path.display()
                );
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if kind.is_dir() {
                    entries.insert(relative, None);
                    visit(root, &path, entries);
                } else {
                    assert!(kind.is_file());
                    entries.insert(relative, Some(fs::read(&path).unwrap()));
                }
            }
        }
        let mut entries = Tree::new();
        visit(root, root, &mut entries);
        entries
    }

    fn manifest(entries: &Tree) -> BTreeMap<String, Option<String>> {
        entries
            .iter()
            .map(|(path, bytes)| {
                (
                    path.to_str().unwrap().to_owned(),
                    bytes
                        .as_ref()
                        .map(|bytes| crate::domain::Blake3Hash::digest(bytes).to_string()),
                )
            })
            .collect()
    }

    fn assert_author_unchanged(wiki: &Path, before: &Tree) {
        let after = tree(wiki);
        for (path, bytes) in before {
            assert_eq!(
                after.get(path),
                Some(bytes),
                "author entry: {}",
                path.display()
            );
        }
        // Entire authored subtrees must remain exact, including their names.
        for directory in ["notes", "attachments", ".obsidian"] {
            let selected = |entries: &Tree| {
                entries
                    .iter()
                    .filter(|(path, _)| path.starts_with(directory))
                    .map(|(path, bytes)| (path.clone(), bytes.clone()))
                    .collect::<Tree>()
            };
            assert_eq!(
                selected(&after),
                selected(before),
                "author subtree: {directory}"
            );
        }
    }

    #[test]
    #[ignore = "invoked explicitly by the native bootstrap process-death matrix"]
    fn bootstrap_native_child() {
        let wiki = PathBuf::from(std::env::var_os("LWIKI_BOOTSTRAP_NATIVE_VAULT").unwrap());
        let control = PathBuf::from(std::env::var_os("LWIKI_BOOTSTRAP_NATIVE_CONTROL").unwrap());
        let mode = std::env::var("LWIKI_BOOTSTRAP_NATIVE_MODE").unwrap();
        assert!(wiki.is_absolute() && control.is_absolute());
        assert_eq!(wiki.parent(), control.parent());
        assert_ne!(wiki, control);
        let preferences = control.join("settings/config.toml");
        let mut argv = vec![
            "lwiki".into(),
            "--no-user-config".into(),
            "--preferences".into(),
            preferences.into_os_string(),
            "--offline".into(),
            "--json".into(),
            "setup".into(),
            wiki.clone().into_os_string(),
            "--existing".into(),
            "--non-interactive".into(),
        ];
        if mode != "retry" {
            assert!(
                [
                    "bootstrap_staged",
                    "bootstrap_installed",
                    "bootstrap_published",
                    "marker_published",
                    "marker_synced",
                ]
                .contains(&mode.as_str())
            );
            argv.extend(["--title".into(), TITLE.into()]);
        }
        let args = crate::cli::Arguments::try_parse_from(argv).unwrap();
        let execute = || crate::cli::execute(&args);
        let (report, exit) = if mode == "retry" {
            // This fresh process has no checkpoint hook and no title argument.
            execute()
        } else {
            let point = mode.clone();
            let rendezvous = control.clone();
            with_checkpoint(
                Box::new(move |name, _| {
                    if name == point {
                        fs::write(
                            rendezvous.join("checkpoint.json"),
                            serde_json::to_vec(
                                &serde_json::json!({"point":name,"pid":std::process::id()}),
                            )
                            .unwrap(),
                        )
                        .unwrap();
                        fs::write(rendezvous.join("ready"), b"ready\n").unwrap();
                        loop {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                    Ok(())
                }),
                execute,
            )
        };
        fs::write(
            control.join("result.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        assert_eq!(exit, 0, "{report:?}");
        assert!(report.ok, "{report:?}");
        assert_eq!(report.data["completion"], "complete");
        assert!(!report.meta.network_used);
        assert_eq!(mode, "retry", "initial child did not reach its checkpoint");
    }

    #[test]
    fn native_bootstrap_kills_retry_through_setup_with_omitted_title() {
        use std::os::unix::process::ExitStatusExt;

        for point in [
            "bootstrap_staged",
            "bootstrap_installed",
            "bootstrap_published",
            "marker_published",
            "marker_synced",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let base = fs::canonicalize(temp.path()).unwrap();
            let wiki = base.join("Author notes α");
            let control = base.join("control");
            fs::create_dir(&wiki).unwrap();
            fs::create_dir(&control).unwrap();
            for directory in ["notes", "attachments", ".obsidian"] {
                fs::create_dir(wiki.join(directory)).unwrap();
            }
            fs::write(
                wiki.join("Author.md"),
                b"# Author\r\nKeep my exact wording.\r\n",
            )
            .unwrap();
            fs::write(wiki.join("notes/Second.md"), b"[[Author]]\nMy own link.\n").unwrap();
            fs::write(wiki.join("attachments/scan.bin"), [0, 255, 1, 13, 10]).unwrap();
            fs::write(wiki.join(".obsidian/app.json"), b"{\"vimMode\":true}\n").unwrap();
            let author = tree(&wiki);
            let mut interrupted = OwnedChild::spawn(&wiki, &control, point);
            let killed_pid = interrupted.0.id();
            interrupted.ready(&control, point);
            interrupted.0.kill().unwrap();
            let killed = interrupted.finish(&control, point);
            assert_eq!(killed.signal(), Some(9), "{point}: {killed}");
            drop(interrupted);
            assert_author_unchanged(&wiki, &author);
            assert!(!wiki.join(".wiki/state/writer.lock").exists());
            assert!(!wiki.join("knowledge").exists());
            assert!(!control.join("settings/config.toml").exists());

            let marker_published = matches!(point, "marker_published" | "marker_synced");
            assert_eq!(wiki.join("WIKI.md").exists(), marker_published);
            let before_retry = tree(&wiki);
            let mut abandoned = None;
            let mut authority = None;
            let expected_marker = if point == "bootstrap_staged" {
                assert!(!wiki.join(".wiki").exists());
                let stages: Vec<_> = fs::read_dir(&wiki)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| {
                        path.file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .starts_with(".lwiki-state-")
                    })
                    .collect();
                assert_eq!(stages.len(), 1);
                let stage = stages.into_iter().next().unwrap();
                let layout: crate::storage::layout::Layout = crate::storage::layout::decode(
                    &fs::read(stage.join("state/storage/layout.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(layout.version, 3);
                let receipt = fs::read(stage.join(format!(
                    "state/storage/receipts/{}.plan.json",
                    layout.migration_id
                )))
                .unwrap();
                assert_eq!(
                    layout.migration_hash,
                    crate::domain::Blake3Hash::digest(&receipt)
                );
                let epoch: serde_json::Value = crate::storage::layout::decode(&receipt).unwrap();
                assert_eq!(epoch["id"], layout.migration_id.as_str());
                assert_eq!(epoch["vault_id"], layout.vault_id.as_str());
                assert_eq!(epoch["bootstrap"], true);
                abandoned = Some((stage.clone(), tree(&stage), layout.vault_id));
                None
            } else {
                authority = Some(tree(&wiki.join(".wiki")));
                Some(if marker_published {
                    fs::read(wiki.join("WIKI.md")).unwrap()
                } else {
                    crate::storage::source_layout::resume_bootstrap_marker(
                        &VaultRoot::for_initialization(&wiki).unwrap(),
                        None,
                    )
                    .unwrap()
                    .unwrap()
                })
            };

            println!(
                "bootstrap-native-cut {}",
                serde_json::json!({
                    "point":point,"killed_pid":killed_pid,"kill_signal":killed.signal(),
                    "persisted_marker":expected_marker.as_ref().map(|bytes| {
                        let record = crate::records::parse_note(bytes).canonical.unwrap();
                        serde_json::json!({"wiki_id":record.id(),"title":record.title(),
                            "hash":crate::domain::Blake3Hash::digest(bytes)})
                    }),
                    "before":manifest(&before_retry),
                })
            );

            let mut retry = OwnedChild::spawn(&wiki, &control, "retry");
            let retry_pid = retry.0.id();
            let completed = retry.finish(&control, "retry");
            assert!(
                completed.success(),
                "{point}: retry failed: {completed}; logs: {}",
                child_logs(&control, "retry")
            );
            drop(retry);
            let result: serde_json::Value =
                serde_json::from_slice(&fs::read(control.join("result.json")).unwrap()).unwrap();
            assert_eq!(result["ok"], true);
            assert_eq!(result["data"]["completion"], "complete");
            assert_eq!(result["meta"]["network_used"], false);
            assert_author_unchanged(&wiki, &author);
            let marker = fs::read(wiki.join("WIKI.md")).unwrap();
            let record = crate::records::parse_note(&marker).canonical.unwrap();
            if let Some(expected) = expected_marker {
                assert_eq!(marker, expected, "{point}: persisted marker changed");
                assert_eq!(record.title(), TITLE);
            } else {
                assert_eq!(record.title(), "My wiki");
            }
            if let Some((stage, staged, id)) = abandoned {
                assert_eq!(tree(&stage), staged, "abandoned private stage changed");
                assert_ne!(record.id(), &id, "private staging was adopted");
            }
            if let Some(authority) = authority {
                let after = tree(&wiki.join(".wiki"));
                for (path, bytes) in authority.iter().filter(|(_, bytes)| bytes.is_some()) {
                    assert_eq!(
                        after.get(path),
                        Some(bytes),
                        "{point}: authority changed: {}",
                        path.display()
                    );
                }
            }
            let identity = crate::vault::discovery::default_from_path(&wiki).unwrap();
            assert_eq!(&identity.wiki_id, record.id());
            assert_eq!(result["meta"]["wiki_id"], record.id().as_str());
            assert_eq!(
                crate::config::local::load_user_config(
                    Some(&control.join("settings/config.toml")),
                    true
                )
                .unwrap()
                .default_wiki(),
                Some(&identity)
            );
            let root = VaultRoot::explicit(&wiki).unwrap();
            assert_eq!(
                crate::storage::layout::layout_kind(&root).unwrap(),
                crate::storage::layout::LayoutKind::ReadableSources
            );
            VaultFs::new(root).require_storage_ready().unwrap();
            verify_init_durability(&wiki).unwrap();
            println!(
                "bootstrap-native {}",
                serde_json::json!({
                    "point":point,"killed_pid":killed_pid,"kill_signal":killed.signal(),
                    "retry_pid":retry_pid,"retry_exit":completed.code(),
                    "wiki_id":record.id(),"title":record.title(),
                    "before":manifest(&before_retry),"after":manifest(&tree(&wiki)),
                })
            );
        }
    }
}

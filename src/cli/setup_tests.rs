use super::*;
use clap::Parser;
use std::fs;

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Arguments) {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let wiki = base.join("My notes α");
    let preferences = base.join("settings/config.toml");
    let args = Arguments::try_parse_from([
        "lwiki",
        "--preferences",
        preferences.to_str().unwrap(),
        "capabilities",
    ])
    .unwrap();
    (temp, wiki, preferences, args)
}
fn choices(wiki: &Path) -> SetupArguments {
    SetupArguments {
        path: Some(wiki.to_path_buf()),
        non_interactive: true,
        ..Default::default()
    }
}
fn model(wiki: &Path) -> SetupArguments {
    SetupArguments {
        embedding_model: Some("my-selected-embedding-model".into()),
        embedding_url: Some("https://example.invalid/v1/embeddings".into()),
        embedding_dimensions: Some("auto".into()),
        key_env: Some("MY_LWIKI_KEY".into()),
        grant_embed: true,
        ..choices(wiki)
    }
}

#[test]
fn preview_creates_neither_wiki_preferences_nor_provider_files() {
    let (_temp, wiki, preferences, mut args) = fixture();
    args.dry_run = true;
    let report = execute(&args, &model(&wiki)).unwrap();
    assert!(report.ok);
    assert_eq!(report.data["completion"], "preview");
    assert!(!wiki.exists());
    assert!(!preferences.parent().unwrap().exists());
    assert!(!report.meta.network_used);
    assert!(
        report.data["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|step| step["applied"] == false)
    );
}

#[test]
fn creates_persistent_real_wiki_and_rerun_preserves_identity_and_notes() {
    let (_temp, wiki, preferences, args) = fixture();
    let first = execute(&args, &choices(&wiki)).unwrap();
    assert!(first.ok, "{:?}", first.error);
    let identity = crate::vault::discovery::default_from_path(&wiki).unwrap();
    assert_eq!(
        config::local::load_user_config(Some(&preferences), false)
            .unwrap()
            .default_wiki(),
        Some(&identity)
    );
    let authored = "# My notes\n\nI picked this real note's wording.\n";
    fs::write(wiki.join("thoughts.md"), authored).unwrap();
    let marker = fs::read(wiki.join("WIKI.md")).unwrap();
    let second = execute(&args, &choices(&wiki)).unwrap();
    assert!(second.ok, "{:?}", second.error);
    assert_eq!(
        crate::vault::discovery::default_from_path(&wiki).unwrap(),
        identity
    );
    assert_eq!(fs::read(wiki.join("WIKI.md")).unwrap(), marker);
    assert_eq!(
        fs::read_to_string(wiki.join("thoughts.md")).unwrap(),
        authored
    );
    assert_eq!(second.data["steps"][0]["applied"], false);
    assert!(
        !preferences
            .parent()
            .unwrap()
            .join("providers.toml")
            .exists()
    );
    assert!(
        second.data["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step
                .as_str()
                .is_some_and(|text| text.contains("your-note.md")))
    );
}

#[test]
fn existing_notes_folder_needs_intent_and_keeps_editor_bytes() {
    let (_temp, wiki, _preferences, args) = fixture();
    fs::create_dir(&wiki).unwrap();
    fs::create_dir(wiki.join(".obsidian")).unwrap();
    fs::write(wiki.join(".obsidian/settings.json"), "{\"mine\":true}\n").unwrap();
    fs::write(wiki.join("my note.md"), "Original author note\n").unwrap();
    assert_eq!(
        execute(&args, &choices(&wiki)).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert!(!wiki.join("WIKI.md").exists());
    let mut deliberate = choices(&wiki);
    deliberate.existing = true;
    assert!(execute(&args, &deliberate).unwrap().ok);
    assert_eq!(
        fs::read_to_string(wiki.join("my note.md")).unwrap(),
        "Original author note\n"
    );
    assert_eq!(
        fs::read_to_string(wiki.join(".obsidian/settings.json")).unwrap(),
        "{\"mine\":true}\n"
    );
}

#[test]
fn closed_input_without_a_destination_returns_actionable_usage() {
    let (_temp, _wiki, _preferences, args) = fixture();
    let setup = SetupArguments {
        non_interactive: true,
        ..Default::default()
    };
    let mut prompts = Prompts {
        input: io::Cursor::new(b""),
        output: Vec::new(),
    };
    let error = collect(&setup, &args, false, &mut prompts).unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(error.message.contains("lwiki setup PATH --non-interactive"));
    assert!(prompts.output.is_empty());
}

#[test]
fn optional_embedding_setup_writes_references_and_grants_without_reading_the_key() {
    let (_temp, wiki, preferences, args) = fixture();
    let setup = model(&wiki);
    let report = execute(&args, &setup).unwrap();
    assert!(report.ok, "{:?}", report.error);
    let loaded = config::local::load_user_config(Some(&preferences), false).unwrap();
    let provider_path = loaded.document().providers_config.as_ref().unwrap();
    let provider_before = fs::read(provider_path).unwrap();
    let provider = config::providers::ProviderConfig::load(provider_path).unwrap();
    let profile = loaded
        .document()
        .vaults
        .values()
        .next()
        .unwrap()
        .profile
        .as_ref()
        .unwrap();
    let root = crate::vault::VaultRoot::explicit(&wiki).unwrap();
    provider
        .authorize(
            &crate::vault::VaultFs::new(root),
            &crate::vault::discovery::default_from_path(&wiki)
                .unwrap()
                .wiki_id,
            profile,
            crate::jobs::Capability::Embed,
        )
        .unwrap();
    assert!(!report.meta.network_used);
    assert!(execute(&args, &setup).unwrap().ok);
    assert_eq!(fs::read(provider_path).unwrap(), provider_before);
}

#[test]
fn missing_or_invalid_provider_settings_fail_before_creating_the_wiki() {
    let (_temp, wiki, preferences, args) = fixture();
    let mut setup = model(&wiki);
    setup.embedding_url = Some("https://example.invalid/embed?api_key=never-display-this".into());
    let error = execute(&args, &setup).unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    assert!(!error.message.contains("never-display-this"));
    assert!(!wiki.exists());
    assert!(!preferences.exists());
    setup.embedding_url = None;
    assert!(
        execute(&args, &setup)
            .unwrap_err()
            .message
            .contains("--embedding-url")
    );
}

#[test]
fn selecting_invalid_wiki_never_initializes_or_falls_back() {
    let (_temp, wiki, _preferences, args) = fixture();
    fs::create_dir(&wiki).unwrap();
    fs::write(
        wiki.join("WIKI.md"),
        "This is my author file, not a wiki marker.\n",
    )
    .unwrap();
    assert!(execute(&args, &choices(&wiki)).is_err());
    assert_eq!(
        fs::read_to_string(wiki.join("WIKI.md")).unwrap(),
        "This is my author file, not a wiki marker.\n"
    );
    assert!(!wiki.join(".wiki").exists());
}

#[test]
fn failure_after_creation_reports_completed_step_and_retry_selects_same_id() {
    let (_temp, wiki, preferences, args) = fixture();
    CHECKPOINT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|stage, _| {
            if stage == "wiki_complete" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected settings failure",
                ))
            } else {
                Ok(())
            }
        }))
    });
    let result = execute(&args, &choices(&wiki));
    CHECKPOINT.with(|hook| *hook.borrow_mut() = None);
    let report = result.unwrap();
    assert!(!report.ok);
    assert!(report.meta.partial);
    assert_eq!(report.data["steps"][0]["status"], "complete");
    assert_eq!(report.data["steps"][1]["status"], "failed");
    assert!(
        report.data["retry"]
            .as_str()
            .unwrap()
            .contains("--non-interactive")
    );
    assert!(!preferences.exists());
    let identity = crate::vault::discovery::default_from_path(&wiki).unwrap();
    assert!(execute(&args, &choices(&wiki)).unwrap().ok);
    assert_eq!(
        crate::vault::discovery::default_from_path(&wiki).unwrap(),
        identity
    );
}

#[test]
fn provider_builder_preserves_other_settings_and_grants() {
    let wiki = DefaultWiki {
        path: PathBuf::from("/tmp/wiki"),
        wiki_id: RecordId::generate(RecordKind::Vault).unwrap(),
    };
    let selected = EmbeddingSetup {
        model: "a-model".into(),
        url: "https://example.invalid/embeddings".into(),
        dimensions: None,
        auth: SetupAuth::Environment("EXISTING_KEY".into()),
        grant: true,
    };
    let mut first =
        config::provider_setup::build_embedding_document(None, &selected, &wiki).unwrap();
    let mut with_comments =
        b"# Author's provider comments and formatting stay exactly as written.\n".to_vec();
    with_comments.extend_from_slice(&first.bytes);
    first.bytes = with_comments;
    let mut other = selected.clone();
    other.model = "another-model".into();
    let second =
        config::provider_setup::build_embedding_document(Some(&first.bytes), &other, &wiki)
            .unwrap();
    let document: toml::Table =
        toml::from_str(std::str::from_utf8(&second.bytes).unwrap()).unwrap();
    assert_eq!(document["services"].as_table().unwrap().len(), 2);
    assert_eq!(document["profiles"].as_table().unwrap().len(), 2);
    assert!(second.bytes.starts_with(&first.bytes));
    let grants = document["vault_bindings"].as_table().unwrap();
    assert_eq!(grants.len(), 2);
    assert!(
        grants
            .values()
            .all(|grant| grant["allowed_profiles"].as_array().unwrap().len() == 1)
    );
    let third =
        config::provider_setup::build_embedding_document(Some(&second.bytes), &other, &wiki)
            .unwrap();
    assert_eq!(third.bytes, second.bytes);
}

#[test]
fn setup_flags_parse_complete_provider_and_agent_choices() {
    #[derive(Parser)]
    struct ParserOnly {
        #[command(flatten)]
        setup: SetupArguments,
    }
    let parsed = ParserOnly::try_parse_from([
        "setup",
        "/tmp/a wiki",
        "--non-interactive",
        "--agent",
        "codex",
        "--agent-directory",
        "/tmp/agent",
        "--embedding-model",
        "model",
        "--embedding-url",
        "https://example.invalid/embeddings",
        "--key-file",
        "/tmp/key",
        "--grant-embed",
    ])
    .unwrap();
    assert_eq!(parsed.setup.agent.as_deref(), Some("codex"));
    assert!(parsed.setup.grant_embed);
    assert!(
        ParserOnly::try_parse_from(["setup", "--key-env", "KEY", "--key-file", "/tmp/key"])
            .is_err()
    );
}

fn relative_to_current_directory(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let mut relative = PathBuf::new();
    for component in cwd.components() {
        if matches!(component, std::path::Component::Normal(_)) {
            relative.push("..");
        }
    }
    relative.push(path.strip_prefix("/").unwrap());
    relative
}

#[test]
fn explicit_existing_and_missing_json_use_published_absolute_toml_in_next_steps() {
    for present in [false, true] {
        for relative in [false, true] {
            let (_temp, wiki, preferences, mut args) = fixture();
            let json = preferences.with_extension("json");
            let original = b"{\"schema_version\":\"2\"}\n";
            if present {
                fs::create_dir_all(json.parent().unwrap()).unwrap();
                fs::write(&json, original).unwrap();
            }
            args.preferences = Some(if relative {
                relative_to_current_directory(&json)
            } else {
                json.clone()
            });
            let report = execute(&args, &choices(&wiki)).unwrap();
            assert!(report.ok, "{:?}", report.error);
            assert!(preferences.is_file());
            for step in report.data["next_steps"].as_array().unwrap() {
                let text = step.as_str().unwrap();
                if text.contains("Capture one")
                    || text.contains("Find its facts")
                    || text.contains("Inspect saved")
                {
                    assert!(
                        text.contains(&format!("--preferences {}", quote_path(&preferences))),
                        "{text}"
                    );
                    assert!(!text.contains(&quote_path(&json)), "{text}");
                }
            }
            assert!(
                report.data["retry"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("--preferences {}", quote_path(&preferences)))
            );
            if present {
                assert_eq!(fs::read(&json).unwrap(), original);
            } else {
                assert!(!json.exists());
            }
        }
    }
}

#[test]
fn failed_setup_retry_uses_source_before_config_publication_and_destination_afterward() {
    for phase in ["wiki_complete", "configuration_complete"] {
        let (_temp, wiki, preferences, mut args) = fixture();
        let json = preferences.with_extension("json");
        args.preferences = Some(relative_to_current_directory(&json));
        CHECKPOINT.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |stage, _| {
                if stage == phase {
                    Err(WikiError::new(
                        ErrorCode::Internal,
                        "injected setup interruption",
                    ))
                } else {
                    Ok(())
                }
            }))
        });
        let report = execute(&args, &choices(&wiki));
        CHECKPOINT.with(|hook| *hook.borrow_mut() = None);
        let report = report.unwrap();
        assert!(!report.ok);
        let expected = if phase == "wiki_complete" {
            &json
        } else {
            &preferences
        };
        assert!(
            report.data["retry"]
                .as_str()
                .unwrap()
                .contains(&format!("--preferences {}", quote_path(expected)))
        );
        args.preferences = Some(expected.clone());
        let identity = crate::vault::discovery::default_from_path(&wiki).unwrap();
        assert!(execute(&args, &choices(&wiki)).unwrap().ok);
        assert_eq!(
            crate::vault::discovery::default_from_path(&wiki).unwrap(),
            identity
        );
    }
}

#[test]
fn pending_hidden_key_retry_is_interactive_and_saved_key_retry_is_unattended() {
    let (_temp, wiki, preferences, args) = fixture();
    let mut setup = model(&wiki);
    setup.key_env = None;
    setup.prompt_key = true;
    setup.credential_file = Some(preferences.with_file_name("private-key"));
    let pending = retry_command(&args, &setup, Some(&preferences));
    assert!(pending.contains("--prompt-key"));
    assert!(!pending.contains("--non-interactive"));
    setup.prompt_key = false;
    setup.key_file = setup.credential_file.take();
    let saved = retry_command(&args, &setup, Some(&preferences));
    assert!(!saved.contains("--prompt-key"));
    assert!(!saved.contains("--credential-file"));
    assert!(saved.contains("--non-interactive"));
    assert!(saved.contains("--key-file"));
}

#[test]
fn selecting_without_config_updates_keeps_the_original_explicit_json_prefix() {
    let (_temp, wiki, preferences, mut args) = fixture();
    let json = preferences.with_extension("json");
    fs::create_dir_all(json.parent().unwrap()).unwrap();
    fs::write(&json, b"{\"schema_version\":\"2\"}\n").unwrap();
    args.preferences = Some(relative_to_current_directory(&json));
    let mut setup = choices(&wiki);
    setup.no_default = true;
    let report = execute(&args, &setup).unwrap();
    assert!(report.ok);
    assert!(!preferences.exists());
    assert!(
        report.data["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .any(
                |step| step.contains(&format!("--preferences {}", quote_path(&json)))
                    && step.contains("--wiki")
            )
    );
}

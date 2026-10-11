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
fn interactive_use_offers_initialization_and_preserves_notes() {
    let (_temp, wiki, preferences, args) = fixture();
    fs::create_dir(&wiki).unwrap();
    fs::create_dir(wiki.join(".obsidian")).unwrap();
    let note = b"My own note, kept exactly.\r\n";
    let editor = b"{\"mine\":true}\n";
    fs::write(wiki.join("my note.md"), note).unwrap();
    fs::write(wiki.join(".obsidian/settings.json"), editor).unwrap();
    let input = format!("use\n{}\nyes\nMy notes\nyes\nskip\nno\n", wiki.display());
    let mut prompts = Prompts {
        input: io::Cursor::new(input.as_bytes()),
        output: Vec::new(),
    };
    let mut chosen = collect(&SetupArguments::default(), &args, true, &mut prompts).unwrap();
    assert!(chosen.existing);
    assert!(!chosen.select);
    assert_eq!(chosen.path.as_deref(), Some(wiki.as_path()));
    assert!(!wiki.join("WIKI.md").exists());
    assert!(!preferences.exists());
    let text = String::from_utf8(prompts.output).unwrap();
    assert!(text.contains("This folder is not a wiki yet"));
    assert!(text.find("keeping your notes").unwrap() < text.find("Connect an agent").unwrap());
    chosen.non_interactive = true;
    assert!(execute(&args, &chosen).unwrap().ok);
    assert_eq!(fs::read(wiki.join("my note.md")).unwrap(), note);
    assert_eq!(
        fs::read(wiki.join(".obsidian/settings.json")).unwrap(),
        editor
    );
}

#[test]
fn folder_failures_precede_agent_provider_and_key_questions() {
    for occupied in ["WIKI.md", "pages"] {
        let (_temp, wiki, preferences, args) = fixture();
        fs::create_dir(&wiki).unwrap();
        let original = b"Author-owned bytes\n";
        fs::write(wiki.join(occupied), original).unwrap();
        let mut prompts = Prompts {
            input: io::Cursor::new(b"yes\ncodex\nyes\nkey\n"),
            output: Vec::new(),
        };
        let error = collect(&choices(&wiki), &args, true, &mut prompts).unwrap_err();
        assert!(matches!(
            error.code,
            ErrorCode::RecordInvalid | ErrorCode::ContentConflict
        ));
        assert_eq!(prompts.input.position(), 0);
        assert!(prompts.output.is_empty());
        assert_eq!(fs::read(wiki.join(occupied)).unwrap(), original);
        assert!(!wiki.join(".wiki").exists());
        assert!(!preferences.exists());
    }

    let (_temp, wiki, preferences, args) = fixture();
    let unavailable = wiki.join("parent not created").join("wiki");
    let mut chosen = choices(&unavailable);
    chosen.title = Some("My wiki".into());
    let mut prompts = Prompts {
        input: io::Cursor::new(b"yes\ncodex\nyes\nkey\n"),
        output: Vec::new(),
    };
    assert!(collect(&chosen, &args, true, &mut prompts).is_err());
    assert_eq!(prompts.input.position(), 0);
    assert!(prompts.output.is_empty());
    assert!(!wiki.exists());
    assert!(!preferences.exists());
}

#[test]
fn declined_initialization_and_noninteractive_select_leave_notes_untouched() {
    let (_temp, wiki, preferences, args) = fixture();
    fs::create_dir(&wiki).unwrap();
    fs::write(wiki.join("note.md"), b"Keep this note\n").unwrap();
    let mut selected = choices(&wiki);
    selected.select = true;
    let mut prompts = Prompts {
        input: io::Cursor::new(b"no\n"),
        output: Vec::new(),
    };
    assert_eq!(
        collect(&selected, &args, true, &mut prompts)
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    let mut no_prompts = Prompts {
        input: io::Cursor::new(b"yes\n"),
        output: Vec::new(),
    };
    let error = collect(&selected, &args, false, &mut no_prompts).unwrap_err();
    assert_eq!(error.code, ErrorCode::VaultNotFound);
    assert!(error.hint.unwrap().contains("--existing"));
    assert_eq!(no_prompts.input.position(), 0);
    assert!(no_prompts.output.is_empty());
    assert!(!wiki.join("WIKI.md").exists());
    assert!(!wiki.join(".wiki").exists());
    assert!(!preferences.exists());
    assert_eq!(fs::read(wiki.join("note.md")).unwrap(), b"Keep this note\n");
}

#[test]
fn select_remediation_does_not_describe_missing_folders_as_existing_notes() {
    let (_temp, wiki, preferences, args) = fixture();
    let mut selected = choices(&wiki);
    selected.select = true;
    let mut prompts = Prompts {
        input: io::Cursor::new(b""),
        output: Vec::new(),
    };
    let error = collect(&selected, &args, false, &mut prompts).unwrap_err();
    assert!(
        !error
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("--existing"))
    );
    assert!(!wiki.exists());
    assert!(!preferences.exists());
}

#[test]
fn initialized_folder_is_selected_before_optional_questions() {
    let (_temp, wiki, _preferences, mut args) = fixture();
    assert!(execute(&args, &choices(&wiki)).unwrap().ok);
    let identity = crate::vault::discovery::default_from_path(&wiki).unwrap();
    let marker = fs::read(wiki.join("WIKI.md")).unwrap();
    args.expect_wiki_id = Some(identity.wiki_id);
    let setup = SetupArguments {
        path: Some(wiki.clone()),
        select: true,
        no_default: true,
        ..Default::default()
    };
    let mut prompts = Prompts {
        input: io::Cursor::new(b"skip\nno\n"),
        output: Vec::new(),
    };
    let chosen = collect(&setup, &args, true, &mut prompts).unwrap();
    assert!(chosen.select);
    assert!(!chosen.existing);
    assert!(chosen.title.is_none());
    assert_eq!(fs::read(wiki.join("WIKI.md")).unwrap(), marker);
    let output = String::from_utf8(prompts.output).unwrap();
    assert!(!output.contains("not a wiki yet"));
    assert!(!output.contains("Wiki title"));
}

#[test]
fn file_target_is_rejected_before_optional_questions() {
    let (_temp, wiki, _preferences, args) = fixture();
    fs::write(&wiki, b"Author file\n").unwrap();
    let mut prompts = Prompts {
        input: io::Cursor::new(b"yes\ncodex\nyes\n"),
        output: Vec::new(),
    };
    let error = collect(&choices(&wiki), &args, true, &mut prompts).unwrap_err();
    assert!(error.message.contains(wiki.to_str().unwrap()));
    assert_eq!(prompts.input.position(), 0);
    assert!(prompts.output.is_empty());
    assert_eq!(fs::read(wiki).unwrap(), b"Author file\n");
}

#[cfg(unix)]
#[test]
fn symlink_folder_is_rejected_before_optional_questions() {
    let (_temp, wiki, _preferences, args) = fixture();
    let target = wiki.with_file_name("Author notes");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("note.md"), b"Author note\n").unwrap();
    std::os::unix::fs::symlink(&target, &wiki).unwrap();
    let mut prompts = Prompts {
        input: io::Cursor::new(b"yes\ncodex\nyes\n"),
        output: Vec::new(),
    };
    assert!(collect(&choices(&wiki), &args, true, &mut prompts).is_err());
    assert_eq!(prompts.input.position(), 0);
    assert!(prompts.output.is_empty());
    assert!(!target.join("WIKI.md").exists());
    assert!(!target.join(".wiki").exists());
    assert_eq!(fs::read(target.join("note.md")).unwrap(), b"Author note\n");
}

#[test]
fn prompted_paths_correct_pasted_delimiters_and_keep_spaces() {
    let (_temp, wiki, _preferences, _args) = fixture();
    for delimiter in ['\'', '"', '`'] {
        let input = format!(
            "{delimiter}{}{delimiter}\n{}\n",
            wiki.display(),
            wiki.display()
        );
        let mut prompts = Prompts {
            input: io::Cursor::new(input.as_bytes()),
            output: Vec::new(),
        };
        assert_eq!(prompts.path("Wiki folder", None).unwrap(), wiki);
        let text = String::from_utf8(prompts.output).unwrap();
        assert!(text.contains("without surrounding quotes or backticks"));
        assert!(text.contains("Spaces are kept as typed"));
        assert!(!wiki.exists());
    }
}

#[test]
fn explicit_path_keeps_literal_quotes_and_backticks() {
    let (_temp, wiki, _preferences, mut args) = fixture();
    let literal = wiki.with_file_name("`My 'notes'`");
    args.dry_run = true;
    let report = execute(&args, &choices(&literal)).unwrap();
    assert!(report.ok);
    assert_eq!(report.data["plan"]["wiki"], json!(literal));
    assert!(!literal.exists());
}

#[test]
fn agent_next_steps_explain_session_restart() {
    let (_temp, wiki, preferences, args) = fixture();
    let mut setup = choices(&wiki);
    setup.agent = Some("codex".into());
    let steps = next_steps(&args, &setup, &wiki, Some(&preferences));
    assert_eq!(
        steps[0],
        "Restart your codex session so it can find the installed llm-wiki instructions."
    );
    assert!(steps[1].starts_with("Ask codex:"));
}

#[test]
fn embedding_consent_names_model_destination_and_explicit_text_scope() {
    for (answer, granted) in [("yes", true), ("no", false)] {
        let (_temp, wiki, preferences, args) = fixture();
        let mut setup = model(&wiki);
        setup.title = Some("My notes".into());
        setup.no_default = true;
        setup.grant_embed = false;
        setup.embedding_url = Some(
            "https://gateway.example.invalid:8443/private-deployment?tenant=private-query".into(),
        );
        let input = format!("skip\n{answer}\n");
        let mut prompts = Prompts {
            input: io::Cursor::new(input.as_bytes()),
            output: Vec::new(),
        };
        let choices = collect(&setup, &args, true, &mut prompts).unwrap();
        assert_eq!(choices.grant_embed, granted);
        let output = String::from_utf8(prompts.output).unwrap();
        assert!(output.contains("Permit explicit requests for similarity search"));
        assert!(
            output.contains("my-selected-embedding-model at https://gateway.example.invalid:8443")
        );
        assert!(output.contains("wiki text and search questions and cost money"));
        assert!(output.contains("Setup makes no requests"));
        assert!(!output.contains("private-deployment"));
        assert!(!output.contains("private-query"));
        assert!(!wiki.exists());
        assert!(!preferences.exists());
    }
}

#[test]
fn embedding_preview_discloses_safe_destination_and_scope_without_remote_work() {
    for granted in [false, true] {
        let (_temp, wiki, preferences, mut args) = fixture();
        args.dry_run = true;
        args.json = true;
        let mut setup = model(&wiki);
        setup.grant_embed = granted;
        setup.embedding_url =
            Some("https://[2001:db8::1]:8443/private-deployment?tenant=private-query".into());
        let report = execute(&args, &setup).unwrap();
        let embedding = &report.data["plan"]["embedding"];
        assert_eq!(
            embedding["destination"],
            json!({"scheme":"https","host":"[2001:db8::1]","port":8443})
        );
        assert_eq!(embedding["text_scope"], "wiki text and search questions");
        assert_eq!(embedding["grant_embed"], granted);
        assert_eq!(embedding["network_requests"], 0);
        let mut output = Vec::new();
        present_report(&report, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Embedding model: my-selected-embedding-model"));
        assert!(output.contains("Destination: https://[2001:db8::1]:8443"));
        assert!(
            output.contains("Text that explicit requests may send: wiki text and search questions")
        );
        assert!(output.contains(if granted {
            "permitted when explicitly requested"
        } else {
            "not permitted"
        }));
        assert!(!output.contains("private-deployment"));
        assert!(!output.contains("private-query"));
        assert!(!report.meta.network_used);
        assert!(!wiki.exists());
        assert!(!preferences.exists());
    }
}

#[test]
fn credential_bearing_endpoint_fails_before_the_consent_question() {
    let (_temp, wiki, preferences, args) = fixture();
    let mut setup = model(&wiki);
    setup.title = Some("My notes".into());
    setup.no_default = true;
    setup.grant_embed = false;
    setup.embedding_url = Some("https://private-user:private-password@gateway.example.invalid/private-path?tenant=private-query".into());
    let mut prompts = Prompts {
        input: io::Cursor::new(b"skip\nyes\n"),
        output: Vec::new(),
    };
    let error = collect(&setup, &args, true, &mut prompts).unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    let output = String::from_utf8(prompts.output).unwrap();
    assert!(!output.contains("Permit explicit"));
    for private in [
        "private-user",
        "private-password",
        "private-path",
        "private-query",
    ] {
        assert!(!output.contains(private));
        assert!(!error.message.contains(private));
    }
    assert!(!wiki.exists());
    assert!(!preferences.exists());
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

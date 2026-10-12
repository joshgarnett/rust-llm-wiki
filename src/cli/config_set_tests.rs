use super::*;
use clap::Parser;
use std::fs;

struct Fixture {
    _temp: tempfile::TempDir,
    wiki: DefaultWiki,
    other: DefaultWiki,
    preferences: PathBuf,
    providers: PathBuf,
    args: Arguments,
    original: Vec<u8>,
}
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let wiki_path = base.join("my notes");
    let other_path = base.join("other notes");
    let preferences = base.join("settings/config.toml");
    let providers = base.join("settings/providers.toml");
    let args = Arguments::try_parse_from([
        "lwiki",
        "--preferences",
        preferences.to_str().unwrap(),
        "--wiki",
        wiki_path.to_str().unwrap(),
        "capabilities",
    ])
    .unwrap();
    let setup = super::super::setup::SetupArguments {
        path: Some(wiki_path),
        non_interactive: true,
        ..Default::default()
    };
    assert!(super::super::setup::execute(&args, &setup).unwrap().ok);
    crate::app::offline::init(
        &other_path,
        "Other notes",
        crate::app::OperationOptions::default(),
    )
    .unwrap();
    let wiki = crate::vault::discovery::default_from_path(setup.path.as_ref().unwrap()).unwrap();
    let other = crate::vault::discovery::default_from_path(&other_path).unwrap();
    let original = format!(
        r#"# Owner comments, order, and private provider settings stay intact.
version = 1
[profiles.manual]
embedding = "embed"
generation = "generate"
[services.embed]
adapter = "embeddings-v1"
url = "https://embed.example/private-endpoint?tenant=private-tenant"
model = "old-embedding"
dimensions = 1024
[services.embed.auth]
kind = "static"
key_env = "LWIKI_UNUSED_TEST_KEY"
header = "X-Provider-Key"
prefix = "private-prefix "
[services.embed.headers]
X-Deployment = "private-header-marker"
[services.generate]
adapter = "responses-v1"
url = "https://generate.example/private-generation"
model = "old-generation"
max_output_tokens = 4096
[services.generate.auth]
kind = "static"
key_env = "LWIKI_UNUSED_GENERATION_KEY"
[vault_bindings.one]
root = {}
wiki_id = {}
allowed_profiles = ["manual"]
[vault_bindings.two]
root = {}
wiki_id = {}
allowed_profiles = ["manual"]
"#,
        json!(wiki.path),
        json!(wiki.wiki_id),
        json!(other.path),
        json!(other.wiki_id)
    )
    .into_bytes();
    fs::write(&providers, &original).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&providers, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let loaded = config::local::load_user_config(Some(&preferences), false).unwrap();
    let mut document = loaded.document().clone();
    document.providers_config = Some(providers.clone());
    document.defaults.profile = Some("manual".into());
    document.allowed_profiles = vec!["manual".into()];
    document
        .vaults
        .entry(wiki.wiki_id.clone())
        .or_default()
        .profile = Some("manual".into());
    document
        .vaults
        .entry(other.wiki_id.clone())
        .or_default()
        .profile = Some("manual".into());
    config::local::update_document(&loaded, document, None, false, 5000).unwrap();
    Fixture {
        _temp: temp,
        wiki,
        other,
        preferences,
        providers,
        args,
        original,
    }
}
fn choices(role: ProviderRoleArgument) -> ProviderSetArguments {
    ProviderSetArguments {
        role,
        model: None,
        url: None,
        adapter: None,
        dimensions: None,
        max_output_tokens: None,
        reasoning_effort: None,
        key_env: None,
        key_file: None,
        credential_command: None,
        credential_output: None,
        reuse_auth: None,
        providers_config: None,
        grant_provider: false,
        auto_embed: false,
        non_interactive: true,
    }
}
fn read(path: &Path) -> toml::Table {
    toml::from_str(std::str::from_utf8(&fs::read(path).unwrap()).unwrap()).unwrap()
}

#[test]
fn automatic_choice_enables_existing_model_without_an_auth_or_model_edit() {
    let mut fixture = fixture();
    fixture.args.offline = true;
    let mut settings = choices(ProviderRoleArgument::Embedding);
    settings.auto_embed = true;
    let outcome = execute(&fixture.args, &settings).unwrap();
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(
        outcome.data["providers"]["automatic_embedding_consent"],
        "matching"
    );
    assert_eq!(
        outcome.data["providers"]["embedding"]["model"],
        "old-embedding"
    );
    assert_eq!(
        outcome.data["providers"]["generation"]["model"],
        "old-generation"
    );
    assert!(!outcome.meta.network_used);
    let providers = config::providers::ProviderConfig::load(&fixture.providers).unwrap();
    let fs =
        crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&fixture.wiki.path).unwrap());
    assert_eq!(
        providers
            .automatic_embedding_consent(&fs, &fixture.wiki.wiki_id, &selected_profile(&fixture))
            .unwrap(),
        AutomaticEmbeddingConsent::Matching
    );
    assert_eq!(
        read(&fixture.providers)["services"]["generate"],
        toml::from_str::<toml::Table>(std::str::from_utf8(&fixture.original).unwrap()).unwrap()["services"]
            ["generate"]
    );
}

#[test]
fn automatic_choice_does_not_authorize_an_unbound_generation_model() {
    let mut fixture = fixture();
    fixture.args.offline = true;
    let private = std::str::from_utf8(&fixture.original)
        .unwrap()
        .replace("allowed_profiles = [\"manual\"]", "allowed_profiles = []");
    fs::write(&fixture.providers, private).unwrap();
    let mut settings = choices(ProviderRoleArgument::Embedding);
    settings.auto_embed = true;
    let outcome = execute(&fixture.args, &settings).unwrap();
    assert!(outcome.ok, "{:?}", outcome.error);
    let providers = config::providers::ProviderConfig::load(&fixture.providers).unwrap();
    let fs =
        crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&fixture.wiki.path).unwrap());
    let profile = selected_profile(&fixture);
    assert!(
        providers
            .authorize_automatic_embeddings(&fs, &fixture.wiki.wiki_id, &profile)
            .is_ok()
    );
    assert!(
        providers
            .authorize(
                &fs,
                &fixture.wiki.wiki_id,
                &profile,
                crate::jobs::Capability::Generate
            )
            .is_err()
    );
    assert!(
        providers
            .authorize(
                &fs,
                &fixture.wiki.wiki_id,
                &profile,
                crate::jobs::Capability::Embed
            )
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn generation_edits_keep_automatic_permission_without_running_embedding_helper() {
    use std::os::unix::fs::PermissionsExt;

    for edit in ["cap", "credentials", "effort"] {
        let mut fixture = fixture();
        let program = fixture.wiki.path.parent().unwrap().join("embedding helper");
        let sentinel = program.with_file_name("embedding helper ran");
        fs::write(
            &program,
            format!("#!/bin/sh\ntouch '{}'\nexit 47\n", sentinel.display()),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();

        fixture.args.offline = true;
        let mut automatic = choices(ProviderRoleArgument::Embedding);
        automatic.auto_embed = true;
        automatic.credential_command = Some(format!("'{}'", program.display()));
        assert!(execute(&fixture.args, &automatic).unwrap().ok);
        assert!(!sentinel.exists());

        let vault_fs = crate::vault::VaultFs::new(
            crate::vault::VaultRoot::explicit(&fixture.wiki.path).unwrap(),
        );
        let app =
            crate::app::OfflineApp::new(vault_fs.clone(), crate::app::OperationOptions::default())
                .unwrap();
        app.page_put(
            VaultRelativePath::new("knowledge/pages/pending.md").unwrap(),
            b"---\nwiki_schema: '1'\nwiki_id: Page.Pending\nwiki_kind: page\ntitle: Pending\nwiki_status: reviewed\n---\nA saved note awaits similarity search preparation.\n".to_vec(),
            None,
        )
        .unwrap();

        fixture.args.offline = false;
        let mut generation = choices(ProviderRoleArgument::Generation);
        generation.grant_provider = true;
        if edit == "cap" {
            generation.max_output_tokens = Some(2048);
        } else if edit == "effort" {
            generation.reasoning_effort = Some("low".into());
        } else {
            generation.key_env = Some("LWIKI_UNREAD_UPDATED_GENERATION_KEY".into());
        }
        let outcome = execute(&fixture.args, &generation).unwrap();
        assert!(outcome.ok, "{edit}: {:?}", outcome.error);
        assert!(!outcome.meta.network_used, "{edit}");
        assert!(outcome.data.get("automatic_embeddings").is_none(), "{edit}");
        assert_eq!(outcome.data["configuration_network_requests"], 0);
        assert_eq!(outcome.data["configuration_credentials_resolved"], false);
        assert!(outcome.data.get("network_requests").is_none());
        assert!(outcome.data.get("credentials_resolved").is_none());
        assert!(
            !sentinel.exists(),
            "{edit} ran the embedding credential helper"
        );
        let providers = config::providers::ProviderConfig::load(&fixture.providers).unwrap();
        assert_eq!(
            providers
                .automatic_embedding_consent(
                    &vault_fs,
                    &fixture.wiki.wiki_id,
                    &selected_profile(&fixture),
                )
                .unwrap(),
            AutomaticEmbeddingConsent::Matching,
            "{edit} lost the unchanged embedding permission"
        );
    }
}

fn selected_profile(fixture: &Fixture) -> String {
    config::local::load_user_config(Some(&fixture.preferences), false)
        .unwrap()
        .document()
        .vaults[&fixture.wiki.wiki_id]
        .profile
        .clone()
        .unwrap()
}

#[test]
fn changes_one_model_and_keeps_manual_bytes_other_model_and_other_wiki() {
    let fixture = fixture();
    let before = read(&fixture.providers);
    let mut settings = choices(ProviderRoleArgument::Embedding);
    settings.model = Some("new-embedding".into());
    settings.grant_provider = true;
    let result = execute(&fixture.args, &settings).unwrap();
    assert!(result.ok, "{:?}", result.error);
    assert!(!result.meta.network_used);
    assert_eq!(
        result.data["providers"]["changed_fields"],
        json!([{"role":"embedding","field":"model"}])
    );
    let after = read(&fixture.providers);
    let profile = selected_profile(&fixture);
    assert!(
        fs::read(&fixture.providers)
            .unwrap()
            .starts_with(&fixture.original)
    );
    assert_eq!(
        after["profiles"][profile.as_str()]["generation"].as_str(),
        Some("generate")
    );
    let embed = after["profiles"][profile.as_str()]["embedding"]
        .as_str()
        .unwrap();
    let mut expected = before["services"]["embed"].as_table().unwrap().clone();
    expected.insert("model".into(), toml::Value::String("new-embedding".into()));
    assert_eq!(after["services"][embed].as_table().unwrap(), &expected);
    let loaded = config::local::load_user_config(Some(&fixture.preferences), false).unwrap();
    assert_eq!(
        loaded.document().vaults[&fixture.other.wiki_id]
            .profile
            .as_deref(),
        Some("manual")
    );
    let providers = config::providers::ProviderConfig::load(&fixture.providers).unwrap();
    let other_fs =
        crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&fixture.other.path).unwrap());
    assert!(
        providers
            .authorize(
                &other_fs,
                &fixture.other.wiki_id,
                &profile,
                crate::jobs::Capability::Embed
            )
            .is_err()
    );
    providers
        .authorize(
            &other_fs,
            &fixture.other.wiki_id,
            "manual",
            crate::jobs::Capability::Generate,
        )
        .unwrap();
    for marker in [
        "private-endpoint",
        "private-tenant",
        "private-header-marker",
        "private-prefix",
    ] {
        assert!(!serde_json::to_string(&result).unwrap().contains(marker));
    }
}

#[test]
fn changes_generation_cap_then_replays_without_growth_or_repeat_permission() {
    let fixture = fixture();
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.max_output_tokens = Some(8192);
    settings.grant_provider = true;
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    let profile = selected_profile(&fixture);
    let document = read(&fixture.providers);
    assert_eq!(
        document["profiles"][profile.as_str()]["embedding"].as_str(),
        Some("embed")
    );
    let generate = document["profiles"][profile.as_str()]["generation"]
        .as_str()
        .unwrap();
    assert_eq!(
        document["services"][generate]["max_output_tokens"].as_integer(),
        Some(8192)
    );
    let provider_bytes = fs::read(&fixture.providers).unwrap();
    let preferences_bytes = fs::read(&fixture.preferences).unwrap();
    settings.grant_provider = false;
    let repeat = execute(&fixture.args, &settings).unwrap();
    assert!(repeat.ok);
    assert_eq!(repeat.data["providers"]["changed_fields"], json!([]));
    assert_eq!(fs::read(&fixture.providers).unwrap(), provider_bytes);
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences_bytes);
}

#[test]
fn noop_keeps_exact_manual_profile_and_preferences_and_setup_rerun() {
    let fixture = fixture();
    let preferences = fs::read(&fixture.preferences).unwrap();
    let mut settings = choices(ProviderRoleArgument::Embedding);
    settings.model = Some("old-embedding".into());
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    assert_eq!(selected_profile(&fixture), "manual");
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
    let setup = super::super::setup::SetupArguments {
        path: Some(fixture.wiki.path.clone()),
        select: true,
        non_interactive: true,
        ..Default::default()
    };
    let rerun = super::super::setup::execute(&fixture.args, &setup).unwrap();
    assert!(rerun.ok);
    assert_eq!(rerun.data["plan"]["generation"]["model"], "old-generation");
    assert_eq!(selected_profile(&fixture), "manual");
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
}

#[test]
fn readonly_preview_and_missing_permission_refusal_preserve_both_files() {
    let mut fixture = fixture();
    let preferences = fs::read(&fixture.preferences).unwrap();
    let mut settings = choices(ProviderRoleArgument::Embedding);
    settings.model = Some("new-embedding".into());
    fixture.args.dry_run = true;
    fixture.args.offline = true;
    fixture.args.json = true;
    let preview = execute(&fixture.args, &settings).unwrap();
    assert!(preview.ok);
    assert_eq!(preview.data["completion"], "preview");
    assert_eq!(preview.data["providers"]["permission_complete"], false);
    assert!(!preview.meta.network_used);
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
    fixture.args.dry_run = false;
    let error = execute(&fixture.args, &settings).unwrap_err();
    assert!(error.message.contains("--grant-provider"));
    assert_eq!(error.details["writes"], 0);
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
}

#[test]
fn partial_provider_publication_and_retry_reuse_entries() {
    let fixture = fixture();
    let preferences = fs::read(&fixture.preferences).unwrap();
    let mut settings = choices(ProviderRoleArgument::Embedding);
    settings.model = Some("changed".into());
    settings.grant_provider = true;
    CHECKPOINT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|stage, _| {
            if stage == "provider_complete" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected preferences publication failure",
                ))
            } else {
                Ok(())
            }
        }))
    });
    let result = execute(&fixture.args, &settings);
    CHECKPOINT.with(|hook| *hook.borrow_mut() = None);
    let report = result.unwrap();
    assert!(!report.ok);
    assert!(report.meta.partial);
    assert_eq!(report.data["steps"][0]["status"], "complete");
    assert_eq!(report.data["steps"][1]["status"], "failed");
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
    let retained = fs::read(&fixture.providers).unwrap();
    assert!(retained.starts_with(&fixture.original));
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    assert_eq!(fs::read(&fixture.providers).unwrap(), retained);
}

#[test]
fn concurrent_preferences_edit_is_kept_after_provider_publication() {
    let fixture = fixture();
    let preferences_path = fixture.preferences.clone();
    let mut concurrent = fs::read(&fixture.preferences).unwrap();
    concurrent.extend_from_slice(b"\n# concurrent owner edit\n");
    let expected = concurrent.clone();
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.max_output_tokens = Some(8192);
    settings.grant_provider = true;
    CHECKPOINT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |stage, _| {
            if stage == "provider_complete" {
                fs::write(&preferences_path, &concurrent).unwrap();
            }
            Ok(())
        }))
    });
    let result = execute(&fixture.args, &settings);
    CHECKPOINT.with(|hook| *hook.borrow_mut() = None);
    let report = result.unwrap();
    assert!(!report.ok);
    assert!(report.meta.partial);
    assert_eq!(report.error.unwrap().code, "CONTENT_CONFLICT");
    assert_eq!(fs::read(&fixture.preferences).unwrap(), expected);
}

#[test]
fn interactive_and_noninteractive_single_setting_collection() {
    let settings = choices(ProviderRoleArgument::Generation);
    let mut none = Prompts {
        input: io::Cursor::new(b""),
        output: Vec::new(),
    };
    assert!(
        collect(&settings, false, &mut none)
            .unwrap_err()
            .message
            .contains("supply a setting")
    );
    assert!(none.output.is_empty());
    let mut prompts = Prompts {
        input: io::Cursor::new(b"max-output-tokens\n8192\n"),
        output: Vec::new(),
    };
    let chosen = collect(&settings, true, &mut prompts).unwrap();
    assert_eq!(chosen.max_output_tokens, Some(8192));
    assert_eq!(chosen.model, None);
    assert_eq!(chosen.url, None);
    assert!(matches!(
        patch(&chosen).unwrap().auth,
        ProviderAuthPatch::Preserve
    ));
}

#[test]
fn config_set_flags_parse_and_reject_role_incompatible_fields_and_auth_conflicts() {
    let args = Arguments::try_parse_from([
        "lwiki",
        "config",
        "set",
        "generation",
        "--model",
        "extract",
        "--url",
        "https://generate.example/responses",
        "--adapter",
        "responses-v1",
        "--reuse-auth",
        "embedding",
        "--grant-provider",
        "--non-interactive",
    ])
    .unwrap();
    assert_eq!(args.command.name(), "config set");
    assert!(
        Arguments::try_parse_from([
            "lwiki",
            "config",
            "set",
            "embedding",
            "--key-env",
            "KEY",
            "--credential-command",
            "helper auth"
        ])
        .is_err()
    );
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.dimensions = Some("auto".into());
    assert!(patch(&settings).is_err());
}

#[test]
fn noop_follows_default_manual_profile_and_keeps_commented_preferences_exact() {
    let fixture = fixture();
    let loaded = config::local::load_user_config(Some(&fixture.preferences), false).unwrap();
    let mut document = loaded.document().clone();
    document.vaults.remove(&fixture.wiki.wiki_id);
    config::local::update_document(&loaded, document, None, false, 5000).unwrap();
    let mut original =
        b"# My owner preferences comment and formatting stay on an unchanged edit.\n".to_vec();
    original.extend(fs::read(&fixture.preferences).unwrap());
    fs::write(&fixture.preferences, &original).unwrap();
    let mut settings = choices(ProviderRoleArgument::Embedding);
    settings.model = Some("old-embedding".into());
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    assert_eq!(fs::read(&fixture.preferences).unwrap(), original);
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    let loaded = config::local::load_user_config(Some(&fixture.preferences), false).unwrap();
    assert!(!loaded.document().vaults.contains_key(&fixture.wiki.wiki_id));
}

#[test]
fn returning_to_a_historical_granted_profile_still_needs_review() {
    let fixture = fixture();
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.max_output_tokens = Some(8192);
    settings.grant_provider = true;
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    let provider = fs::read(&fixture.providers).unwrap();
    let preferences = fs::read(&fixture.preferences).unwrap();
    settings.max_output_tokens = Some(4096);
    settings.grant_provider = false;
    let error = execute(&fixture.args, &settings).unwrap_err();
    assert!(error.message.contains("--grant-provider"));
    assert_eq!(fs::read(&fixture.providers).unwrap(), provider);
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
}

#[test]
fn concurrent_provider_edit_is_kept_and_prevents_selecting_the_new_profile() {
    let fixture = fixture();
    let provider_path = fixture.providers.clone();
    let preferences = fs::read(&fixture.preferences).unwrap();
    let mut settings = choices(ProviderRoleArgument::Embedding);
    settings.model = Some("changed".into());
    settings.grant_provider = true;
    CHECKPOINT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |stage, _| {
            if stage == "provider_complete" {
                let mut bytes = fs::read(&provider_path).unwrap();
                bytes.extend_from_slice(b"\n# concurrent private owner edit\n");
                fs::write(&provider_path, bytes).unwrap();
            }
            Ok(())
        }))
    });
    let result = execute(&fixture.args, &settings);
    CHECKPOINT.with(|hook| *hook.borrow_mut() = None);
    let result = result.unwrap();
    assert!(!result.ok);
    assert!(result.meta.partial);
    assert!(
        result
            .error
            .unwrap()
            .message
            .contains("provider settings changed")
    );
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
    assert!(
        fs::read(&fixture.providers)
            .unwrap()
            .ends_with(b"# concurrent private owner edit\n")
    );
}

#[cfg(unix)]
#[test]
fn command_credentials_in_interactive_collection_and_offline_publication_do_not_run() {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = fixture();
    let program = fixture
        .wiki
        .path
        .parent()
        .unwrap()
        .join("credential program");
    let sentinel = program.with_file_name("unused helper sentinel");
    fs::write(
        &program,
        format!("#!/bin/sh\ntouch '{}'\nprintf secret\n", sentinel.display()),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let input = format!(
        "credentials\ncommand\n'{}' auth 'two words' '$HOME' '|'\njson\n",
        program.display()
    );
    let settings = choices(ProviderRoleArgument::Generation);
    let mut prompts = Prompts {
        input: io::Cursor::new(input.into_bytes()),
        output: Vec::new(),
    };
    let mut chosen = collect(&settings, true, &mut prompts).unwrap();
    chosen.grant_provider = true;
    assert_eq!(chosen.credential_output, Some(CredentialOutput::Json));
    assert!(!sentinel.exists());
    fixture.args.offline = true;
    fixture.args.dry_run = true;
    fixture.args.json = true;
    let preview = execute(&fixture.args, &chosen).unwrap();
    assert!(preview.ok);
    assert!(!sentinel.exists());
    assert!(
        !serde_json::to_string(&preview)
            .unwrap()
            .contains("two words")
    );
    fixture.args.dry_run = false;
    assert!(execute(&fixture.args, &chosen).unwrap().ok);
    assert!(!sentinel.exists());
    let document = read(&fixture.providers);
    let profile = selected_profile(&fixture);
    let service = document["profiles"][profile.as_str()]["generation"]
        .as_str()
        .unwrap();
    let auth = &document["services"][service]["auth"];
    assert_eq!(
        auth["command"].as_array().unwrap()[0].as_str(),
        Some(program.to_str().unwrap())
    );
    assert_eq!(
        auth["command"].as_array().unwrap()[2].as_str(),
        Some("two words")
    );
    assert_eq!(
        auth["command"].as_array().unwrap()[3].as_str(),
        Some("$HOME")
    );
    assert_eq!(auth["command"].as_array().unwrap()[4].as_str(), Some("|"));
}

#[test]
fn completion_inspection_keeps_explicit_other_wiki_and_spaced_preferences() {
    let mut fixture = fixture();
    let preferences = fixture
        .preferences
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("saved settings/config file.toml");
    fs::create_dir(preferences.parent().unwrap()).unwrap();
    fs::rename(&fixture.preferences, &preferences).unwrap();
    fixture.args.preferences = Some(preferences.clone());
    fixture.args.wiki = Some(fixture.other.path.clone());
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.max_output_tokens = Some(8192);
    settings.grant_provider = true;
    let result = execute(&fixture.args, &settings).unwrap();
    assert!(result.ok, "{:?}", result.error);
    let next_step = result.data["next_step"].as_str().unwrap();
    assert_eq!(
        next_step,
        format!(
            "Run lwiki --wiki '{}' --preferences '{}' config show to inspect both models.",
            fixture.other.path.display(),
            preferences.display()
        )
    );
    let mut human = Vec::new();
    present(&result, &mut human).unwrap();
    assert!(
        String::from_utf8(human)
            .unwrap()
            .lines()
            .any(|line| line == next_step)
    );
    let command = next_step
        .strip_prefix("Run ")
        .unwrap()
        .strip_suffix(" to inspect both models.")
        .unwrap();
    let continuation = Arguments::try_parse_from(shlex::split(command).unwrap()).unwrap();
    let inspected = super::super::config_show::execute(&continuation).unwrap();
    assert_eq!(
        inspected.data["selected_wiki"]["path"],
        json!(fixture.other.path)
    );
    assert_eq!(
        inspected.data["provider"]["generation"]["effective_output_token_limit"],
        8192
    );
    let mut default_args = continuation;
    default_args.wiki = Some(fixture.wiki.path.clone());
    let original = super::super::config_show::execute(&default_args).unwrap();
    assert_eq!(
        original.data["provider"]["generation"]["effective_output_token_limit"],
        4096
    );
    for secret in [
        "private-endpoint",
        "private-tenant",
        "private-generation",
        "private-header-marker",
        "LWIKI_UNUSED_TEST_KEY",
    ] {
        assert!(!next_step.contains(secret));
    }
}

#[test]
fn explicit_file_with_same_profile_is_saved_only_for_target_wiki_and_replays_as_noop() {
    let mut fixture = fixture();
    let b_providers = fixture.providers.with_file_name("B providers.toml");
    fs::copy(&fixture.providers, &b_providers).unwrap();
    fixture.args.wiki = Some(fixture.other.path.clone());
    let before_a = fs::read(&fixture.providers).unwrap();
    let loaded = config::local::load_user_config(Some(&fixture.preferences), false).unwrap();
    let before_a_preferences =
        serde_json::to_value(&loaded.document().vaults[&fixture.wiki.wiki_id]).unwrap();
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.providers_config = Some(b_providers.clone());
    settings.max_output_tokens = Some(4096);
    let result = execute(&fixture.args, &settings).unwrap();
    assert!(result.ok, "{:?}", result.error);
    let loaded = config::local::load_user_config(Some(&fixture.preferences), false).unwrap();
    assert_eq!(
        loaded.document().providers_config.as_ref(),
        Some(&fixture.providers)
    );
    assert_eq!(
        serde_json::to_value(&loaded.document().vaults[&fixture.wiki.wiki_id]).unwrap(),
        before_a_preferences
    );
    assert_eq!(
        loaded.document().vaults[&fixture.other.wiki_id]
            .profile
            .as_deref(),
        Some("manual")
    );
    assert_eq!(
        loaded.document().vaults[&fixture.other.wiki_id]
            .providers_config
            .as_ref(),
        Some(&b_providers)
    );
    let preferences = fs::read(&fixture.preferences).unwrap();
    let preferences_time = fs::metadata(&fixture.preferences)
        .unwrap()
        .modified()
        .unwrap();
    let providers_time = fs::metadata(&b_providers).unwrap().modified().unwrap();
    for explicit in [true, false] {
        if !explicit {
            settings.providers_config = None;
        }
        assert!(execute(&fixture.args, &settings).unwrap().ok);
        assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
        assert_eq!(
            fs::metadata(&fixture.preferences)
                .unwrap()
                .modified()
                .unwrap(),
            preferences_time
        );
        assert_eq!(
            fs::metadata(&b_providers).unwrap().modified().unwrap(),
            providers_time
        );
    }
    assert_eq!(fs::read(&fixture.providers).unwrap(), before_a);
    assert_eq!(fs::read(&b_providers).unwrap(), before_a);
}

#[test]
fn scoped_file_partial_publication_keeps_old_selection_and_retry_saves_complete_pair() {
    let mut fixture = fixture();
    let b_providers = fixture.providers.with_file_name("B providers.toml");
    fs::copy(&fixture.providers, &b_providers).unwrap();
    fixture.args.wiki = Some(fixture.other.path.clone());
    let before = fs::read(&fixture.preferences).unwrap();
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.providers_config = Some(b_providers.clone());
    settings.max_output_tokens = Some(8192);
    settings.grant_provider = true;
    CHECKPOINT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|stage, _| {
            if stage == "provider_complete" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected second-publication failure",
                ))
            } else {
                Ok(())
            }
        }))
    });
    let result = execute(&fixture.args, &settings);
    CHECKPOINT.with(|hook| *hook.borrow_mut() = None);
    let result = result.unwrap();
    assert!(!result.ok && result.meta.partial);
    assert_eq!(fs::read(&fixture.preferences).unwrap(), before);
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    let published = fs::read(&b_providers).unwrap();
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    assert_eq!(fs::read(&b_providers).unwrap(), published);
    let loaded = config::local::load_user_config(Some(&fixture.preferences), false).unwrap();
    assert_eq!(
        loaded.document().providers_config.as_ref(),
        Some(&fixture.providers)
    );
    assert_eq!(
        loaded.document().vaults[&fixture.other.wiki_id]
            .providers_config
            .as_ref(),
        Some(&b_providers)
    );
    assert_ne!(
        loaded.document().vaults[&fixture.other.wiki_id]
            .profile
            .as_deref(),
        Some("manual")
    );
    assert_eq!(
        loaded.document().vaults[&fixture.wiki.wiki_id]
            .profile
            .as_deref(),
        Some("manual")
    );
}

#[test]
fn scoped_provider_preview_refusal_and_wiki_replacement_preserve_both_saved_selections() {
    let mut fixture = fixture();
    let b_providers = fixture.providers.with_file_name("B providers.toml");
    fs::copy(&fixture.providers, &b_providers).unwrap();
    fixture.args.wiki = Some(fixture.other.path.clone());
    let preferences = fs::read(&fixture.preferences).unwrap();
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.providers_config = Some(b_providers.clone());
    settings.max_output_tokens = Some(8192);
    fixture.args.dry_run = true;
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    fixture.args.dry_run = false;
    assert!(execute(&fixture.args, &settings).is_err());
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    assert_eq!(fs::read(&b_providers).unwrap(), fixture.original);
    settings.grant_provider = true;
    let marker = fixture.other.path.join("WIKI.md");
    let before = fs::read_to_string(&marker).unwrap();
    let changed = before.replace(fixture.other.wiki_id.as_str(), "Vault.Replaced");
    CHECKPOINT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |stage, _| {
            if stage == "provider_complete" {
                fs::write(&marker, &changed).unwrap();
            }
            Ok(())
        }))
    });
    let result = execute(&fixture.args, &settings);
    CHECKPOINT.with(|hook| *hook.borrow_mut() = None);
    let result = result.unwrap();
    assert!(!result.ok && result.meta.partial);
    assert_eq!(result.error.unwrap().code, "CONTENT_CONFLICT");
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
}

#[test]
fn explicit_already_effective_global_file_does_not_add_a_redundant_wiki_path() {
    let fixture = fixture();
    let preferences = fs::read(&fixture.preferences).unwrap();
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.providers_config = Some(fixture.providers.clone());
    settings.max_output_tokens = Some(4096);
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    assert_eq!(fs::read(&fixture.preferences).unwrap(), preferences);
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    let loaded = config::local::load_user_config(Some(&fixture.preferences), false).unwrap();
    assert!(
        loaded.document().vaults[&fixture.wiki.wiki_id]
            .providers_config
            .is_none()
    );
}

#[test]
fn reasoning_edit_preview_save_show_rerun_and_default_are_scoped() {
    let mut fixture = fixture();
    let mut settings = choices(ProviderRoleArgument::Generation);
    settings.reasoning_effort = Some("low".into());
    fixture.args.dry_run = true;
    let preview = execute(&fixture.args, &settings).unwrap();
    assert!(preview.ok, "{:?}", preview.error);
    assert_eq!(
        preview.data["providers"]["generation"]["reasoning_effort"],
        "low"
    );
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    fixture.args.dry_run = false;
    assert_eq!(
        execute(&fixture.args, &settings).unwrap_err().code,
        ErrorCode::Usage
    );
    assert_eq!(fs::read(&fixture.providers).unwrap(), fixture.original);
    settings.grant_provider = true;
    let saved = execute(&fixture.args, &settings).unwrap();
    assert!(saved.ok, "{:?}", saved.error);
    assert!(!saved.meta.network_used);
    assert_eq!(
        saved.data["providers"]["embedding"]["model"],
        "old-embedding"
    );
    let show = super::super::config_show::execute(&fixture.args).unwrap();
    assert_eq!(
        show.data["provider"]["generation"]["reasoning_effort"],
        "low"
    );
    let setup = super::super::setup::SetupArguments {
        path: Some(fixture.wiki.path.clone()),
        non_interactive: true,
        ..Default::default()
    };
    let rerun = super::super::setup::execute(&fixture.args, &setup).unwrap();
    assert!(rerun.ok);
    assert_eq!(rerun.data["plan"]["generation"]["reasoning_effort"], "low");
    settings.reasoning_effort = Some("none".into());
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    settings.reasoning_effort = Some("default".into());
    assert!(execute(&fixture.args, &settings).unwrap().ok);
    let document = read(&fixture.providers);
    let profile = selected_profile(&fixture);
    let service = document["profiles"][profile.as_str()]["generation"]
        .as_str()
        .unwrap();
    assert!(
        !document["services"][service]
            .as_table()
            .unwrap()
            .contains_key("reasoning_effort")
    );
    settings.role = ProviderRoleArgument::Embedding;
    assert!(patch(&settings).is_err());
}

#[test]
fn interactive_generation_effort_default_is_not_explicit_none() {
    for (value, expected) in [
        ("default", None),
        (
            "none",
            Some(crate::config::providers::ReasoningEffort::None),
        ),
        (
            "high",
            Some(crate::config::providers::ReasoningEffort::High),
        ),
    ] {
        let settings = choices(ProviderRoleArgument::Generation);
        let mut prompts = Prompts {
            input: io::Cursor::new(format!("reasoning-effort\n{value}\n").into_bytes()),
            output: Vec::new(),
        };
        let result = collect(&settings, true, &mut prompts).unwrap();
        assert_eq!(patch(&result).unwrap().reasoning_effort, Some(expected));
    }
}

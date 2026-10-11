use super::*;
use clap::Parser;
use std::fs;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    (temp, base)
}
fn arguments(extra: &[&str]) -> super::super::Arguments {
    let mut argv = vec!["lwiki"];
    argv.extend_from_slice(extra);
    argv.extend_from_slice(&["config", "show"]);
    super::super::Arguments::try_parse_from(argv).unwrap()
}
fn loaded(base: &Path, document: &config::LocalConfigDocument) -> config::local::LoadedUserConfig {
    let path = base.join("config.toml");
    fs::write(&path, toml::to_string(document).unwrap()).unwrap();
    config::local::load_user_config(Some(&path), false).unwrap()
}
fn wiki(base: &Path) -> config::DefaultWiki {
    let path = base.join("My Wiki 猫");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: Vault.Inspection\nwiki_kind: vault\ntitle: My notes\n---\n").unwrap();
    crate::vault::discovery::default_from_path(&path).unwrap()
}

#[test]
fn inspection_resolves_every_default_with_runtime_values_and_origins() {
    let document = config::LocalConfigDocument::default();
    let settings = effective_settings(&document, None, true).unwrap();
    let embedding = super::super::embeddings::EmbeddingSettingsArguments::default()
        .settings(&document.embeddings);
    let remote = super::super::remote::RemoteArguments::default();
    let limits = remote.limits().unwrap();
    assert_eq!(
        settings["embeddings"]["values"],
        serde_json::to_value(embedding).unwrap()
    );
    assert_eq!(
        settings["remote"]["values"]["max_requests"],
        limits.requests
    );
    assert_eq!(
        settings["remote"]["values"]["concurrency"],
        limits.concurrency
    );
    assert_eq!(
        settings["remote"]["values"]["attempts_per_task"],
        limits.attempts_per_task
    );
    assert_eq!(
        settings["remote"]["values"]["deadline_ms"],
        crate::jobs::DEFAULT_DEADLINE_MS
    );
    assert_eq!(
        settings["remote"]["values"]["currency"],
        remote.currency_name()
    );
    for section in ["embeddings", "remote"] {
        let values = settings[section]["values"].as_object().unwrap();
        let origins = settings[section]["origins"].as_object().unwrap();
        assert_eq!(values.len(), origins.len());
        for key in values.keys() {
            assert_eq!(origins[key], "built_in");
        }
    }
}

#[test]
fn inspection_preserves_explicit_empty_prefix_zero_units_and_configured_currency() {
    let mut document = config::LocalConfigDocument::default();
    document.embeddings.document_prefix = Some(String::new());
    document.embeddings.query_prefix = Some("query: ".into());
    document.embeddings.max_input_bytes = Some(24000);
    document.embeddings.quality_target_bytes = Some(6000);
    document.remote.max_requests = Some(11);
    document.remote.max_input_units = Some(0);
    document.remote.max_output_units = Some(123);
    document.remote.max_cost = Some("0.12".into());
    document.remote.currency = Some("EUR".into());
    document.remote.deadline_ms = Some(10000);
    document.remote.requests_per_minute = Some(3);
    let settings = effective_settings(&document, Some("saved"), true).unwrap();
    assert_eq!(settings["embeddings"]["values"]["document_prefix"], "");
    assert_eq!(
        settings["embeddings"]["origins"]["document_prefix"],
        "user_config"
    );
    assert_eq!(settings["remote"]["values"]["max_input_units"], 0);
    assert_eq!(
        settings["remote"]["origins"]["max_input_units"],
        "user_config"
    );
    assert_eq!(settings["remote"]["values"]["currency"], "EUR");
    assert_eq!(settings["remote"]["values"]["max_cost"], "0.12");
    assert_eq!(
        settings["remote"]["values"]["concurrency"],
        super::super::remote::RemoteArguments::default()
            .limits()
            .unwrap()
            .concurrency
    );
    assert_eq!(settings["remote"]["origins"]["concurrency"], "built_in");
}

#[test]
fn inspection_uses_ambient_provider_path_and_selected_wiki_profile_without_credentials() {
    let (_temp, base) = fixture();
    let selected = wiki(&base);
    let provider_path = base.join("standard-provider.toml");
    let missing_key = base.join("never-created-secret.key");
    let candidate = config::provider_setup::build_embedding_document(
        None,
        &config::provider_setup::EmbeddingSetup {
            model: "test-model".into(),
            url: "https://example.invalid/embeddings".into(),
            dimensions: Some("auto".into()),
            auth: config::provider_setup::SetupAuth::PrivateFile(missing_key.clone()),
            grant: true,
        },
        &selected,
    )
    .unwrap();
    let snapshot = config::setup_io::load_private_file(&provider_path, 65536).unwrap();
    config::setup_io::write_private_file(&snapshot, &candidate.bytes, false, false, 5000).unwrap();
    let mut document = config::LocalConfigDocument {
        default_wiki: Some(selected.clone()),
        ..Default::default()
    };
    document.defaults.profile = Some("other-profile".into());
    document.vaults.insert(
        selected.wiki_id.clone(),
        config::LocalPreferences {
            profile: Some(candidate.profile.clone()),
            ..Default::default()
        },
    );
    let config = loaded(&base, &document);
    let envelope = inspect_loaded(
        &arguments(&[]),
        &config,
        &base,
        || Ok(provider_path.clone()),
    )
    .unwrap();
    assert_eq!(
        envelope.data["provider_configuration"]["path"],
        json!(provider_path)
    );
    assert_eq!(envelope.data["provider_configuration"]["origin"], "ambient");
    assert_eq!(envelope.data["selected_profile"], candidate.profile);
    assert_eq!(envelope.data["selected_profile_origin"], "user_vault");
    assert_eq!(
        envelope.data["provider"]["embedding"]["model"],
        "test-model"
    );
    assert_eq!(
        envelope.data["provider"]["embedding"]["auth_source"],
        "private_file"
    );
    assert_eq!(
        envelope.data["provider"]["embedding"]["destination"],
        json!({"scheme":"https","host":"example.invalid","port":443})
    );
    assert_eq!(
        envelope.data["provider"]["embedding"]["adapter"],
        "embeddings-v1"
    );
    assert!(envelope.data["provider"]["embedding"]["effective_output_token_limit"].is_null());
    assert!(envelope.data["provider"]["embedding"]["command_auth"].is_null());
    assert_eq!(
        envelope.data["provider"]["embedding"]["permitted_for_selected_wiki"],
        true
    );
    assert_eq!(envelope.data["credentials_resolved"], false);
    assert_eq!(envelope.data["network_used"], false);
    assert!(!missing_key.exists());
    let mut output = Vec::new();
    present(&envelope, &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("standard provider location"));
    assert!(text.contains("test-model"));
    assert!(text.contains("private key file"));
    assert!(text.contains("Destination: https://example.invalid"));
    assert!(text.contains("Use with selected wiki: Allowed"));
}

#[test]
fn inspection_disabled_config_never_consults_ambient_provider_location() {
    let (_temp, base) = fixture();
    let config = config::local::load_user_config(None, true).unwrap();
    let envelope = inspect_loaded(&arguments(&["--no-user-config"]), &config, &base, || {
        panic!("disabled configuration must not inspect ambient providers")
    })
    .unwrap();
    assert_eq!(
        envelope.data["provider_configuration"]["origin"],
        "disabled"
    );
    assert!(envelope.data["provider_configuration"]["path"].is_null());
    assert!(envelope.data["selected_profile"].is_null());
    assert_eq!(
        envelope.data["effective_embeddings"]["origins"]["max_input_bytes"],
        "built_in"
    );
    assert!(envelope.data["provider"].is_null());
}

#[test]
fn explicit_config_still_applies_with_disabled_ambient_discovery_and_does_not_show_secrets() {
    let (_temp, base) = fixture();
    let selected = wiki(&base);
    let provider_path = base.join("private-provider.toml");
    let candidate = config::provider_setup::build_embedding_document(
        None,
        &config::provider_setup::EmbeddingSetup {
            model: "a-model".into(),
            url: "https://example.invalid/embeddings".into(),
            dimensions: None,
            auth: config::provider_setup::SetupAuth::Environment("UNUSED_KEY".into()),
            grant: false,
        },
        &selected,
    )
    .unwrap();
    let private = String::from_utf8(candidate.bytes).unwrap().replace(
        "key_env = \"UNUSED_KEY\"",
        "key = \"synthetic-secret-never-display\"",
    );
    assert!(private.contains("synthetic-secret-never-display"));
    let snapshot = config::setup_io::load_private_file(&provider_path, 65536).unwrap();
    config::setup_io::write_private_file(&snapshot, private.as_bytes(), false, false, 5000)
        .unwrap();
    let mut document = config::LocalConfigDocument {
        default_wiki: Some(selected),
        providers_config: Some(provider_path.clone()),
        ..Default::default()
    };
    document.defaults.profile = Some("different-profile".into());
    let config = loaded(&base, &document);
    let args = arguments(&["--no-user-config", "--profile", &candidate.profile]);
    let envelope = inspect_loaded(&args, &config, &base, || {
        panic!("explicit provider path must win")
    })
    .unwrap();
    assert_eq!(
        envelope.data["provider_configuration"]["origin"],
        "user_config"
    );
    assert_eq!(envelope.data["selected_profile_origin"], "command_line");
    assert_eq!(
        envelope.data["provider"]["embedding"]["auth_source"],
        "private_literal"
    );
    assert_eq!(
        envelope.data["provider"]["embedding"]["permitted_for_selected_wiki"],
        false
    );
    let mut output = Vec::new();
    present(&envelope, &mut output).unwrap();
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("synthetic-secret-never-display")
    );
    assert!(
        !serde_json::to_string(&envelope)
            .unwrap()
            .contains("synthetic-secret-never-display")
    );
}

#[test]
fn missing_ambient_provider_file_is_actionable_without_hiding_local_settings() {
    let (_temp, base) = fixture();
    let document = config::LocalConfigDocument::default();
    let config = loaded(&base, &document);
    let provider_path = base.join("not-created/providers.toml");
    let envelope = inspect_loaded(
        &arguments(&[]),
        &config,
        &base,
        || Ok(provider_path.clone()),
    )
    .unwrap();
    assert_eq!(envelope.data["provider_configuration"]["present"], false);
    assert_eq!(
        envelope.data["provider_configuration"]["path"],
        json!(provider_path)
    );
    let mut output = Vec::new();
    present(&envelope, &mut output).unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("Provider settings file is missing. Run lwiki setup to add a model."));
    assert!(text.contains("Maximum bytes per input: 12000 (built-in default)"));
    assert!(text.contains("Maximum request attempts: 60 (built-in default)"));
}

#[test]
fn inspection_reports_destinations_and_command_policy_without_executing_or_exposing_auth() {
    for output_limit in [None, Some(8192)] {
        let (_temp, base) = fixture();
        let selected = wiki(&base);
        let provider_path = base.join("private-provider.toml");
        let sentinel = base.join("helper-must-not-run");
        let helper = base.join("private credential helper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let quoted = sentinel.to_string_lossy().replace('\'', "'\\''");
            fs::write(&helper, format!("#!/bin/sh\n: > '{quoted}'\nexit 47\n")).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let limit = output_limit
            .map(|limit| format!("max_output_tokens = {limit}\n"))
            .unwrap_or_default();
        let private = format!(
            r#"version = 1
[profiles.notes]
embedding = "embed"
generation = "generate"
[services.embed]
adapter = "embeddings-v1"
url = "https://embedding.example.invalid:8443/private-deployment?tenant=synthetic-query-never-display"
model = "embedding-model"
[services.embed.headers]
X-Private = "synthetic-header-never-display"
[services.embed.auth]
kind = "static"
key = "synthetic-key-never-display"
[services.generate]
adapter = "responses-v1"
url = "https://[2001:db8::1]/private-output?tenant=synthetic-second-query"
model = "generation-model"
{limit}[services.generate.auth]
kind = "command"
command = [{helper}, "synthetic-private-argument"]
output = "json"
ttl_seconds = 240
refresh_skew_seconds = 30
[vault_bindings.notes]
root = {root}
wiki_id = "Vault.Inspection"
allowed_profiles = ["notes"]
"#,
            helper = toml::Value::String(helper.to_str().unwrap().into()),
            root = toml::Value::String(selected.path.to_str().unwrap().into()),
        );
        let snapshot = config::setup_io::load_private_file(&provider_path, 65536).unwrap();
        config::setup_io::write_private_file(&snapshot, private.as_bytes(), false, false, 5000)
            .unwrap();
        let mut document = config::LocalConfigDocument {
            default_wiki: Some(selected),
            providers_config: Some(provider_path.clone()),
            ..Default::default()
        };
        document.defaults.profile = Some("notes".into());
        let config = loaded(&base, &document);
        for extra in [vec![], vec!["--offline"], vec!["--dry-run"]] {
            let envelope = inspect_loaded(&arguments(&extra), &config, &base, || {
                panic!("explicit private provider path must win")
            })
            .unwrap();
            let embedding = &envelope.data["provider"]["embedding"];
            let generation = &envelope.data["provider"]["generation"];
            assert_eq!(
                embedding["destination"],
                json!({"scheme":"https","host":"embedding.example.invalid","port":8443})
            );
            assert_eq!(
                generation["destination"],
                json!({"scheme":"https","host":"[2001:db8::1]","port":443})
            );
            assert_eq!(generation["adapter"], "responses-v1");
            assert_eq!(
                generation["effective_output_token_limit"],
                output_limit.unwrap_or(4096)
            );
            assert_eq!(
                generation["command_auth"],
                json!({"output":"json","ttl_seconds":240,"refresh_skew_seconds":30})
            );
            assert_eq!(generation["permitted_for_selected_wiki"], true);
            assert_eq!(envelope.data["credentials_resolved"], false);
            assert_eq!(envelope.data["network_used"], false);
            assert!(!envelope.meta.network_used);
            let mut output = Vec::new();
            present(&envelope, &mut output).unwrap();
            let human = String::from_utf8(output).unwrap();
            assert!(human.contains("Destination: https://embedding.example.invalid:8443"));
            assert!(human.contains("Destination: https://[2001:db8::1]"));
            assert!(human.contains(&format!(
                "Service output token limit: {}",
                output_limit.unwrap_or(4096)
            )));
            assert!(human.contains("Credential output: json; reuse for at most 240 seconds"));
            for secret in [
                "private-deployment",
                "synthetic-query-never-display",
                "synthetic-second-query",
                "synthetic-key-never-display",
                "synthetic-header-never-display",
                "private credential helper",
                "synthetic-private-argument",
            ] {
                assert!(!human.contains(secret), "human output exposed {secret}");
                assert!(
                    !serde_json::to_string(&envelope).unwrap().contains(secret),
                    "JSON exposed {secret}"
                );
            }
            assert!(!sentinel.exists());
            assert_eq!(fs::read(&provider_path).unwrap(), private.as_bytes());
        }
    }
}

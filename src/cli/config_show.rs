//! Inspect one configuration snapshot without resolving credentials.
use crate::{config, domain::*, output::Envelope, vault::VaultFs};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn origin_label(origin: &str) -> &str {
    match origin {
        "built_in" => "built-in default",
        "user_config" | "user_defaults" => "configuration file",
        "user_vault" => "settings saved for this wiki",
        "portable_wiki" => "wiki settings",
        "command_line" => "command line",
        "ambient" => "standard provider location",
        "disabled" => "user configuration disabled",
        other => other,
    }
}
fn display_value(value: &Value) -> String {
    if value.is_null() {
        "Not set".into()
    } else if let Some(text) = value.as_str() {
        // Prefixes may contain newlines or terminal control characters.
        json!(text).to_string()
    } else {
        value.to_string()
    }
}
pub(super) fn destination_label(destination: &Value) -> String {
    let (Some(scheme), Some(host)) = (destination["scheme"].as_str(), destination["host"].as_str())
    else {
        return "Not set".into();
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = destination["port"].as_u64();
    if port.is_some_and(|port| !matches!((scheme, port), ("https", 443) | ("http", 80))) {
        format!("{scheme}://{host}:{}", port.expect("checked port"))
    } else {
        format!("{scheme}://{host}")
    }
}
fn present_setting(
    output: &mut impl std::io::Write,
    label: &str,
    value: &Value,
    origin: &Value,
) -> std::io::Result<()> {
    writeln!(
        output,
        "  {label}: {} ({})",
        display_value(value),
        origin_label(origin.as_str().unwrap_or("built_in"))
    )
}

pub(super) fn present(
    envelope: &Envelope,
    output: &mut impl std::io::Write,
) -> std::io::Result<()> {
    let data = &envelope.data;
    let text = |value: &Value| value.as_str().unwrap_or("Not set").to_owned();
    writeln!(
        output,
        "Configuration: {}",
        text(&data["configuration"]["path"])
    )?;
    if let Some(warnings) = data["configuration"]["warnings"].as_array() {
        for warning in warnings.iter().filter_map(Value::as_str) {
            writeln!(output, "{warning}")?;
        }
    }
    writeln!(
        output,
        "Default wiki: {}",
        text(&data["settings"]["default_wiki"]["path"])
    )?;
    writeln!(
        output,
        "Selected wiki: {}",
        text(&data["selected_wiki"]["path"])
    )?;
    if let Some(message) = data["selection_error"]["message"].as_str() {
        writeln!(output, "{message}")?;
    }
    writeln!(
        output,
        "Provider settings: {} ({})",
        text(&data["provider_configuration"]["path"]),
        origin_label(
            data["provider_configuration"]["origin"]
                .as_str()
                .unwrap_or("built_in")
        )
    )?;
    if data["provider_configuration"]["present"] == false {
        let optional_ambient = data["provider_configuration"]["origin"] == "ambient"
            && data["selected_profile"].is_null();
        writeln!(
            output,
            "{}",
            if optional_ambient {
                "No API model configured. Capture and word search work without one."
            } else {
                "Provider settings file is missing. Restore the selected file or run lwiki setup to choose a model."
            }
        )?;
    }
    if let Some(message) = data["provider_configuration"]["location_error"]["message"].as_str() {
        writeln!(output, "{message}")?;
    }
    writeln!(
        output,
        "Selected profile: {} ({})",
        text(&data["selected_profile"]),
        origin_label(
            data["selected_profile_origin"]
                .as_str()
                .unwrap_or("built_in")
        )
    )?;
    for (key, label) in [
        ("embedding", "Embedding model"),
        ("generation", "Generation model"),
    ] {
        if data["provider"][key].is_object() {
            writeln!(output, "{label}: {}", text(&data["provider"][key]["model"]))?;
            writeln!(
                output,
                "  Destination: {}",
                destination_label(&data["provider"][key]["destination"])
            )?;
            writeln!(
                output,
                "  Service: {}",
                text(&data["provider"][key]["service"])
            )?;
            writeln!(
                output,
                "  API format: {}",
                text(&data["provider"][key]["adapter"])
            )?;
            if key == "embedding" {
                let configured = &data["provider"][key]["dimensions"];
                let dimensions = if let Some(count) = configured.as_u64() {
                    count.to_string()
                } else {
                    configured.as_str().unwrap_or("provider default").to_owned()
                };
                writeln!(output, "  Configured vector dimensions: {dimensions}")?;
                writeln!(
                    output,
                    "  Automatic similarity search: {}",
                    super::automatic_embeddings::consent_label(
                        &data["provider"][key]["automatic_embedding_consent"]
                    )
                )?;
            }
            if key == "generation" {
                writeln!(
                    output,
                    "  Reasoning effort: {}",
                    data["provider"][key]["reasoning_effort"]
                        .as_str()
                        .unwrap_or("Provider default")
                )?;
            }
            if let Some(limit) = data["provider"][key]["effective_output_token_limit"].as_u64() {
                writeln!(output, "  Service output token limit: {limit}")?;
            }
            writeln!(
                output,
                "  Authentication: {}",
                match data["provider"][key]["auth_source"].as_str() {
                    Some("private_file") => "private key file",
                    Some("private_literal") => "key stored in private provider settings",
                    Some("environment") => "environment variable",
                    Some("command") => "credential command",
                    _ => "Not set",
                }
            )?;
            if let Some(command) = data["provider"][key]["command_auth"].as_object() {
                writeln!(
                    output,
                    "  Credential output: {}; reuse for at most {} seconds; refresh {} seconds before expiry",
                    command["output"].as_str().unwrap_or("Not set"),
                    command["ttl_seconds"],
                    command["refresh_skew_seconds"],
                )?;
            }
            let permission = match data["provider"][key]["permitted_for_selected_wiki"].as_bool() {
                Some(true) => "Allowed",
                Some(false) => "Not allowed",
                None => "Choose a wiki to check",
            };
            writeln!(output, "  Use with selected wiki: {permission}")?;
        }
    }
    if data["effective_preferences"].is_object() {
        let prefs = &data["effective_preferences"];
        writeln!(output, "Wiki options:")?;
        for (key, label) in [
            ("offline", "Offline"),
            ("read_max_bytes", "Maximum bytes to read"),
            ("lock_timeout_ms", "Writer wait in milliseconds"),
        ] {
            present_setting(output, label, &prefs[key], &prefs["origins"][key])?;
        }
    }
    writeln!(output, "Embedding options:")?;
    for (key, label) in [
        ("document_prefix", "Document prefix"),
        ("query_prefix", "Query prefix"),
        ("max_input_bytes", "Maximum bytes per input"),
        ("quality_target_bytes", "Preferred segment bytes"),
    ] {
        present_setting(
            output,
            label,
            &data["effective_embeddings"]["values"][key],
            &data["effective_embeddings"]["origins"][key],
        )?;
    }
    writeln!(output, "Limits for new remote jobs:")?;
    for (key, label) in [
        ("max_requests", "Maximum request attempts"),
        ("concurrency", "Simultaneous requests"),
        ("attempts_per_task", "Attempts per task"),
        ("deadline_ms", "Deadline in milliseconds"),
        ("max_request_bytes", "Maximum bytes sent"),
        ("max_response_bytes", "Maximum bytes received"),
        ("max_input_units", "Maximum input units"),
        ("max_output_units", "Maximum output units"),
        ("max_cost", "Maximum cost"),
        ("currency", "Currency"),
        ("requests_per_minute", "Requests per minute"),
        ("tokens_per_minute", "Tokens per minute"),
    ] {
        present_setting(
            output,
            label,
            &data["effective_remote"]["values"][key],
            &data["effective_remote"]["origins"][key],
        )?;
    }
    writeln!(
        output,
        "Saved limits apply to new jobs. Existing jobs keep their saved limits."
    )?;
    writeln!(
        output,
        "Run lwiki setup to change your setup, or edit the configuration file."
    )?;
    writeln!(
        output,
        "Use lwiki --json config show to see all settings and where they came from."
    )
}

fn configured_origin(present: bool) -> &'static str {
    if present { "user_config" } else { "built_in" }
}
fn effective_settings(
    document: &config::LocalConfigDocument,
    profile: Option<&str>,
    wiki_id: Option<&RecordId>,
    allow_ambient_provider: bool,
) -> Result<Value> {
    let embeddings =
        super::embeddings::EmbeddingSettingsArguments::default().settings(&document.embeddings);
    embeddings.validate()?;
    let context = super::remote::RemoteContext {
        profile,
        wiki_id,
        document,
        allow_ambient_provider,
    };
    let remote = super::remote::RemoteArguments::default().effective(&context);
    let limits = remote.limits()?;
    let embedding_origins: serde_json::Map<String, Value> = [
        (
            "document_prefix",
            document.embeddings.document_prefix.is_some(),
        ),
        ("query_prefix", document.embeddings.query_prefix.is_some()),
        (
            "max_input_bytes",
            document.embeddings.max_input_bytes.is_some(),
        ),
        (
            "quality_target_bytes",
            document.embeddings.quality_target_bytes.is_some(),
        ),
    ]
    .into_iter()
    .map(|(key, present)| (key.into(), json!(configured_origin(present))))
    .collect();
    let remote_origins: serde_json::Map<String, Value> = [
        ("max_requests", document.remote.max_requests.is_some()),
        ("concurrency", document.remote.concurrency.is_some()),
        (
            "attempts_per_task",
            document.remote.attempts_per_task.is_some(),
        ),
        ("deadline_ms", document.remote.deadline_ms.is_some()),
        (
            "max_request_bytes",
            document.remote.max_request_bytes.is_some(),
        ),
        (
            "max_response_bytes",
            document.remote.max_response_bytes.is_some(),
        ),
        ("max_input_units", document.remote.max_input_units.is_some()),
        (
            "max_output_units",
            document.remote.max_output_units.is_some(),
        ),
        ("max_cost", document.remote.max_cost.is_some()),
        ("currency", document.remote.currency.is_some()),
        (
            "requests_per_minute",
            document.remote.requests_per_minute.is_some(),
        ),
        (
            "tokens_per_minute",
            document.remote.tokens_per_minute.is_some(),
        ),
    ]
    .into_iter()
    .map(|(key, present)| (key.into(), json!(configured_origin(present))))
    .collect();
    Ok(json!({
        "embeddings":{"values":embeddings,"origins":embedding_origins},
        "remote":{"values":{
            "max_requests":limits.requests, "concurrency":limits.concurrency, "attempts_per_task":limits.attempts_per_task,
            "deadline_ms":remote.deadline_ms.unwrap_or(crate::jobs::DEFAULT_DEADLINE_MS),
            "max_request_bytes":limits.request_bytes,"max_response_bytes":limits.response_bytes,
            "max_input_units":limits.billable_units.get(&crate::jobs::BillableClass::Input),
            "max_output_units":limits.billable_units.get(&crate::jobs::BillableClass::Output),
            "max_cost":remote.max_cost,"currency":remote.currency_name(),
            "requests_per_minute":limits.requests_per_minute,"tokens_per_minute":limits.tokens_per_minute,
        },"origins":remote_origins}
    }))
}

pub(super) fn execute(args: &super::Arguments) -> Result<Envelope> {
    let loaded = config::local::load_user_config(args.preferences.as_deref(), args.no_user_config)?;
    let cwd = std::env::current_dir()
        .map_err(|error| WikiError::new(ErrorCode::Internal, error.to_string()))?;
    inspect_loaded(
        args,
        &loaded,
        &cwd,
        crate::app::remote::default_provider_path,
    )
}
fn inspect_loaded(
    args: &super::Arguments,
    loaded: &config::local::LoadedUserConfig,
    cwd: &Path,
    ambient_provider: impl FnOnce() -> Result<PathBuf>,
) -> Result<Envelope> {
    let document = loaded.document();
    let selected =
        crate::vault::discovery::select(args.wiki.as_deref(), cwd, loaded.default_wiki());
    let mut envelope = Envelope::success(
        "config show",
        json!({
            "configuration":loaded.summary(), "settings":document,
            "credentials_resolved":false, "network_used":false,
        }),
    );
    let mut profile = args
        .profile
        .clone()
        .or_else(|| document.defaults.profile.clone());
    let mut profile_origin = if args.profile.is_some() {
        "command_line".into()
    } else if document.defaults.profile.is_some() {
        "user_defaults".into()
    } else {
        "built_in".into()
    };
    let mut selected_fs = None;
    match selected {
        Ok(selected) => {
            crate::vault::discovery::expect_wiki_id(&selected, args.expect_wiki_id.as_ref())?;
            let fs = VaultFs::new(selected.root.clone());
            let preferences = config::local::resolve_loaded(
                &fs,
                &selected.wiki_id,
                &config::PreferenceOptions {
                    offline: args.offline,
                    profile: args.profile.clone(),
                    lock_timeout_ms: args.lock_timeout_ms,
                    ..Default::default()
                },
                loaded,
            )?;
            profile = preferences.profile.clone();
            profile_origin = preferences.origins["profile"].clone();
            envelope.data["effective_preferences"] = serde_json::to_value(preferences)
                .map_err(|error| WikiError::new(ErrorCode::Internal, error.to_string()))?;
            envelope.data["selected_wiki"] = json!({"path":selected.root.path(),"wiki_id":selected.wiki_id,"origin":selected.origin});
            envelope.meta.wiki_id = Some(selected.wiki_id.to_string());
            selected_fs = Some((fs, selected.wiki_id));
        }
        Err(error) => {
            envelope.data["selection_error"] = json!({"code":error.code,"message":error.message});
        }
    }
    let allow_ambient_provider =
        loaded.summary().origin != config::local::UserConfigOrigin::Disabled;
    let wiki_id = selected_fs.as_ref().map(|(_, id)| id);
    let settings = effective_settings(
        document,
        profile.as_deref(),
        wiki_id,
        allow_ambient_provider,
    )?;
    envelope.data["effective_embeddings"] = settings["embeddings"].clone();
    envelope.data["effective_remote"] = settings["remote"].clone();
    envelope.data["selected_profile"] = json!(profile);
    envelope.data["selected_profile_origin"] = json!(profile_origin);
    let (path, origin) = if let Some((path, origin)) = document.saved_provider_config(wiki_id) {
        (Some(path.to_path_buf()), origin)
    } else if allow_ambient_provider {
        match ambient_provider() {
            Ok(path) => (Some(path), "ambient"),
            Err(error) => {
                envelope.data["provider_configuration"]["location_error"] =
                    json!({"code":error.code,"message":error.message});
                (None, "ambient")
            }
        }
    } else {
        (None, "disabled")
    };
    envelope.data["provider_configuration"]["path"] = json!(path);
    envelope.data["provider_configuration"]["origin"] = json!(origin);
    if let Some(path) = path {
        let present = match std::fs::symlink_metadata(&path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => {
                return Err(WikiError::new(
                    ErrorCode::ConfigInvalid,
                    "provider settings could not be inspected",
                ));
            }
        };
        envelope.data["provider_configuration"]["present"] = json!(present);
        if let Some(profile) = profile.filter(|_| present) {
            let providers = config::providers::ProviderConfig::load(&path)?;
            envelope.data["provider"] = providers.profile_summary(&profile)?;
            for (name, capability) in [
                ("embedding", crate::jobs::Capability::Embed),
                ("generation", crate::jobs::Capability::Generate),
            ] {
                if envelope.data["provider"][name].is_object() {
                    envelope.data["provider"][name]["origin"] = json!("provider_configuration");
                    if let Some((fs, id)) = &selected_fs {
                        envelope.data["provider"][name]["permitted_for_selected_wiki"] =
                            json!(providers.authorize(fs, id, &profile, capability).is_ok());
                        if name == "embedding" {
                            envelope.data["provider"][name]["automatic_embedding_consent"] =
                                json!(providers.automatic_embedding_consent(fs, id, &profile)?);
                        }
                    }
                }
            }
        }
    }
    Ok(envelope)
}

#[cfg(test)]
#[path = "config_show_tests.rs"]
mod tests;

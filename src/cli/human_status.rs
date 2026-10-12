//! Concise status output and credential-free provider inspection.
use super::remote::{RemoteArguments, RemoteContext};
use crate::{app::OfflineApp, config::providers::ProviderConfig, domain::*, output::Envelope};
use serde_json::{Value, json};
use std::io::{self, Write};

/// Inspect private service metadata only. This does not authorize a request or
/// resolve credentials, and never constructs a provider runtime.
pub(super) fn provider_summary(remote: &RemoteArguments, context: &RemoteContext<'_>) -> Value {
    let mut summary = json!({"profile":context.profile,"inspection":"not_configured","credentials_resolved":false});
    let Some(profile) = context.profile else {
        return summary;
    };
    let effective = remote.effective(context);
    let path = match effective.providers_config {
        Some(path) => Ok(path),
        None if context.allow_ambient_provider => crate::app::remote::default_provider_path(),
        None => return summary,
    };
    let inspected = path
        .and_then(|path| ProviderConfig::load(&path))
        .and_then(|providers| providers.profile_summary(profile));
    match inspected {
        Ok(roles) => {
            summary["inspection"] = json!("available");
            for role in ["embedding", "generation"] {
                if let Some(service) = roles.get(role) {
                    summary[role] = service.clone();
                }
            }
        }
        Err(_) => {
            // Parser/IO detail is intentionally not copied into a safe summary.
            summary["inspection"] = json!("unavailable");
        }
    }
    summary
}

pub(super) fn attach_provider_summary(
    envelope: &mut Envelope,
    remote: &RemoteArguments,
    context: &RemoteContext<'_>,
) {
    envelope.data["provider"] = provider_summary(remote, context);
}

/// Observe the selected marker through the same bounded, contained read used
/// for selection, and refuse a replacement identity before reporting its title.
pub(super) fn wiki_title(app: &OfflineApp) -> Result<String> {
    let marker = crate::changes::prepare::read_bounded(
        app.fs(),
        &VaultRelativePath::new("WIKI.md")?,
        crate::app::MAX_INPUT_BYTES,
    )?
    .ok_or_else(|| WikiError::new(ErrorCode::VaultNotFound, "selected wiki has no WIKI.md"))?;
    let parsed = crate::records::parse_note(&marker);
    let record = parsed
        .canonical
        .as_ref()
        .filter(|record| record.kind() == RecordKind::Vault && record.id() == app.vault_id())
        .ok_or_else(|| {
            WikiError::new(
                ErrorCode::ContentConflict,
                "wiki marker changed during selection; inspect WIKI.md and retry",
            )
        })?;
    Ok(record.title().to_owned())
}

pub(super) fn present_wiki(envelope: &Envelope, output: &mut impl Write) -> io::Result<()> {
    let data = &envelope.data;
    writeln!(
        output,
        "Wiki: {}",
        data["title"].as_str().unwrap_or("Untitled wiki")
    )?;
    writeln!(
        output,
        "Folder: {}",
        data["root"].as_str().unwrap_or("Unknown")
    )?;
    let origin = match data["origin"].as_str() {
        Some("explicit") => "the --wiki option",
        Some("ancestor") => "the current folder",
        Some("default") => "your saved default",
        _ => "unknown",
    };
    writeln!(output, "Selected from: {origin}")
}

fn present_provider(provider: &Value, output: &mut impl Write) -> io::Result<()> {
    for (role, label) in [("embedding", "Embedding"), ("generation", "Generation")] {
        let service = &provider[role];
        if let Some(model) = service["model"].as_str() {
            writeln!(
                output,
                "{label} model: {model} at {} ({})",
                super::config_show::destination_label(&service["destination"]),
                service["adapter"].as_str().unwrap_or("unknown API format")
            )?;
            if role == "embedding" {
                let configured = &service["dimensions"];
                let dimensions = if let Some(count) = configured.as_u64() {
                    count.to_string()
                } else {
                    configured.as_str().unwrap_or("provider default").to_owned()
                };
                writeln!(output, "  Configured vector dimensions: {dimensions}")?;
            }
            if let Some(limit) = service["effective_output_token_limit"].as_u64() {
                writeln!(output, "  Maximum output tokens: {limit}")?;
            }
        }
    }
    if provider["inspection"] == "unavailable" {
        writeln!(
            output,
            "Provider settings are unavailable. Inspect them with lwiki config show."
        )?;
    }
    Ok(())
}

fn present_probe_usage(inspection: &Value, output: &mut impl Write) -> io::Result<()> {
    let budget = &inspection["budget"];
    match budget["dispatched_requests"].as_u64() {
        Some(count) => writeln!(output, "Requests dispatched: {count}")?,
        None => writeln!(output, "Requests dispatched: unknown")?,
    }
    let units = &budget["settled"]["billable_units"];
    let known = ["input", "cached_input", "output", "reasoning"]
        .into_iter()
        .filter_map(|class| {
            units[class]
                .as_u64()
                .map(|value| format!("{class} {value}"))
        })
        .collect::<Vec<_>>();
    writeln!(
        output,
        "Settled units: {}",
        if known.is_empty() {
            "unknown".into()
        } else {
            known.join(", ")
        }
    )?;
    if let Some(costs) = budget["known_costs"]
        .as_object()
        .filter(|costs| !costs.is_empty())
    {
        let amounts = costs
            .iter()
            .filter_map(|(currency, amount)| {
                amount.as_u64().map(|amount| {
                    format!(
                        "{currency} {}.{:09}",
                        amount / 1_000_000_000,
                        amount % 1_000_000_000
                    )
                })
            })
            .collect::<Vec<_>>();
        writeln!(
            output,
            "Known cost: {}",
            if amounts.is_empty() {
                "unknown".into()
            } else {
                amounts.join(", ")
            }
        )?;
    } else {
        writeln!(output, "Cost: unknown")?;
    }
    if let Some(unknown) = budget["unknown_attempts"]
        .as_array()
        .filter(|attempts| !attempts.is_empty())
    {
        writeln!(
            output,
            "Additional usage is unknown for {} request attempt{}; billing holds remain.",
            unknown.len(),
            if unknown.len() == 1 { "" } else { "s" }
        )?;
    }
    if inspection["completeness"].is_string() && inspection["completeness"] != "complete_history" {
        writeln!(
            output,
            "Accounting history is incomplete; these totals may omit earlier usage."
        )?;
    }
    Ok(())
}

pub(super) fn present_doctor(
    envelope: &Envelope,
    output: &mut impl Write,
    command_prefix: &str,
) -> io::Result<()> {
    let data = &envelope.data;
    let doctor = data.get("doctor").unwrap_or(data);
    if let Some(state) = doctor["cache_state"].as_str() {
        writeln!(
            output,
            "Cache: {} / {state}",
            doctor["cache_layout"].as_str().unwrap_or("unknown")
        )?;
        if let Some(compatible) = doctor["parser_compatible"].as_bool() {
            writeln!(output, "Parser compatible: {compatible}")?;
        }
        if let Some(note) = doctor["cache_note"].as_str() {
            writeln!(output, "{note}")?;
        }
        if let Some(message) = doctor["cache_error"]["message"].as_str() {
            writeln!(output, "Cache observation failed: {message}")?;
            if let Some(hint) = doctor["cache_error"]["hint"].as_str() {
                writeln!(output, "{hint}")?;
            }
        }
        writeln!(
            output,
            "Operation status: {}",
            doctor["operation_state"].as_str().unwrap_or("unknown")
        )?;
        if let Some(change) = doctor["active_change"].as_str() {
            writeln!(
                output,
                "Active change: {change}\nRecover with: {command_prefix} recover"
            )?;
        }
        writeln!(
            output,
            "Notes and history were not audited. Check your notes with: {command_prefix} check"
        )?;
    }
    let error_details = envelope.error.as_ref().map(|error| &error.details);
    let provider = data
        .get("provider")
        .or_else(|| error_details.and_then(|details| details.get("provider")));
    if let Some(provider) = provider {
        present_provider(provider, output)?;
    }
    let probe = data.get("probe");
    if let Some(error) = &envelope.error {
        writeln!(output, "Doctor failed: {}: {}", error.code, error.message)?;
        if let Some(role) = error.details["probe_role"].as_str() {
            writeln!(output, "Provider probe ({role}): failed")?;
        }
        if let Some(hint) = &error.hint {
            writeln!(output, "{hint}")?;
        }
        if probe.is_none() && error.details["run_id"].is_string() {
            writeln!(
                output,
                "Requests and usage: unknown; inspect retained accounting below."
            )?;
        }
        if let Some(action) = error.details["recovery_action"].as_str() {
            writeln!(output, "{action}")?;
        }
    } else if let Some(probe) = probe {
        let role = probe["role"].as_str().unwrap_or("unknown");
        let outcome = if probe["dry_run"] == true {
            "not performed (preview)"
        } else if probe["validated"] == true {
            "succeeded"
        } else {
            "not validated"
        };
        writeln!(output, "Provider probe ({role}): {outcome}")?;
        present_probe_usage(&probe["inspection"], output)?;
        if probe["dry_run"] == true {
            writeln!(
                output,
                "Remote work and cache availability: unknown. No credentials were resolved."
            )?;
        }
    }
    let run = probe
        .and_then(|probe| probe["run_id"].as_str())
        .or_else(|| error_details.and_then(|details| details["run_id"].as_str()));
    if let Some(run) = run {
        writeln!(output, "Details: {command_prefix} jobs status --run {run}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(envelope: &Envelope) -> String {
        let mut output = Vec::new();
        present_doctor(envelope, &mut output, "lwiki --wiki '/my notes'").unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn nested_probe_is_concise_and_preserves_known_and_unknown_accounting() {
        let envelope = Envelope::success(
            "doctor",
            json!({
                "doctor":{"cache_layout":"legacy","cache_state":"ready","operation_state":"idle"},
                "provider":{"generation":{"model":"chosen-model","adapter":"responses-v1","destination":{"scheme":"https","host":"provider.example.invalid","port":8443}}},
                "probe":{"role":"generate","validated":true,"run_id":"Run.Probe","inspection":{
                    "budget":{"dispatched_requests":2,"settled":{"billable_units":{"input":8,"output":0,"reasoning":17}},"known_costs":{"USD":12000000},"unknown_attempts":["Attempt.Unknown"]},
                    "completeness":"complete_history","spec":{"private_debug":"must-not-render"},"attempts":[{"private":"must-not-render"}]
                }}
            }),
        );
        let before = serde_json::to_value(&envelope).unwrap();
        let output = render(&envelope);
        assert!(output.contains("Provider probe (generate): succeeded"));
        assert!(output.contains("chosen-model at https://provider.example.invalid:8443"));
        assert!(output.contains("Requests dispatched: 2"));
        assert!(output.contains("Settled units: input 8, output 0, reasoning 17"));
        assert!(output.contains("Known cost: USD 0.012000000"));
        assert!(output.contains("unknown for 1 request attempt"));
        assert!(output.contains("jobs status --run Run.Probe"));
        assert!(!output.contains("must-not-render"));
        assert!(!output.contains("network"));
        assert_eq!(serde_json::to_value(&envelope).unwrap(), before);
    }

    #[test]
    fn flat_doctor_and_preview_keep_their_observation_limits() {
        let flat = Envelope::success(
            "doctor",
            json!({"cache_state":"missing","cache_layout":"legacy","operation_state":"not_checked","cache_note":"No saved index."}),
        );
        let output = render(&flat);
        assert!(output.contains("Cache: legacy / missing"));
        assert!(output.contains("Notes and history were not audited"));
        assert!(!output.contains("Provider probe"));
        let preview = Envelope::success(
            "doctor",
            json!({"doctor":flat.data,"probe":{"role":"embed","dry_run":true,"remote_work":"unknown"}}),
        );
        let output = render(&preview);
        assert!(output.contains("not performed (preview)"));
        assert!(output.contains("Requests dispatched: unknown"));
        assert!(output.contains("Settled units: unknown"));
        assert!(output.contains("Remote work and cache availability: unknown"));
        assert!(!output.contains("Requests dispatched: 0"));
    }

    #[test]
    fn failed_probe_reports_recovery_without_inventing_usage() {
        let mut envelope = Envelope::failure(
            "doctor",
            "PROVIDER_RESPONSE",
            "bounded operation failed".into(),
        );
        let error = envelope.error.as_mut().unwrap();
        error.hint = Some("Raise the bounded output allowance deliberately.".into());
        error.details = json!({"run_id":"Run.Retained","probe_role":"generate","cause":{"private_debug":"must-not-render"},"recovery_action":"The run could not be paused."});
        let output = render(&envelope);
        assert!(output.contains("Provider probe (generate): failed"));
        assert!(output.contains("Requests and usage: unknown"));
        assert!(output.contains("Raise the bounded output allowance"));
        assert!(output.contains("The run could not be paused"));
        assert!(output.contains("jobs status --run Run.Retained"));
        assert!(!output.contains("must-not-render"));
    }

    #[test]
    fn wiki_selection_is_readable_and_has_no_configuration_dump() {
        for (origin, expected) in [
            ("explicit", "the --wiki option"),
            ("ancestor", "the current folder"),
            ("default", "your saved default"),
        ] {
            let envelope = Envelope::success(
                "wiki show",
                json!({"title":"My notes 猫","root":"/my notes","origin":origin,"configuration":{"private_debug":"must-not-render"}}),
            );
            let mut output = Vec::new();
            present_wiki(&envelope, &mut output).unwrap();
            let output = String::from_utf8(output).unwrap();
            assert_eq!(
                output,
                format!("Wiki: My notes 猫\nFolder: /my notes\nSelected from: {expected}\n")
            );
        }
    }

    #[test]
    fn wiki_title_refuses_a_marker_with_a_replacement_identity() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("WIKI.md");
        let original = "---\nwiki_schema: '1'\nwiki_kind: vault\nwiki_id: Vault.Original\ntitle: My notes\n---\n";
        std::fs::write(&marker, original).unwrap();
        let root = crate::vault::VaultRoot::explicit(temp.path()).unwrap();
        let app = OfflineApp::new(
            crate::vault::VaultFs::new(root),
            crate::app::OperationOptions {
                dry_run: true,
                offline: true,
                stage_only: false,
                lock_timeout_ms: 5000,
            },
        )
        .unwrap();
        assert_eq!(wiki_title(&app).unwrap(), "My notes");
        std::fs::write(
            &marker,
            original.replace("Vault.Original", "Vault.Replacement"),
        )
        .unwrap();
        assert_eq!(
            wiki_title(&app).unwrap_err().code,
            ErrorCode::ContentConflict
        );
    }

    #[test]
    fn metadata_inspection_never_executes_helpers_and_keeps_private_values_out() {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let path = base.join("private-provider.toml");
        let helper = base.join("private credential helper");
        let sentinel = base.join("helper-must-not-run");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let quoted = sentinel.to_string_lossy().replace('\'', "'\\''");
            std::fs::write(&helper, format!("#!/bin/sh\n: > '{quoted}'\nexit 47\n")).unwrap();
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let private = format!(
            r#"version = 1
[profiles.notes]
embedding = "embed"
[services.embed]
adapter = "embeddings-v1"
url = "https://provider.example.invalid/private-deployment?tenant=private-query"
model = "chosen-model"
[services.embed.auth]
kind = "command"
command = [{helper}, "private-argument"]
output = "text"
ttl_seconds = 240
refresh_skew_seconds = 30
[vault_bindings]
"#,
            helper = toml::Value::String(helper.to_str().unwrap().into())
        );
        let snapshot = crate::config::setup_io::load_private_file(&path, 65536).unwrap();
        crate::config::setup_io::write_private_file(
            &snapshot,
            private.as_bytes(),
            false,
            false,
            5000,
        )
        .unwrap();
        ProviderConfig::load(&path).expect("valid private metadata fixture");
        let document = crate::config::LocalConfigDocument {
            providers_config: Some(path.clone()),
            ..Default::default()
        };
        let context = RemoteContext {
            profile: Some("notes"),
            wiki_id: None,
            document: &document,
            allow_ambient_provider: false,
        };
        let mut envelope = Envelope::success(
            "embeddings sync",
            json!({"dry_run":true,"cache_state_unknown":true}),
        );
        attach_provider_summary(&mut envelope, &RemoteArguments::default(), &context);
        assert_eq!(envelope.data["provider"]["inspection"], "available");
        assert_eq!(
            envelope.data["provider"]["embedding"]["destination"],
            json!({"scheme":"https","host":"provider.example.invalid","port":443})
        );
        assert_eq!(envelope.data["provider"]["credentials_resolved"], false);
        assert!(!envelope.meta.network_used);
        let encoded = serde_json::to_string(&envelope).unwrap();
        for private in [
            "private-deployment",
            "private-query",
            "private credential helper",
            "private-argument",
        ] {
            assert!(!encoded.contains(private));
        }
        assert!(!sentinel.exists());
        assert_eq!(std::fs::read(&path).unwrap(), private.as_bytes());

        let override_remote = RemoteArguments {
            providers_config: Some(base.join("missing-provider.toml")),
            ..Default::default()
        };
        assert_eq!(
            provider_summary(&override_remote, &context)["inspection"],
            "unavailable"
        );
        let unconfigured = crate::config::LocalConfigDocument::default();
        let disabled = RemoteContext {
            profile: Some("notes"),
            wiki_id: None,
            document: &unconfigured,
            allow_ambient_provider: false,
        };
        assert_eq!(
            provider_summary(&RemoteArguments::default(), &disabled)["inspection"],
            "not_configured"
        );
        assert!(!sentinel.exists());
    }
}

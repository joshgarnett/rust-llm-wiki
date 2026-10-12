//! Change one provider role from the selected wiki's effective settings.
use super::{
    Arguments, OutputFormat,
    setup::{self, CredentialOutput, Prompts},
};
use crate::{
    config::{
        self, DefaultWiki,
        provider_edit::{
            self, ProviderAuthPatch, ProviderGrantDecision, ProviderRole, ProviderRolePatch,
        },
        providers::{AutomaticEmbeddingConsent, ProviderAutomaticEmbeddingDecision},
    },
    domain::*,
    output::{Envelope, ErrorOutput},
};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderRoleArgument {
    Embedding,
    Generation,
}
impl From<ProviderRoleArgument> for ProviderRole {
    fn from(role: ProviderRoleArgument) -> Self {
        match role {
            ProviderRoleArgument::Embedding => Self::Embedding,
            ProviderRoleArgument::Generation => Self::Generation,
        }
    }
}

#[derive(Clone, Args)]
pub struct ProviderSetArguments {
    /// Model role to change. Embedding edits may prepare search with saved automatic permission.
    #[arg(value_enum)]
    pub role: ProviderRoleArgument,
    /// Model name from your provider.
    #[arg(long)]
    pub model: Option<String>,
    /// Full endpoint URL. Review its destination before permitting requests.
    #[arg(long)]
    pub url: Option<String>,
    /// API format for this role; omitted settings are kept.
    #[arg(long, value_parser = ["embeddings-v1", "responses-v1", "chat-completions-v1"])]
    pub adapter: Option<String>,
    /// Embedding vector dimensions (number or auto).
    #[arg(long)]
    pub dimensions: Option<String>,
    /// Generation model's maximum output tokens.
    #[arg(long)]
    pub max_output_tokens: Option<u64>,
    /// Environment variable holding the key. Its name is saved for provider requests.
    #[arg(long, conflicts_with_all = ["key_file", "credential_command", "reuse_auth"])]
    pub key_env: Option<String>,
    /// Existing private key file; only its absolute path is saved.
    #[arg(long, conflicts_with_all = ["credential_command", "reuse_auth"])]
    pub key_file: Option<PathBuf>,
    /// Credential program and arguments; quotes preserve spaces and no shell runs.
    #[arg(long, conflicts_with = "reuse_auth")]
    pub credential_command: Option<String>,
    /// Command output: token-only text (default), or key/expiry JSON.
    #[arg(long, value_enum, requires = "credential_command")]
    pub credential_output: Option<CredentialOutput>,
    /// Deliberately copy credentials from the indicated model role.
    #[arg(long, value_enum)]
    pub reuse_auth: Option<ProviderRoleArgument>,
    /// Private provider settings file; otherwise use your saved location.
    #[arg(long)]
    pub providers_config: Option<PathBuf>,
    /// Permit explicitly requested API calls for every model shown; this grant does not enable automatic preparation.
    #[arg(long)]
    pub grant_provider: bool,
    /// Prepare similarity search now and after saves; permit eligible wiki text and search questions for this embedding model.
    #[arg(long)]
    pub auto_embed: bool,
    /// Use supplied settings without prompts; at least one setting is required.
    #[arg(long)]
    pub non_interactive: bool,
}
impl std::fmt::Debug for ProviderSetArguments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSetArguments")
            .field("role", &self.role)
            .field("settings", &"private_choices")
            .finish()
    }
}

fn patch(settings: &ProviderSetArguments) -> Result<ProviderRolePatch> {
    if settings.auto_embed && settings.role != ProviderRoleArgument::Embedding {
        return Err(setup::usage(
            "--auto-embed applies to embedding; use config set embedding --auto-embed",
        ));
    }
    let mut patch = ProviderRolePatch::new(settings.role.into());
    patch.model = settings.model.clone();
    patch.url = settings.url.clone();
    patch.adapter = settings
        .adapter
        .as_deref()
        .map(setup::adapter)
        .transpose()?;
    patch.dimensions = settings.dimensions.clone();
    patch.max_output_tokens = settings.max_output_tokens;
    if [
        settings.key_env.is_some(),
        settings.key_file.is_some(),
        settings.credential_command.is_some(),
        settings.reuse_auth.is_some(),
    ]
    .into_iter()
    .filter(|value| *value)
    .count()
        > 1
    {
        return Err(setup::usage(
            "choose one credential source for the selected model",
        ));
    }
    patch.auth = if let Some(name) = &settings.key_env {
        ProviderAuthPatch::Replace(config::provider_setup::SetupAuth::Environment(name.clone()))
    } else if let Some(path) = &settings.key_file {
        ProviderAuthPatch::Replace(config::provider_setup::SetupAuth::PrivateFile(
            setup::absolute(path)?,
        ))
    } else if let Some(text) = &settings.credential_command {
        ProviderAuthPatch::Replace(setup::command_auth(
            text,
            settings.credential_output.unwrap_or_default(),
        )?)
    } else if let Some(role) = settings.reuse_auth {
        ProviderAuthPatch::CopyFrom(role.into())
    } else {
        ProviderAuthPatch::Preserve
    };
    if patch.role == ProviderRole::Generation && patch.dimensions.is_some() {
        return Err(setup::usage(
            "--dimensions changes embedding vectors; use config set embedding",
        ));
    }
    if patch.role == ProviderRole::Embedding && patch.max_output_tokens.is_some() {
        return Err(setup::usage(
            "--max-output-tokens changes generation output; use config set generation",
        ));
    }
    Ok(patch)
}

fn collect<R: io::BufRead, W: Write>(
    settings: &ProviderSetArguments,
    interactive: bool,
    prompts: &mut Prompts<R, W>,
) -> Result<ProviderSetArguments> {
    let mut choices = settings.clone();
    if !patch(&choices)?.is_empty() || choices.auto_embed || choices.grant_provider {
        return Ok(choices);
    }
    if !interactive {
        return Err(setup::usage(
            "supply a setting, for example config set embedding --model MODEL; omitted settings are kept",
        ));
    }
    let setting = prompts.text(
        if choices.role == ProviderRoleArgument::Embedding {
            "Setting to change (model/url/credentials/dimensions)"
        } else {
            "Setting to change (model/url/credentials/max-output-tokens)"
        },
        Some("model"),
    )?;
    match setting.as_str() {
        "model" => choices.model = Some(prompts.text("Model name", None)?),
        "url" => choices.url = Some(prompts.text("Full endpoint URL", None)?),
        "dimensions" if choices.role == ProviderRoleArgument::Embedding => {
            choices.dimensions = Some(prompts.text("Vector dimensions (number or auto)", None)?)
        }
        "max-output-tokens" if choices.role == ProviderRoleArgument::Generation => {
            choices.max_output_tokens = Some(
                prompts
                    .text("Maximum output tokens", None)?
                    .parse()
                    .map_err(|_| setup::usage("enter a positive output token limit"))?,
            )
        }
        "credentials" => loop {
            match prompts
                .text(
                    "Credentials (command/environment/private file/reuse)",
                    Some("command"),
                )?
                .as_str()
            {
                "command" => {
                    writeln!(prompts.output,"Use one program and its arguments. Shell pipes and substitutions do not run.").map_err(|error| setup::usage(error.to_string()))?;
                    loop {
                        let text = prompts.text("Credential command", None)?;
                        match setup::command_auth(&text, CredentialOutput::Text) {
                            Ok(_) => {
                                choices.credential_command = Some(text);
                                break;
                            }
                            Err(error) => writeln!(prompts.output, "{}", error.message)
                                .map_err(|error| setup::usage(error.to_string()))?,
                        }
                    }
                    choices.credential_output = Some(loop {
                        match prompts
                            .text("Command output (text/json)", Some("text"))?
                            .as_str()
                        {
                            "text" => break CredentialOutput::Text,
                            "json" => break CredentialOutput::Json,
                            _ => writeln!(prompts.output, "Choose text or json.")
                                .map_err(|error| setup::usage(error.to_string()))?,
                        }
                    });
                    break;
                }
                "environment" | "env" => {
                    choices.key_env = Some(prompts.text("Environment variable name", None)?);
                    break;
                }
                "private file" | "file" => {
                    choices.key_file = Some(PathBuf::from(prompts.text("Private key file", None)?));
                    break;
                }
                "reuse" => {
                    choices.reuse_auth = Some(match choices.role {
                        ProviderRoleArgument::Embedding => ProviderRoleArgument::Generation,
                        ProviderRoleArgument::Generation => ProviderRoleArgument::Embedding,
                    });
                    break;
                }
                _ => writeln!(
                    prompts.output,
                    "Choose command, environment, private file or reuse."
                )
                .map_err(|error| setup::usage(error.to_string()))?,
            }
        },
        _ => {
            return Err(setup::usage(
                "choose one of the settings shown in the prompt",
            ));
        }
    }
    Ok(choices)
}

fn show_changes(summary: &Value, output: &mut impl Write) -> io::Result<()> {
    if let Some(fields) = summary["changed_fields"].as_array() {
        if fields.is_empty() {
            writeln!(
                output,
                "The selected model settings already match. No new provider entries are needed."
            )?;
        } else {
            let changes = fields
                .iter()
                .map(|field| {
                    format!(
                        "{} {}",
                        field["role"].as_str().unwrap_or("model"),
                        field["field"]
                            .as_str()
                            .unwrap_or("setting")
                            .replace('_', " ")
                    )
                })
                .collect::<Vec<_>>();
            writeln!(output, "Changes: {}.", changes.join(", "))?;
        }
    }
    setup::present_providers(summary, output)?;
    if let Some(roles) = summary["roles"].as_array() {
        for role in roles.iter().filter(|role| role["changed"] == false) {
            writeln!(
                output,
                "Kept {} settings.",
                role["role"].as_str().unwrap_or("other model")
            )?;
        }
    }
    Ok(())
}

fn inspection_next_step(
    args: &Arguments,
    wiki: &Path,
    configuration: Option<&Path>,
) -> Result<String> {
    let wiki = wiki
        .to_str()
        .ok_or_else(|| setup::usage("selected wiki folder must be UTF-8"))?;
    let mut words = vec!["lwiki", "--wiki", wiki];
    if args.preferences.is_some() {
        let path = configuration
            .or(args.preferences.as_deref())
            .expect("explicit preferences location");
        let path = path
            .to_str()
            .ok_or_else(|| setup::usage("preferences file path must be UTF-8"))?;
        words.extend(["--preferences", path]);
    }
    if args.no_user_config {
        words.push("--no-user-config");
    }
    words.extend(["config", "show"]);
    let command = shlex::try_join(words)
        .map_err(|_| setup::usage("saved settings command cannot contain NUL bytes"))?;
    Ok(format!("Run {command} to inspect both models."))
}

#[cfg(test)]
type CheckpointHook = Box<dyn FnMut(&str, &Path) -> Result<()>>;
#[cfg(test)]
std::thread_local! { static CHECKPOINT: std::cell::RefCell<Option<CheckpointHook>> = const { std::cell::RefCell::new(None) }; }
fn checkpoint(stage: &str, wiki: &Path) -> Result<()> {
    #[cfg(test)]
    return CHECKPOINT.with(|hook| {
        hook.borrow_mut()
            .as_mut()
            .map_or(Ok(()), |hook| hook(stage, wiki))
    });
    #[cfg(not(test))]
    {
        let _ = (stage, wiki);
        Ok(())
    }
}

pub(super) fn execute(args: &Arguments, settings: &ProviderSetArguments) -> Result<Envelope> {
    if args.stage {
        return Err(setup::usage(
            "config set cannot be staged; use --dry-run to preview",
        ));
    }
    let interactive = !settings.non_interactive
        && args.output_format() == OutputFormat::Human
        && io::stdin().is_terminal()
        && io::stderr().is_terminal();
    let loaded = config::local::load_user_config_for_setup(
        args.preferences.as_deref(),
        args.no_user_config,
    )?;
    config::local::require_write_target(&loaded)?;
    let cwd = std::env::current_dir()
        .map_err(|_| setup::usage("cannot find the current folder; use --wiki PATH"))?;
    let selected =
        crate::vault::discovery::select(args.wiki.as_deref(), &cwd, loaded.default_wiki())?;
    crate::vault::discovery::expect_wiki_id(&selected, args.expect_wiki_id.as_ref())?;
    let wiki = DefaultWiki {
        path: selected.root.path().to_path_buf(),
        wiki_id: selected.wiki_id.clone(),
    };
    let profile = setup::effective_profile(args, &loaded, &wiki, true)?;
    let prior_saved_profile = setup::saved_profile(&loaded, &wiki, true)?;
    let target = loaded.toml_migration_target();
    let preferences_path = target
        .as_deref()
        .or(loaded.path())
        .expect("required write target");
    let provider_path = settings
        .providers_config
        .clone()
        .or_else(|| {
            loaded
                .document()
                .saved_provider_config(Some(&wiki.wiki_id))
                .map(|(path, _)| path.to_path_buf())
        })
        .unwrap_or_else(|| {
            preferences_path
                .parent()
                .expect("absolute preferences path")
                .join("providers.toml")
        });
    let snapshot = config::setup_io::load_private_file(
        &setup::absolute(&provider_path)?,
        config::providers::MAX_PROVIDER_CONFIG_BYTES,
    )?;
    let mut prompts = Prompts {
        input: io::stdin().lock(),
        output: io::stderr().lock(),
    };
    let choices = collect(settings, interactive, &mut prompts)?;
    let patches = [patch(&choices)?];
    let mut choices = choices;
    let mut decision = if choices.grant_provider {
        ProviderGrantDecision::ReviewedAllRoles
    } else {
        ProviderGrantDecision::Preserve
    };
    let mut automatic = if choices.auto_embed {
        ProviderAutomaticEmbeddingDecision::Reviewed
    } else {
        ProviderAutomaticEmbeddingDecision::Preserve
    };
    let mut plan = provider_edit::plan_provider_edit_with_automatic(
        snapshot.bytes(),
        &wiki,
        profile.as_deref(),
        &patches,
        decision,
        automatic,
    )?;
    if interactive
        && choices.role == ProviderRoleArgument::Embedding
        && !choices.auto_embed
        && plan.automatic_embedding_consent != AutomaticEmbeddingConsent::Matching
        && setup::review_automatic_permission(&plan, &wiki, &mut prompts)?
    {
        choices.auto_embed = true;
        automatic = ProviderAutomaticEmbeddingDecision::Reviewed;
        plan = provider_edit::plan_provider_edit_with_automatic(
            snapshot.bytes(),
            &wiki,
            profile.as_deref(),
            &patches,
            decision,
            automatic,
        )?;
    }
    if interactive
        && !plan.configuration_consent_satisfied
        && setup::review_provider_permission(&plan, &mut prompts)?
    {
        decision = ProviderGrantDecision::ReviewedAllRoles;
        plan = provider_edit::plan_provider_edit_with_automatic(
            snapshot.bytes(),
            &wiki,
            profile.as_deref(),
            &patches,
            decision,
            automatic,
        )?;
    }
    let summary = setup::safe_provider_summary(&plan);
    if args.output_format() == OutputFormat::Human {
        show_changes(&summary, &mut prompts.output)
            .map_err(|error| setup::usage(error.to_string()))?;
    }
    if !args.dry_run && !plan.configuration_consent_satisfied {
        let mut error = setup::usage(
            "model changes need permission; review the --dry-run preview, then rerun with --grant-provider to permit explicit requests for all models shown",
        );
        error.details = json!({"providers":summary,"writes":0});
        return Err(error);
    }
    let timeout = args.lock_timeout_ms.unwrap_or(5000);
    if timeout > 30_000 {
        return Err(setup::usage(
            "config set writer timeout must be at most 30000 ms",
        ));
    }
    let mut proposed = loaded.document().clone();
    let selection_changed = setup::save_provider_selection(
        &mut proposed,
        &wiki,
        prior_saved_profile.as_deref(),
        plan.profile(),
        snapshot.path(),
    )?;
    if selection_changed
        && !proposed.allowed_profiles.is_empty()
        && !proposed
            .allowed_profiles
            .iter()
            .any(|profile| profile == plan.profile())
    {
        proposed.allowed_profiles.push(plan.profile().into());
    }
    // Both inspected files must pass their write preflights before either publishes.
    let provider_preflight = config::setup_io::write_private_file(
        &snapshot,
        plan.proposed_bytes(),
        true,
        true,
        timeout,
    )?;
    let preferences_preflight = config::local::update_document(
        &loaded,
        proposed.clone(),
        target.as_deref(),
        true,
        timeout,
    )?;
    // Prepare both continuations before writes, including an explicit JSON migration.
    let original_next_step = inspection_next_step(args, &wiki.path, loaded.path())?;
    let saved_next_step = inspection_next_step(args, &wiki.path, Some(preferences_path))?;
    let mut steps = vec![
        json!({"name":"provider","destination":snapshot.path(),"status":"planned","applied":false,"durable":false}),
        json!({"name":"configuration","destination":preferences_path,"status":"planned","applied":false,"durable":false}),
    ];
    let mut failure = None;
    let mut cancelled = false;
    if !args.dry_run {
        if interactive && !prompts.yes("Apply these model settings?", true)? {
            cancelled = true;
        } else {
            let result: Result<()> = (|| {
                if crate::vault::discovery::default_from_path(&wiki.path)? != wiki {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "selected wiki changed since the preview",
                    ));
                }
                let provider = config::setup_io::write_private_file(
                    &snapshot,
                    plan.proposed_bytes(),
                    true,
                    !provider_preflight.changed,
                    timeout,
                )?;
                steps[0]["applied"] = json!(provider.applied);
                steps[0]["durable"] = json!(provider.durable);
                steps[0]["changed"] = json!(provider.changed);
                if let Some(error) = provider.failure {
                    return Err(error);
                }
                steps[0]["status"] = json!("complete");
                checkpoint("provider_complete", &wiki.path)?;
                setup::recheck_provider_publication(snapshot.path(), plan.proposed_bytes())?;
                if crate::vault::discovery::default_from_path(&wiki.path)? != wiki {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "selected wiki changed before saving model settings",
                    ));
                }
                let preferences = config::local::update_document(
                    &loaded,
                    proposed.clone(),
                    target.as_deref(),
                    !preferences_preflight.changed,
                    timeout,
                )?;
                steps[1]["applied"] = json!(preferences.applied);
                steps[1]["durable"] = json!(preferences.durable);
                if let Some(error) = preferences.failure {
                    return Err(error);
                }
                steps[1]["status"] = json!("complete");
                Ok(())
            })();
            if let Err(error) = result {
                if let Some(step) = steps.iter_mut().find(|step| step["status"] != "complete") {
                    step["status"] = json!("failed");
                }
                failure = Some(error);
            }
        }
    }
    let partial = failure.is_some() && steps.iter().any(|step| step["applied"] == true);
    let next_step =
        if !args.dry_run && !cancelled && (failure.is_none() || steps[1]["applied"] == true) {
            saved_next_step
        } else {
            original_next_step
        };
    let mut envelope = Envelope::success(
        "config set",
        json!({"role":choices.role.into_provider_name(),"wiki":wiki.path,
        "providers":summary,"provider_configuration":snapshot.path(),"configuration":preferences_path,"steps":steps,
        "completion":if cancelled { "cancelled" } else if failure.is_some() { "incomplete" } else if args.dry_run { "preview" } else { "complete" },
        "next_step":next_step,"configuration_credentials_resolved":false,"configuration_network_requests":0}),
    );
    if choices.role == ProviderRoleArgument::Embedding {
        envelope.data["automatic_embeddings"] = super::automatic_embeddings::preview(
            plan.automatic_embedding_consent == AutomaticEmbeddingConsent::Matching,
            crate::app::embeddings::PreparationTrigger::Setup,
            &proposed,
        );
    }
    if let Some(error) = failure {
        envelope.ok = false;
        envelope.error = Some(ErrorOutput { code:error.code.to_string(),message:error.message,retryable:error.retryable,
            hint:Some(if partial {
                "Some settings were published and are preserved. Retry the same config set command; unchanged settings reuse those entries."
            } else { "Correct the failed step, then retry the same config set command." }.into()),details:error.details });
    }
    if cancelled {
        envelope.ok = false;
        envelope.error = Some(ErrorOutput {
            code: ErrorCode::Cancelled.to_string(),
            message: "model settings were not applied".into(),
            retryable: false,
            hint: None,
            details: Value::Null,
        });
    }
    envelope.meta.partial = partial;
    envelope.meta.wiki_id = Some(wiki.wiki_id.to_string());
    envelope.warnings.extend(loaded.summary().warnings.clone());
    if envelope.ok && !args.dry_run && choices.role == ProviderRoleArgument::Embedding {
        super::automatic_embeddings::after_setup(
            args,
            &wiki,
            Some(plan.profile()),
            Some(preferences_path),
            &mut envelope,
        );
    }
    Ok(envelope)
}
impl ProviderRoleArgument {
    fn into_provider_name(self) -> &'static str {
        ProviderRole::from(self).as_str()
    }
}

pub(super) fn present(envelope: &Envelope, output: &mut impl Write) -> io::Result<()> {
    writeln!(
        output,
        "{}",
        match envelope.data["completion"].as_str() {
            Some("preview") => "Model settings preview. No files were changed.",
            Some("complete") => "Your model settings were saved.",
            Some("cancelled") => "Model settings were kept.",
            _ => "Saving model settings is incomplete.",
        }
    )?;
    show_changes(&envelope.data["providers"], output)?;
    if let Some(error) = &envelope.error {
        writeln!(output, "{}: {}", error.code, error.message)?;
        if let Some(hint) = &error.hint {
            writeln!(output, "{hint}")?;
        }
    }
    if envelope.meta.partial {
        writeln!(
            output,
            "Some files changed. Check the two saved locations before retrying."
        )?;
    }
    if envelope.ok
        && envelope.data["completion"] != "preview"
        && let Some(next_step) = envelope.data["next_step"].as_str()
    {
        writeln!(output, "{next_step}")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "config_set_tests.rs"]
mod tests;

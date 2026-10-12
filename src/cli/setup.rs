//! Guided creation of a persistent wiki, preferences and selected agent instructions.
use super::{Arguments, OutputFormat};
use crate::{
    app::OperationOptions,
    config::{
        self, DefaultWiki,
        provider_edit::{
            self, ProviderAdapter, ProviderAuthPatch, ProviderEditPlan, ProviderGrantDecision,
            ProviderRole, ProviderRolePatch,
        },
        provider_setup::{SetupAuth, SetupCommandOutput},
        providers::{AutomaticEmbeddingConsent, ProviderAutomaticEmbeddingDecision},
    },
    domain::*,
    output::{Envelope, ErrorOutput},
};
use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum CredentialOutput {
    #[default]
    Text,
    Json,
}
impl From<CredentialOutput> for SetupCommandOutput {
    fn from(value: CredentialOutput) -> Self {
        match value {
            CredentialOutput::Text => Self::Text,
            CredentialOutput::Json => Self::Json,
        }
    }
}

#[derive(Clone, Default, Args)]
pub struct SetupArguments {
    /// Folder for your wiki. An existing wiki is selected safely on repeated runs.
    pub path: Option<PathBuf>,
    /// Title for a new wiki.
    #[arg(long)]
    pub title: Option<String>,
    /// Deliberately initialize an existing notes folder without changing its notes.
    #[arg(long, conflicts_with = "select")]
    pub existing: bool,
    /// Select an initialized wiki; never create one.
    #[arg(long)]
    pub select: bool,
    /// Keep the current saved default instead of selecting this wiki.
    #[arg(long)]
    pub no_default: bool,
    /// Use supplied choices without prompts. Optional agent and model setup default to skipped.
    #[arg(long)]
    pub non_interactive: bool,
    /// Install wiki instructions for Codex, Sagex, Claude Code or Cursor.
    #[arg(long, value_parser = ["codex", "sagex", "claude-code", "cursor"])]
    pub agent: Option<String>,
    /// Install for your user account (default) or one project.
    #[arg(long, value_parser = ["user", "project"])]
    pub agent_scope: Option<String>,
    /// Agent home or project folder. Defaults to HOME or the current folder for project scope.
    #[arg(long)]
    pub agent_directory: Option<PathBuf>,
    /// Embedding model from your provider. Saved automatic permission may prepare search after setup.
    #[arg(long)]
    pub embedding_model: Option<String>,
    /// Your provider's full embeddings-v1 endpoint URL.
    #[arg(long)]
    pub embedding_url: Option<String>,
    /// Requested vector dimensions, from 1 to 65536, or auto.
    #[arg(long)]
    pub embedding_dimensions: Option<String>,
    /// Environment variable holding your key. Its name is saved for provider requests.
    #[arg(long, conflicts_with_all = ["key_file", "prompt_key", "credential_command"])]
    pub key_env: Option<String>,
    /// Existing private key file. Setup stores only its absolute path.
    #[arg(long, conflicts_with_all = ["prompt_key", "credential_command"])]
    pub key_file: Option<PathBuf>,
    /// Read your key without displaying it and save it in a private file. Requires a terminal.
    #[arg(long)]
    pub prompt_key: bool,
    /// Program and arguments that return a token. Quotes preserve spaces; no shell runs.
    #[arg(long, conflicts_with_all = ["key_env", "key_file", "prompt_key"])]
    pub credential_command: Option<String>,
    /// Credential command output: token-only text (default) or key/expiry JSON.
    #[arg(long, value_enum, requires = "credential_command")]
    pub credential_output: Option<CredentialOutput>,
    /// Private file destination for --prompt-key; defaults beside the provider configuration.
    #[arg(long, requires = "prompt_key")]
    pub credential_file: Option<PathBuf>,
    /// Deliberately replace an existing key file when using --prompt-key.
    #[arg(long, requires = "prompt_key")]
    pub replace_key: bool,
    /// Private provider configuration destination; defaults beside your user configuration.
    #[arg(long)]
    pub providers_file: Option<PathBuf>,
    /// Permit explicitly requested embedding calls; this grant does not enable automatic preparation.
    #[arg(long)]
    pub grant_embed: bool,
    /// Prepare similarity search now and after saves; send eligible wiki text and search questions to this embedding model.
    #[arg(long)]
    pub auto_embed: bool,
    /// Permit explicit future API requests for every final model shown for this wiki.
    #[arg(long, conflicts_with = "grant_embed")]
    pub grant_provider: bool,
    /// Optional generation model for API extraction. Your agent can extract without it.
    #[arg(long)]
    pub generation_model: Option<String>,
    /// Full endpoint URL for the generation model.
    #[arg(long)]
    pub generation_url: Option<String>,
    /// Generation API format; new models default to responses-v1.
    #[arg(long, value_parser = ["responses-v1", "chat-completions-v1"])]
    pub generation_adapter: Option<String>,
    /// Maximum output tokens for the generation model; omitted settings are kept.
    #[arg(long)]
    pub generation_max_output_tokens: Option<u64>,
    /// Reuse the embedding credentials after reviewing both destinations.
    #[arg(long, conflicts_with_all = ["generation_key_env", "generation_key_file", "generation_prompt_key", "generation_credential_command"])]
    pub generation_reuse_auth: bool,
    /// Environment variable holding the generation key; its value is not read.
    #[arg(long, conflicts_with_all = ["generation_key_file", "generation_prompt_key", "generation_credential_command", "generation_reuse_auth"])]
    pub generation_key_env: Option<String>,
    /// Existing private generation key file.
    #[arg(long, conflicts_with_all = ["generation_prompt_key", "generation_credential_command", "generation_reuse_auth"])]
    pub generation_key_file: Option<PathBuf>,
    /// Read and save a generation key privately; requires a terminal.
    #[arg(long, conflicts_with_all = ["generation_credential_command", "generation_reuse_auth"])]
    pub generation_prompt_key: bool,
    /// Generation credential program and arguments; no shell runs.
    #[arg(long, conflicts_with = "generation_reuse_auth")]
    pub generation_credential_command: Option<String>,
    /// Generation command output: token-only text (default) or key/expiry JSON.
    #[arg(long, value_enum, requires = "generation_credential_command")]
    pub generation_credential_output: Option<CredentialOutput>,
    /// Private file destination for --generation-prompt-key.
    #[arg(long, requires = "generation_prompt_key")]
    pub generation_credential_file: Option<PathBuf>,
    /// Deliberately replace the generation private key file.
    #[arg(long, requires = "generation_prompt_key")]
    pub generation_replace_key: bool,
    /// Text added before documents sent for embedding.
    #[arg(long)]
    pub embedding_document_prefix: Option<String>,
    /// Text added before search queries sent for embedding.
    #[arg(long)]
    pub embedding_query_prefix: Option<String>,
    /// Maximum UTF-8 input bytes per embedding request.
    #[arg(long)]
    pub embedding_max_input_bytes: Option<usize>,
    /// Preferred UTF-8 byte size for splitting long documents into embedding inputs.
    #[arg(long)]
    pub embedding_quality_target_bytes: Option<usize>,
}

// Commands and endpoint queries may contain private data.
impl std::fmt::Debug for SetupArguments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SetupArguments")
            .field("path", &self.path)
            .field("non_interactive", &self.non_interactive)
            .field("provider_settings", &"private_choices")
            .finish()
    }
}

/// Parse one familiar command line, resolve its program, and validate exact argv.
/// No program, shell, environment expansion or credential lookup is executed.
pub(super) fn command_auth(text: &str, output: CredentialOutput) -> Result<SetupAuth> {
    command_auth_with_path(text, output, std::env::var_os("PATH").as_deref())
}
fn command_auth_with_path(
    text: &str,
    output: CredentialOutput,
    search_path: Option<&std::ffi::OsStr>,
) -> Result<SetupAuth> {
    if text.is_empty() || text.len() > 4096 {
        return Err(usage(
            "credential command must contain 1..=4096 bytes; use one program and its arguments",
        ));
    }
    let mut argv = shlex::split(text).ok_or_else(|| {
        usage("credential command has unmatched quotes; correct the quotes and try again")
    })?;
    let program = argv
        .first()
        .filter(|program| !program.is_empty())
        .ok_or_else(|| usage("credential command needs a program name"))?;
    let supplied = Path::new(program);
    let has_separator = supplied.components().count() > 1 || supplied.is_absolute();
    let candidates = if has_separator {
        vec![absolute(supplied)?]
    } else {
        search_path
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .map(|directory| absolute(&directory.join(program)))
            .collect::<Result<Vec<_>>>()?
    };
    let executable = candidates.into_iter().find_map(|path| {
        let metadata = std::fs::metadata(&path).ok()?;
        if !metadata.is_file() { return None; }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 { return None; }
        }
        std::fs::canonicalize(path).ok()
    }).ok_or_else(|| usage("credential program was not found or is not executable; install it, check PATH, or supply its absolute path"))?;
    argv[0] = executable
        .to_str()
        .ok_or_else(|| usage("credential program path must be UTF-8"))?
        .into();
    SetupAuth::command(argv, output.into(), 300, 60, 10)
}

pub(super) fn adapter(value: &str) -> Result<ProviderAdapter> {
    match value {
        "embeddings-v1" => Ok(ProviderAdapter::EmbeddingsV1),
        "responses-v1" => Ok(ProviderAdapter::ResponsesV1),
        "chat-completions-v1" => Ok(ProviderAdapter::ChatCompletionsV1),
        _ => Err(usage(
            "choose embeddings-v1, responses-v1 or chat-completions-v1",
        )),
    }
}

pub(super) fn effective_profile(
    args: &Arguments,
    loaded: &config::local::LoadedUserConfig,
    wiki: &DefaultWiki,
    existing: bool,
) -> Result<Option<String>> {
    if existing {
        let root = crate::vault::VaultRoot::explicit(&wiki.path)?;
        return Ok(config::local::resolve_loaded(
            &crate::vault::VaultFs::new(root),
            &wiki.wiki_id,
            &config::PreferenceOptions {
                profile: args.profile.clone(),
                ..Default::default()
            },
            loaded,
        )?
        .profile);
    }
    Ok(args
        .profile
        .clone()
        .or_else(|| loaded.document().defaults.profile.clone()))
}

pub(super) fn saved_profile(
    loaded: &config::local::LoadedUserConfig,
    wiki: &DefaultWiki,
    existing: bool,
) -> Result<Option<String>> {
    if existing {
        let root = crate::vault::VaultRoot::explicit(&wiki.path)?;
        return Ok(config::local::resolve_loaded(
            &crate::vault::VaultFs::new(root),
            &wiki.wiki_id,
            &config::PreferenceOptions::default(),
            loaded,
        )?
        .profile);
    }
    Ok(loaded.document().defaults.profile.clone())
}

/// Publish the selected wiki's file/profile pair without changing another wiki's fallback.
pub(super) fn save_provider_selection(
    proposed: &mut config::LocalConfigDocument,
    wiki: &DefaultWiki,
    previous_profile: Option<&str>,
    profile: &str,
    provider_path: &Path,
) -> Result<bool> {
    let same_file = if let Some((saved, _)) = proposed.saved_provider_config(Some(&wiki.wiki_id)) {
        config::local::normalize_private_path(saved)? == provider_path
    } else {
        // A writer's preferences-sibling default can differ from ordinary remote reads.
        crate::app::remote::default_provider_path()
            .ok()
            .and_then(|path| config::local::normalize_private_path(&path).ok())
            .is_some_and(|path| path == provider_path)
    };
    let profile_changed = previous_profile != Some(profile);
    if profile_changed || !same_file {
        let preferences = proposed.vaults.entry(wiki.wiki_id.clone()).or_default();
        preferences.profile = Some(profile.into());
        preferences.providers_config = Some(provider_path.to_path_buf());
    }
    Ok(profile_changed)
}

/// Confirm the exact private definition before selecting it in preferences.
pub(super) fn recheck_provider_publication(path: &Path, expected: &[u8]) -> Result<()> {
    let current =
        config::setup_io::load_private_file(path, config::providers::MAX_PROVIDER_CONFIG_BYTES)?;
    if current.bytes() != Some(expected) {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "provider settings changed before saving the wiki's model choice; your edits were kept",
        ));
    }
    Ok(())
}

fn provider_summary(plan: &ProviderEditPlan) -> Value {
    let mut value = json!({"roles": plan.roles, "changed_fields": plan.changed_fields,
        "consent_requirements":plan.consent_requirements,"permission_complete":plan.configuration_consent_satisfied,
        "explicit_permission_complete":plan.consent_satisfied,
        "projected_bytes":plan.projected_bytes,"configuration_network_requests":0,"configuration_credentials_resolved":false,
        "automatic_embedding_consent":plan.automatic_embedding_consent});
    for role in &plan.roles {
        let mut summary = serde_json::to_value(role).expect("safe provider summary");
        summary["text_scope"] = json!(if role.role == ProviderRole::Embedding {
            EMBEDDING_TEXT_SCOPE
        } else {
            "wiki text selected for API extraction"
        });
        summary["permitted"] = json!(plan.consent_satisfied);
        summary["configuration_network_requests"] = json!(0);
        if role.role == ProviderRole::Embedding {
            summary["grant_embed"] = json!(plan.consent_satisfied);
            summary["automatic_embedding_consent"] = json!(plan.automatic_embedding_consent);
        }
        value[role.role.as_str()] = summary;
    }
    value
}

pub(super) fn present_providers(summary: &Value, output: &mut impl Write) -> io::Result<()> {
    for (name, label) in [("embedding", "Embedding"), ("generation", "Generation")] {
        let role = &summary[name];
        if !role.is_object() {
            continue;
        }
        writeln!(
            output,
            "{label} model: {}",
            role["model"].as_str().unwrap_or("Not set")
        )?;
        writeln!(
            output,
            "Destination: {}",
            super::config_show::destination_label(&role["destination"])
        )?;
        if let Some(adapter) = role["adapter"].as_str() {
            writeln!(output, "API format: {adapter}")?;
        }
        if name == "embedding" {
            writeln!(
                output,
                "Vector dimensions: {}",
                role["dimensions"].as_str().unwrap_or("provider default")
            )?;
        }
        if let Some(limit) = role["effective_output_token_limit"].as_u64() {
            writeln!(output, "Maximum output tokens: {limit}")?;
        }
        writeln!(
            output,
            "Text that explicit requests may send: {}.",
            role["text_scope"].as_str().unwrap_or("wiki text")
        )?;
        writeln!(
            output,
            "Credentials: {}.",
            match role["auth_source"].as_str() {
                Some("command") => "credential command",
                Some("environment") => "environment variable",
                Some("private_file") => "private key file",
                _ => "saved private key",
            }
        )?;
        if let Some(command) = role["command_auth"].as_object() {
            writeln!(
                output,
                "Command output: {}; reuse for at most {} seconds; refresh {} seconds before expiry; timeout {} seconds.",
                command["output"].as_str().unwrap_or("text"),
                command["ttl_seconds"],
                command["refresh_skew_seconds"],
                command["timeout_seconds"]
            )?;
        }
        writeln!(
            output,
            "Explicit API requests: {}.",
            if role["permitted"] == true {
                "permitted when explicitly requested"
            } else {
                "not permitted"
            }
        )?;
        if name == "embedding" {
            writeln!(
                output,
                "Automatic similarity search: {}.",
                super::automatic_embeddings::consent_label(&role["automatic_embedding_consent"])
            )?;
        }
    }
    Ok(())
}

pub(super) fn safe_provider_summary(plan: &ProviderEditPlan) -> Value {
    provider_summary(plan)
}

pub(super) fn usage(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::Usage, message)
}
const EMBEDDING_TEXT_SCOPE: &str = "wiki text and search questions";

fn embedding_destination(url: &str) -> Result<Value> {
    let endpoint = url::Url::parse(url).map_err(|_| {
        WikiError::new(
            ErrorCode::ConfigInvalid,
            "invalid embeddings-v1 endpoint URL",
        )
    })?;
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return Err(WikiError::new(
            ErrorCode::ConfigInvalid,
            "embedding endpoint must not contain credentials",
        ));
    }
    let host = endpoint.host_str().ok_or_else(|| {
        WikiError::new(
            ErrorCode::ConfigInvalid,
            "embedding endpoint needs a destination host",
        )
    })?;
    Ok(json!({"scheme":endpoint.scheme(),"host":host,"port":endpoint.port_or_known_default()}))
}

pub(super) fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| usage(format!("cannot resolve folder: {error}")))
}
fn home() -> Result<PathBuf> {
    let path = std::env::var_os("HOME").map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| usage("HOME is unavailable or relative; supply the wiki folder and --agent-directory when installing an agent"))?;
    Ok(path)
}
fn agent_directory_default(setup: &SetupArguments) -> Result<PathBuf> {
    if setup.agent_scope.as_deref() == Some("project") {
        std::env::current_dir()
            .map_err(|error| usage(format!("cannot resolve project folder: {error}")))
    } else {
        home()
    }
}
// Sagex reads the shared .agents skill layout. Reuse its existing Codex
// installation identity so choosing Sagex does not change package ownership.
fn agent_install_target(agent: &str) -> &str {
    if agent == "sagex" { "codex" } else { agent }
}
fn agent_label(agent: &str) -> &str {
    if agent == "sagex" { "Sagex" } else { agent }
}
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
fn quote_path(path: &Path) -> String {
    quote(&path.to_string_lossy())
}

pub(super) struct Prompts<R, W> {
    pub(super) input: R,
    pub(super) output: W,
}
impl<R: BufRead, W: Write> Prompts<R, W> {
    pub(super) fn text(&mut self, label: &str, default: Option<&str>) -> Result<String> {
        if let Some(default) = default {
            write!(self.output, "{label} [{default}]: ")
        } else {
            write!(self.output, "{label}: ")
        }
        .and_then(|_| self.output.flush())
        .map_err(|error| usage(format!("cannot display setup prompt: {error}")))?;
        let mut line = String::new();
        // Bounded prompts never load arbitrary redirected input into memory.
        let read = self
            .input
            .by_ref()
            .take(4097)
            .read_line(&mut line)
            .map_err(|error| usage(format!("cannot read setup choice: {error}")))?;
        if read == 0 {
            return Err(WikiError::new(
                ErrorCode::Cancelled,
                "setup stopped because input closed; no further steps were applied",
            ));
        }
        if line.len() > 4096 {
            return Err(usage("setup choice exceeds 4096 bytes"));
        }
        let line = line.trim().to_string();
        Ok(if line.is_empty() {
            default.unwrap_or("").to_string()
        } else {
            line
        })
    }
    pub(super) fn yes(&mut self, label: &str, default: bool) -> Result<bool> {
        loop {
            let answer = self.text(label, Some(if default { "yes" } else { "no" }))?;
            match answer.to_ascii_lowercase().as_str() {
                "yes" | "y" => return Ok(true),
                "no" | "n" => return Ok(false),
                _ => writeln!(self.output, "Enter yes or no.")
                    .map_err(|error| usage(error.to_string()))?,
            }
        }
    }

    fn path(&mut self, label: &str, default: Option<&Path>) -> Result<PathBuf> {
        loop {
            let default = default.map(|path| path.to_string_lossy());
            let value = self.text(label, default.as_deref())?;
            let wrapped = ['\'', '"', '`'].into_iter().any(|delimiter| {
                value.len() > 1 && value.starts_with(delimiter) && value.ends_with(delimiter)
            });
            if wrapped && !Path::new(&value).exists() {
                writeln!(self.output, "Enter the path without surrounding quotes or backticks. Spaces are kept as typed. If those characters are part of a new folder name, supply the path with lwiki setup PATH.")
                    .map_err(|error| usage(error.to_string()))?;
                continue;
            }
            return Ok(PathBuf::from(value));
        }
    }
}

fn marker_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path.join("WIKI.md")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(usage(format!(
            "cannot inspect wiki folder {}: {error}",
            path.display()
        ))),
    }
}

/// Preflight before optional setup questions, then repeat before publishing files.
fn validate_folder(setup: &SetupArguments, args: &Arguments) -> Result<(DefaultWiki, bool)> {
    let path = setup.path.as_deref().expect("chosen path");
    if let Some(explicit) = &args.wiki
        && absolute(explicit)? != path
    {
        return Err(usage("setup PATH and --wiki must name the same folder"));
    }
    let timeout = args.lock_timeout_ms.unwrap_or(5000);
    if timeout > 30_000 {
        return Err(usage("setup writer timeout must be at most 30000 ms"));
    }
    let marker_exists = marker_exists(path)?;
    let wiki = if setup.select || marker_exists {
        let wiki = crate::vault::discovery::default_from_path(path).map_err(|mut error| {
            if !marker_exists
                && error.code == ErrorCode::VaultNotFound
                && std::fs::symlink_metadata(path)
                    .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            {
                error.hint = Some(format!(
                    "This folder is not initialized as a wiki. To keep its notes and add wiki files, run lwiki setup {} --existing. To select a wiki, choose a folder containing a valid WIKI.md.",
                    quote_path(path)
                ));
            }
            error
        })?;
        if args
            .expect_wiki_id
            .as_ref()
            .is_some_and(|id| *id != wiki.wiki_id)
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "selected wiki has a different ID from --expect-wiki-id",
            ));
        }
        wiki
    } else {
        if args.expect_wiki_id.is_some() {
            return Err(usage(
                "a new wiki has no existing ID to check; omit --expect-wiki-id or use --select",
            ));
        }
        let preview = crate::app::offline::init_with_existing(
            path,
            setup.title.as_deref().unwrap_or("My wiki"),
            setup.existing,
            OperationOptions {
                dry_run: true,
                offline: true,
                stage_only: false,
                lock_timeout_ms: timeout,
            },
        )?;
        DefaultWiki {
            path: PathBuf::from(preview.path),
            wiki_id: preview.id,
        }
    };
    Ok((wiki, marker_exists))
}

#[cfg(test)]
fn collect<R: BufRead, W: Write>(
    setup: &SetupArguments,
    args: &Arguments,
    interactive: bool,
    prompts: &mut Prompts<R, W>,
) -> Result<SetupArguments> {
    collect_loaded(setup, args, interactive, prompts).map(|(choices, _)| choices)
}
fn collect_loaded<R: BufRead, W: Write>(
    setup: &SetupArguments,
    args: &Arguments,
    interactive: bool,
    prompts: &mut Prompts<R, W>,
) -> Result<(SetupArguments, config::local::LoadedUserConfig)> {
    let mut choices = setup.clone();
    choices.path = choices.path.or_else(|| args.wiki.clone());
    if choices.path.is_none() {
        if !interactive {
            return Err(usage(
                "setup needs a wiki folder when input is not interactive; run lwiki setup PATH --non-interactive, or lwiki setup PATH --select --non-interactive for an existing wiki",
            ));
        }
        loop {
            let action = prompts.text(
                "Create a wiki or use an existing wiki? (create/use)",
                Some("create"),
            )?;
            match action.to_ascii_lowercase().as_str() {
                "create" => break,
                "use" | "select" => {
                    choices.select = true;
                    break;
                }
                _ => writeln!(prompts.output, "Enter create or use.")
                    .map_err(|error| usage(error.to_string()))?,
            }
        }
        let default = home()?.join("Wiki");
        choices.path = Some(prompts.path("Wiki folder", Some(&default))?);
    }
    let path = absolute(choices.path.as_deref().expect("chosen path"))?;
    choices.path = Some(path.clone());
    let marker_exists = marker_exists(&path)?;
    if interactive && !marker_exists {
        let exists = match std::fs::symlink_metadata(&path) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(usage(format!(
                    "cannot inspect wiki folder {}: {error}",
                    path.display()
                )));
            }
        };
        if exists {
            let mut preview = choices.clone();
            preview.select = false;
            preview.existing = true;
            validate_folder(&preview, args)?;
        } else if choices.select {
            validate_folder(&choices, args)?;
        }
        if exists && !choices.existing {
            choices.existing = prompts.yes("This folder is not a wiki yet. Add wiki folders and WIKI.md while keeping your notes?", false)?;
            if !choices.existing {
                return Err(WikiError::new(
                    ErrorCode::Cancelled,
                    "existing folder was not initialized; choose a new folder or rerun with --existing",
                ));
            }
        }
        choices.select = false;
        if choices.title.is_none() {
            choices.title = Some(prompts.text("Wiki title", Some("My wiki"))?);
        }
    }
    validate_folder(&choices, args)?;
    let loaded = config::local::load_user_config_for_setup(
        args.preferences.as_deref(),
        args.no_user_config,
    )?;
    if interactive {
        if !choices.no_default {
            choices.no_default =
                !prompts.yes("Use this wiki when running lwiki from other folders?", true)?;
        }
        if choices.agent.is_none() {
            loop {
                let agent = prompts
                    .text(
                        "Connect an agent (codex/Sagex/claude-code/cursor/skip)",
                        Some("skip"),
                    )?
                    .to_ascii_lowercase();
                match agent.as_str() {
                    "codex" | "sagex" | "claude-code" | "cursor" => {
                        choices.agent = Some(agent);
                        break;
                    }
                    "skip" => break,
                    _ => writeln!(
                        prompts.output,
                        "Choose codex, Sagex, claude-code, cursor or skip."
                    )
                    .map_err(|error| usage(error.to_string()))?,
                }
            }
        }
        if choices.agent.is_some() && choices.agent_directory.is_none() {
            let default = agent_directory_default(&choices)?;
            choices.agent_directory = Some(PathBuf::from(prompts.text(
                if choices.agent_scope.as_deref() == Some("project") {
                    "Project folder for agent instructions"
                } else {
                    "Agent's home folder"
                },
                Some(&default.to_string_lossy()),
            )?));
        }
        let current = current_provider_summary(args, &choices, &loaded)?;
        let embedding_current = current
            .as_ref()
            .map(|value| &value["embedding"])
            .filter(|value| value.is_object());
        let configure = embedding_requested(&choices)
            || prompts.yes(
                if embedding_current.is_some() {
                    "Change the embedding model for similarity search?"
                } else {
                    "Add an embedding model for similarity search?"
                },
                false,
            )?;
        if configure {
            if choices.embedding_model.is_none() {
                choices.embedding_model = Some(prompts.text(
                    "Embedding model name",
                    embedding_current.and_then(|value| value["model"].as_str()),
                )?);
            }
            if choices.embedding_url.is_none() {
                let value = prompts.text(
                    if embedding_current.is_some() {
                        "Full embeddings-v1 endpoint URL (leave empty to keep it)"
                    } else {
                        "Full embeddings-v1 endpoint URL"
                    },
                    None,
                )?;
                if !value.is_empty() {
                    choices.embedding_url = Some(value);
                }
            }
            if let Some(url) = &choices.embedding_url {
                embedding_destination(url)?;
            }
            if choices.embedding_dimensions.is_none() && embedding_current.is_none() {
                let dimensions = prompts.text(
                    "Vector dimensions (number, auto, or empty for provider default)",
                    None,
                )?;
                if !dimensions.is_empty() {
                    choices.embedding_dimensions = Some(dimensions);
                }
            }
            collect_auth(&mut choices, false, embedding_current.is_some(), prompts)?;
        }
        if !configure
            && !choices.auto_embed
            && let Some(summary) = current.as_ref().filter(|_| embedding_current.is_some())
            && summary["automatic_embedding_consent"] != json!("matching")
        {
            let (wiki, _) = validate_folder(&choices, args)?;
            choices.auto_embed = review_automatic_summary(summary, &wiki, prompts)?;
        }
        let generation_current = current
            .as_ref()
            .map(|value| &value["generation"])
            .filter(|value| value.is_object());
        let configure_generation = generation_requested(&choices)
            || prompts.yes(
                if generation_current.is_some() {
                    "Change the generation model for API extraction?"
                } else {
                    "Add a generation model for API extraction? Your agent can extract without one."
                },
                false,
            )?;
        if configure_generation {
            if choices.generation_model.is_none() {
                choices.generation_model = Some(prompts.text(
                    "Generation model name",
                    generation_current.and_then(|value| value["model"].as_str()),
                )?);
            }
            if choices.generation_url.is_none() {
                let value = prompts.text(
                    if generation_current.is_some() {
                        "Full generation endpoint URL (leave empty to keep it)"
                    } else {
                        "Full generation endpoint URL"
                    },
                    None,
                )?;
                if !value.is_empty() {
                    choices.generation_url = Some(value);
                }
            }
            if let Some(url) = &choices.generation_url {
                embedding_destination(url)?;
            }
            if choices.generation_adapter.is_none() && generation_current.is_none() {
                loop {
                    let value = prompts.text(
                        "Generation API format (responses-v1/chat-completions-v1)",
                        Some("responses-v1"),
                    )?;
                    if matches!(value.as_str(), "responses-v1" | "chat-completions-v1") {
                        choices.generation_adapter = Some(value);
                        break;
                    }
                    writeln!(
                        prompts.output,
                        "Choose responses-v1 or chat-completions-v1."
                    )
                    .map_err(|error| usage(error.to_string()))?;
                }
            }
            if !generation_auth_supplied(&choices) && generation_current.is_none() {
                let embedding_host = choices
                    .embedding_url
                    .as_deref()
                    .map(embedding_destination)
                    .transpose()?
                    .or_else(|| embedding_current.map(|value| value["destination"].clone()));
                let generation_host = choices
                    .generation_url
                    .as_deref()
                    .map(embedding_destination)
                    .transpose()?
                    .or_else(|| generation_current.map(|value| value["destination"].clone()));
                if let (Some(embedding_host), Some(generation_host)) =
                    (embedding_host, generation_host)
                {
                    writeln!(
                        prompts.output,
                        "Embedding destination: {}. Generation destination: {}.",
                        super::config_show::destination_label(&embedding_host),
                        super::config_show::destination_label(&generation_host)
                    )
                    .map_err(|error| usage(error.to_string()))?;
                    choices.generation_reuse_auth = prompts.yes(
                        "Use the same credentials for these two destinations?",
                        false,
                    )?;
                }
            }
            if !choices.generation_reuse_auth {
                collect_auth(&mut choices, true, generation_current.is_some(), prompts)?;
            }
        }
    }
    Ok((choices, loaded))
}

fn embedding_auth_supplied(choices: &SetupArguments) -> bool {
    choices.key_env.is_some()
        || choices.key_file.is_some()
        || choices.prompt_key
        || choices.credential_command.is_some()
}
fn generation_auth_supplied(choices: &SetupArguments) -> bool {
    choices.generation_key_env.is_some()
        || choices.generation_key_file.is_some()
        || choices.generation_prompt_key
        || choices.generation_credential_command.is_some()
        || choices.generation_reuse_auth
}
fn embedding_requested(choices: &SetupArguments) -> bool {
    choices.embedding_model.is_some()
        || choices.embedding_url.is_some()
        || choices.embedding_dimensions.is_some()
        || embedding_auth_supplied(choices)
}
fn generation_requested(choices: &SetupArguments) -> bool {
    choices.generation_model.is_some()
        || choices.generation_url.is_some()
        || choices.generation_adapter.is_some()
        || choices.generation_max_output_tokens.is_some()
        || generation_auth_supplied(choices)
}

fn current_provider_summary(
    args: &Arguments,
    setup: &SetupArguments,
    loaded: &config::local::LoadedUserConfig,
) -> Result<Option<Value>> {
    let (wiki, existing) = validate_folder(setup, args)?;
    let profile = effective_profile(args, loaded, &wiki, existing)?;
    let Some(profile) = profile else {
        return Ok(None);
    };
    let migration = loaded.toml_migration_target();
    let path = setup
        .providers_file
        .clone()
        .or_else(|| {
            loaded
                .document()
                .saved_provider_config(Some(&wiki.wiki_id))
                .map(|(path, _)| path.to_path_buf())
        })
        .or_else(|| {
            migration
                .as_deref()
                .or(loaded.path())
                .and_then(Path::parent)
                .map(|parent| parent.join("providers.toml"))
        });
    let Some(path) = path else {
        return Ok(None);
    };
    let snapshot = config::setup_io::load_private_file(
        &absolute(&path)?,
        config::providers::MAX_PROVIDER_CONFIG_BYTES,
    )?;
    if !snapshot.present() {
        return Ok(None);
    }
    let plan = provider_edit::plan_provider_edit(
        snapshot.bytes(),
        &wiki,
        Some(&profile),
        &[],
        ProviderGrantDecision::Preserve,
    )?;
    Ok(Some(provider_summary(&plan)))
}

fn collect_auth<R: BufRead, W: Write>(
    choices: &mut SetupArguments,
    generation: bool,
    existing: bool,
    prompts: &mut Prompts<R, W>,
) -> Result<()> {
    if if generation {
        generation_auth_supplied(choices)
    } else {
        embedding_auth_supplied(choices)
    } {
        return Ok(());
    }
    loop {
        let value = prompts.text(
            "Credentials (command/environment/private file/save privately/keep)",
            Some(if existing { "keep" } else { "command" }),
        )?;
        match value.as_str() {
            "keep" if existing => return Ok(()),
            "command" => {
                writeln!(
                    prompts.output,
                    "Use one program and its arguments. Shell pipes and substitutions do not run."
                )
                .map_err(|error| usage(error.to_string()))?;
                let text = loop {
                    let text = prompts.text("Credential command", None)?;
                    match command_auth(&text, CredentialOutput::Text) {
                        Ok(_) => break text,
                        Err(error) => writeln!(prompts.output, "{}", error.message)
                            .map_err(|error| usage(error.to_string()))?,
                    }
                };
                let output = loop {
                    let output = prompts.text("Command output (text/json)", Some("text"))?;
                    match output.as_str() {
                        "text" => break CredentialOutput::Text,
                        "json" => break CredentialOutput::Json,
                        _ => writeln!(
                            prompts.output,
                            "Choose text for a token, or json for key and optional expires_at."
                        )
                        .map_err(|error| usage(error.to_string()))?,
                    }
                };
                if generation {
                    choices.generation_credential_command = Some(text);
                    choices.generation_credential_output = Some(output);
                } else {
                    choices.credential_command = Some(text);
                    choices.credential_output = Some(output);
                }
                return Ok(());
            }
            "environment" | "env" => {
                let name = prompts.text("Environment variable name", None)?;
                if generation {
                    choices.generation_key_env = Some(name);
                } else {
                    choices.key_env = Some(name);
                }
                return Ok(());
            }
            "private file" | "file" => {
                let path = prompts.path("Existing private key file", None)?;
                if generation {
                    choices.generation_key_file = Some(path);
                } else {
                    choices.key_file = Some(path);
                }
                return Ok(());
            }
            "save privately" | "save" => {
                if generation {
                    choices.generation_prompt_key = true;
                } else {
                    choices.prompt_key = true;
                }
                return Ok(());
            }
            _ => writeln!(
                prompts.output,
                "Choose command, environment, private file{}.",
                if existing {
                    ", save privately or keep"
                } else {
                    " or save privately"
                }
            )
            .map_err(|error| usage(error.to_string()))?,
        }
    }
}

pub(super) fn review_provider_permission<R: BufRead, W: Write>(
    plan: &ProviderEditPlan,
    prompts: &mut Prompts<R, W>,
) -> Result<bool> {
    present_providers(&provider_summary(plan), &mut prompts.output)
        .map_err(|error| usage(error.to_string()))?;
    prompts.yes("Permit explicit API requests for all models and destinations shown above? They can send the described text and cost money.", false)
}

pub(super) fn review_automatic_permission<R: BufRead, W: Write>(
    plan: &ProviderEditPlan,
    wiki: &DefaultWiki,
    prompts: &mut Prompts<R, W>,
) -> Result<bool> {
    review_automatic_summary(&provider_summary(plan), wiki, prompts)
}

fn review_automatic_summary<R: BufRead, W: Write>(
    summary: &Value,
    wiki: &DefaultWiki,
    prompts: &mut Prompts<R, W>,
) -> Result<bool> {
    let embedding = &summary["embedding"];
    writeln!(prompts.output,
        "Automatic similarity search for {} uses {} at {}. It can send eligible wiki text after setup and saves, and your search questions. Requests can cost money. Offline, --no-sync and --dry-run skip automatic requests.",
        wiki.path.display(), embedding["model"].as_str().unwrap_or("the selected model"),
        super::config_show::destination_label(&embedding["destination"]))
        .map_err(|error| usage(error.to_string()))?;
    prompts.yes(
        "Allow automatic similarity search for this wiki and embedding model?",
        false,
    )
}

fn collect_setup_auth(
    choices: &mut SetupArguments,
    generation: bool,
    parent: &Path,
    snapshots: &mut Vec<(bool, config::setup_io::PrivateFileSnapshot, bool)>,
) -> Result<ProviderAuthPatch> {
    let (environment, file, prompt, command, output, reuse) = if generation {
        (
            choices.generation_key_env.clone(),
            choices.generation_key_file.clone(),
            choices.generation_prompt_key,
            choices.generation_credential_command.clone(),
            choices.generation_credential_output,
            choices.generation_reuse_auth,
        )
    } else {
        (
            choices.key_env.clone(),
            choices.key_file.clone(),
            choices.prompt_key,
            choices.credential_command.clone(),
            choices.credential_output,
            false,
        )
    };
    if [
        environment.is_some(),
        file.is_some(),
        prompt,
        command.is_some(),
        reuse,
    ]
    .into_iter()
    .filter(|value| *value)
    .count()
        > 1
    {
        return Err(usage("choose one credential source for each model"));
    }
    if reuse {
        return Ok(ProviderAuthPatch::CopyFrom(ProviderRole::Embedding));
    }
    if let Some(command) = command {
        return Ok(ProviderAuthPatch::Replace(command_auth(
            &command,
            output.unwrap_or_default(),
        )?));
    }
    if let Some(name) = environment {
        return Ok(ProviderAuthPatch::Replace(SetupAuth::Environment(name)));
    }
    if let Some(file) = file {
        let path = absolute(&file)?;
        if generation {
            choices.generation_key_file = Some(path.clone());
        } else {
            choices.key_file = Some(path.clone());
        }
        return Ok(ProviderAuthPatch::Replace(SetupAuth::PrivateFile(path)));
    }
    if prompt {
        let (destination, replace, model, url) = if generation {
            (
                choices.generation_credential_file.clone(),
                choices.generation_replace_key,
                choices.generation_model.as_deref(),
                choices.generation_url.as_deref(),
            )
        } else {
            (
                choices.credential_file.clone(),
                choices.replace_key,
                choices.embedding_model.as_deref(),
                choices.embedding_url.as_deref(),
            )
        };
        let role = if generation {
            "generation"
        } else {
            "embedding"
        };
        let hash = blake3::hash(
            format!("{role}\n{}\n{}", model.unwrap_or(""), url.unwrap_or("")).as_bytes(),
        )
        .to_hex()
        .to_string();
        let path = absolute(
            &destination.unwrap_or_else(|| parent.join(format!("{role}-key-{}", &hash[..16]))),
        )?;
        let snapshot =
            config::setup_io::load_private_file(&path, config::providers::MAX_SECRET_BYTES)?;
        if snapshot.present() && !replace {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "private key destination exists; reuse it with --key-file, choose another credential file, or explicitly replace it",
            ));
        }
        if snapshots
            .iter()
            .any(|(_, existing, _)| existing.path() == snapshot.path())
        {
            return Err(usage(
                "two saved keys need different private file destinations; choose separate paths or reuse the embedding credentials",
            ));
        }
        if generation {
            choices.generation_credential_file = Some(snapshot.path().to_path_buf());
        } else {
            choices.credential_file = Some(snapshot.path().to_path_buf());
        }
        let auth = SetupAuth::PrivateFile(snapshot.path().to_path_buf());
        snapshots.push((generation, snapshot, replace));
        return Ok(ProviderAuthPatch::Replace(auth));
    }
    Ok(ProviderAuthPatch::Preserve)
}
fn clear_provider_choices(choices: &mut SetupArguments) {
    choices.embedding_model = None;
    choices.embedding_url = None;
    choices.embedding_dimensions = None;
    choices.key_env = None;
    choices.key_file = None;
    choices.prompt_key = false;
    choices.credential_command = None;
    choices.credential_output = None;
    choices.credential_file = None;
    choices.replace_key = false;
    choices.generation_model = None;
    choices.generation_url = None;
    choices.generation_adapter = None;
    choices.generation_max_output_tokens = None;
    choices.generation_key_env = None;
    choices.generation_key_file = None;
    choices.generation_prompt_key = false;
    choices.generation_credential_command = None;
    choices.generation_credential_output = None;
    choices.generation_credential_file = None;
    choices.generation_replace_key = false;
    choices.generation_reuse_auth = false;
    choices.grant_provider = false;
    choices.grant_embed = false;
    choices.auto_embed = false;
}

#[derive(Clone, Serialize)]
struct Step {
    name: &'static str,
    destination: Option<PathBuf>,
    status: &'static str,
    applied: bool,
    durable: bool,
    message: String,
}
fn step(name: &'static str, destination: Option<PathBuf>, message: impl Into<String>) -> Step {
    Step {
        name,
        destination,
        status: "pending",
        applied: false,
        durable: false,
        message: message.into(),
    }
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

fn retry_command(args: &Arguments, setup: &SetupArguments, configuration: Option<&Path>) -> String {
    retry_command_profile(args, setup, configuration, args.profile.as_deref())
}
fn retry_command_profile(
    args: &Arguments,
    setup: &SetupArguments,
    configuration: Option<&Path>,
    profile: Option<&str>,
) -> String {
    let mut command = String::from("lwiki");
    if args.preferences.is_some()
        && let Some(path) = configuration
    {
        command += &format!(" --preferences {}", quote_path(path));
    }
    if args.no_user_config {
        command += " --no-user-config";
    }
    if let Some(profile) = profile {
        command += &format!(" --profile {}", quote(profile));
    }
    if let Some(timeout) = args.lock_timeout_ms {
        command += &format!(" --lock-timeout-ms {timeout}");
    }
    if let Some(id) = &args.expect_wiki_id {
        command += &format!(" --expect-wiki-id {}", quote(id.as_str()));
    }
    command += &format!(
        " setup {}",
        quote_path(setup.path.as_deref().expect("chosen path"))
    );
    if !setup.prompt_key && !setup.generation_prompt_key {
        command += " --non-interactive";
    }
    if setup.select {
        command += " --select";
    }
    if setup.existing {
        command += " --existing";
    }
    if setup.no_default {
        command += " --no-default";
    }
    if let Some(title) = &setup.title {
        command += &format!(" --title {}", quote(title));
    }
    if let Some(agent) = &setup.agent {
        command += &format!(" --agent {}", quote(agent));
    }
    if let Some(scope) = &setup.agent_scope {
        command += &format!(" --agent-scope {}", quote(scope));
    }
    if let Some(path) = &setup.agent_directory {
        command += &format!(" --agent-directory {}", quote_path(path));
    }
    for (flag, value) in [
        ("--embedding-model", &setup.embedding_model),
        ("--embedding-dimensions", &setup.embedding_dimensions),
        ("--key-env", &setup.key_env),
        ("--generation-model", &setup.generation_model),
        ("--generation-adapter", &setup.generation_adapter),
        ("--generation-key-env", &setup.generation_key_env),
        (
            "--embedding-document-prefix",
            &setup.embedding_document_prefix,
        ),
        ("--embedding-query-prefix", &setup.embedding_query_prefix),
    ] {
        if let Some(value) = value {
            command += &format!(" {flag} {}", quote(value));
        }
    }
    for (flag, path) in [
        ("--key-file", &setup.key_file),
        ("--providers-file", &setup.providers_file),
        ("--credential-file", &setup.credential_file),
        ("--generation-key-file", &setup.generation_key_file),
        (
            "--generation-credential-file",
            &setup.generation_credential_file,
        ),
    ] {
        if let Some(path) = path {
            command += &format!(" {flag} {}", quote_path(path));
        }
    }
    // Retrying before provider publication needs the original private endpoint/command.
    // Placeholders keep argv, paths and query values out of ordinary reports.
    for (flag, supplied) in [
        ("--embedding-url", setup.embedding_url.is_some()),
        ("--generation-url", setup.generation_url.is_some()),
        ("--credential-command", setup.credential_command.is_some()),
        (
            "--generation-credential-command",
            setup.generation_credential_command.is_some(),
        ),
    ] {
        if supplied {
            command += &format!(
                " {flag} {}",
                quote(if flag.contains("command") {
                    "<repeat your credential command>"
                } else {
                    "<repeat your full endpoint URL>"
                })
            );
        }
    }
    for (flag, value) in [
        ("--credential-output", setup.credential_output),
        (
            "--generation-credential-output",
            setup.generation_credential_output,
        ),
    ] {
        if let Some(value) = value {
            command += &format!(
                " {flag} {}",
                if value == CredentialOutput::Json {
                    "json"
                } else {
                    "text"
                }
            );
        }
    }
    if setup.generation_prompt_key {
        command += " --generation-prompt-key";
    }
    if setup.generation_replace_key {
        command += " --generation-replace-key";
    }
    if setup.generation_reuse_auth {
        command += " --generation-reuse-auth";
    }
    if setup.grant_provider {
        command += " --grant-provider";
    }
    if let Some(value) = setup.generation_max_output_tokens {
        command += &format!(" --generation-max-output-tokens {value}");
    }
    if setup.prompt_key {
        command += " --prompt-key";
    }
    if setup.replace_key {
        command += " --replace-key";
    }
    if setup.grant_embed {
        command += " --grant-embed";
    }
    if setup.auto_embed {
        command += " --auto-embed";
    }
    if let Some(value) = setup.embedding_max_input_bytes {
        command += &format!(" --embedding-max-input-bytes {value}");
    }
    if let Some(value) = setup.embedding_quality_target_bytes {
        command += &format!(" --embedding-quality-target-bytes {value}");
    }
    command
}

fn next_steps(
    args: &Arguments,
    setup: &SetupArguments,
    path: &Path,
    configuration: Option<&Path>,
) -> Vec<String> {
    let mut prefix = String::from("lwiki");
    if args.preferences.is_some()
        && let Some(preferences) = configuration
    {
        prefix += &format!(" --preferences {}", quote_path(preferences));
    }
    if args.no_user_config {
        prefix += " --no-user-config";
    }
    if setup.no_default {
        prefix += &format!(" --wiki {}", quote_path(path));
    }
    let mut result = Vec::new();
    if let Some(agent) = &setup.agent {
        let agent = agent_label(agent);
        result.push(format!(
            "Restart your {agent} session so it can find the installed llm-wiki instructions."
        ));
        result.push(format!("Ask {agent}: Use llm-wiki with my wiki at {}. Capture the notes I choose, then answer my question with citations. Give the agent your notes or folder and your actual question.", path.display()));
        if args.preferences.is_some() || setup.no_default || args.no_user_config {
            result.push(format!(
                "Tell your agent to use this command prefix: {prefix}"
            ));
        }
    } else {
        result.push("Connect your agent later by rerunning setup with --agent codex, --agent sagex, --agent claude-code or --agent cursor.".into());
    }
    result.push(format!("Capture one of your notes: {prefix} --offline source add '/absolute/path/to/your-note.md' --title 'Your note title'"));
    result.push(format!(
        "Find its facts: {prefix} --offline context 'your question about that note'"
    ));
    result.push(format!("Inspect saved settings: {prefix} config show"));
    result
}

fn finish(
    plan: &Value,
    steps: &[Step],
    retry: &str,
    next: &[String],
    wiki: &DefaultWiki,
    dry_run: bool,
    failure: Option<WikiError>,
) -> Envelope {
    let partial = !dry_run && failure.is_some() && steps.iter().any(|step| step.applied);
    let mut envelope = Envelope::success(
        "setup",
        json!({
            "plan":plan, "steps":steps,
            "completion": if failure.is_some() { "incomplete" } else if dry_run { "preview" } else { "complete" },
            "retry":retry, "next_steps":next,
        }),
    );
    if let Some(error) = failure {
        envelope.ok = false;
        envelope.error = Some(ErrorOutput {
            code: error.code.to_string(),
            message: error.message,
            retryable: error.retryable,
            hint: Some(format!("Review the failed step, then retry: {retry}")),
            details: error.details,
        });
    }
    envelope.meta.partial = partial;
    if !dry_run {
        envelope.meta.wiki_id = Some(wiki.wiki_id.to_string());
    }
    envelope
}

/// Save local setup, then optionally prepare the explicitly permitted embedding model.
pub fn execute(args: &Arguments, setup: &SetupArguments) -> Result<Envelope> {
    if args.stage {
        return Err(usage(
            "setup cannot be staged; use --dry-run to preview destinations",
        ));
    }
    let interactive = !setup.non_interactive
        && args.output_format() == OutputFormat::Human
        && io::stdin().is_terminal()
        && io::stderr().is_terminal();
    let mut prompts = Prompts {
        input: io::stdin().lock(),
        output: io::stderr().lock(),
    };
    let (mut choices, loaded) = collect_loaded(setup, args, interactive, &mut prompts)?;
    let requested = choices.path.as_deref().expect("chosen path");
    if let Some(explicit) = &args.wiki
        && absolute(explicit)? != requested
    {
        return Err(usage("setup PATH and --wiki must name the same folder"));
    }
    if choices.agent.is_none()
        && (choices.agent_directory.is_some() || choices.agent_scope.is_some())
    {
        return Err(usage(
            "--agent-directory and --agent-scope require a chosen --agent",
        ));
    }
    let timeout = args.lock_timeout_ms.unwrap_or(5000);
    if timeout > 30_000 {
        return Err(usage("setup writer timeout must be at most 30000 ms"));
    }
    let config_target = loaded.toml_migration_target();
    let config_path = config_target.as_deref().or(loaded.path());
    if !choices.no_default {
        config::local::require_write_target(&loaded)?;
    }
    let provider_requested = embedding_requested(&choices)
        || generation_requested(&choices)
        || choices.grant_embed
        || choices.auto_embed
        || choices.grant_provider
        || choices.providers_file.is_some()
        || choices.embedding_document_prefix.is_some()
        || choices.embedding_query_prefix.is_some()
        || choices.embedding_max_input_bytes.is_some()
        || choices.embedding_quality_target_bytes.is_some();
    if (choices.prompt_key || choices.generation_prompt_key)
        && !args.dry_run
        && (choices.non_interactive || !io::stdin().is_terminal() || !io::stderr().is_terminal())
    {
        return Err(usage(
            "saving a new key requires interactive terminal input; use a credential command, environment variable or private key file with --non-interactive",
        ));
    }
    if choices.grant_embed && choices.grant_provider {
        return Err(usage("choose --grant-provider or --grant-embed, not both"));
    }
    if choices.agent.is_some() {
        let directory = choices
            .agent_directory
            .clone()
            .map(Ok)
            .unwrap_or_else(|| agent_directory_default(&choices))?;
        choices.agent_directory = Some(absolute(&directory)?);
    }
    let (mut wiki, marker_exists) = validate_folder(&choices, args)?;
    let mut proposed = loaded.document().clone();
    if !choices.no_default {
        proposed.default_wiki = Some(wiki.clone());
    }
    let selected_profile = effective_profile(args, &loaded, &wiki, marker_exists)?;
    let prior_saved_profile = saved_profile(&loaded, &wiki, marker_exists)?;
    let mut provider_snapshot = None;
    let mut credential_snapshots = Vec::new();
    let mut provider_document = None;
    let mut provider_write_needed = false;
    let mut patches = Vec::new();
    let mut providers_preview = None;
    let mut grant = if choices.grant_provider {
        ProviderGrantDecision::ReviewedAllRoles
    } else if choices.grant_embed {
        ProviderGrantDecision::EmbeddingOnly
    } else {
        ProviderGrantDecision::Preserve
    };
    if provider_requested {
        let parent = config_path.and_then(Path::parent).ok_or_else(|| usage("model setup needs saved user settings; omit --no-user-config or provide --preferences FILE"))?;
        let provider_path = absolute(
            &choices
                .providers_file
                .clone()
                .or_else(|| {
                    proposed
                        .saved_provider_config(Some(&wiki.wiki_id))
                        .map(|(path, _)| path.to_path_buf())
                })
                .unwrap_or_else(|| parent.join("providers.toml")),
        )?;
        let snapshot = config::setup_io::load_private_file(
            &provider_path,
            config::providers::MAX_PROVIDER_CONFIG_BYTES,
        )?;
        choices.providers_file = Some(snapshot.path().to_path_buf());
        if embedding_requested(&choices) {
            let mut patch = ProviderRolePatch::new(ProviderRole::Embedding);
            patch.model = choices.embedding_model.clone();
            patch.url = choices.embedding_url.clone();
            patch.dimensions = choices.embedding_dimensions.clone();
            patch.auth =
                collect_setup_auth(&mut choices, false, parent, &mut credential_snapshots)?;
            patches.push(patch);
        }
        if generation_requested(&choices) {
            let mut patch = ProviderRolePatch::new(ProviderRole::Generation);
            patch.model = choices.generation_model.clone();
            patch.url = choices.generation_url.clone();
            patch.adapter = choices
                .generation_adapter
                .as_deref()
                .map(adapter)
                .transpose()?;
            patch.max_output_tokens = choices.generation_max_output_tokens;
            patch.auth = collect_setup_auth(&mut choices, true, parent, &mut credential_snapshots)?;
            patches.push(patch);
        }
        if selected_profile.is_none() {
            if embedding_requested(&choices)
                && (choices.embedding_model.is_none() || choices.embedding_url.is_none())
            {
                return Err(usage(
                    "new embedding setup needs --embedding-model MODEL and --embedding-url URL",
                ));
            }
            if generation_requested(&choices)
                && (choices.generation_model.is_none() || choices.generation_url.is_none())
            {
                return Err(usage(
                    "new generation setup needs --generation-model MODEL and --generation-url URL",
                ));
            }
        }
        let mut automatic = if choices.auto_embed {
            ProviderAutomaticEmbeddingDecision::Reviewed
        } else {
            ProviderAutomaticEmbeddingDecision::Preserve
        };
        let mut document = provider_edit::plan_provider_edit_with_automatic(
            snapshot.bytes(),
            &wiki,
            selected_profile.as_deref(),
            &patches,
            grant,
            automatic,
        )?;
        if interactive
            && !choices.auto_embed
            && document.new_embedding_identity.is_some()
            && document.automatic_embedding_consent != AutomaticEmbeddingConsent::Matching
            && review_automatic_permission(&document, &wiki, &mut prompts)?
        {
            choices.auto_embed = true;
            automatic = ProviderAutomaticEmbeddingDecision::Reviewed;
            document = provider_edit::plan_provider_edit_with_automatic(
                snapshot.bytes(),
                &wiki,
                selected_profile.as_deref(),
                &patches,
                grant,
                automatic,
            )?;
        }
        if interactive && !document.configuration_consent_satisfied {
            if review_provider_permission(&document, &mut prompts)? {
                choices.grant_provider = true;
                choices.grant_embed = false;
                grant = ProviderGrantDecision::ReviewedAllRoles;
                document = provider_edit::plan_provider_edit_with_automatic(
                    snapshot.bytes(),
                    &wiki,
                    selected_profile.as_deref(),
                    &patches,
                    grant,
                    automatic,
                )?;
            } else if !args.dry_run {
                return Err(WikiError::new(
                    ErrorCode::Cancelled,
                    "model changes were not applied; rerun setup without model choices to keep local use, or review them with --grant-provider",
                ));
            }
        }
        if !args.dry_run && !document.configuration_consent_satisfied {
            let mut error = usage(
                "model changes need permission; review the --dry-run preview, then rerun with --grant-provider (or --grant-embed for embedding-only changes)",
            );
            error.details = json!({"providers":provider_summary(&document),"writes":0});
            return Err(error);
        }
        provider_write_needed = config::setup_io::write_private_file(
            &snapshot,
            document.proposed_bytes(),
            true,
            true,
            timeout,
        )?
        .changed;
        for (_, key_snapshot, replace) in &credential_snapshots {
            config::setup_io::write_private_file(
                key_snapshot,
                b"setup preview only",
                *replace,
                true,
                timeout,
            )?;
        }
        let selection_changed = save_provider_selection(
            &mut proposed,
            &wiki,
            prior_saved_profile.as_deref(),
            document.profile(),
            snapshot.path(),
        )?;
        if selection_changed
            && !proposed.allowed_profiles.is_empty()
            && !proposed
                .allowed_profiles
                .iter()
                .any(|profile| profile == document.profile())
        {
            proposed.allowed_profiles.push(document.profile().into());
        }
        if let Some(value) = &choices.embedding_document_prefix {
            proposed.embeddings.document_prefix = Some(value.clone());
        }
        if let Some(value) = &choices.embedding_query_prefix {
            proposed.embeddings.query_prefix = Some(value.clone());
        }
        if let Some(value) = choices.embedding_max_input_bytes {
            proposed.embeddings.max_input_bytes = Some(value);
        }
        if let Some(value) = choices.embedding_quality_target_bytes {
            proposed.embeddings.quality_target_bytes = Some(value);
        }
        providers_preview = Some(provider_summary(&document));
        provider_snapshot = Some(snapshot);
        provider_document = Some(document);
    } else if selected_profile.is_some() {
        providers_preview = current_provider_summary(args, &choices, &loaded)?;
    }
    let writes_config = !choices.no_default || provider_requested;
    let configuration_write_needed = if writes_config {
        config::local::update_document(
            &loaded,
            proposed.clone(),
            config_target.as_deref(),
            true,
            timeout,
        )?
        .changed
    } else {
        false
    };
    let agent_preview = if let Some(agent) = &choices.agent {
        Some(super::skill_install::operate(
            "install",
            agent_install_target(agent),
            choices.agent_directory.as_deref().expect("agent home"),
            choices.agent_scope.as_deref().unwrap_or("user"),
            true,
        )?)
    } else {
        None
    };
    let embedding_preview = providers_preview
        .as_ref()
        .map(|value| value["embedding"].clone())
        .filter(Value::is_object);
    let generation_preview = providers_preview
        .as_ref()
        .map(|value| value["generation"].clone())
        .filter(Value::is_object);
    let plan = json!({"wiki":wiki.path,"wiki_action":if marker_exists || choices.select { "select" } else { "create" }, "initialize_existing_folder":choices.existing,
        "save_default":!choices.no_default,"configuration":if writes_config { config_path } else { None },
        "provider_configuration":choices.providers_file,"credential_file":choices.credential_file,
        "embedding":embedding_preview,"generation":generation_preview,"providers":providers_preview,
        "generation_credential_file":choices.generation_credential_file,
        "agent":choices.agent,"agent_installation":agent_preview,"configuration_network_requests":0,
        "automatic_embeddings":super::automatic_embeddings::preview(
            choices.auto_embed || providers_preview.as_ref().is_some_and(|summary| summary["automatic_embedding_consent"] == "matching"),
            crate::app::embeddings::PreparationTrigger::Setup, &proposed)});
    if args.output_format() == OutputFormat::Human {
        writeln!(prompts.output, "Wiki folder: {}", wiki.path.display())
            .map_err(|error| usage(error.to_string()))?;
        if let Some(path) = config_path.filter(|_| writes_config) {
            writeln!(prompts.output, "Saved settings: {}", path.display())
                .map_err(|error| usage(error.to_string()))?;
        }
        if let Some(path) = &choices.providers_file {
            writeln!(
                prompts.output,
                "Private provider settings: {}",
                path.display()
            )
            .map_err(|error| usage(error.to_string()))?;
        }
        if let Some(path) = &choices.credential_file {
            writeln!(prompts.output, "Private key file: {}", path.display())
                .map_err(|error| usage(error.to_string()))?;
        }
        if let Some(providers) = &providers_preview {
            present_providers(providers, &mut prompts.output)
                .map_err(|error| usage(error.to_string()))?;
        }
        if let Some(preview) = &agent_preview {
            if let Some(destination) = preview.get("destination").and_then(Value::as_str) {
                let label = if choices.agent.as_deref() == Some("sagex") {
                    "Sagex instructions (shared agent skills folder)"
                } else {
                    "Agent instructions"
                };
                writeln!(prompts.output, "{label}: {destination}")
                    .map_err(|error| usage(error.to_string()))?;
            } else if let Some(directory) = &choices.agent_directory {
                writeln!(prompts.output, "Agent home: {}", directory.display())
                    .map_err(|error| usage(error.to_string()))?;
            }
        }
    }
    let mut retry_choices = choices.clone();
    let mut retry_configuration = loaded.path().map(Path::to_path_buf);
    let mut retry = retry_command(args, &retry_choices, retry_configuration.as_deref());
    let next = next_steps(
        args,
        &choices,
        &wiki.path,
        if writes_config {
            config_path
        } else {
            loaded.path()
        },
    );
    let mut steps = vec![step(
        "wiki",
        Some(wiki.path.clone()),
        if marker_exists || choices.select {
            "Select existing wiki"
        } else {
            "Create your wiki"
        },
    )];
    for (generation, snapshot, _) in &credential_snapshots {
        steps.push(step(
            if *generation {
                "generation_credential"
            } else {
                "credential"
            },
            Some(snapshot.path().to_path_buf()),
            "Save your key privately",
        ));
    }
    if let Some(snapshot) = &provider_snapshot {
        steps.push(step(
            "provider",
            Some(snapshot.path().to_path_buf()),
            "Save your model settings",
        ));
    }
    if writes_config {
        steps.push(step(
            "configuration",
            config_path.map(Path::to_path_buf),
            "Save your wiki choice and settings",
        ));
    }
    if choices.agent.is_some() {
        steps.push(step(
            "agent",
            choices.agent_directory.clone(),
            if choices.agent.as_deref() == Some("sagex") {
                "Install Sagex's wiki instructions in the shared agent skills folder"
            } else {
                "Install your chosen agent's wiki instructions"
            },
        ));
    }
    if args.dry_run {
        for step in &mut steps {
            step.status = "planned";
        }
        return Ok(finish(&plan, &steps, &retry, &next, &wiki, true, None));
    }
    if interactive && !prompts.yes("Apply these choices?", true)? {
        steps[0].status = "cancelled";
        return Ok(finish(
            &plan,
            &steps,
            &retry,
            &next,
            &wiki,
            false,
            Some(WikiError::new(
                ErrorCode::Cancelled,
                "setup cancelled before any writes",
            )),
        ));
    }
    drop(prompts);
    let result: Result<()> = (|| {
        if !marker_exists && !choices.select {
            let outcome = crate::app::offline::init_with_existing(
                &wiki.path,
                choices.title.as_deref().unwrap_or("My wiki"),
                choices.existing,
                OperationOptions {
                    dry_run: false,
                    offline: true,
                    stage_only: false,
                    lock_timeout_ms: timeout,
                },
            )
            .inspect_err(|error| {
                let state = &error.details["initialization"];
                steps[0].applied = state["applied"].as_bool().unwrap_or(false);
                steps[0].durable = state["durable"].as_bool().unwrap_or(false);
                if state["published"] == true
                    && let Some(id) = state["wiki_id"]
                        .as_str()
                        .and_then(|value| RecordId::new(value).ok())
                {
                    wiki.wiki_id = id;
                }
            })?;
            let old_id = wiki.wiki_id.clone();
            wiki = DefaultWiki {
                path: PathBuf::from(outcome.path),
                wiki_id: outcome.id,
            };
            steps[0].applied = outcome.created;
            steps[0].durable = outcome.created;
            steps[0].status = "complete";
            if !choices.no_default {
                proposed.default_wiki = Some(wiki.clone());
            }
            if let Some(preferences) = proposed.vaults.remove(&old_id) {
                proposed.vaults.insert(wiki.wiki_id.clone(), preferences);
            }
            if let Some(snapshot) = &provider_snapshot {
                provider_document = Some(provider_edit::plan_provider_edit_with_automatic(
                    snapshot.bytes(),
                    &wiki,
                    selected_profile.as_deref(),
                    &patches,
                    grant,
                    if choices.auto_embed {
                        ProviderAutomaticEmbeddingDecision::Reviewed
                    } else {
                        ProviderAutomaticEmbeddingDecision::Preserve
                    },
                )?);
            }
        } else if crate::vault::discovery::default_from_path(&wiki.path)? != wiki {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "selected wiki changed since setup preview",
            ));
        } else {
            crate::app::offline::verify_init_durability(&wiki.path)?;
            if crate::vault::discovery::default_from_path(&wiki.path)? != wiki {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "selected wiki changed while confirming setup",
                ));
            }
            steps[0].durable = true;
        }
        steps[0].status = "complete";
        checkpoint("wiki_complete", &wiki.path)?;
        let mut index = 1;
        for (generation, snapshot, replace) in &credential_snapshots {
            let key = rpassword::prompt_password(if *generation {
                "Generation provider key (hidden): "
            } else {
                "Embedding provider key (hidden): "
            })
            .map_err(|_| {
                WikiError::new(
                    ErrorCode::Cancelled,
                    "private key input stopped; key was not saved",
                )
            })?;
            if key.is_empty()
                || key.len() > config::providers::MAX_SECRET_BYTES
                || key.chars().any(char::is_control)
            {
                return Err(usage(
                    "key must be nonempty, at most 16384 bytes and contain no control characters; no key bytes are displayed",
                ));
            }
            let report = config::setup_io::write_private_file(
                snapshot,
                key.as_bytes(),
                *replace,
                false,
                timeout,
            )?;
            steps[index].applied = report.applied;
            steps[index].durable = report.durable;
            if report.applied || (!report.changed && report.durable) {
                if *generation {
                    retry_choices.generation_prompt_key = false;
                    retry_choices.generation_replace_key = false;
                    retry_choices.generation_key_file = Some(snapshot.path().to_path_buf());
                    retry_choices.generation_credential_file = None;
                } else {
                    retry_choices.prompt_key = false;
                    retry_choices.replace_key = false;
                    retry_choices.key_file = Some(snapshot.path().to_path_buf());
                    retry_choices.credential_file = None;
                }
                retry = retry_command(args, &retry_choices, retry_configuration.as_deref());
            }
            if let Some(failure) = report.failure {
                return Err(failure);
            }
            steps[index].status = "complete";
            index += 1;
            checkpoint("credential_complete", &wiki.path)?;
        }
        if let (Some(snapshot), Some(document)) = (&provider_snapshot, &provider_document) {
            let report = config::setup_io::write_private_file(
                snapshot,
                document.proposed_bytes(),
                true,
                !provider_write_needed,
                timeout,
            )?;
            steps[index].applied = report.applied;
            steps[index].durable = report.durable;
            if report.applied || !report.changed {
                // Published private settings provide an exact, secret-free retry.
                clear_provider_choices(&mut retry_choices);
                retry_choices.providers_file = Some(snapshot.path().to_path_buf());
                retry = retry_command_profile(
                    args,
                    &retry_choices,
                    retry_configuration.as_deref(),
                    Some(document.profile()),
                );
            }
            if let Some(failure) = report.failure {
                return Err(failure);
            }
            steps[index].status = "complete";
            index += 1;
            checkpoint("provider_complete", &wiki.path)?;
        }
        if writes_config {
            if let (Some(snapshot), Some(document)) = (&provider_snapshot, &provider_document) {
                recheck_provider_publication(snapshot.path(), document.proposed_bytes())?;
            }
            if crate::vault::discovery::default_from_path(&wiki.path)? != wiki {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "selected wiki changed before saving your settings",
                ));
            }
            let report = config::local::update_document(
                &loaded,
                proposed.clone(),
                config_target.as_deref(),
                !configuration_write_needed,
                timeout,
            )?;
            steps[index].applied = report.applied;
            steps[index].durable = report.durable;
            if report.applied || report.durable || !report.changed {
                retry_configuration = Some(report.configuration_path.clone());
                retry = retry_command_profile(
                    args,
                    &retry_choices,
                    retry_configuration.as_deref(),
                    provider_document
                        .as_ref()
                        .map(|document| document.profile())
                        .or(args.profile.as_deref()),
                );
            }
            if let Some(failure) = report.failure {
                return Err(failure);
            }
            steps[index].status = "complete";
            index += 1;
            checkpoint("configuration_complete", &wiki.path)?;
        }
        if let Some(agent) = &choices.agent {
            let report = super::skill_install::operate(
                "install",
                agent_install_target(agent),
                choices.agent_directory.as_deref().expect("agent home"),
                choices.agent_scope.as_deref().unwrap_or("user"),
                false,
            )?;
            steps[index].destination = report
                .get("destination")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .or_else(|| choices.agent_directory.clone());
            steps[index].applied = report
                .get("applied")
                .or_else(|| report.get("created"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            steps[index].durable = report
                .get("durable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(failure) = report.get("failure").filter(|failure| !failure.is_null()) {
                return Err(
                    serde_json::from_value::<WikiError>(failure.clone()).map_err(|_| {
                        WikiError::new(
                            ErrorCode::Internal,
                            "agent installer returned an invalid failure report",
                        )
                    })?,
                );
            }
            steps[index].status = "complete";
        }
        Ok(())
    })();
    if let Err(error) = result {
        if let Some(step) = steps.iter_mut().find(|step| step.status == "pending") {
            step.status = "failed";
            step.message = error.message.clone();
        }
        return Ok(finish(
            &plan,
            &steps,
            &retry,
            &next,
            &wiki,
            false,
            Some(error),
        ));
    }
    let mut envelope = finish(&plan, &steps, &retry, &next, &wiki, false, None);
    envelope.warnings.extend(loaded.summary().warnings.clone());
    super::automatic_embeddings::after_setup(
        args,
        &wiki,
        provider_document
            .as_ref()
            .map(|plan| plan.profile())
            .or(selected_profile.as_deref()),
        retry_configuration.as_deref(),
        &mut envelope,
    );
    Ok(envelope)
}

pub(crate) fn present_report(envelope: &Envelope, output: &mut impl Write) -> io::Result<()> {
    writeln!(
        output,
        "{}",
        match envelope.data["completion"].as_str() {
            Some("preview") => "Setup preview. No files were changed.",
            Some("complete") => "Your wiki is ready.",
            _ => "Setup is incomplete.",
        }
    )?;
    let plan = &envelope.data["plan"];
    if plan["providers"].is_object() {
        present_providers(&plan["providers"], output)?;
    }
    if let Some(steps) = envelope.data["steps"].as_array() {
        for step in steps {
            writeln!(
                output,
                "{}: {}{}",
                step["status"].as_str().unwrap_or("pending"),
                step["message"].as_str().unwrap_or("setup step"),
                step["destination"]
                    .as_str()
                    .map(|path| format!(" ({path})"))
                    .unwrap_or_default()
            )?;
            if step["applied"] == true && step["durable"] != true {
                writeln!(
                    output,
                    "The file changed, but lwiki could not confirm it was saved to disk. Inspect it before retrying."
                )?;
            }
        }
    }
    if let Some(error) = &envelope.error {
        writeln!(output, "{}: {}", error.code, error.message)?;
    }
    if !envelope.ok {
        writeln!(
            output,
            "Retry (replace any placeholders with your original private choices): {}",
            envelope.data["retry"]
                .as_str()
                .unwrap_or("lwiki setup --help")
        )?;
    } else if envelope.data["completion"] != "preview"
        && let Some(next) = envelope.data["next_steps"].as_array()
    {
        for next in next {
            if let Some(next) = next.as_str() {
                writeln!(output, "{next}")?;
            }
        }
    }
    for warning in &envelope.warnings {
        writeln!(output, "{warning}")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;

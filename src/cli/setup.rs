//! Guided creation of a persistent wiki, preferences and selected agent instructions.
use super::{Arguments, OutputFormat};
use crate::{
    app::OperationOptions,
    config::{
        self, DefaultWiki,
        provider_setup::{EmbeddingSetup, SetupAuth},
    },
    domain::*,
    output::{Envelope, ErrorOutput},
};
use clap::Args;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, Args)]
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
    /// Install wiki instructions for codex, claude-code or cursor.
    #[arg(long, value_parser = ["codex", "claude-code", "cursor"])]
    pub agent: Option<String>,
    /// Install for your user account (default) or one project.
    #[arg(long, value_parser = ["user", "project"])]
    pub agent_scope: Option<String>,
    /// Agent home or project folder. Defaults to HOME or the current folder for project scope.
    #[arg(long)]
    pub agent_directory: Option<PathBuf>,
    /// Optional embedding model name supplied by your provider; makes no requests.
    #[arg(long)]
    pub embedding_model: Option<String>,
    /// Your provider's full embeddings-v1 endpoint URL.
    #[arg(long)]
    pub embedding_url: Option<String>,
    /// Requested vector dimensions, from 1 to 65536, or auto.
    #[arg(long)]
    pub embedding_dimensions: Option<String>,
    /// Environment variable holding your key. Setup does not read its value.
    #[arg(long, conflicts_with_all = ["key_file", "prompt_key"])]
    pub key_env: Option<String>,
    /// Existing private key file. Setup stores only its absolute path.
    #[arg(long, conflicts_with = "prompt_key")]
    pub key_file: Option<PathBuf>,
    /// Read your key without displaying it and save it in a private file. Requires a terminal.
    #[arg(long)]
    pub prompt_key: bool,
    /// Private file destination for --prompt-key; defaults beside the provider configuration.
    #[arg(long, requires = "prompt_key")]
    pub credential_file: Option<PathBuf>,
    /// Deliberately replace an existing key file when using --prompt-key.
    #[arg(long, requires = "prompt_key")]
    pub replace_key: bool,
    /// Private provider configuration destination; defaults beside your user configuration.
    #[arg(long)]
    pub providers_file: Option<PathBuf>,
    /// Permit explicit future embedding requests for this wiki and chosen model.
    #[arg(long)]
    pub grant_embed: bool,
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

fn usage(message: impl Into<String>) -> WikiError {
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

fn present_embedding_summary(
    embedding: &Value,
    wiki: &Path,
    output: &mut impl Write,
) -> io::Result<()> {
    writeln!(
        output,
        "Embedding model: {}",
        embedding["model"].as_str().unwrap_or("Not set")
    )?;
    writeln!(
        output,
        "Destination: {}",
        super::config_show::destination_label(&embedding["destination"])
    )?;
    writeln!(
        output,
        "Text that explicit requests may send: {EMBEDDING_TEXT_SCOPE}."
    )?;
    writeln!(
        output,
        "Embedding requests for {}: {}. Setup makes no requests.",
        wiki.display(),
        if embedding["grant_embed"] == true {
            "permitted when explicitly requested"
        } else {
            "not permitted"
        }
    )
}

fn absolute(path: &Path) -> Result<PathBuf> {
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
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
fn quote_path(path: &Path) -> String {
    quote(&path.to_string_lossy())
}

struct Prompts<R, W> {
    input: R,
    output: W,
}
impl<R: BufRead, W: Write> Prompts<R, W> {
    fn text(&mut self, label: &str, default: Option<&str>) -> Result<String> {
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
    fn yes(&mut self, label: &str, default: bool) -> Result<bool> {
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

fn collect<R: BufRead, W: Write>(
    setup: &SetupArguments,
    args: &Arguments,
    interactive: bool,
    prompts: &mut Prompts<R, W>,
) -> Result<SetupArguments> {
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
    if interactive {
        if !choices.no_default {
            choices.no_default =
                !prompts.yes("Use this wiki when running lwiki from other folders?", true)?;
        }
        if choices.agent.is_none() {
            loop {
                let agent = prompts.text(
                    "Connect an agent (codex/claude-code/cursor/skip)",
                    Some("skip"),
                )?;
                match agent.as_str() {
                    "codex" | "claude-code" | "cursor" => {
                        choices.agent = Some(agent);
                        break;
                    }
                    "skip" => break,
                    _ => writeln!(prompts.output, "Choose codex, claude-code, cursor or skip.")
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
        let configure = choices.embedding_model.is_some() || choices.embedding_url.is_some()
            || prompts.yes("Configure an optional embedding model for similarity search? No requests will be made.", false)?;
        if configure {
            if choices.embedding_model.is_none() {
                choices.embedding_model =
                    Some(prompts.text("Embedding model name from your provider", None)?);
            }
            if choices.embedding_url.is_none() {
                choices.embedding_url =
                    Some(prompts.text("Full embeddings-v1 endpoint URL from your provider", None)?);
            }
            if choices.embedding_dimensions.is_none() {
                let dimensions = prompts.text("Requested vector dimensions (number, auto, or leave empty to use provider default)", None)?;
                if !dimensions.is_empty() {
                    choices.embedding_dimensions = Some(dimensions);
                }
            }
            if choices.key_env.is_none() && choices.key_file.is_none() && !choices.prompt_key {
                loop {
                    let auth = prompts.text(
                        "Key source (save privately/environment/private file)",
                        Some("save privately"),
                    )?;
                    match auth.as_str() {
                        "save privately" | "save" => {
                            choices.prompt_key = true;
                            break;
                        }
                        "environment" | "env" => {
                            choices.key_env =
                                Some(prompts.text("Environment variable name", None)?);
                            break;
                        }
                        "private file" | "file" => {
                            choices.key_file = Some(PathBuf::from(
                                prompts.text("Existing private key file", None)?,
                            ));
                            break;
                        }
                        _ => writeln!(
                            prompts.output,
                            "Choose save privately, environment or private file."
                        )
                        .map_err(|error| usage(error.to_string()))?,
                    }
                }
            }
            if !choices.grant_embed {
                let destination = embedding_destination(
                    choices.embedding_url.as_deref().expect("chosen endpoint"),
                )?;
                choices.grant_embed = prompts.yes(&format!(
                    "Permit explicit requests for similarity search in {} with model {} at {}? They can send {EMBEDDING_TEXT_SCOPE} and cost money. Setup makes no requests.",
                    path.display(),
                    choices.embedding_model.as_deref().expect("chosen model"),
                    super::config_show::destination_label(&destination),
                ), false)?;
            }
        }
    }
    Ok(choices)
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
    let mut command = String::from("lwiki");
    if args.preferences.is_some()
        && let Some(path) = configuration
    {
        command += &format!(" --preferences {}", quote_path(path));
    }
    if args.no_user_config {
        command += " --no-user-config";
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
    if !setup.prompt_key {
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
        ("--embedding-url", &setup.embedding_url),
        ("--embedding-dimensions", &setup.embedding_dimensions),
        ("--key-env", &setup.key_env),
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
    ] {
        if let Some(path) = path {
            command += &format!(" {flag} {}", quote_path(path));
        }
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
        result.push("Connect your agent later by rerunning setup with --agent codex, --agent claude-code or --agent cursor.".into());
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

/// Setup configures files only. It never resolves credentials or contacts providers.
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
    let mut choices = collect(setup, args, interactive, &mut prompts)?;
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
    let loaded = config::local::load_user_config_for_setup(
        args.preferences.as_deref(),
        args.no_user_config,
    )?;
    let config_target = loaded.toml_migration_target();
    let config_path = config_target.as_deref().or(loaded.path());
    if !choices.no_default {
        config::local::require_write_target(&loaded)?;
    }
    let provider_requested = choices.embedding_model.is_some() || choices.embedding_url.is_some();
    if !provider_requested
        && (choices.embedding_dimensions.is_some()
            || choices.key_env.is_some()
            || choices.key_file.is_some()
            || choices.prompt_key
            || choices.providers_file.is_some()
            || choices.grant_embed
            || choices.embedding_document_prefix.is_some()
            || choices.embedding_query_prefix.is_some()
            || choices.embedding_max_input_bytes.is_some()
            || choices.embedding_quality_target_bytes.is_some())
    {
        return Err(usage(
            "embedding setup requires --embedding-model and --embedding-url; omit model options to skip it",
        ));
    }
    if choices.prompt_key
        && !args.dry_run
        && (choices.non_interactive || !io::stdin().is_terminal() || !io::stderr().is_terminal())
    {
        return Err(usage(
            "--prompt-key requires interactive terminal input; use --key-env NAME or --key-file FILE with --non-interactive or redirected input",
        ));
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
    let mut provider_snapshot = None;
    let mut credential_snapshot = None;
    let mut embedding = None;
    let mut provider_document = None;
    if provider_requested {
        let model = choices
            .embedding_model
            .clone()
            .ok_or_else(|| usage("provide --embedding-model MODEL"))?;
        let url = choices
            .embedding_url
            .clone()
            .ok_or_else(|| usage("provide --embedding-url URL"))?;
        let parent = config_path.and_then(Path::parent).ok_or_else(|| usage("model setup needs a persistent user configuration; omit --no-user-config or provide --preferences FILE"))?;
        let provider_path = absolute(
            &choices
                .providers_file
                .clone()
                .or_else(|| proposed.providers_config.clone())
                .unwrap_or_else(|| parent.join("providers.toml")),
        )?;
        let snapshot = config::setup_io::load_private_file(
            &provider_path,
            config::providers::MAX_PROVIDER_CONFIG_BYTES,
        )?;
        choices.providers_file = Some(snapshot.path().to_path_buf());
        let auth = if let Some(name) = &choices.key_env {
            SetupAuth::Environment(name.clone())
        } else if let Some(path) = &choices.key_file {
            let path = absolute(path)?;
            choices.key_file = Some(path.clone());
            SetupAuth::PrivateFile(path)
        } else if choices.prompt_key {
            let hash = blake3::hash(format!("{model}\n{url}").as_bytes())
                .to_hex()
                .to_string();
            let path = absolute(
                &choices
                    .credential_file
                    .clone()
                    .unwrap_or_else(|| parent.join(format!("embedding-key-{}", &hash[..16]))),
            )?;
            let key_snapshot =
                config::setup_io::load_private_file(&path, config::providers::MAX_SECRET_BYTES)?;
            if key_snapshot.bytes().is_some() && !choices.replace_key {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "private key destination already exists; use --key-file to reuse it, choose --credential-file elsewhere, or explicitly use --replace-key",
                ));
            }
            choices.credential_file = Some(key_snapshot.path().to_path_buf());
            let auth = SetupAuth::PrivateFile(key_snapshot.path().to_path_buf());
            credential_snapshot = Some(key_snapshot);
            auth
        } else {
            return Err(usage(
                "model setup needs --key-env NAME, --key-file FILE or --prompt-key; no key is required when model setup is skipped",
            ));
        };
        let selected = EmbeddingSetup {
            model,
            url,
            dimensions: choices.embedding_dimensions.clone(),
            auth,
            grant: choices.grant_embed,
        };
        let document =
            config::provider_setup::build_embedding_document(snapshot.bytes(), &selected, &wiki)?;
        config::setup_io::write_private_file(&snapshot, &document.bytes, true, true, timeout)?;
        if let Some(key_snapshot) = &credential_snapshot {
            config::setup_io::write_private_file(
                key_snapshot,
                b"setup preview only",
                choices.replace_key,
                true,
                timeout,
            )?;
        }
        proposed.providers_config = Some(snapshot.path().to_path_buf());
        proposed
            .vaults
            .entry(wiki.wiki_id.clone())
            .or_default()
            .profile = Some(document.profile.clone());
        if !proposed.allowed_profiles.is_empty()
            && !proposed.allowed_profiles.contains(&document.profile)
        {
            proposed.allowed_profiles.push(document.profile.clone());
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
        provider_snapshot = Some(snapshot);
        provider_document = Some(document);
        embedding = Some(selected);
    }
    let writes_config = !choices.no_default || provider_requested;
    if writes_config {
        config::local::update_document(
            &loaded,
            proposed.clone(),
            config_target.as_deref(),
            true,
            timeout,
        )?;
    }
    let agent_preview = if let Some(agent) = &choices.agent {
        Some(super::skill_install::operate(
            "install",
            agent,
            choices.agent_directory.as_deref().expect("agent home"),
            choices.agent_scope.as_deref().unwrap_or("user"),
            true,
        )?)
    } else {
        None
    };
    let embedding_preview = embedding.as_ref().map(|selected| -> Result<Value> {
        Ok(json!({"model":selected.model,"destination":embedding_destination(&selected.url)?,"text_scope":EMBEDDING_TEXT_SCOPE,"dimensions":selected.dimensions,"grant_embed":selected.grant,"network_requests":0}))
    }).transpose()?;
    let plan = json!({"wiki":wiki.path,"wiki_action":if marker_exists || choices.select { "select" } else { "create" }, "initialize_existing_folder":choices.existing,
        "save_default":!choices.no_default,"configuration":if writes_config { config_path } else { None },
        "provider_configuration":choices.providers_file,"credential_file":choices.credential_file,
        "embedding":embedding_preview,
        "agent":choices.agent,"agent_installation":agent_preview,"network_requests":0});
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
        if let Some(embedding) = &embedding_preview {
            present_embedding_summary(embedding, &wiki.path, &mut prompts.output)
                .map_err(|error| usage(error.to_string()))?;
        }
        if let Some(preview) = &agent_preview {
            if let Some(destination) = preview.get("destination").and_then(Value::as_str) {
                writeln!(prompts.output, "Agent instructions: {destination}")
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
    if let Some(snapshot) = &credential_snapshot {
        steps.push(step(
            "credential",
            Some(snapshot.path().to_path_buf()),
            "Save your key privately",
        ));
    }
    if let Some(snapshot) = &provider_snapshot {
        steps.push(step(
            "provider",
            Some(snapshot.path().to_path_buf()),
            "Save optional embedding settings",
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
            "Install your chosen agent's wiki instructions",
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
            if let (Some(snapshot), Some(selected)) = (&provider_snapshot, &embedding) {
                provider_document = Some(config::provider_setup::build_embedding_document(
                    snapshot.bytes(),
                    selected,
                    &wiki,
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
        if let Some(snapshot) = &credential_snapshot {
            let key = rpassword::prompt_password("Provider key (hidden): ").map_err(|_| {
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
                choices.replace_key,
                false,
                timeout,
            )?;
            steps[index].applied = report.applied;
            steps[index].durable = report.durable;
            if report.applied || (!report.changed && report.durable) {
                retry_choices.prompt_key = false;
                retry_choices.replace_key = false;
                retry_choices.key_file = Some(snapshot.path().to_path_buf());
                retry_choices.credential_file = None;
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
                &document.bytes,
                true,
                false,
                timeout,
            )?;
            steps[index].applied = report.applied;
            steps[index].durable = report.durable;
            if let Some(failure) = report.failure {
                return Err(failure);
            }
            steps[index].status = "complete";
            index += 1;
            checkpoint("provider_complete", &wiki.path)?;
        }
        if writes_config {
            let report = config::local::update_document(
                &loaded,
                proposed.clone(),
                config_target.as_deref(),
                false,
                timeout,
            )?;
            steps[index].applied = report.applied;
            steps[index].durable = report.durable;
            if report.applied || report.durable {
                retry_configuration = Some(report.configuration_path.clone());
                retry = retry_command(args, &retry_choices, retry_configuration.as_deref());
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
                agent,
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
    if provider_requested && !choices.grant_embed {
        envelope.warnings.push("Model settings were saved without permission for embedding requests. Rerun setup with the same model choices and --grant-embed if you want to permit explicit embedding commands.".into());
    }
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
    if plan["embedding"].is_object()
        && let Some(wiki) = plan["wiki"].as_str()
    {
        present_embedding_summary(&plan["embedding"], Path::new(wiki), output)?;
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
            "Retry: {}",
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

//! Research only coordinates durable local handoffs; the host agent executes tools.
use crate::{
    app::OfflineApp,
    domain::*,
    research::{self, ResearchScope, ResearchSourceRange},
};
use clap::{Args, Subcommand};
use serde_json::Value;
use std::io::Read;

#[derive(Debug, Args)]
pub struct ResearchArguments {
    #[command(subcommand)]
    pub command: ResearchCommand,
}
#[derive(Debug, Subcommand)]
pub enum ResearchCommand {
    /// Preview agent tasks and current passages without persisting a run.
    Plan(ResearchPlanArguments),
    /// Start a local research handoff for an agent with its own tools.
    Run(ResearchPlanArguments),
    /// Return the outstanding agent packet; never execute tools.
    Resume(ResearchResumeArguments),
    /// Validate an agent submission and publish captures/report through guarded recovery.
    Import {
        /// JSON submission path, or - for standard input.
        #[arg(long)]
        file: std::path::PathBuf,
    },
    /// Read local handoff progress and remaining work.
    Status {
        /// Research run ID returned by run.
        run_id: RecordId,
    },
    /// Read the most recent imported answer and its unresolved gaps.
    Report {
        /// Research run ID returned by run.
        run_id: RecordId,
    },
    /// List concrete host repair tasks for stale support, contradictions and missing synthesis.
    Maintenance {
        /// Research run ID whose sources and retained answers define repair scope.
        run_id: RecordId,
    },
}
impl ResearchCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Plan(_) => "research plan",
            Self::Run(_) => "research run",
            Self::Resume(_) => "research resume",
            Self::Import { .. } => "research import",
            Self::Status { .. } => "research status",
            Self::Report { .. } => "research report",
            Self::Maintenance { .. } => "research maintenance",
        }
    }
}
#[derive(Debug, Args)]
pub struct ResearchPlanArguments {
    /// Question for the host agent to investigate.
    pub question: String,
    /// Suggested origin for the host agent; lwiki does not fetch it.
    #[arg(long = "url")]
    pub urls: Vec<String>,
    /// Scope exclusion to include in the agent packet.
    #[arg(long = "exclude")]
    pub exclusions: Vec<String>,
    /// Include the current captured text of this source; repeat as needed.
    #[arg(long = "source-id")]
    pub source_ids: Vec<RecordId>,
    /// Exact current UTF-8 source range SOURCE:START:END; repeat for selected passages.
    #[arg(long = "source-range", value_parser = parse_source_range)]
    pub source_ranges: Vec<ResearchSourceRange>,
    /// Maximum collection/answer rounds (1–8), including the initial round.
    #[arg(long, default_value_t = 3)]
    pub max_rounds: u32,
    /// Maximum total sources accepted from the agent (0–64).
    #[arg(long, default_value_t = 15)]
    pub max_sources: u32,
    /// Maximum lifetime source-content bytes accepted locally (up to 4 MiB).
    #[arg(long, default_value_t = 524288)]
    pub max_source_bytes: u64,
    /// New run ID; omit to allocate one. Existing IDs require resume.
    #[arg(long)]
    pub run_id: Option<RecordId>,
}
#[derive(Debug, Args)]
pub struct ResearchResumeArguments {
    /// Research run ID returned by run.
    pub run_id: RecordId,
    /// Replace a stale packet using current sources; invalidate its old fingerprint.
    #[arg(long)]
    pub refresh: bool,
}
pub struct ResearchCliOutcome {
    pub data: Value,
    pub network_used: bool,
    pub partial: bool,
    pub warnings: Vec<String>,
}
fn value(value: impl serde::Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| WikiError::invalid("research result encoding"))
}
pub fn execute(command: &ResearchCommand, app: &OfflineApp) -> Result<ResearchCliOutcome> {
    let data = match command {
        ResearchCommand::Plan(args) | ResearchCommand::Run(args) => value(research::start(
            app,
            ResearchScope {
                question: args.question.clone(),
                urls: args.urls.clone(),
                exclusions: args.exclusions.clone(),
                source_ids: args.source_ids.clone(),
                source_ranges: args.source_ranges.clone(),
                offline: app.options().offline,
                max_rounds: args.max_rounds,
                max_sources: args.max_sources,
                max_source_bytes: args.max_source_bytes,
            },
            args.run_id.clone(),
            matches!(command, ResearchCommand::Plan(_)),
        )?)?,
        ResearchCommand::Resume(args) => value(research::resume(app, &args.run_id, args.refresh)?)?,
        ResearchCommand::Import { file } => {
            let input: Box<dyn Read> = if file.as_os_str() == "-" {
                Box::new(std::io::stdin())
            } else {
                Box::new(std::fs::File::open(file).map_err(|_| {
                    WikiError::new(ErrorCode::Usage, "cannot read research submission file")
                })?)
            };
            let mut bytes = vec![];
            input
                .take(research::MAX_SUBMISSION_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| WikiError::new(ErrorCode::Usage, "cannot read research submission"))?;
            value(research::import(app, &bytes)?)?
        }
        ResearchCommand::Status { run_id } => research::status(app, run_id)?,
        ResearchCommand::Report { run_id } => research::report_view(app, run_id)?,
        ResearchCommand::Maintenance { run_id } => research::maintenance(app, run_id)?,
    };
    let partial = data
        .get("partial")
        .or_else(|| data.get("report").and_then(|r| r.get("partial")))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(ResearchCliOutcome {
        data,
        network_used: false,
        partial,
        warnings: vec![],
    })
}

fn parse_source_range(value: &str) -> std::result::Result<ResearchSourceRange, String> {
    let fields: Vec<_> = value.split(':').collect();
    if fields.len() != 3 {
        return Err("expected SOURCE:START:END".into());
    }
    let source_id = RecordId::new(fields[0]).map_err(|e| e.message)?;
    let start = fields[1]
        .parse::<u64>()
        .map_err(|_| "START must be a byte offset")?;
    let end = fields[2]
        .parse::<u64>()
        .map_err(|_| "END must be a byte offset")?;
    let span = ByteSpan::new(start, end).map_err(|e| e.message)?;
    if span.is_empty() || end - start > 4096 {
        return Err("range must contain 1–4096 UTF-8 bytes".into());
    }
    Ok(ResearchSourceRange { source_id, span })
}

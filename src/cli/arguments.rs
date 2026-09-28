use crate::{app::RecordSelector, domain::*, retrieval::*};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
    Jsonl,
}
#[derive(Debug, Parser)]
#[command(
    name = "lwiki",
    version,
    about = "A local Markdown wiki with traceable evidence"
)]
pub struct Arguments {
    #[arg(long, global = true)]
    pub wiki: Option<PathBuf>,
    #[arg(long, global = true, value_enum, conflicts_with_all = ["json", "jsonl"])]
    pub format: Option<OutputFormat>,
    #[arg(long, global = true, conflicts_with = "jsonl")]
    pub json: bool,
    #[arg(long, global = true)]
    pub jsonl: bool,
    #[arg(long, global = true)]
    pub offline: bool,
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Retain a guarded preparation for a later explicit changes apply.
    #[arg(long, global = true)]
    pub stage: bool,
    /// Explicit trusted local JSON preferences; never read ambient credentials.
    #[arg(long, global = true)]
    pub preferences: Option<PathBuf>,
    #[arg(long, global = true)]
    pub profile: Option<String>,
    #[arg(long, global = true)]
    pub lock_timeout_ms: Option<u64>,
    #[command(subcommand)]
    pub command: Command,
}
impl Arguments {
    pub fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else if self.jsonl {
            OutputFormat::Jsonl
        } else {
            self.format.unwrap_or_default()
        }
    }
}
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct Selector {
    #[arg(long)]
    pub id: Option<RecordId>,
    #[arg(long)]
    pub path: Option<VaultRelativePath>,
}
impl Selector {
    pub fn record_selector(&self) -> Result<RecordSelector> {
        match (&self.id, &self.path) {
            (Some(id), None) => Ok(RecordSelector::Id(id.clone())),
            (None, Some(path)) => Ok(RecordSelector::Path(path.clone())),
            _ => Err(WikiError::new(
                ErrorCode::Usage,
                "choose exactly one of --id or --path",
            )),
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum Command {
    Capabilities,
    Schema {
        name: String,
    },
    Init {
        path: PathBuf,
        #[arg(long, default_value = "Local wiki")]
        title: String,
    },
    Read {
        #[command(flatten)]
        selector: Selector,
        #[arg(long)]
        max_bytes: Option<usize>,
        #[arg(long, requires = "end")]
        start: Option<u64>,
        #[arg(long, requires = "start")]
        end: Option<u64>,
        #[arg(long)]
        no_sync: bool,
    },
    Page {
        #[command(subcommand)]
        command: PageCommand,
    },
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    Evidence {
        #[command(subcommand)]
        command: EvidenceCommand,
    },
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    Search(SearchArguments),
    Context(Box<super::context::ContextArguments>),
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
    Check,
    Doctor {
        #[arg(long)]
        probe: bool,
    },
    Changes {
        #[command(subcommand)]
        command: ChangesCommand,
    },
    Recover,
    /// Stage a compatible schema migration; future schemas are never downgraded.
    Migrate {
        #[command(flatten)]
        selector: Selector,
        #[arg(long)]
        if_match: Blake3Hash,
        #[arg(long, default_value = "1")]
        to_schema: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum PageCommand {
    Put {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        path: Option<VaultRelativePath>,
        #[arg(long)]
        if_match: Option<Blake3Hash>,
    },
    Rename {
        id: RecordId,
        #[arg(long)]
        to: VaultRelativePath,
        #[arg(long)]
        if_match: Blake3Hash,
    },
}
#[derive(Debug, Subcommand)]
pub enum SourceCommand {
    Add {
        file: PathBuf,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        media_type: Option<String>,
    },
    Refresh {
        id: RecordId,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        media_type: Option<String>,
    },
    Withdraw {
        id: RecordId,
        #[arg(long)]
        reason: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum EvidenceCommand {
    Revalidate {
        id: RecordId,
        #[arg(long)]
        to_revision: RecordId,
        #[arg(long)]
        if_match: Blake3Hash,
    },
}
#[derive(Debug, Subcommand)]
pub enum IndexCommand {
    Sync,
    Rebuild,
}
#[derive(Debug, Subcommand)]
pub enum ChangesCommand {
    Show {
        id: RecordId,
        #[arg(long)]
        operation: Option<usize>,
    },
    Apply {
        id: RecordId,
    },
    Abort {
        id: RecordId,
    },
    Rollback {
        id: RecordId,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Mode {
    Literal,
    Lexical,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Strategy {
    Entity,
    Relationship,
    Combined,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Seed {
    Lexical,
}
#[derive(Debug, Subcommand)]
pub enum GraphCommand {
    Extract(super::extraction::ExtractArguments),
    Import(super::extraction::ImportArguments),
    Resolve(super::resolution::ResolveArguments),
    Query {
        query: String,
        #[command(flatten)]
        options: GraphOptions,
    },
    Neighbors {
        id: RecordId,
        #[command(flatten)]
        options: GraphOptions,
    },
}
#[derive(Debug, Args)]
pub struct GraphOptions {
    #[arg(long, value_enum, default_value = "combined")]
    pub strategy: Strategy,
    #[arg(long, value_enum, default_value = "lexical")]
    pub seed: Seed,
    #[arg(long = "kind")]
    pub kinds: Vec<RecordKind>,
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    #[arg(long = "source-id")]
    pub source_ids: Vec<RecordId>,
    #[arg(long)]
    pub path_prefix: Option<String>,
    #[arg(long = "status")]
    pub authored_statuses: Vec<String>,
    #[arg(long)]
    pub include_proposed: bool,
    #[arg(long)]
    pub include_historical: bool,
    #[arg(long)]
    pub navigation: bool,
    #[arg(long, default_value_t = 80)]
    pub candidates: usize,
    #[arg(long, default_value_t = 12)]
    pub seeds: usize,
    #[arg(long, default_value_t = 1)]
    pub depth: usize,
    #[arg(long, default_value_t = 16)]
    pub incident_per_seed: usize,
    #[arg(long, default_value_t = 128)]
    pub assertions: usize,
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    #[arg(long, default_value_t = 240)]
    pub excerpt_bytes: usize,
    #[arg(long, default_value_t = 2)]
    pub support_per_assertion: usize,
    #[arg(long, default_value_t = 1)]
    pub contradictions_per_assertion: usize,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub no_sync: bool,
}
impl GraphOptions {
    pub fn plan(&self, neighbors: bool) -> crate::graph::GraphPlan {
        use crate::graph::*;
        GraphPlan {
            strategy: match self.strategy {
                Strategy::Entity => GraphStrategy::Entity,
                Strategy::Relationship => GraphStrategy::Relationship,
                Strategy::Combined => GraphStrategy::Combined,
            },
            seed_mode: GraphSeedMode::Lexical,
            filters: SearchFilters {
                kinds: self.kinds.clone(),
                tags: self.tags.clone(),
                source_ids: self.source_ids.clone(),
                path_prefix: self.path_prefix.clone(),
                authored_statuses: self.authored_statuses.clone(),
                include_proposed: self.include_proposed,
                include_historical: self.include_historical,
            },
            limits: GraphLimits {
                candidates: self.candidates,
                seeds: self.seeds,
                depth: self.depth,
                incident_per_seed: self.incident_per_seed,
                assertions: self.assertions,
                hits: self.limit,
                excerpt_bytes: self.excerpt_bytes,
                support_per_assertion: self.support_per_assertion,
                contradictions_per_assertion: self.contradictions_per_assertion,
            },
            include_navigation: self.navigation || neighbors,
            cursor: self.cursor.clone(),
        }
    }
}
#[derive(Debug, Args)]
pub struct SearchArguments {
    pub query: String,
    #[arg(long, value_enum, default_value = "lexical")]
    pub mode: Mode,
    #[arg(long = "kind")]
    pub kinds: Vec<RecordKind>,
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    #[arg(long = "source-id")]
    pub source_ids: Vec<RecordId>,
    #[arg(long)]
    pub path_prefix: Option<String>,
    #[arg(long = "status")]
    pub authored_statuses: Vec<String>,
    #[arg(long)]
    pub include_proposed: bool,
    #[arg(long)]
    pub include_historical: bool,
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    #[arg(long, default_value_t = 80)]
    pub candidates: usize,
    #[arg(long, default_value_t = 240)]
    pub excerpt_bytes: usize,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub no_sync: bool,
}
impl SearchArguments {
    pub fn plan(&self) -> QueryPlan {
        QueryPlan {
            mode: match self.mode {
                Mode::Literal => SearchMode::Literal,
                Mode::Lexical => SearchMode::Lexical,
            },
            filters: SearchFilters {
                kinds: self.kinds.clone(),
                tags: self.tags.clone(),
                source_ids: self.source_ids.clone(),
                path_prefix: self.path_prefix.clone(),
                authored_statuses: self.authored_statuses.clone(),
                include_proposed: self.include_proposed,
                include_historical: self.include_historical,
            },
            limits: SearchLimits {
                hits: self.limit,
                candidates: self.candidates,
                excerpt_bytes: self.excerpt_bytes,
            },
            cursor: self.cursor.clone(),
        }
    }
}
impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Capabilities => "capabilities",
            Self::Schema { .. } => "schema",
            Self::Init { .. } => "init",
            Self::Read { .. } => "read",
            Self::Page {
                command: PageCommand::Put { .. },
            } => "page put",
            Self::Page {
                command: PageCommand::Rename { .. },
            } => "page rename",
            Self::Source {
                command: SourceCommand::Add { .. },
            } => "source add",
            Self::Source {
                command: SourceCommand::Refresh { .. },
            } => "source refresh",
            Self::Source {
                command: SourceCommand::Withdraw { .. },
            } => "source withdraw",
            Self::Evidence { .. } => "evidence revalidate",
            Self::Index {
                command: IndexCommand::Sync,
            } => "index sync",
            Self::Index {
                command: IndexCommand::Rebuild,
            } => "index rebuild",
            Self::Search(_) => "search",
            Self::Context(_) => "context",
            Self::Graph {
                command: GraphCommand::Extract(_),
            } => "graph extract",
            Self::Graph {
                command: GraphCommand::Import(_),
            } => "graph import",
            Self::Graph {
                command: GraphCommand::Resolve(_),
            } => "graph resolve",
            Self::Graph {
                command: GraphCommand::Query { .. },
            } => "graph query",
            Self::Graph {
                command: GraphCommand::Neighbors { .. },
            } => "graph neighbors",
            Self::Check => "check",
            Self::Doctor { .. } => "doctor",
            Self::Changes {
                command: ChangesCommand::Show { .. },
            } => "changes show",
            Self::Changes {
                command: ChangesCommand::Apply { .. },
            } => "changes apply",
            Self::Changes {
                command: ChangesCommand::Abort { .. },
            } => "changes abort",
            Self::Changes {
                command: ChangesCommand::Rollback { .. },
            } => "changes rollback",
            Self::Recover => "recover",
            Self::Migrate { .. } => "migrate",
        }
    }
    pub fn streaming(&self) -> bool {
        matches!(
            self,
            Self::Index { .. }
                | Self::Recover
                | Self::Changes {
                    command: ChangesCommand::Apply { .. }
                }
                | Self::Source {
                    command: SourceCommand::Add { .. } | SourceCommand::Refresh { .. }
                }
        )
    }
}

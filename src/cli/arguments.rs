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
    /// Wiki root containing WIKI.md; defaults to discovery from the current directory.
    #[arg(long, global = true)]
    pub wiki: Option<PathBuf>,
    /// Output format: human-readable text, a JSON envelope or JSON Lines events.
    #[arg(long, global = true, value_enum, conflicts_with_all = ["json", "jsonl"])]
    pub format: Option<OutputFormat>,
    /// Emit one structured JSON envelope.
    #[arg(long, global = true, conflicts_with = "jsonl")]
    pub json: bool,
    /// Emit JSON Lines events for supported streaming commands.
    #[arg(long, global = true)]
    pub jsonl: bool,
    /// Prevent provider requests and credential helper calls; local operations remain available.
    #[arg(long, global = true)]
    pub offline: bool,
    /// Preview without writes, provider requests or credential resolution.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Retain a guarded preparation for a later explicit changes apply.
    #[arg(long, global = true)]
    pub stage: bool,
    /// Explicit trusted local JSON preferences; never read ambient credentials.
    #[arg(long, global = true)]
    pub preferences: Option<PathBuf>,
    /// Trusted provider profile name from the private provider configuration.
    #[arg(long, global = true)]
    pub profile: Option<String>,
    /// Maximum writer-lock wait in milliseconds (default: 5000; preferences may override).
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
    /// Select a record by its stable ID.
    #[arg(long)]
    pub id: Option<RecordId>,
    /// Select a record by its vault-relative path.
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
    /// List implemented commands, schemas and supported modes.
    Capabilities,
    /// Export portable instructions and examples for a coding agent.
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    /// Print a published JSON input or output schema.
    Schema {
        /// Published schema name; inspect capabilities for available schemas.
        name: String,
    },
    /// Create a new Markdown wiki in a directory that does not exist.
    Init {
        /// New wiki directory; initialization refuses an existing path.
        path: PathBuf,
        /// Human-readable title.
        #[arg(long, default_value = "Local wiki")]
        title: String,
    },
    /// Read a record by ID or vault-relative path.
    Read {
        #[command(flatten)]
        selector: Selector,
        /// Maximum UTF-8 bytes to return.
        #[arg(long)]
        max_bytes: Option<usize>,
        /// Start of the zero-based, half-open UTF-8 byte range.
        #[arg(long, requires = "end")]
        start: Option<u64>,
        /// End of the zero-based, half-open UTF-8 byte range.
        #[arg(long, requires = "start")]
        end: Option<u64>,
        /// Read the existing index snapshot without syncing; freshness is not verified.
        #[arg(long)]
        no_sync: bool,
    },
    /// Create, update or rename pages with guarded changes.
    Page {
        #[command(subcommand)]
        command: PageCommand,
    },
    /// Inspect storage and preview or apply supported retention and migration.
    Storage(super::storage::StorageArguments),
    /// Capture source files, refresh immutable revisions or withdraw support.
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// Revalidate quoted evidence against a newer source revision.
    Evidence {
        #[command(subcommand)]
        command: EvidenceCommand,
    },
    /// Sync or rebuild the disposable local search index.
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    /// Coordinate research with an agent using durable local packets and cited submissions.
    Research(super::research::ResearchArguments),
    /// Inspect local vector coverage or explicitly generate embeddings.
    Embeddings(super::embeddings::EmbeddingArguments),
    /// Find text with literal, lexical, semantic or hybrid retrieval.
    Search(SearchArguments),
    /// Assemble cited context within byte and token budgets.
    Context(Box<super::context::ContextArguments>),
    /// Extract, resolve, review and query evidence-backed assertions.
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
    /// Check canonical records, references and source integrity.
    Check,
    /// Inspect local status without a full audit; probe providers only with --probe.
    Doctor {
        /// Explicitly contact the selected provider within the supplied request limits.
        #[arg(long)]
        probe: bool,
        /// Provider capability to probe: embed or generate.
        #[arg(long, value_enum, default_value = "embed", requires = "probe")]
        role: super::remote::ProbeRole,
        /// Probe the real extraction JSON schema; requires --probe --role generate.
        #[arg(long, requires = "probe")]
        extraction_schema: bool,
        #[command(flatten)]
        remote: super::remote::RemoteArguments,
    },
    /// Inspect, apply, abort or prepare an inverse of retained changes.
    Changes {
        #[command(subcommand)]
        command: ChangesCommand,
    },
    /// Amend retained provider-job limits without discarding accounting.
    Jobs {
        #[command(subcommand)]
        command: JobsCommand,
    },
    /// Reconcile interrupted changes while preserving unfamiliar edits.
    Recover,
    /// Stage a compatible schema migration; future schemas are never downgraded.
    Migrate {
        #[command(flatten)]
        selector: Selector,
        /// Expected current BLAKE3 content hash; reject intervening edits.
        #[arg(long)]
        if_match: Blake3Hash,
        /// Target schema version; future schemas cannot be downgraded.
        #[arg(long, default_value = "1")]
        to_schema: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum PageCommand {
    /// Initialize plain Markdown with a stable page identity and draft envelope.
    Init {
        /// Plain UTF-8 Markdown body; use - for bounded standard input.
        #[arg(long)]
        file: PathBuf,
        /// Human-readable title for the new draft page.
        #[arg(long)]
        title: String,
        /// Stable page identity; omit to allocate one.
        #[arg(long)]
        id: Option<RecordId>,
        /// New vault-relative path; defaults to pages/<record-id>.md.
        #[arg(long)]
        path: Option<VaultRelativePath>,
    },
    /// Prepare/apply 1–16 coupled page updates, each with its own author hash.
    Batch {
        /// JSON request matching schema page-batch; use - for stdin.
        #[arg(long)]
        file: PathBuf,
    },
    /// Create a page or replace one using its expected content hash.
    Put {
        /// Markdown page file with a valid page envelope; use - for stdin.
        #[arg(long)]
        file: PathBuf,
        /// Destination vault-relative path; defaults to pages/<record-id>.md.
        #[arg(long)]
        path: Option<VaultRelativePath>,
        /// Required current BLAKE3 hash when replacing an existing page.
        #[arg(long)]
        if_match: Option<Blake3Hash>,
    },
    /// Move a page and update known links while preserving its ID.
    Rename {
        /// Stable record or changeset ID returned by an earlier command.
        id: RecordId,
        /// New vault-relative page path.
        #[arg(long)]
        to: VaultRelativePath,
        /// Expected current BLAKE3 content hash; reject intervening edits.
        #[arg(long)]
        if_match: Blake3Hash,
    },
}
#[derive(Debug, Subcommand)]
pub enum SourceCommand {
    /// Capture a local file as a new source with an immutable revision.
    Add {
        /// Input file; use - to read bounded standard input.
        file: PathBuf,
        /// Human-readable title.
        #[arg(long)]
        title: Option<String>,
        /// Explicit media type for the captured input.
        #[arg(long)]
        media_type: Option<String>,
    },
    /// Capture changed content as a new revision of an existing source.
    Refresh {
        /// Stable record or changeset ID returned by an earlier command.
        id: RecordId,
        /// Input file; use - to read bounded standard input.
        #[arg(long)]
        file: PathBuf,
        /// Human-readable title.
        #[arg(long)]
        title: Option<String>,
        /// Explicit media type for the captured input.
        #[arg(long)]
        media_type: Option<String>,
    },
    /// Withdraw a source from current support while retaining its history.
    Withdraw {
        /// Stable record or changeset ID returned by an earlier command.
        id: RecordId,
        /// Reason recorded with the withdrawal.
        #[arg(long)]
        reason: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum EvidenceCommand {
    /// Stage successor evidence when its quotation uniquely matches the target revision.
    Revalidate {
        /// Stable record or changeset ID returned by an earlier command.
        id: RecordId,
        /// Target immutable source revision ID.
        #[arg(long)]
        to_revision: RecordId,
        /// Expected current BLAKE3 content hash; reject intervening edits.
        #[arg(long)]
        if_match: Blake3Hash,
    },
}
#[derive(Debug, Subcommand)]
pub enum IndexCommand {
    /// Refresh the index from changed Markdown and source records.
    Sync,
    /// Recreate the disposable index from canonical files without provider calls.
    Rebuild {
        /// Use the normalized catalog; currently supports cached lexical reads and source refresh.
        #[arg(long)]
        normalized: bool,
    },
}
#[derive(Debug, Subcommand)]
pub enum ChangesCommand {
    /// Resolve a durable conflict using an inspected, hash-bound request.
    Resolve {
        /// Conflicted changeset ID to inspect or resolve.
        id: RecordId,
        /// Produce a resolution request with --dry-run.
        #[arg(long, value_enum, default_value = "resume")]
        mode: ResolutionMode,
        /// JSON request returned by changes resolve --dry-run; use - for stdin.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Inspect a retained changeset and its exact proposed operations.
    Show {
        /// Stable record or changeset ID returned by an earlier command.
        id: RecordId,
        /// Zero-based operation index to inspect within the changeset.
        #[arg(long)]
        operation: Option<usize>,
    },
    /// Apply a prepared changeset with expected-hash guards and recovery.
    Apply {
        /// Stable record or changeset ID returned by an earlier command.
        id: RecordId,
    },
    /// Discard an unapplied preparation while retaining its history.
    Abort {
        /// Stable record or changeset ID returned by an earlier command.
        id: RecordId,
    },
    /// Stage a guarded inverse; immutable source captures remain retained.
    Rollback {
        /// Stable record or changeset ID returned by an earlier command.
        id: RecordId,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ResolutionMode {
    Resume,
    Abandon,
}
impl ResolutionMode {
    pub fn mode(self) -> crate::changes::ConflictResolutionMode {
        match self {
            Self::Resume => crate::changes::ConflictResolutionMode::Resume,
            Self::Abandon => crate::changes::ConflictResolutionMode::Abandon,
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum JobsCommand {
    /// Inspect retained progress, effective limits and unknown holds without provider access.
    Status {
        /// Retained provider run ID returned by the original operation.
        #[arg(long)]
        run: RecordId,
    },
    /// Explicitly inspect or remove bounded private attempt diagnostics.
    Diagnostics {
        #[command(subcommand)]
        command: DiagnosticsCommand,
    },
    /// Explicitly preserve or raise cumulative limits of a planned/paused/stopped job.
    Amend {
        /// Retained provider run ID whose cumulative limits will be raised.
        #[arg(long)]
        run: RecordId,
        /// Brief explanation retained with the amendment.
        #[arg(long)]
        reason: String,
        #[command(flatten)]
        remote: Box<super::remote::RemoteArguments>,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DiagnosticKindArgument {
    SemanticRejection,
    HttpError,
}
impl DiagnosticKindArgument {
    pub fn kind(self) -> crate::jobs::diagnostics::DiagnosticKind {
        match self {
            Self::SemanticRejection => crate::jobs::diagnostics::DiagnosticKind::SemanticRejection,
            Self::HttpError => crate::jobs::diagnostics::DiagnosticKind::HttpError,
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum DiagnosticsCommand {
    /// Metadata is safe by default; --raw explicitly includes private provider text.
    Inspect {
        /// Retained provider run ID.
        #[arg(long)]
        run: RecordId,
        /// Attempt ID from jobs status; identifies the private diagnostic.
        #[arg(long)]
        attempt: RecordId,
        /// Diagnostic family to inspect.
        #[arg(long, value_enum)]
        kind: DiagnosticKindArgument,
        /// Include bounded private provider text instead of safe metadata only.
        #[arg(long)]
        raw: bool,
    },
    /// Prune only a settled diagnostic with known billing; unknown holds remain protected.
    Prune {
        /// Retained provider run ID.
        #[arg(long)]
        run: RecordId,
        /// Settled, known-billing attempt ID from jobs status.
        #[arg(long)]
        attempt: RecordId,
        /// Diagnostic family to remove explicitly.
        #[arg(long, value_enum)]
        kind: DiagnosticKindArgument,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Mode {
    Literal,
    Lexical,
    Semantic,
    Hybrid,
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
    Semantic,
}
#[derive(Debug, Subcommand)]
pub enum GraphCommand {
    /// Export an agent packet or run bounded API extraction; never accept assertions.
    Extract(super::extraction::ExtractArguments),
    /// Validate a packet-bound extraction response and stage proposed records.
    Import(super::extraction::ImportArguments),
    /// Stage explicit mention bindings or new entities using expected hashes.
    Resolve(super::resolution::ResolveArguments),
    /// Stage explicit entity merges, splits or aliases with complete remaps.
    Decide(super::decisions::DecideArguments),
    /// Stage assertion decisions and assessments of every active evidence item.
    Review(super::review::ReviewArguments),
    /// Search and traverse eligible graph assertions with cited evidence.
    Query {
        /// Search text; lexical terms are treated as data, not query operators.
        query: String,
        #[command(flatten)]
        options: GraphOptions,
    },
    /// Inspect bounded assertion and navigation links around a record.
    Neighbors {
        /// Stable record or changeset ID returned by an earlier command.
        id: RecordId,
        #[command(flatten)]
        options: GraphOptions,
    },
}
#[derive(Debug, Args)]
pub struct GraphOptions {
    #[command(flatten)]
    pub remote: super::remote::RemoteArguments,
    /// Use lexical results when compatible embeddings are unavailable.
    #[arg(long)]
    pub lexical_fallback: bool,

    /// Rank entities, relationships or both when traversing the graph.
    #[arg(long, value_enum, default_value = "combined")]
    pub strategy: Strategy,
    /// Seed the graph with lexical matches or compatible semantic embeddings.
    #[arg(long, value_enum, default_value = "lexical")]
    pub seed: Seed,
    /// Filter by record kind; repeat for multiple kinds.
    #[arg(long = "kind")]
    pub kinds: Vec<RecordKind>,
    /// Filter by tag; repeat for multiple tags.
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// Filter by source ID before applying result limits; repeat as needed.
    #[arg(long = "source-id")]
    pub source_ids: Vec<RecordId>,
    /// Restrict results to a vault-relative path prefix.
    #[arg(long)]
    pub path_prefix: Option<String>,
    /// Filter by authored status; this does not change derived eligibility.
    #[arg(long = "status")]
    pub authored_statuses: Vec<String>,
    /// Include labeled proposed assertions without accepting them.
    #[arg(long)]
    pub include_proposed: bool,
    /// Include labeled historical, withdrawn and otherwise ineligible records.
    #[arg(long)]
    pub include_historical: bool,
    /// Include separately labeled page and provenance links.
    #[arg(long)]
    pub navigation: bool,
    /// Maximum ranked candidates considered before final selection.
    #[arg(long, default_value_t = 80)]
    pub candidates: usize,
    /// Maximum graph seed records.
    #[arg(long, default_value_t = 12)]
    pub seeds: usize,
    /// Maximum graph traversal depth.
    #[arg(long, default_value_t = 1)]
    pub depth: usize,
    /// Maximum incident assertions examined per graph seed.
    #[arg(long, default_value_t = 16)]
    pub incident_per_seed: usize,
    /// Maximum assertions examined during graph traversal.
    #[arg(long, default_value_t = 128)]
    pub assertions: usize,
    /// Maximum result count.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    /// Maximum UTF-8 bytes in each excerpt.
    #[arg(long, default_value_t = 240)]
    pub excerpt_bytes: usize,
    /// Maximum supporting evidence items returned per assertion.
    #[arg(long, default_value_t = 2)]
    pub support_per_assertion: usize,
    /// Maximum contradicting evidence items returned per assertion.
    #[arg(long, default_value_t = 1)]
    pub contradictions_per_assertion: usize,
    /// Continuation cursor from an identical query on the same index generation.
    #[arg(long)]
    pub cursor: Option<String>,
    /// Read the existing index snapshot without syncing; freshness is not verified.
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
            seed_mode: match self.seed {
                Seed::Lexical => GraphSeedMode::Lexical,
                Seed::Semantic => GraphSeedMode::Semantic,
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
    /// Include graph evidence ranks in hybrid search.
    #[arg(long, value_parser = ["entities"])]
    pub graph: Option<String>,
    #[command(flatten)]
    pub remote: super::remote::RemoteArguments,
    /// Use lexical results when compatible embeddings are unavailable.
    #[arg(long)]
    pub lexical_fallback: bool,

    /// Search text; lexical terms are treated as data, not query operators.
    pub query: String,
    /// Retrieval mode; semantic and hybrid modes require compatible embeddings.
    #[arg(long, value_enum, default_value = "lexical")]
    pub mode: Mode,
    /// Filter by record kind; repeat for multiple kinds.
    #[arg(long = "kind")]
    pub kinds: Vec<RecordKind>,
    /// Filter by tag; repeat for multiple tags.
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// Filter by source ID before applying result limits; repeat as needed.
    #[arg(long = "source-id")]
    pub source_ids: Vec<RecordId>,
    /// Restrict results to a vault-relative path prefix.
    #[arg(long)]
    pub path_prefix: Option<String>,
    /// Filter by authored status; this does not change derived eligibility.
    #[arg(long = "status")]
    pub authored_statuses: Vec<String>,
    /// Include labeled proposed assertions without accepting them.
    #[arg(long)]
    pub include_proposed: bool,
    /// Include labeled historical, withdrawn and otherwise ineligible records.
    #[arg(long)]
    pub include_historical: bool,
    /// Maximum result count.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    /// Maximum ranked candidates considered before final selection.
    #[arg(long, default_value_t = 80)]
    pub candidates: usize,
    /// Maximum UTF-8 bytes in each excerpt (default: search 240, context 1024).
    #[arg(long)]
    pub excerpt_bytes: Option<usize>,
    /// Continuation cursor from an identical query on the same index generation.
    #[arg(long)]
    pub cursor: Option<String>,
    /// Read the existing index snapshot without syncing; freshness is not verified.
    #[arg(long)]
    pub no_sync: bool,
}
impl SearchArguments {
    pub fn plan(&self) -> QueryPlan {
        QueryPlan {
            mode: match self.mode {
                Mode::Literal => SearchMode::Literal,
                Mode::Lexical => SearchMode::Lexical,
                Mode::Semantic => SearchMode::Semantic,
                Mode::Hybrid => SearchMode::Hybrid,
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
                excerpt_bytes: self.excerpt_bytes.unwrap_or(240),
            },
            cursor: self.cursor.clone(),
        }
    }
}
#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Write host-specific skill files to a new output directory.
    Export {
        /// Host format for the exported skill.
        #[arg(long, value_parser = ["codex", "claude-code", "cursor"])]
        target: String,
        /// New directory for exported skill files; existing files are not overwritten.
        #[arg(long)]
        output: PathBuf,
    },
}
impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Capabilities => "capabilities",
            Self::Skill { .. } => "skill export",
            Self::Schema { .. } => "schema",
            Self::Init { .. } => "init",
            Self::Read { .. } => "read",
            Self::Storage(options) => options.command.name(),
            Self::Page {
                command: PageCommand::Init { .. },
            } => "page init",
            Self::Page {
                command: PageCommand::Batch { .. },
            } => "page batch",
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
                command: IndexCommand::Rebuild { .. },
            } => "index rebuild",
            Self::Embeddings(options) => match options.command {
                super::embeddings::EmbeddingCommand::Check(_) => "embeddings check",
                super::embeddings::EmbeddingCommand::Sync(_) => "embeddings sync",
            },
            Self::Research(arguments) => arguments.command.name(),
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
                command: GraphCommand::Decide(_),
            } => "graph decide",
            Self::Graph {
                command: GraphCommand::Review(_),
            } => "graph review",
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
            Self::Changes {
                command: ChangesCommand::Resolve { .. },
            } => "changes resolve",
            Self::Jobs {
                command: JobsCommand::Amend { .. },
            } => "jobs amend",
            Self::Jobs {
                command: JobsCommand::Status { .. },
            } => "jobs status",
            Self::Jobs {
                command:
                    JobsCommand::Diagnostics {
                        command: DiagnosticsCommand::Inspect { .. },
                    },
            } => "jobs diagnostics inspect",
            Self::Jobs {
                command:
                    JobsCommand::Diagnostics {
                        command: DiagnosticsCommand::Prune { .. },
                    },
            } => "jobs diagnostics prune",
            Self::Recover => "recover",
            Self::Migrate { .. } => "migrate",
        }
    }
    pub fn streaming(&self) -> bool {
        matches!(
            self,
            Self::Index { .. }
                | Self::Recover
                | Self::Doctor { probe: true, .. }
                | Self::Research(super::research::ResearchArguments {
                    command: super::research::ResearchCommand::Run(_)
                        | super::research::ResearchCommand::Resume(_)
                        | super::research::ResearchCommand::Import { .. }
                })
                | Self::Changes {
                    command: ChangesCommand::Apply { .. }
                }
                | Self::Source {
                    command: SourceCommand::Add { .. } | SourceCommand::Refresh { .. }
                }
        )
    }
}

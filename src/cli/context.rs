//! Bounded context request adapter; evidence verification stays in the coordinator.
use super::arguments::{SearchArguments, Seed, Strategy};
use crate::{graph::*, retrieval::*};
use clap::{Args, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Scope {
    Current,
    Historical,
    Snapshot,
    IndexedEvidence,
    IndexedDocuments,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Target {
    Documents,
    Graph,
    Combined,
}
#[derive(Debug, Args)]
pub struct ContextArguments {
    #[command(flatten)]
    pub search: SearchArguments,
    /// Prepare a bounded candidate packet for one host-agent selection; does not run a model.
    #[arg(long, conflicts_with = "selection")]
    pub prepare_selection: bool,
    /// Apply an ID-only host reply to the exact current candidate packet (file or - for stdin).
    #[arg(long, value_name = "FILE")]
    pub selection: Option<std::path::PathBuf>,
    /// Evidence scope. Defaults to indexed-documents on normalized vaults, current otherwise.
    /// Indexed-documents verifies selected authored/captured dependencies; indexed-evidence is captured-only.
    #[arg(long, value_enum)]
    pub scope: Option<Scope>,
    /// Retrieve document passages, graph evidence or both.
    #[arg(long, value_enum, default_value = "documents")]
    pub target: Target,
    /// Rank entities, relationships or both when traversing the graph.
    #[arg(long, value_enum, default_value = "combined")]
    pub strategy: Strategy,
    /// Seed the graph with lexical matches or compatible semantic embeddings.
    #[arg(long, value_enum, default_value = "lexical")]
    pub seed: Seed,
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
    /// Include separately labeled page and provenance links.
    #[arg(long)]
    pub navigation: bool,
    /// Maximum total context bytes, including reserved instructions and output.
    #[arg(long, default_value_t = 12000)]
    pub max_bytes: usize,
    /// Maximum estimated context tokens, including reserved instructions and output.
    #[arg(long, default_value_t = 3000)]
    pub max_tokens: usize,
    /// Bytes reserved for caller instructions within the context ceiling.
    #[arg(long, default_value_t = 0)]
    pub instruction_bytes: usize,
    /// Estimated tokens reserved for caller instructions.
    #[arg(long, default_value_t = 0)]
    pub instruction_tokens: usize,
    /// Bytes reserved for the model response within the context ceiling.
    #[arg(long, default_value_t = 0)]
    pub output_bytes: usize,
    /// Estimated tokens reserved for the model response.
    #[arg(long, default_value_t = 0)]
    pub output_tokens: usize,
    /// Maximum bytes read while verifying context evidence.
    #[arg(long, default_value_t = 67108864)]
    pub verification_max_bytes: usize,
    /// Maximum files read while verifying context evidence.
    #[arg(long, default_value_t = 4096)]
    pub verification_max_files: usize,
    /// Maximum directory entries inspected during evidence verification.
    #[arg(long, default_value_t = 16384)]
    pub verification_max_entries: usize,
    /// Maximum elapsed milliseconds for evidence verification.
    #[arg(long, default_value_t = 2000)]
    pub verification_max_elapsed_ms: u64,
}
impl ContextArguments {
    pub fn selection_action(
        &self,
    ) -> crate::domain::Result<context_selection_packet::SelectionAction> {
        use crate::domain::{ErrorCode, WikiError};
        use context_selection_packet::{SelectionAction, parse_reply};
        use std::io::Read;
        if self.prepare_selection {
            return Ok(SelectionAction::Prepare);
        }
        let Some(path) = &self.selection else {
            return Ok(SelectionAction::Automatic);
        };
        let mut bytes = Vec::new();
        let read = if path == std::path::Path::new("-") {
            std::io::stdin().lock().take(4097).read_to_end(&mut bytes)
        } else {
            let metadata = std::fs::symlink_metadata(path).map_err(|_| {
                WikiError::new(ErrorCode::Usage, "cannot inspect context selection reply")
            })?;
            if !metadata.is_file() {
                return Err(WikiError::new(
                    ErrorCode::Usage,
                    "context selection reply must be a regular file or stdin",
                ));
            }
            let mut open = std::fs::OpenOptions::new();
            open.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                open.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
            }
            let file = open.open(path).map_err(|_| {
                WikiError::new(ErrorCode::Usage, "cannot open context selection reply")
            })?;
            if !file
                .metadata()
                .map_err(|_| {
                    WikiError::new(ErrorCode::Usage, "cannot inspect context selection reply")
                })?
                .is_file()
            {
                return Err(WikiError::new(
                    ErrorCode::Usage,
                    "context selection reply must be a regular file or stdin",
                ));
            }
            file.take(4097).read_to_end(&mut bytes)
        };
        read.map_err(|_| WikiError::new(ErrorCode::Usage, "cannot read context selection reply"))?;
        Ok(SelectionAction::Apply(parse_reply(&bytes)?))
    }
    pub fn request(&self) -> ContextRequest {
        let mut documents = self.search.plan();
        // Context needs enough surrounding prose to support an answer; search
        // keeps its shorter discovery snippets. Explicit caller bounds win.
        documents.limits.excerpt_bytes = self.search.excerpt_bytes.unwrap_or(1024);
        let target = match self.target {
            Target::Documents => ContextTarget::Documents,
            Target::Graph => ContextTarget::Graph,
            Target::Combined => ContextTarget::Combined,
        };
        let graph = if target == ContextTarget::Documents {
            None
        } else {
            Some(GraphPlan {
                strategy: match self.strategy {
                    Strategy::Entity => GraphStrategy::Entity,
                    Strategy::Relationship => GraphStrategy::Relationship,
                    Strategy::Combined => GraphStrategy::Combined,
                },
                seed_mode: match self.seed {
                    Seed::Lexical => GraphSeedMode::Lexical,
                    Seed::Semantic => GraphSeedMode::Semantic,
                },
                filters: documents.filters.clone(),
                limits: GraphLimits {
                    candidates: documents.limits.candidates,
                    seeds: self.seeds,
                    depth: self.depth,
                    incident_per_seed: self.incident_per_seed,
                    assertions: self.assertions,
                    hits: documents.limits.hits,
                    excerpt_bytes: documents.limits.excerpt_bytes,
                    ..Default::default()
                },
                include_navigation: self.navigation,
                cursor: None,
            })
        };
        ContextRequest {
            scope: match self.scope.unwrap_or(Scope::Current) {
                Scope::Current => ContextScope::Current,
                Scope::Historical => ContextScope::Historical,
                Scope::Snapshot => ContextScope::Snapshot,
                Scope::IndexedEvidence => ContextScope::IndexedEvidence,
                Scope::IndexedDocuments => ContextScope::IndexedDocuments,
            },
            target,
            documents,
            graph,
            budget: ContextBudget {
                max_bytes: self.max_bytes,
                max_tokens: self.max_tokens,
                instruction_bytes: self.instruction_bytes,
                instruction_tokens: self.instruction_tokens,
                output_bytes: self.output_bytes,
                output_tokens: self.output_tokens,
            },
            verification_budget: VerificationBudget {
                max_bytes: self.verification_max_bytes,
                max_files: self.verification_max_files,
                max_entries: self.verification_max_entries,
                max_elapsed_ms: self.verification_max_elapsed_ms,
            },
        }
    }
}

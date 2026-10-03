//! Bounded context request adapter; evidence verification stays in the coordinator.
use super::arguments::{SearchArguments, Seed, Strategy};
use crate::{graph::*, retrieval::*};
use clap::{Args, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Scope {
    Current,
    Historical,
    Snapshot,
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
    /// Evidence scope: verified current, historical, or unverified index snapshot.
    #[arg(long, value_enum, default_value = "current")]
    pub scope: Scope,
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
            scope: match self.scope {
                Scope::Current => ContextScope::Current,
                Scope::Historical => ContextScope::Historical,
                Scope::Snapshot => ContextScope::Snapshot,
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

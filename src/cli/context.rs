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
    #[arg(long, value_enum, default_value = "current")]
    pub scope: Scope,
    #[arg(long, value_enum, default_value = "documents")]
    pub target: Target,
    #[arg(long, value_enum, default_value = "combined")]
    pub strategy: Strategy,
    #[arg(long, value_enum, default_value = "lexical")]
    pub seed: Seed,
    #[arg(long, default_value_t = 12)]
    pub seeds: usize,
    #[arg(long, default_value_t = 1)]
    pub depth: usize,
    #[arg(long, default_value_t = 16)]
    pub incident_per_seed: usize,
    #[arg(long, default_value_t = 128)]
    pub assertions: usize,
    #[arg(long)]
    pub navigation: bool,
    #[arg(long, default_value_t = 12000)]
    pub max_bytes: usize,
    #[arg(long, default_value_t = 3000)]
    pub max_tokens: usize,
    #[arg(long, default_value_t = 0)]
    pub instruction_bytes: usize,
    #[arg(long, default_value_t = 0)]
    pub instruction_tokens: usize,
    #[arg(long, default_value_t = 0)]
    pub output_bytes: usize,
    #[arg(long, default_value_t = 0)]
    pub output_tokens: usize,
    #[arg(long, default_value_t = 67108864)]
    pub verification_max_bytes: usize,
    #[arg(long, default_value_t = 4096)]
    pub verification_max_files: usize,
    #[arg(long, default_value_t = 16384)]
    pub verification_max_entries: usize,
    #[arg(long, default_value_t = 2000)]
    pub verification_max_elapsed_ms: u64,
}
impl ContextArguments {
    pub fn request(&self) -> ContextRequest {
        let documents = self.search.plan();
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

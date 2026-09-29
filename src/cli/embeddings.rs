//! Explicit embedding commands/settings. Shared runtime and dispatch stay root-owned.
use crate::retrieval::spaces::EmbeddingSettings;
use clap::{Args, Subcommand};
#[derive(Debug, Args)]
pub struct EmbeddingArguments {
    #[command(subcommand)]
    pub command: EmbeddingCommand,
}
#[derive(Debug, Subcommand)]
pub enum EmbeddingCommand {
    /// Inspect local embedding coverage; --probe explicitly contacts the provider.
    Check(EmbeddingCheckArguments),
    /// Generate missing embeddings through a trusted remote provider.
    Sync(EmbeddingSyncArguments),
}
#[derive(Debug, Args, Default)]
pub struct EmbeddingSettingsArguments {
    /// Text prepended to document embedding inputs; part of the embedding space identity.
    #[arg(long, default_value = "")]
    pub document_prefix: String,
    /// Text prepended to query embedding inputs; part of the embedding space identity.
    #[arg(long, default_value = "")]
    pub query_prefix: String,
    /// Maximum UTF-8 bytes per embedding input.
    #[arg(long, default_value_t = 12000)]
    pub max_input_bytes: usize,
    /// Preferred segment size below the hard embedding input ceiling.
    #[arg(long)]
    pub quality_target_bytes: Option<usize>,
}
impl EmbeddingSettingsArguments {
    pub fn settings(&self) -> EmbeddingSettings {
        EmbeddingSettings {
            document_prefix: self.document_prefix.clone(),
            query_prefix: self.query_prefix.clone(),
            max_input_bytes: self.max_input_bytes,
            quality_target_bytes: self.quality_target_bytes,
        }
    }
}
#[derive(Debug, Args)]
pub struct EmbeddingCheckArguments {
    #[command(flatten)]
    pub settings: EmbeddingSettingsArguments,
    #[command(flatten)]
    pub remote: super::remote::RemoteArguments,
    /// Explicitly contact the selected provider within the supplied request limits.
    #[arg(long)]
    pub probe: bool,
}
#[derive(Debug, Args)]
pub struct EmbeddingSyncArguments {
    #[command(flatten)]
    pub settings: EmbeddingSettingsArguments,
    #[command(flatten)]
    pub remote: super::remote::RemoteArguments,
}

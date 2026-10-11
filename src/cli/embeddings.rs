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
    #[arg(long)]
    pub document_prefix: Option<String>,
    /// Text prepended to query embedding inputs; part of the embedding space identity.
    #[arg(long)]
    pub query_prefix: Option<String>,
    /// Maximum UTF-8 bytes per embedding input.
    #[arg(long)]
    pub max_input_bytes: Option<usize>,
    /// Preferred segment size below the hard embedding input ceiling.
    #[arg(long)]
    pub quality_target_bytes: Option<usize>,
}
impl EmbeddingSettingsArguments {
    pub fn settings(&self, defaults: &crate::config::EmbeddingDefaults) -> EmbeddingSettings {
        EmbeddingSettings {
            document_prefix: self
                .document_prefix
                .clone()
                .or_else(|| defaults.document_prefix.clone())
                .unwrap_or_default(),
            query_prefix: self
                .query_prefix
                .clone()
                .or_else(|| defaults.query_prefix.clone())
                .unwrap_or_default(),
            max_input_bytes: self
                .max_input_bytes
                .or(defaults.max_input_bytes)
                .unwrap_or(12000),
            quality_target_bytes: self.quality_target_bytes.or(defaults.quality_target_bytes),
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

#[cfg(test)]
mod configuration_tests {
    use super::*;
    #[test]
    fn saved_embedding_settings_allow_explicit_empty_prefix_override() {
        let defaults = crate::config::EmbeddingDefaults {
            document_prefix: Some("document: ".into()),
            query_prefix: Some("query: ".into()),
            max_input_bytes: Some(4000),
            quality_target_bytes: Some(2000),
        };
        let cli = EmbeddingSettingsArguments::default();
        let settings = cli.settings(&defaults);
        assert_eq!(settings.document_prefix, "document: ");
        assert_eq!(settings.query_prefix, "query: ");
        assert_eq!(settings.max_input_bytes, 4000);
        assert_eq!(settings.quality_target_bytes, Some(2000));
        let cli = EmbeddingSettingsArguments {
            query_prefix: Some(String::new()),
            max_input_bytes: Some(5000),
            ..Default::default()
        };
        let settings = cli.settings(&defaults);
        assert_eq!(settings.query_prefix, "");
        assert_eq!(settings.max_input_bytes, 5000);
        assert_eq!(settings.document_prefix, "document: ");
    }
}

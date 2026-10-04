//! Explicit local collection imports share the application's durable group lifecycle.
use crate::{
    app::OfflineApp,
    domain::{ErrorCode, Result, WikiError},
};
use clap::{Args, Subcommand};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct SourceImportArguments {
    #[command(subcommand)]
    pub command: SourceImportCommand,
}

#[derive(Debug, Subcommand)]
pub enum SourceImportCommand {
    /// Hash an explicit JSON Lines list into a new manifest without opening a wiki.
    Prepare {
        /// JSON Lines local input declarations; relative paths use this list's directory.
        #[arg(long)]
        input_list: PathBuf,
        /// New manifest file; the parent must exist and existing files are preserved.
        #[arg(long)]
        output: PathBuf,
    },
    /// Import a prepared manifest in bounded committed groups, or continue the same key.
    Run {
        /// Immutable manifest produced by source import prepare.
        #[arg(long)]
        manifest: PathBuf,
        /// Caller-chosen import identity; reusing the key requires the same manifest.
        #[arg(long)]
        key: String,
        /// Maximum sources per committed group (1–8); group members become visible together.
        #[arg(long, default_value_t = 4, value_parser = parse_group_size)]
        group_size: usize,
        /// Maximum groups to process in this invocation (1–10000).
        #[arg(long, default_value_t = 64, value_parser = parse_max_groups)]
        max_groups: usize,
    },
    /// Continue an import using its retained manifest and exact pending group.
    Resume {
        /// Caller-chosen identity supplied to source import run.
        #[arg(long)]
        key: String,
        /// Maximum groups to process in this invocation (1–10000).
        #[arg(long, default_value_t = 64, value_parser = parse_max_groups)]
        max_groups: usize,
    },
    /// Read retained import progress and committed source/revision mappings.
    Status {
        /// Caller-chosen identity supplied to source import run.
        #[arg(long)]
        key: String,
    },
}

impl SourceImportCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Prepare { .. } => "source import prepare",
            Self::Run { .. } => "source import run",
            Self::Resume { .. } => "source import resume",
            Self::Status { .. } => "source import status",
        }
    }

    pub(super) fn requires_vault(&self) -> bool {
        match self {
            Self::Prepare { .. } => false,
            Self::Run { .. } | Self::Resume { .. } | Self::Status { .. } => true,
        }
    }
}

fn bounded(value: &str, maximum: usize, name: &str) -> std::result::Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|parsed| (1..=maximum).contains(parsed))
        .ok_or_else(|| format!("{name} must be between 1 and {maximum}"))
}

fn parse_group_size(value: &str) -> std::result::Result<usize, String> {
    bounded(value, 8, "group size")
}

fn parse_max_groups(value: &str) -> std::result::Result<usize, String> {
    bounded(value, 10_000, "maximum groups")
}

pub(super) fn execute(command: &SourceImportCommand, app: &OfflineApp) -> Result<Value> {
    let outcome = match command {
        SourceImportCommand::Run {
            manifest,
            key,
            group_size,
            max_groups,
        } => app.source_import_run(manifest, key, *group_size, *max_groups)?,
        SourceImportCommand::Resume { key, max_groups } => {
            app.source_import_resume(key, *max_groups)?
        }
        SourceImportCommand::Status { key } => app.source_import_status(key)?,
        SourceImportCommand::Prepare { .. } => {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "source import prepare must dispatch before vault discovery",
            ));
        }
    };
    serde_json::to_value(outcome)
        .map_err(|error| WikiError::new(ErrorCode::Internal, error.to_string()))
}

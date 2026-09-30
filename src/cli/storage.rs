//! Read-only storage plans and explicit guarded cleanup.
use crate::{app::OfflineApp, domain::*, storage, vault::WriterPermit};
use clap::{Args, Subcommand};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Args)]
pub struct StorageArguments {
    #[command(subcommand)]
    pub command: StorageCommand,
}
#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// Measure bounded file, logical-byte and duplicate-content totals.
    Inventory(StorageLimits),
    /// Preview exact retained/protected data and migration blockers without writes.
    Plan(StorageLimits),
    /// Migrate and compact retained operational data; keep full-vault backups.
    Cleanup(StorageLimits),
}
impl StorageCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Inventory(_) => "storage inventory",
            Self::Plan(_) => "storage plan",
            Self::Cleanup(_) => "storage cleanup",
        }
    }
}
#[derive(Debug, Args)]
pub struct StorageLimits {
    /// Completed changes to retain for ordinary undo; dependencies remain protected.
    #[arg(long, default_value_t = 20)]
    pub retain_undo_changes: usize,
    /// Maximum physical files to inspect; partial inventory is disclosed.
    #[arg(long, default_value_t = 100_000)]
    pub max_files: usize,
    /// Maximum aggregate bytes to hash during inventory and cleanup planning.
    #[arg(long, default_value_t = 1_073_741_824)]
    pub max_bytes: u64,
    /// Bind cleanup to the exact hash returned by storage plan.
    #[arg(long)]
    pub expected_plan: Option<Blake3Hash>,
}
pub fn execute(command: &StorageCommand, app: &OfflineApp) -> Result<Value> {
    let limits = match command {
        StorageCommand::Inventory(x) | StorageCommand::Plan(x) | StorageCommand::Cleanup(x) => x,
    };
    let options = storage::StorageOptions {
        retain_undo_changes: limits.retain_undo_changes,
        max_files: limits.max_files,
        max_bytes: limits.max_bytes,
        expected_plan: limits.expected_plan.clone(),
    };
    if app.options().stage_only {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "storage operations cannot be staged; use storage plan",
        ));
    }
    match command {
        StorageCommand::Inventory(_) => {
            serde_json::to_value(storage::inventory(app.fs(), &options)?)
                .map_err(|_| WikiError::invalid("storage result encoding"))
        }
        StorageCommand::Plan(_) => serde_json::to_value(storage::plan_cleanup(app.fs(), &options)?)
            .map_err(|_| WikiError::invalid("storage result encoding")),
        StorageCommand::Cleanup(_) if app.options().dry_run => {
            serde_json::to_value(storage::plan_cleanup(app.fs(), &options)?)
                .map_err(|_| WikiError::invalid("storage result encoding"))
        }
        StorageCommand::Cleanup(_) => {
            let writer = WriterPermit::acquire(
                app.fs().root(),
                Duration::from_millis(app.options().lock_timeout_ms),
            )?;
            serde_json::to_value(storage::cleanup(app.fs(), &writer, &options)?)
                .map_err(|_| WikiError::invalid("storage result encoding"))
        }
    }
}

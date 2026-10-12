//! Read-only storage plans and explicit guarded cleanup.
use crate::{app::OfflineApp, catalog::Catalog, domain::*, storage, vault::WriterPermit};
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
    /// Make sources readable in Obsidian while preserving all notes and history.
    Migrate(SourceLayoutLimits),
}
impl StorageCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Inventory(_) => "storage inventory",
            Self::Plan(_) => "storage plan",
            Self::Cleanup(_) => "storage cleanup",
            Self::Migrate(_) => "storage migrate",
        }
    }
}
#[derive(Debug, Args)]
pub struct SourceLayoutLimits {
    /// Maximum files to inspect before making any changes.
    #[arg(long, default_value_t = 100_000)]
    pub max_files: usize,
    /// Maximum bytes to inspect before making any changes.
    #[arg(long, default_value_t = 1_073_741_824)]
    pub max_bytes: u64,
    /// Use only the exact plan returned by storage migrate --dry-run.
    #[arg(long)]
    pub expected_plan: Option<Blake3Hash>,
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
    if let StorageCommand::Migrate(limits) = command {
        if app.options().stage_only {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "storage migration cannot be staged; use storage migrate --dry-run to preview it",
            ));
        }
        let options = storage::SourceLayoutOptions {
            max_files: limits.max_files,
            max_bytes: limits.max_bytes,
            expected_plan: limits.expected_plan.clone(),
        };
        let result = if app.options().dry_run {
            serde_json::to_value(storage::plan_source_layout(app.fs(), &options)?)
        } else {
            let writer = WriterPermit::acquire(
                app.fs().root(),
                Duration::from_millis(app.options().lock_timeout_ms),
            )?;
            // The migration publishes the resulting catalog before returning.
            serde_json::to_value(storage::migrate_source_layout(app.fs(), &writer, &options)?)
        };
        return result.map_err(|_| WikiError::invalid("storage result encoding"));
    }
    let limits = match command {
        StorageCommand::Inventory(x) | StorageCommand::Plan(x) | StorageCommand::Cleanup(x) => x,
        StorageCommand::Migrate(_) => unreachable!("handled above"),
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
        StorageCommand::Migrate(_) => unreachable!("handled above"),
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
            let mut report = storage::cleanup(app.fs(), &writer, &options)?;
            // Migration changes canonical identity inputs, including WIKI.md.
            // Publish their derived rows before returning command success.
            let mut completion_phase = "catalog_publication";
            let mut publication_completed = false;
            let publication = (|| -> Result<()> {
                let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
                if catalog.operation_state()?.is_some() {
                    catalog.sync_normalized(&writer)?;
                    publication_completed = true;
                    completion_phase = "storage_inventory";
                    let after = storage::inventory(app.fs(), &options)?;
                    if !after.complete {
                        return Err(WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "post-publication storage inventory exceeds its bounds",
                        ));
                    }
                    report.after = after.totals;
                }
                Ok(())
            })();
            if let Err(mut error) = publication {
                error.message = format!(
                    "Storage cleanup committed; {completion_phase} failed: {}",
                    error.message
                );
                error.details = serde_json::json!({
                    "cleanup_committed": true,
                    "cleanup_operation_id": report.operation_id,
                    "layout_version": report.layout_version,
                    "completion_phase": completion_phase,
                    "catalog_publication_completed": publication_completed,
                    "publication_error": error.details,
                });
                error.hint = Some(if publication_completed {
                    "Inspect storage inventory with larger scan bounds; catalog publication already completed.".into()
                } else {
                    "Run index sync to finish catalog publication; back up the entire vault including .wiki.".into()
                });
                return Err(error);
            }
            serde_json::to_value(report).map_err(|_| WikiError::invalid("storage result encoding"))
        }
    }
}

pub(super) fn present_migration(
    data: &Value,
    output: &mut impl std::io::Write,
) -> std::io::Result<()> {
    if data.get("plan_hash").is_some() {
        writeln!(output, "Preview only. No files were changed.")?;
        if data["from_layout"] == 3 {
            return writeln!(output, "Your sources are already organized for Obsidian.");
        }
        if data["complete"] == false {
            writeln!(
                output,
                "The preview could not check every file. Resolve the issues below before migrating."
            )?;
        }
        writeln!(
            output,
            "Sources to organize: {}. Historical copies needed for existing links: {}.",
            data["sources"], data["historical_copies"]
        )?;
        writeln!(
            output,
            "All notes and history will be preserved, with a full backup before migration."
        )?;
        if let Some(blockers) = data["blockers"].as_array() {
            for blocker in blockers.iter().filter_map(Value::as_str) {
                writeln!(
                    output,
                    "Needs attention: {}",
                    super::dispatch::terminal_text(blocker, false)
                )?;
            }
        }
        if let Some(hash) = data["plan_hash"].as_str() {
            writeln!(output, "Plan: {hash}")?;
        }
    } else {
        if data["backup"].is_null() {
            return writeln!(
                output,
                "Your sources are already organized for Obsidian. No migration is needed."
            );
        }
        writeln!(output, "Sources are organized for browsing in Obsidian.")?;
        writeln!(
            output,
            "Sources: {}. Saved historical copies: {}.",
            data["sources"], data["historical_copies"]
        )?;
        if let Some(backup) = data["backup"].as_str() {
            writeln!(
                output,
                "Full backup: {}",
                super::dispatch::terminal_text(backup, false)
            )?;
        }
    }
    Ok(())
}

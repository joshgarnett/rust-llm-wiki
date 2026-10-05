//! Selected, recoverable authored Page reorganization on normalized vaults.
use super::{MutationOutcome, OfflineApp, PlanSummary};
use crate::{
    catalog::{
        query_types::QueryReadLimits, rename_projection::project_page_rename,
        source_projection::RefreshProjectionLimits,
    },
    domain::{Blake3Hash, RecordId, Result, VaultRelativePath, WikiError},
};
use std::collections::BTreeMap;

impl OfflineApp {
    pub(super) fn page_rename_indexed(
        &self,
        id: RecordId,
        to: VaultRelativePath,
        hash: Blake3Hash,
    ) -> Result<MutationOutcome> {
        if !crate::sources::revision::canonical_path(&to) {
            return Err(WikiError::invalid(
                "rename destination must be canonical Markdown path",
            ));
        }
        let mut outcome = MutationOutcome {
            source_capture: None,
            plan: PlanSummary {
                title: format!("Rename Page {id} to {to}"),
                read_preconditions: vec![],
                operations: vec![],
            },
            change: None,
            status: None,
            snapshot: None,
            reused: false,
            allocated_ids: BTreeMap::new(),
        };
        // A WAL reader can mutate coordination sidecars. This request-only
        // preview does not resolve identity, inspect the target or seal a move.
        if self.options().dry_run {
            outcome.plan.title = format!(
                "Would rename Page {id} to {to} with author hash {hash}; selected admission, incoming links and destination availability are unperformed"
            );
            return Ok(outcome);
        }
        let writer = self.writer()?;
        let catalog = self.catalog();
        catalog.guard_current(None)?;
        let reader = catalog.query_snapshot(QueryReadLimits::default())?;
        let projected = project_page_rename(
            self.fs(),
            &reader,
            id,
            to,
            hash,
            &RefreshProjectionLimits::default(),
        )?;
        // Publication opens its own retained base reader. Release this planner
        // snapshot after all owned rows and commitments have been produced.
        drop(reader);
        match projected {
            Some(projected) => {
                // The common publisher reacquires the writer and rechecks the
                // sealed base and exact before-images before preparation.
                drop(writer);
                self.publish_source_write(&catalog, projected, None)
            }
            None => {
                outcome.reused = true;
                Ok(outcome)
            }
        }
    }
}

#[cfg(test)]
#[path = "page_rename_tests.rs"]
mod tests;

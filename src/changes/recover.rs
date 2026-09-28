//! Automatic recovery requires trustworthy applying intent, never matching bytes alone.
use super::{apply::recovery_error, journal, outcome, types::*};
use crate::{domain::Result, vault::WriterPermit};
impl ChangeEngine {
    pub fn recover(
        &self,
        permit: &WriterPermit,
        validator: &dyn GraphValidator,
        publisher: &dyn PublicationBackend,
    ) -> Result<RecoveryReport> {
        permit.require_root(self.fs.root())?;
        self.require_binding()?;
        let mut report = RecoveryReport {
            changes: Vec::new(),
            staged: Vec::new(),
        };
        let mut pending = Vec::new();
        // Discover every unresolved record before giving any publisher authority.
        for id in self.change_ids()? {
            let (manifest, hash) = self.load_manifest(&id)?;
            if let Some(terminal) = outcome::terminal_report(&self.fs, &manifest, &hash)? {
                outcome::sync_receipt(&self.fs, permit, &manifest.change_id)?;
                report.changes.push(terminal);
                continue;
            }
            let state = journal::load_journal(&self.fs, &manifest, &hash)?;
            let change = PreparedChange {
                change_id: id,
                manifest_hash: hash,
            };
            match state.status {
                Some(ChangeStatus::Committed | ChangeStatus::Aborted) => {
                    report.changes.push(outcome::retain_terminal(
                        self,
                        permit,
                        &manifest,
                        &change.manifest_hash,
                        &state,
                    )?);
                }
                Some(ChangeStatus::Conflict) => {
                    return Err(recovery_error("durable conflict blocks automatic recovery"));
                }
                Some(ChangeStatus::Prepared) => {
                    report.staged.push(change);
                }
                None => {
                    let observations = self.observe(&manifest)?;
                    if observations
                        .iter()
                        .any(|o| o.observed != o.before && o.observed != o.after)
                    {
                        return self.conflict(
                            permit,
                            &manifest,
                            &change.manifest_hash,
                            "staged recovery",
                            observations,
                        );
                    }
                    if observations.iter().any(|o| o.observed != o.before) {
                        return Err(recovery_error(
                            "retained proposal has mixed/new targets without trustworthy applying intent",
                        ));
                    }
                    report.staged.push(change);
                }
                Some(
                    ChangeStatus::Applying | ChangeStatus::FilesApplied | ChangeStatus::Indexed,
                ) => pending.push((change, manifest, state)),
            }
        }
        for (change, manifest, state) in pending {
            report.changes.push(
                self.continue_apply(permit, &change, &manifest, &state, validator, publisher)?,
            );
        }
        Ok(report)
    }
}

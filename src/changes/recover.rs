//! Automatic recovery requires trustworthy applying intent, never matching bytes alone.
use super::{apply::recovery_error, journal, outcome, types::*};
use crate::{domain::Result, vault::WriterPermit};
impl ChangeEngine {
    /// Explicit maintenance enumeration preserves the complete historical and
    /// staged report. Ordinary refresh and exact `changes apply` never call it.
    pub(crate) fn recover_indexed(
        &self,
        permit: &WriterPermit,
        catalog: &crate::catalog::Catalog,
    ) -> Result<RecoveryReport> {
        use crate::{
            catalog::source_refresh::IndexedRefreshSession,
            domain::{ErrorCode, WikiError},
        };
        permit.require_root(self.fs.root())?;
        self.require_binding()?;
        let authority = catalog
            .operation_state()?
            .ok_or_else(|| recovery_error("normalized recovery authority is absent"))?;
        let mut report = RecoveryReport {
            changes: vec![],
            staged: vec![],
        };
        let mut pending = Vec::new();
        for id in self.change_ids()? {
            let (manifest, hash) = self.load_manifest_structure(&id)?;
            let change = PreparedChange {
                change_id: id.clone(),
                manifest_hash: hash.clone(),
            };
            if let Some(proof) = self.indexed_replay_proof(&manifest, &change, Some(&authority))? {
                if let Some(terminal) = self.indexed_refresh_terminal_report(permit, &change)? {
                    report.changes.push(terminal);
                    continue;
                }
                let state = journal::load_journal(&self.fs, &manifest, &hash)?;
                if state.status == Some(ChangeStatus::Conflict) {
                    return Err(recovery_error("durable conflict blocks automatic recovery"));
                }
                if authority
                    .active()
                    .is_some_and(|active| active.change == change)
                {
                    pending.push(proof);
                } else if matches!(state.status, None | Some(ChangeStatus::Prepared)) {
                    self.validate_manifest(&manifest, &id)?;
                    if self
                        .observe(&manifest)?
                        .iter()
                        .any(|o| o.observed != o.before)
                    {
                        return Err(recovery_error(
                            "staged indexed proposal has changed targets without applying authority",
                        ));
                    }
                    report.staged.push(change);
                } else {
                    return Err(recovery_error(
                        "unfinished indexed change is not named by active authority",
                    ));
                }
                continue;
            }
            if let Some(terminal) = outcome::terminal_report(&self.fs, &manifest, &hash)? {
                outcome::sync_receipt(&self.fs, permit, &id)?;
                report.changes.push(terminal);
                continue;
            }
            self.validate_manifest(&manifest, &id)?;
            let state = journal::load_journal(&self.fs, &manifest, &hash)?;
            match state.status {
                Some(ChangeStatus::Committed | ChangeStatus::Aborted) => {
                    report.changes.push(outcome::retain_terminal(
                        self, permit, &manifest, &hash, &state,
                    )?);
                }
                None | Some(ChangeStatus::Prepared) => {
                    if self
                        .observe(&manifest)?
                        .iter()
                        .any(|o| o.observed != o.before)
                    {
                        return Err(recovery_error(
                            "legacy staged proposal has changed targets behind normalized catalog",
                        ));
                    }
                    report.staged.push(change);
                }
                _ => {
                    return Err(WikiError::new(
                        ErrorCode::CapabilityUnavailable,
                        "legacy unfinished change cannot replay behind a normalized catalog",
                    ));
                }
            }
        }
        if authority.active().is_some() && pending.len() != 1 {
            return Err(recovery_error(
                "active indexed operation is missing from retained history",
            ));
        }
        for proof in pending {
            let mut session = IndexedRefreshSession::resume(catalog, permit, proof)?;
            report
                .changes
                .push(self.apply_indexed_refresh(permit, &mut session)?);
        }
        Ok(report)
    }

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
            let (manifest, hash) = self.load_manifest_structure(&id)?;
            if let Some(terminal) = outcome::terminal_report(&self.fs, &manifest, &hash)? {
                outcome::sync_receipt(&self.fs, permit, &manifest.change_id)?;
                report.changes.push(terminal);
                continue;
            }
            self.validate_manifest(&manifest, &id)?;
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

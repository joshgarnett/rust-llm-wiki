//! Explicit, observation-bound exits from a durable conflict. No unfamiliar bytes are written.
use super::{
    apply::{projected_scan, recovery_error},
    journal, outcome,
    prepare::{MAX_JOURNAL_BYTES, read_bounded},
    types::*,
};
use crate::{
    domain::{ErrorCode, Result, VaultRelativePath, WikiError},
    vault::WriterPermit,
};

impl ChangeEngine {
    /// Pure preview. An observation is not authority until resolve rechecks it under the writer lock.
    pub fn resolution_plan(
        &self,
        change: &PreparedChange,
        mode: ConflictResolutionMode,
    ) -> Result<ConflictResolutionRequest> {
        self.require_binding()?;
        let (manifest, hash) = self.load_manifest(&change.change_id)?;
        if hash != change.manifest_hash {
            return Err(WikiError::invalid("resolution manifest binding changed"));
        }
        if outcome::terminal_report(&self.fs, &manifest, &hash)?.is_some() {
            return Err(recovery_error(
                "terminal change does not require conflict resolution",
            ));
        }
        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
        let conflict = state
            .frames
            .last()
            .filter(|frame| matches!(frame.event, ChangeEvent::Conflict { .. }))
            .ok_or_else(|| recovery_error("resolution requires a durable conflict"))?;
        Ok(ConflictResolutionRequest {
            change: change.clone(),
            mode,
            conflict_sequence: conflict.sequence,
            conflict_hash: journal::frame_hash(conflict)?,
            observations: self.observe(&manifest)?,
        })
    }

    pub fn resolve(
        &self,
        permit: &WriterPermit,
        request: &ConflictResolutionRequest,
        validator: &dyn GraphValidator,
        publisher: &dyn PublicationBackend,
    ) -> Result<ApplyReport> {
        permit.require_root(self.fs.root())?;
        self.require_binding()?;
        let change = &request.change;
        let (manifest, hash) = self.load_manifest(&change.change_id)?;
        if hash != change.manifest_hash {
            return Err(WikiError::invalid("resolution manifest binding changed"));
        }
        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
        let event = ChangeEvent::ResolutionAccepted {
            mode: request.mode,
            conflict_sequence: request.conflict_sequence,
            conflict_hash: request.conflict_hash.clone(),
            observations: request.observations.clone(),
        };
        // A retry after the accepted event must not invent a new resolution or bind newer edits.
        // Terminal receipts can survive loss of the operational transcript; ordinary apply/recover
        // remains the supported route for that historical case.
        if state.status != Some(ChangeStatus::Conflict) {
            if !state.frames.iter().any(|frame| frame.event == event) {
                return Err(recovery_error(
                    "request does not bind an accepted conflict resolution",
                ));
            }
            return self.apply(permit, change, validator, publisher);
        }
        if self.resolution_plan(change, request.mode)? != *request {
            return Err(conflict(
                "resolution observations or conflict binding changed; preview again",
            ));
        }
        self.require_no_other_unresolved(change)?;
        match request.mode {
            ConflictResolutionMode::Resume => {
                if request
                    .observations
                    .iter()
                    .any(|o| o.observed != o.before && o.observed != o.after)
                {
                    return Err(conflict(
                        "resume requires every target at its recorded before or proposed state; unfamiliar bytes preserved",
                    ));
                }
                publisher.check_available()?;
                self.verify_read_preconditions(permit, &manifest, &hash)?;
                self.require_revision_baseline(&manifest)?;
                let previously_applying = state.frames.iter().any(|frame| {
                    matches!(
                        frame.event,
                        ChangeEvent::Applying
                            | ChangeEvent::ResolutionAccepted {
                                mode: ConflictResolutionMode::Resume,
                                ..
                            }
                    )
                });
                if previously_applying
                    && read_bounded(
                        &self.fs,
                        &VaultRelativePath::new(format!(
                            "changes/{}/validation.json",
                            change.change_id
                        ))?,
                        MAX_JOURNAL_BYTES,
                    )?
                    .is_none()
                {
                    return Err(recovery_error(
                        "resolution cannot recreate a lost original applying validation baseline",
                    ));
                }
                let input = self.validation_input(&manifest)?;
                let graph = self.validate_graph(&manifest, &hash, validator, &input)?;
                self.retain_validation(
                    permit,
                    change,
                    &manifest,
                    &state,
                    &graph,
                    &projected_scan(&input),
                )?;
                // Preserve lost-journal recovery with retained ownership, but never allow a
                // formerly applying tree to recreate its missing ownership receipt.
                let mut ownership_state = state.clone();
                if previously_applying {
                    ownership_state.status = Some(ChangeStatus::Applying);
                }
                self.preflight_revision_trees(permit, &manifest, &hash, &ownership_state)?;
                self.verify_read_preconditions(permit, &manifest, &hash)?;
                if self.observe(&manifest)? != request.observations {
                    return Err(conflict("targets changed during resolution preflight"));
                }
                let accepted = journal::append_event(&self.fs, permit, &manifest, &hash, event)?;
                self.continue_apply(permit, change, &manifest, &accepted, validator, publisher)
            }
            ConflictResolutionMode::Abandon => {
                if request.observations.iter().any(|o| o.observed != o.before) {
                    return Err(conflict(
                        "abandon requires every target restored to its recorded before state; immutable revisions must remain preserved or resolve by resuming",
                    ));
                }
                self.require_abandoned_revision_trees(&manifest)?;
                let input = ValidationInput {
                    vault_id: self.vault_id.clone(),
                    documents: self.scan_documents()?,
                    overlay: Vec::new(),
                };
                // Validate actual restored content, never the abandoned proposal's overlay or
                // obsolete read guards. No new publication authority is issued by abandonment.
                let graph = validator.validate(&self.fs, &input)?;
                for dependency in &graph.dependencies {
                    if self.target_state(&dependency.path)? != dependency.expected {
                        return Err(conflict(
                            "restored graph dependency changed during abandonment",
                        ));
                    }
                }
                let rechecked = ValidationInput {
                    vault_id: self.vault_id.clone(),
                    documents: self.scan_documents()?,
                    overlay: Vec::new(),
                };
                if projected_scan(&input) != projected_scan(&rechecked)
                    || validator.validate(&self.fs, &rechecked)? != graph
                    || self.observe(&manifest)? != request.observations
                {
                    return Err(conflict("restored graph changed during abandonment"));
                }
                self.require_abandoned_revision_trees(&manifest)?;
                // This event is terminal itself: a crash cannot expose abandonment as Prepared
                // and accidentally authorize ordinary application of the discarded proposal.
                let accepted = journal::append_event(&self.fs, permit, &manifest, &hash, event)?;
                outcome::retain_terminal(self, permit, &manifest, &hash, &accepted)
            }
        }
    }
}

fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}

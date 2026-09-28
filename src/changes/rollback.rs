//! Abort only unapplied proposals; rollback is a separately retained, validated inverse.
use super::{apply::recovery_error, journal, outcome, types::*};
use crate::{
    domain::{Blake3Hash, Result, WikiError},
    vault::WriterPermit,
};
use std::collections::BTreeMap;
impl ChangeEngine {
    pub fn abort(&self, permit: &WriterPermit, change: &PreparedChange) -> Result<ApplyReport> {
        permit.require_root(self.fs.root())?;
        self.require_binding()?;
        let (manifest, hash) = self.load_manifest(&change.change_id)?;
        if hash != change.manifest_hash {
            return Err(WikiError::invalid("prepared manifest binding changed"));
        }
        if let Some(report) = outcome::terminal_report(&self.fs, &manifest, &hash)? {
            if report.status == ChangeStatus::Aborted {
                outcome::sync_receipt(&self.fs, permit, &manifest.change_id)?;
                return Ok(report);
            }
            return Err(recovery_error(
                "committed change requires a guarded inverse",
            ));
        }
        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
        if state.status == Some(ChangeStatus::Aborted) {
            return outcome::retain_terminal(self, permit, &manifest, &hash, &state);
        }
        if !matches!(state.status, None | Some(ChangeStatus::Prepared)) {
            return Err(recovery_error(
                "an applying or conflicted change cannot be aborted",
            ));
        }
        if state.status.is_none() {
            let observations = self.observe(&manifest)?;
            if observations.iter().any(|o| o.observed != o.before) {
                return Err(recovery_error(
                    "abort requires all targets at their prepared old state",
                ));
            }
            journal::append_event(&self.fs, permit, &manifest, &hash, ChangeEvent::Prepared)?;
        }
        outcome::finish(self, permit, &manifest, &hash, ChangeStatus::Aborted)
    }
    pub fn inverse_plan(&self, change: &PreparedChange) -> Result<InversePlan> {
        self.require_binding()?;
        let (manifest, hash) = self.load_manifest(&change.change_id)?;
        if hash != change.manifest_hash {
            return Err(WikiError::invalid("prepared manifest binding changed"));
        }
        let mut operations = Vec::new();
        let mut retained_paths = Vec::new();
        for (index, op) in manifest.operations.iter().enumerate() {
            if op.role == OperationRole::ImmutableAsset {
                retained_paths.push(op.target.clone());
                continue;
            }
            operations.push(ExpectedWrite {
                target: op.target.clone(),
                expected: op.after.clone(),
                proposed: self.verify_payload(
                    &manifest.change_id,
                    index,
                    "before",
                    &op.target,
                    &op.before,
                    &op.before_payload,
                )?,
                apply_after: Vec::new(),
            });
        }
        // Reverse original ordering: rename source is restored before its destination is removed.
        for (index, op) in manifest.operations.iter().enumerate() {
            for dependency in &op.apply_after {
                let dependency_target = &manifest.operations[*dependency].target;
                if operations.iter().any(|o| o.target == op.target)
                    && let Some(inverse) = operations
                        .iter_mut()
                        .find(|o| &o.target == dependency_target)
                {
                    inverse
                        .apply_after
                        .push(manifest.operations[index].target.clone());
                }
            }
        }
        let draft = ChangeDraft {
            title: format!("Inverse: {}", manifest.title),
            origin: None,
            inverse_of: Some(manifest.change_id),
            allocated_ids: BTreeMap::new(),
            read_preconditions: Vec::new(),
            operations,
        };
        self.plan(&draft)?; // Preserves unfamiliar edits and immutable source rules in read-only planning.
        Ok(InversePlan {
            draft,
            retained_paths,
        })
    }
    pub fn prepare_inverse(
        &self,
        permit: &WriterPermit,
        change: &PreparedChange,
        validator: &dyn GraphValidator,
    ) -> Result<PreparedChange> {
        permit.require_root(self.fs.root())?;
        let plan = self.inverse_plan(change)?;
        let input = ValidationInput {
            vault_id: self.vault_id.clone(),
            documents: self
                .fs
                .root()
                .scan_markdown()?
                .into_iter()
                .map(|path| {
                    let bytes = super::prepare::read_bounded(
                        &self.fs,
                        &path,
                        super::prepare::MAX_PAYLOAD_BYTES,
                    )?
                    .ok_or_else(|| recovery_error("inverse scan file disappeared"))?;
                    Ok(ScanDocument {
                        hash: Blake3Hash::digest(&bytes),
                        bytes,
                        path,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            overlay: plan
                .draft
                .operations
                .iter()
                .map(|o| ProposedTarget {
                    path: o.target.clone(),
                    bytes: o.proposed.clone(),
                })
                .collect(),
        };
        validator.validate(&self.fs, &input)?;
        Ok(self.prepare(permit, plan.draft)?.prepared)
    }
}

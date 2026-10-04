//! Bounded source-refresh application over an exact retained publication plan.
//! Canonical mutation uses the same executor as whole-vault changes. Recovery
//! recognizes an already committed SQL epoch before requesting the old snapshot.
use super::{
    journal,
    operation_authority::{self as operations, Authority, Presence, Publication},
    outcome,
    prepare::{MAX_JOURNAL_BYTES, MAX_OPS, read_bounded, strict_json},
    types::*,
};
use crate::{
    catalog::source_refresh::IndexedRefreshSession,
    domain::{Blake3Hash, ErrorCode, ReadSnapshot, RecordId, Result, VaultRelativePath, WikiError},
    vault::{ExpectedState, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexedRefreshPhase {
    AtBase,
    AlreadyPublished,
}

/// Version two is deliberately distinct from whole-vault validation receipts.
/// The delta hash binds the versioned replay payload retained by the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexedRefreshProof {
    pub version: u32,
    pub vault_id: RecordId,
    pub source_id: RecordId,
    pub change: PreparedChange,
    pub base: ReadSnapshot,
    pub intended: ReadSnapshot,
    pub delta_hash: Blake3Hash,
    pub before: Vec<ReadDependency>,
    pub after: Vec<ReadDependency>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    proof: IndexedRefreshProof,
    checksum: Blake3Hash,
}
fn recovery(message: &str) -> WikiError {
    super::apply::recovery_error(message)
}
fn baseline_path(change: &PreparedChange) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("changes/{}/validation.json", change.change_id))
}
fn publication(snapshot: &ReadSnapshot) -> Result<Publication> {
    let binding = snapshot
        .publication()
        .ok_or_else(|| recovery("indexed refresh requires PublishedEpoch bindings"))?;
    if binding.version != 1
        || binding.file_id.len() != 32
        || !binding
            .file_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || snapshot.generation == 0
        || snapshot.generation > i64::MAX as u64
    {
        return Err(recovery("indexed refresh publication binding is invalid"));
    }
    Ok(Publication {
        file_id: binding.file_id.clone(),
        epoch: snapshot.generation,
    })
}
impl IndexedRefreshProof {
    fn validate(&self, vault: &RecordId, change: &PreparedChange) -> Result<()> {
        if self.version != 2 || &self.vault_id != vault || &self.change != change {
            return Err(recovery(
                "indexed refresh baseline identity or version differs",
            ));
        }
        let base = publication(&self.base)?;
        let intended = publication(&self.intended)?;
        if base.file_id != intended.file_id
            || base.epoch.checked_add(1) != Some(intended.epoch)
            || self.base.parser_fingerprint != self.intended.parser_fingerprint
            || self.base.publication().unwrap().publication_hash
                == self.intended.publication().unwrap().publication_hash
        {
            return Err(recovery(
                "indexed refresh baseline does not name one successor publication",
            ));
        }
        if self.before.len() > MAX_OPS
            || self.after.len() > MAX_OPS
            || self.before.len() != self.after.len()
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "indexed refresh dependency scope exceeds its bound",
            ));
        }
        if !self
            .before
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path)
            || !self
                .after
                .windows(2)
                .all(|pair| pair[0].path < pair[1].path)
            || self
                .before
                .iter()
                .zip(&self.after)
                .any(|(before, after)| before.path != after.path)
        {
            return Err(recovery(
                "indexed refresh dependency paths are not one sorted unique scope",
            ));
        }
        Ok(())
    }
    pub(crate) fn validate_manifest(&self, manifest: &ChangeManifest) -> Result<()> {
        self.validate(&manifest.vault_id, &self.change)?;
        if manifest.change_id != self.change.change_id {
            return Err(recovery(
                "indexed refresh manifest differs from baseline change",
            ));
        }
        let before: BTreeMap<_, _> = self
            .before
            .iter()
            .map(|dependency| (&dependency.path, &dependency.expected))
            .collect();
        let after: BTreeMap<_, _> = self
            .after
            .iter()
            .map(|dependency| (&dependency.path, &dependency.expected))
            .collect();
        let operations: BTreeMap<_, _> = manifest
            .operations
            .iter()
            .map(|operation| (&operation.target, operation))
            .collect();
        for operation in &manifest.operations {
            if before.get(&operation.target).copied() != Some(&operation.before)
                || after.get(&operation.target).copied() != Some(&operation.after)
            {
                return Err(recovery(
                    "indexed refresh baseline omits or changes a manifest target",
                ));
            }
        }
        for dependency in &manifest.read_preconditions {
            if before.get(&dependency.path).copied() != Some(&dependency.expected)
                || after.get(&dependency.path).copied() != Some(&dependency.expected)
            {
                return Err(recovery(
                    "indexed refresh baseline omits or changes a retained read precondition",
                ));
            }
        }
        for (before, after) in self.before.iter().zip(&self.after) {
            if !operations.contains_key(&before.path) {
                if before.expected != after.expected {
                    return Err(recovery(
                        "indexed refresh baseline grants an undeclared mutation",
                    ));
                }
                if !manifest
                    .read_preconditions
                    .iter()
                    .any(|dependency| dependency == before)
                {
                    return Err(recovery(
                        "selected refresh boundary is absent from retained read preconditions",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Only this module can construct the seal, after the catalog session verifies
/// exact intended hash/origin/delta and manifest-derived current owner rows.
/// The immutable module uses it solely for complete receipt/member checking.
pub(super) struct VerifiedIndexedRefreshPublication {
    proof: IndexedRefreshProof,
}
impl VerifiedIndexedRefreshPublication {
    pub(super) fn proof(&self) -> &IndexedRefreshProof {
        &self.proof
    }
    pub(super) fn require_for(&self, engine: &ChangeEngine, writer: &WriterPermit) -> Result<()> {
        writer.require_root(engine.fs.root())?;
        engine.require_binding()?;
        self.proof.validate(&engine.vault_id, &self.proof.change)?;
        let retained = engine
            .load_indexed_refresh_proof(&self.proof.change)?
            .ok_or_else(|| recovery("published refresh lost its retained baseline"))?;
        if retained != self.proof {
            return Err(recovery("published refresh baseline changed"));
        }
        let authority = required_authority(engine)?;
        require_active(&authority, &self.proof)
    }
}
fn required_authority(engine: &ChangeEngine) -> Result<Authority> {
    operations::load(&engine.fs, &engine.vault_id, Presence::Required)?
        .ok_or_else(|| recovery("indexed refresh requires operation authority"))
}
fn require_active(authority: &Authority, proof: &IndexedRefreshProof) -> Result<()> {
    let active = authority
        .active()
        .ok_or_else(|| recovery("indexed refresh has no active reservation"))?;
    if active.change != proof.change
        || active.starting != publication(&proof.base)?
        || active.intended != publication(&proof.intended)?
    {
        return Err(recovery(
            "indexed refresh differs from exact active reservation",
        ));
    }
    Ok(())
}

impl ChangeEngine {
    /// Bounded read for recovery dispatch; absence does not authorize creating a
    /// new baseline for an applying or SQL-published operation.
    pub(crate) fn load_indexed_refresh_proof(
        &self,
        change: &PreparedChange,
    ) -> Result<Option<IndexedRefreshProof>> {
        let Some(bytes) = read_bounded(&self.fs, &baseline_path(change)?, MAX_JOURNAL_BYTES)?
        else {
            return Ok(None);
        };
        let receipt: Receipt = strict_json(&bytes)?;
        let encoded = serde_json::to_vec(&receipt.proof)
            .map_err(|error| WikiError::invalid(error.to_string()))?;
        if Blake3Hash::digest(encoded) != receipt.checksum {
            return Err(recovery("indexed refresh baseline checksum differs"));
        }
        receipt.proof.validate(&self.vault_id, change)?;
        Ok(Some(receipt.proof))
    }
    fn retain_indexed_refresh_proof(
        &self,
        writer: &WriterPermit,
        proof: &IndexedRefreshProof,
        allow_create: bool,
    ) -> Result<()> {
        if let Some(retained) = self.load_indexed_refresh_proof(&proof.change)? {
            if retained != *proof {
                return Err(recovery(
                    "indexed refresh must resume its exact retained plan",
                ));
            }
            return journal::require_sync(
                self.fs
                    .sync_target(&baseline_path(&proof.change)?, writer)?,
            );
        }
        if !allow_create {
            return Err(recovery(
                "active indexed refresh lost its original baseline",
            ));
        }
        let checksum = Blake3Hash::digest(
            serde_json::to_vec(proof).map_err(|error| WikiError::invalid(error.to_string()))?,
        );
        let bytes = serde_json::to_vec(&Receipt {
            proof: proof.clone(),
            checksum,
        })
        .map_err(|error| WikiError::invalid(error.to_string()))?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "indexed refresh baseline exceeds byte ceiling",
            ));
        }
        let staged = self
            .fs
            .stage(&baseline_path(&proof.change)?, &bytes, writer)?;
        journal::require_sync(self.fs.replace(staged, &ExpectedState::Absent, writer)?)
    }
    #[cfg(test)]
    fn verify_indexed_dependencies(
        &self,
        proof: &IndexedRefreshProof,
        manifest: &ChangeManifest,
        after_only: bool,
        allow_mixed: bool,
    ) -> Result<()> {
        for (before, after) in proof.before.iter().zip(&proof.after) {
            let actual = self.target_state(&before.path)?;
            let valid = if after_only {
                actual == after.expected
            } else {
                actual == before.expected
                    || (allow_mixed
                        && manifest
                            .operations
                            .iter()
                            .any(|operation| operation.target == before.path)
                        && actual == after.expected)
            };
            if !valid {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    format!(
                        "indexed refresh selected dependency changed: {}",
                        before.path
                    ),
                ));
            }
        }
        Ok(())
    }
    fn seal_indexed_publication(
        &self,
        writer: &WriterPermit,
        session: &IndexedRefreshSession<'_>,
        proof: &IndexedRefreshProof,
    ) -> Result<VerifiedIndexedRefreshPublication> {
        if session.proof() != proof || session.verify_published()? != proof.intended {
            return Err(recovery(
                "catalog did not verify the exact intended indexed refresh",
            ));
        }
        let seal = VerifiedIndexedRefreshPublication {
            proof: proof.clone(),
        };
        seal.require_for(self, writer)?;
        Ok(seal)
    }
    fn acknowledge_indexed_refresh(
        &self,
        writer: &WriterPermit,
        manifest: &ChangeManifest,
        proof: &IndexedRefreshProof,
        report: &ApplyReport,
    ) -> Result<()> {
        if report.status != ChangeStatus::Committed
            || report.change != proof.change
            || report.snapshot.as_ref() != Some(&proof.intended)
        {
            return Err(recovery(
                "indexed refresh terminal receipt does not prove intended publication",
            ));
        }
        outcome::sync_receipt(&self.fs, writer, &proof.change.change_id)?;
        if outcome::terminal_report(&self.fs, manifest, &proof.change.manifest_hash)?.as_ref()
            != Some(report)
        {
            return Err(recovery(
                "indexed refresh terminal receipt changed before acknowledgement",
            ));
        }
        let authority = required_authority(self)?;
        require_active(&authority, proof)?;
        operations::acknowledge(
            &self.fs,
            writer,
            &authority,
            &proof.change,
            publication(&proof.intended)?,
        )?;
        Ok(())
    }

    /// Historical terminal retries must run before constructing a live session:
    /// the original base/intended epoch may have been superseded. This proves
    /// only the retained outcome, never current canonical freshness.
    pub(crate) fn indexed_refresh_terminal_report(
        &self,
        writer: &WriterPermit,
        change: &PreparedChange,
    ) -> Result<Option<ApplyReport>> {
        writer.require_root(self.fs.root())?;
        self.require_binding()?;
        let Some(proof) = self.load_indexed_refresh_proof(change)? else {
            return Ok(None);
        };
        let (manifest, hash) = self.load_manifest_structure(&change.change_id)?;
        if hash != change.manifest_hash {
            return Err(recovery("terminal refresh manifest binding changed"));
        }
        proof.validate_manifest(&manifest)?;
        let Some(report) = outcome::terminal_report(&self.fs, &manifest, &hash)? else {
            return Ok(None);
        };
        if report.change != *change
            || (report.status == ChangeStatus::Committed
                && report.snapshot.as_ref() != Some(&proof.intended))
        {
            return Err(recovery(
                "terminal refresh receipt differs from retained plan",
            ));
        }
        let authority = required_authority(self)?;
        if authority
            .active()
            .is_some_and(|active| active.change == *change)
        {
            return Ok(None); // exact live session must finish acknowledgement/cancel
        }
        let expected = publication(if report.status == ChangeStatus::Committed {
            &proof.intended
        } else {
            &proof.base
        })?;
        let floor = authority.publication();
        if floor.epoch < expected.epoch
            || (floor.epoch == expected.epoch && floor.file_id != expected.file_id)
        {
            return Err(recovery(
                "historical refresh outcome exceeds acknowledged publication",
            ));
        }
        outcome::sync_receipt(&self.fs, writer, &change.change_id)?;
        if outcome::terminal_report(&self.fs, &manifest, &hash)?.as_ref() != Some(&report) {
            return Err(recovery("historical refresh receipt changed while syncing"));
        }
        Ok(Some(report))
    }

    pub(crate) fn apply_indexed_refresh(
        &self,
        writer: &WriterPermit,
        session: &mut IndexedRefreshSession<'_>,
    ) -> Result<ApplyReport> {
        writer.require_root(self.fs.root())?;
        self.require_binding()?;
        let proof = session.proof().clone();
        proof.validate(&self.vault_id, &proof.change)?;
        let (manifest, hash) = self.load_manifest_structure(&proof.change.change_id)?;
        if hash != proof.change.manifest_hash {
            return Err(recovery("indexed refresh retained manifest changed"));
        }
        proof.validate_manifest(&manifest)?;
        let terminal = outcome::terminal_report(&self.fs, &manifest, &hash)?;
        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
        let mut authority = required_authority(self)?;
        let phase = session.phase();
        let allow_create = phase == IndexedRefreshPhase::AtBase
            && authority.active().is_none()
            && terminal.is_none()
            && matches!(state.status, None | Some(ChangeStatus::Prepared));
        self.retain_indexed_refresh_proof(writer, &proof, allow_create)?;

        if let Some(report) = terminal {
            if report.change != proof.change {
                return Err(recovery("indexed refresh terminal identity differs"));
            }
            if report.status == ChangeStatus::Aborted {
                if phase != IndexedRefreshPhase::AtBase {
                    return Err(recovery("aborted indexed refresh has a published delta"));
                }
                session.verify_selected(false, false)?;
                self.require_abandoned_revision_trees(&manifest)?;
                outcome::sync_receipt(&self.fs, writer, &proof.change.change_id)?;
                if authority.active().is_some() {
                    require_active(&authority, &proof)?;
                    operations::cancel(&self.fs, writer, &authority, &proof.change)?;
                }
                return Ok(report);
            }
            if report.snapshot.as_ref() != Some(&proof.intended) {
                return Err(recovery(
                    "terminal indexed refresh names another publication",
                ));
            }
            if authority.active().is_none() {
                let committed = publication(&proof.intended)?;
                let floor = authority.publication();
                if floor.epoch < committed.epoch
                    || (floor.epoch == committed.epoch && floor.file_id != committed.file_id)
                {
                    return Err(recovery(
                        "terminal indexed refresh has not been acknowledged by idle authority",
                    ));
                }
                outcome::sync_receipt(&self.fs, writer, &proof.change.change_id)?;
                return Ok(report);
            }
            require_active(&authority, &proof)?;
            if phase != IndexedRefreshPhase::AlreadyPublished {
                return Err(recovery(
                    "committed indexed refresh SQL publication is absent",
                ));
            }
            session.verify_selected(true, false)?;
            self.require_all_after(writer, &manifest, &hash)?;
            let seal = self.seal_indexed_publication(writer, session, &proof)?;
            self.verify_indexed_finalization(writer, &seal)?;
            self.acknowledge_indexed_refresh(writer, &manifest, &proof, &report)?;
            return Ok(report);
        }
        if matches!(
            state.status,
            Some(ChangeStatus::Conflict | ChangeStatus::Aborted)
        ) {
            return Err(recovery(
                "indexed refresh has a durable conflict or aborted intent",
            ));
        }
        if phase == IndexedRefreshPhase::AlreadyPublished {
            require_active(&authority, &proof)?;
            if !matches!(
                state.status,
                Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed | ChangeStatus::Committed)
            ) {
                return Err(recovery(
                    "published indexed refresh lacks surviving files-applied intent",
                ));
            }
            // Never replay canonical writes or try to open the vanished base.
            session.verify_selected(true, false)?;
            self.require_all_after(writer, &manifest, &hash)?;
        } else {
            if !matches!(
                state.status,
                None | Some(
                    ChangeStatus::Prepared | ChangeStatus::Applying | ChangeStatus::FilesApplied
                )
            ) {
                return Err(recovery(
                    "base indexed refresh has incompatible journal publication",
                ));
            }
            if authority.active().is_none() {
                if authority.publication() != &publication(&proof.base)?
                    || !matches!(state.status, None | Some(ChangeStatus::Prepared))
                {
                    return Err(recovery("indexed refresh base differs from idle authority"));
                }
                session.verify_selected(false, false)?;
                session.validate_before_files()?;
                authority = operations::begin(
                    &self.fs,
                    writer,
                    &authority,
                    proof.change.clone(),
                    publication(&proof.intended)?,
                )?;
            }
            require_active(&authority, &proof)?;
            let mixed = matches!(
                state.status,
                Some(ChangeStatus::Applying | ChangeStatus::FilesApplied)
            );
            session.verify_selected(false, mixed)?;
            session.validate_before_files()?;
            let observations = self.observe(&manifest)?;
            if observations.iter().any(|value| {
                value.observed != value.before && (!mixed || value.observed != value.after)
            }) {
                return self.conflict(
                    writer,
                    &manifest,
                    &hash,
                    "indexed refresh preflight",
                    observations,
                );
            }
            // Verify retained payloads before the shared executor can mutate.
            self.validate_manifest(&manifest, &proof.change.change_id)?;
            let guard = self.indexed_revision_guard(
                writer,
                &proof.change,
                session.starting_ownership_lookup()?,
            )?;
            if let Err(error) = guard.preflight() {
                return self.revision_failure(writer, &manifest, &hash, &state, error);
            }
            self.apply_files_to_files_applied(
                writer,
                &proof.change,
                &manifest,
                &state,
                &observations,
                &|complete| {
                    if let Err(error) = guard.verify(complete) {
                        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
                        return self.revision_failure(writer, &manifest, &hash, &state, error);
                    }
                    Ok(())
                },
            )?;
            self.require_all_after(writer, &manifest, &hash)?;
            session.verify_selected(true, false)?;
            let owners = guard.complete_owner_rows()?;
            drop(guard);
            if session.publish(&owners)? != proof.intended {
                return Err(recovery("indexed refresh published an unexpected snapshot"));
            }
        }
        session.verify_selected(true, false)?;
        self.require_all_after(writer, &manifest, &hash)?;
        let seal = self.seal_indexed_publication(writer, session, &proof)?;
        self.verify_indexed_finalization(writer, &seal)?;
        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
        let report = match state.status {
            Some(ChangeStatus::Committed) => {
                outcome::retain_terminal(self, writer, &manifest, &hash, &state)?
            }
            Some(ChangeStatus::FilesApplied) => {
                journal::append_event(
                    &self.fs,
                    writer,
                    &manifest,
                    &hash,
                    ChangeEvent::Indexed {
                        snapshot: proof.intended.clone(),
                    },
                )?;
                outcome::finish(self, writer, &manifest, &hash, ChangeStatus::Committed)?
            }
            Some(ChangeStatus::Indexed) => {
                let indexed = state
                    .frames
                    .iter()
                    .rev()
                    .find_map(|frame| match &frame.event {
                        ChangeEvent::Indexed { snapshot } => Some(snapshot),
                        _ => None,
                    });
                if indexed != Some(&proof.intended) {
                    return Err(recovery(
                        "indexed journal snapshot differs from retained intended publication",
                    ));
                }
                outcome::finish(self, writer, &manifest, &hash, ChangeStatus::Committed)?
            }
            _ => {
                return Err(recovery(
                    "indexed refresh cannot finalize from its journal state",
                ));
            }
        };
        self.acknowledge_indexed_refresh(writer, &manifest, &proof, &report)?;
        Ok(report)
    }
}

#[cfg(test)]
#[path = "indexed_refresh_tests.rs"]
mod tests;

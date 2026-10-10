//! Strict retained replay dispatch, before loading historical payloads.
use super::{
    ChangeEngine, ChangeManifest, ChangeStatus, PreparedChange,
    apply::recovery_error,
    indexed_refresh::IndexedRefreshProof,
    operation_authority::Authority,
    outcome,
    prepare::{MAX_JOURNAL_BYTES, read_bounded, strict_json},
};
use crate::domain::{Result, VaultRelativePath, WikiError};

impl ChangeEngine {
    pub(crate) fn indexed_replay_proof(
        &self,
        manifest: &ChangeManifest,
        change: &PreparedChange,
        authority: Option<&Authority>,
    ) -> Result<Option<IndexedRefreshProof>> {
        let baseline =
            VaultRelativePath::new(format!("changes/{}/validation.json", change.change_id))?;
        let delta = self.fs.root().resolve(&VaultRelativePath::new(format!(
            "changes/{}/indexed-delta.json",
            change.change_id
        ))?)?;
        let delta_present = match std::fs::symlink_metadata(delta) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(WikiError::invalid(error.to_string())),
        };
        let terminal = outcome::terminal_report(&self.fs, manifest, &change.manifest_hash)?;
        let requires_indexed = delta_present
            || authority
                .and_then(Authority::active)
                .is_some_and(|active| active.change.change_id == change.change_id)
            || terminal
                .as_ref()
                .and_then(|report| report.snapshot.as_ref())
                .is_some_and(|snapshot| snapshot.publication().is_some());
        let Some(bytes) = read_bounded(&self.fs, &baseline, MAX_JOURNAL_BYTES)? else {
            // A never-started indexed proposal may be aborted before its first
            // proof is retained. Its replacement can legitimately own the same
            // targets now; report authenticated history without reopening them.
            // This exception is absence-only: present proofs remain strict.
            if let Some(authority) = authority
                && authority
                    .active()
                    .is_none_or(|active| active.change.change_id != change.change_id)
                && terminal.as_ref().is_some_and(|report| {
                    report.status == ChangeStatus::Aborted && report.snapshot.is_none()
                })
                && outcome::terminal_ever_applying(&self.fs, manifest, &change.manifest_hash)?
                    == Some(false)
            {
                return Ok(None);
            }
            if requires_indexed {
                return Err(recovery_error(
                    "indexed change lost its retained validation proof",
                ));
            }
            return Ok(None);
        };
        let value: serde_json::Value = strict_json(&bytes)?;
        match value
            .get("proof")
            .and_then(|proof| proof.get("version"))
            .and_then(|version| version.as_u64())
        {
            Some(2 | 3 | 4) => {
                let proof = self.load_indexed_refresh_proof(change)?.ok_or_else(|| {
                    recovery_error("indexed validation proof disappeared during dispatch")
                })?;
                proof.validate_manifest(manifest)?;
                Ok(Some(proof))
            }
            Some(1) if !requires_indexed => {
                super::apply::verify_legacy_validation_receipt(&bytes, change)?;
                Ok(None)
            }
            _ => Err(recovery_error(
                "retained validation proof has an unknown or inconsistent replay kind",
            )),
        }
    }
}

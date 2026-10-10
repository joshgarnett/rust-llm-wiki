//! Filesystem-only preview. A SQLite WAL reader can change coordination files.
use super::*;

impl IndexedRefreshSession<'_> {
    /// Check retained intent and selected file guards without asserting SQL
    /// layout, ownership, publication or apply readiness.
    pub(crate) fn check_preview(catalog: &Catalog, proof: &IndexedRefreshProof) -> Result<()> {
        let engine = ChangeEngine::new(catalog.fs.clone())?;
        if engine.load_indexed_refresh_proof(&proof.change)?.as_ref() != Some(proof) {
            return Err(recovery("preview lost its retained indexed proof"));
        }
        let delta = load_delta(catalog, proof)?;
        delta.require_bound(catalog, &engine, proof)?;
        let (manifest, hash) = engine.load_manifest_structure(&proof.change.change_id)?;
        if hash != proof.change.manifest_hash {
            return Err(recovery("preview manifest binding changed"));
        }
        engine.validate_manifest(&manifest, &proof.change.change_id)?;
        let journal_path = journal::journal_path(&proof.change.change_id)?;
        let journal_bytes = read_bounded(
            &catalog.fs,
            &journal_path,
            crate::changes::prepare::MAX_JOURNAL_BYTES,
        )?;
        let state = journal::decode_journal(
            journal_bytes.as_deref().unwrap_or_default(),
            &manifest,
            &hash,
        )?;
        let journal_hash = journal_bytes.as_ref().map(Blake3Hash::digest);
        drop(journal_bytes);
        let authority = catalog
            .operation_state()?
            .ok_or_else(|| recovery("preview authority absent"))?;
        let base = publication(&proof.base)?;
        let intended = publication(&proof.intended)?;
        let (after_only, mixed, active_required) = match state.status {
            None | Some(ChangeStatus::Prepared) => (false, false, false),
            Some(ChangeStatus::Applying) => (false, true, true),
            Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed | ChangeStatus::Committed) => {
                (true, false, true)
            }
            _ => {
                return Err(recovery(
                    "preview requires trustworthy retained replay history",
                ));
            }
        };
        if let Some(active) = authority.active() {
            if active
                != &(ActiveOperation {
                    change: proof.change.clone(),
                    starting: base.clone(),
                    intended,
                })
                || authority.publication() != &base
            {
                return Err(recovery(
                    "preview active authority differs from retained operation",
                ));
            }
        } else if active_required || authority.publication() != &base {
            return Err(recovery(
                "preview acknowledged authority differs from retained base",
            ));
        }
        if matches!(
            state.status,
            Some(ChangeStatus::Indexed | ChangeStatus::Committed)
        ) && state
            .frames
            .iter()
            .rev()
            .find_map(|frame| match &frame.event {
                crate::changes::ChangeEvent::Indexed { snapshot } => Some(snapshot),
                _ => None,
            })
            != Some(&proof.intended)
        {
            return Err(recovery(
                "preview journal names another indexed publication",
            ));
        }
        verify_dependencies(
            catalog,
            if after_only {
                &proof.after
            } else {
                &proof.before
            },
            mixed.then_some(proof.after.as_slice()),
            &Cell::new(MAX_SELECTED_BYTES),
        )?;
        // These reads are observations, not a writer-held execution permit.
        // Refuse concurrent authority/history changes without opening SQLite.
        if catalog.operation_state()?.as_ref() != Some(&authority)
            || read_bounded(
                &catalog.fs,
                &journal_path,
                crate::changes::prepare::MAX_JOURNAL_BYTES,
            )?
            .map(Blake3Hash::digest)
                != journal_hash
            || engine.load_indexed_refresh_proof(&proof.change)?.as_ref() != Some(proof)
            || load_delta(catalog, proof)? != delta
            || engine.load_manifest_structure(&proof.change.change_id)?.1 != hash
        {
            return Err(recovery(
                "preview retained authority changed during observation",
            ));
        }
        Ok(())
    }
}

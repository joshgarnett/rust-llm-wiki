//! A historical v3 view commits to the exact immutable v4 terminal envelope.
//! The view never supplies live replay authority; resolution returns full v4.
use super::*;
use crate::catalog::source_refresh::{RetainedDelta, intended};
use crate::changes::prepare::{MAX_MANIFEST_BYTES, manifest_path};

const DOMAIN: &str = "lwiki.legacy-page-terminal-view.v1";

pub(super) fn archive_path(change: &PreparedChange) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!(
        "changes/{}/legacy-page-validation-v4.json",
        change.change_id
    ))
}

fn required_bytes(
    engine: &ChangeEngine,
    path: &VaultRelativePath,
    limit: usize,
) -> Result<Vec<u8>> {
    read_bounded(&engine.fs, path, limit)?
        .ok_or_else(|| recovery("legacy terminal view lost required retained authority"))
}

struct TerminalObservation {
    manifest: ChangeManifest,
    note: Vec<u8>,
    outcome: Vec<u8>,
    report: ApplyReport,
    authority: Authority,
}

fn outcome_path(change: &PreparedChange) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("changes/{}/outcome.json", change.change_id))
}

/// Historical validation deliberately avoids current author and retained payload
/// reads. The complete delta hash/publication and durable terminal receipt bind
/// the original executed authority even after author edits or payload expiry.
fn terminal(engine: &ChangeEngine, proof: &IndexedRefreshProof) -> Result<TerminalObservation> {
    engine.require_binding()?;
    if proof.version != 4 {
        return Err(recovery(
            "legacy terminal archive must contain full version4",
        ));
    }
    let note_path = manifest_path(&proof.change.change_id)?;
    let note = required_bytes(engine, &note_path, MAX_MANIFEST_BYTES + 65_536)?;
    let (manifest, hash) = engine.load_manifest_structure(&proof.change.change_id)?;
    if hash != proof.change.manifest_hash {
        return Err(recovery("legacy terminal manifest binding differs"));
    }
    proof.validate_manifest(&manifest)?;
    let outcome_path = outcome_path(&proof.change)?;
    let outcome_bytes = required_bytes(engine, &outcome_path, MAX_JOURNAL_BYTES)?;
    let report = outcome::terminal_report(&engine.fs, &manifest, &hash)?
        .ok_or_else(|| recovery("legacy terminal view requires retained Committed outcome"))?;
    if report.change != proof.change
        || report.status != ChangeStatus::Committed
        || report.snapshot.as_ref() != Some(&proof.intended)
    {
        return Err(recovery(
            "legacy terminal outcome differs from full intended publication",
        ));
    }
    let authority = required_authority(engine)?;
    if authority
        .active()
        .is_some_and(|active| active.change == proof.change)
    {
        return Err(recovery(
            "legacy terminal view requires completed acknowledgement",
        ));
    }
    let expected = publication(&proof.intended)?;
    let floor = authority.publication();
    if floor.epoch < expected.epoch
        || (floor.epoch == expected.epoch && floor.file_id != expected.file_id)
    {
        return Err(recovery(
            "legacy terminal outcome exceeds acknowledged publication",
        ));
    }
    if required_bytes(engine, &note_path, MAX_MANIFEST_BYTES + 65_536)? != note
        || required_bytes(engine, &outcome_path, MAX_JOURNAL_BYTES)? != outcome_bytes
    {
        return Err(recovery(
            "legacy terminal retained authority changed during observation",
        ));
    }
    Ok(TerminalObservation {
        manifest,
        note,
        outcome: outcome_bytes,
        report,
        authority,
    })
}

fn require_unchanged(
    engine: &ChangeEngine,
    proof: &IndexedRefreshProof,
    before: &TerminalObservation,
) -> Result<()> {
    let after = terminal(engine, proof)?;
    if before.manifest != after.manifest
        || before.note != after.note
        || before.outcome != after.outcome
        || before.report != after.report
        || !before.authority.same_revision(&after.authority)
    {
        return Err(recovery(
            "legacy terminal authority changed before final acceptance",
        ));
    }
    Ok(())
}

fn view(
    full_bytes: &[u8],
    full: &IndexedRefreshProof,
    manifest: &ChangeManifest,
) -> Result<(IndexedRefreshProof, Vec<u8>)> {
    let marker = Blake3Hash::digest(
        serde_json::to_vec(&(DOMAIN, Blake3Hash::digest(full_bytes)))
            .map_err(|error| WikiError::invalid(error.to_string()))?,
    );
    let mut projected = full.clone();
    projected.version = 3;
    projected.delta_hash = marker;
    let mut boundary = BTreeMap::new();
    for operation in &manifest.operations {
        if boundary
            .insert(
                operation.target.clone(),
                (operation.before.clone(), operation.after.clone()),
            )
            .is_some()
        {
            return Err(recovery("legacy terminal manifest repeats a target"));
        }
    }
    for dependency in &manifest.read_preconditions {
        if boundary
            .insert(
                dependency.path.clone(),
                (dependency.expected.clone(), dependency.expected.clone()),
            )
            .is_some()
        {
            return Err(recovery("legacy terminal manifest repeats a read boundary"));
        }
    }
    projected.before = boundary
        .iter()
        .map(|(path, (before, _))| ReadDependency {
            path: path.clone(),
            expected: before.clone(),
        })
        .collect();
    projected.after = boundary
        .into_iter()
        .map(|(path, (_, after))| ReadDependency {
            path,
            expected: after,
        })
        .collect();
    projected.validate_manifest(manifest)?;
    if intended(&projected.base, &projected.change, &projected.delta_hash, 3)? == projected.intended
    {
        return Err(recovery(
            "legacy terminal marker is indistinguishable from ordinary version3",
        ));
    }
    let checksum = Blake3Hash::digest(
        serde_json::to_vec(&projected).map_err(|error| WikiError::invalid(error.to_string()))?,
    );
    let receipt = Receipt {
        proof: projected.clone(),
        embedded_delta: None,
        checksum,
    };
    crate::catalog::normalized_delta::counted(&receipt, MAX_LEGACY_PAGE_ENVELOPE_BYTES)?;
    let bytes =
        serde_json::to_vec(&receipt).map_err(|error| WikiError::invalid(error.to_string()))?;
    Ok((projected, bytes))
}

/// Called only after the ordinary-v3 intended discriminator mismatches. Never
/// downgrade a missing/bad archive to an ordinary proof or a projected session.
pub(super) fn resolve(
    engine: &ChangeEngine,
    change: &PreparedChange,
    raw_view: &[u8],
    projected: IndexedRefreshProof,
) -> Result<(IndexedRefreshProof, Option<RetainedDelta>)> {
    if projected.version != 3 || raw_view.len() > MAX_LEGACY_PAGE_ENVELOPE_BYTES {
        return Err(recovery("legacy terminal view version or size differs"));
    }
    let path = archive_path(change)?;
    let full_bytes = required_bytes(engine, &path, MAX_LEGACY_PAGE_ENVELOPE_BYTES)?;
    let (full, delta) = engine.decode_indexed_refresh_retention(change, &full_bytes)?;
    if full.version != 4 || delta.is_none() {
        return Err(recovery("legacy terminal archive is not complete version4"));
    }
    let observed = terminal(engine, &full)?;
    let (expected, expected_bytes) = view(&full_bytes, &full, &observed.manifest)?;
    if projected != expected || raw_view != expected_bytes {
        return Err(recovery(
            "legacy terminal view does not exactly bind its full archive",
        ));
    }
    if required_bytes(engine, &path, MAX_LEGACY_PAGE_ENVELOPE_BYTES)? != full_bytes
        || required_bytes(
            engine,
            &baseline_path(change)?,
            MAX_LEGACY_PAGE_ENVELOPE_BYTES,
        )? != raw_view
    {
        return Err(recovery(
            "legacy terminal view or full archive changed during observation",
        ));
    }
    require_unchanged(engine, &full, &observed)?;
    // An executable reduced delta is never allowed, even after terminal commit.
    if read_bounded(
        &engine.fs,
        &crate::catalog::source_refresh::delta_path(change)?,
        0,
    )?
    .is_some()
    {
        return Err(recovery(
            "legacy terminal view has an unexpected separate delta",
        ));
    }
    Ok((full, delta))
}

/// A precreated archive must never become authority for Prepared/Applying
/// work. Only the byte-identical interrupted terminal handoff is admissible.
pub(super) fn validate_full_archive(
    engine: &ChangeEngine,
    change: &PreparedChange,
    raw: &[u8],
    proof: &IndexedRefreshProof,
) -> Result<()> {
    let archive = archive_path(change)?;
    let Some(bytes) = read_bounded(&engine.fs, &archive, MAX_LEGACY_PAGE_ENVELOPE_BYTES)? else {
        if read_bounded(&engine.fs, &archive, MAX_LEGACY_PAGE_ENVELOPE_BYTES)?.is_some() {
            return Err(recovery(
                "legacy terminal archive appeared during observation",
            ));
        }
        return Ok(());
    };
    if proof.version != 4 || bytes != raw {
        return Err(recovery(
            "legacy terminal archive conflicts with original envelope",
        ));
    }
    let observed = terminal(engine, proof)?;
    if required_bytes(engine, &archive, MAX_LEGACY_PAGE_ENVELOPE_BYTES)? != bytes
        || required_bytes(
            engine,
            &baseline_path(change)?,
            MAX_LEGACY_PAGE_ENVELOPE_BYTES,
        )? != raw
    {
        return Err(recovery(
            "legacy terminal full handoff changed during observation",
        ));
    }
    require_unchanged(engine, proof, &observed)
}

/// Complete only this exact committed handoff under the writer. Archive-first
/// ordering leaves either the original envelope or the exact view/archive pair.
pub(super) fn finalize(
    engine: &ChangeEngine,
    writer: &WriterPermit,
    change: &PreparedChange,
) -> Result<()> {
    writer.require_root(engine.fs.root())?;
    engine.require_binding()?;
    let baseline = baseline_path(change)?;
    let raw = required_bytes(engine, &baseline, MAX_JOURNAL_BYTES)?;
    let (proof, _) = engine.decode_indexed_refresh_retention(change, &raw)?;
    match proof.version {
        2 => return Ok(()),
        3 => {
            if intended(&proof.base, change, &proof.delta_hash, 3)? == proof.intended {
                return Ok(());
            }
            resolve(engine, change, &raw, proof.clone())?;
            outcome::sync_receipt(&engine.fs, writer, &change.change_id)?;
            journal::require_sync(engine.fs.sync_target(&archive_path(change)?, writer)?)?;
            journal::require_sync(engine.fs.sync_target(&baseline, writer)?)?;
            resolve(engine, change, &raw, proof)?;
            return Ok(());
        }
        4 => {}
        _ => return Err(recovery("legacy terminal handoff version differs")),
    }
    validate_full_archive(engine, change, &raw, &proof)?;
    let observed = terminal(engine, &proof)?;
    let (_, encoded_view) = view(&raw, &proof, &observed.manifest)?;
    outcome::sync_receipt(&engine.fs, writer, &change.change_id)?;
    require_unchanged(engine, &proof, &observed)?;
    let archive = archive_path(change)?;
    match read_bounded(&engine.fs, &archive, MAX_LEGACY_PAGE_ENVELOPE_BYTES)? {
        Some(existing) if existing == raw => {
            journal::require_sync(engine.fs.sync_target(&archive, writer)?)?;
        }
        Some(_) => {
            return Err(recovery(
                "legacy terminal archive conflicts with original envelope",
            ));
        }
        None => {
            let staged = engine.fs.stage(&archive, &raw, writer)?;
            require_unchanged(engine, &proof, &observed)?;
            journal::require_sync(engine.fs.replace(staged, &ExpectedState::Absent, writer)?)?;
        }
    }
    if required_bytes(engine, &archive, MAX_LEGACY_PAGE_ENVELOPE_BYTES)? != raw {
        return Err(recovery(
            "legacy terminal archive changed before view replacement",
        ));
    }
    require_unchanged(engine, &proof, &observed)?;
    let staged = engine.fs.stage(&baseline, &encoded_view, writer)?;
    require_unchanged(engine, &proof, &observed)?;
    journal::require_sync(engine.fs.replace(
        staged,
        &ExpectedState::Hash(Blake3Hash::digest(&raw)),
        writer,
    )?)?;
    let (projected, _) = engine.decode_indexed_refresh_retention(change, &encoded_view)?;
    resolve(engine, change, &encoded_view, projected)?;
    Ok(())
}

//! Durable retained terminal evidence, independent of editable note metadata.
use super::{
    journal,
    prepare::{
        MAX_JOURNAL_BYTES, MAX_MANIFEST_BYTES, manifest_path, read_bounded, render_prepared_note,
        strict_json,
    },
    types::*,
};
use crate::{
    domain::{Blake3Hash, Result, VaultRelativePath, WikiError},
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalProof {
    version: u32,
    change: PreparedChange,
    status: ChangeStatus,
    snapshot: Option<crate::domain::ReadSnapshot>,
    baseline_note_hash: Blake3Hash,
    finalized_note_hash: Blake3Hash,
    journal: Vec<JournalFrame>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    proof: TerminalProof,
    checksum: Blake3Hash,
}

fn path(id: &crate::domain::RecordId) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("changes/{id}/outcome.json"))
}
fn finalized_note(
    manifest: &ChangeManifest,
    hash: &Blake3Hash,
    status: ChangeStatus,
) -> Result<Vec<u8>> {
    let baseline = render_prepared_note(manifest, hash)?;
    let text =
        String::from_utf8(baseline).map_err(|_| WikiError::invalid("prepared note is not UTF8"))?;
    let replacement = match status {
        ChangeStatus::Committed => "wiki_status: committed\n",
        ChangeStatus::Aborted => "wiki_status: aborted\n",
        _ => return Err(WikiError::invalid("outcome requires terminal status")),
    };
    if text.matches("wiki_status: prepared\n").count() != 1 {
        return Err(WikiError::invalid("invalid engine note baseline"));
    }
    Ok(text
        .replacen("wiki_status: prepared\n", replacement, 1)
        .into_bytes())
}
fn encoded(frames: &[JournalFrame]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend(journal::encode_frame(frame)?);
    }
    Ok(bytes)
}
fn report(proof: &TerminalProof) -> ApplyReport {
    ApplyReport {
        change: proof.change.clone(),
        status: proof.status,
        snapshot: proof.snapshot.clone(),
    }
}

/// A valid receipt recognizes historical outcomes without re-reading obsolete canonical targets.
/// Existing operational corruption is still a refusal; a surviving valid transcript must be a prefix.
pub(crate) fn terminal_report(
    fs: &VaultFs,
    manifest: &ChangeManifest,
    hash: &Blake3Hash,
) -> Result<Option<ApplyReport>> {
    let Some(bytes) = read_bounded(fs, &path(&manifest.change_id)?, MAX_JOURNAL_BYTES)? else {
        return Ok(None);
    };
    let receipt: Receipt = strict_json(&bytes)?;
    let proof = &receipt.proof;
    let checksum = Blake3Hash::digest(
        serde_json::to_vec(proof).map_err(|e| WikiError::invalid(e.to_string()))?,
    );
    if checksum != receipt.checksum
        || proof.version != 1
        || proof.change.change_id != manifest.change_id
        || &proof.change.manifest_hash != hash
        || !matches!(
            proof.status,
            ChangeStatus::Committed | ChangeStatus::Aborted
        )
    {
        return Err(WikiError::invalid(
            "invalid retained terminal receipt binding/checksum",
        ));
    }
    let baseline = render_prepared_note(manifest, hash)?;
    let finalized = finalized_note(manifest, hash, proof.status)?;
    if proof.baseline_note_hash != Blake3Hash::digest(baseline)
        || proof.finalized_note_hash != Blake3Hash::digest(finalized)
    {
        return Err(WikiError::invalid("retained receipt note binding mismatch"));
    }
    let verified = journal::decode_journal(&encoded(&proof.journal)?, manifest, hash)?;
    let snapshot = proof.journal.iter().rev().find_map(|f| match &f.event {
        ChangeEvent::Indexed { snapshot } => Some(snapshot.clone()),
        _ => None,
    });
    if verified.status != Some(proof.status) || snapshot != proof.snapshot {
        return Err(WikiError::invalid(
            "retained receipt does not prove terminal outcome",
        ));
    }
    let surviving = journal::load_journal(fs, manifest, hash)?;
    if surviving.frames.len() > proof.journal.len()
        || surviving.frames != proof.journal[..surviving.frames.len()]
    {
        return Err(WikiError::invalid(
            "operational journal disagrees with retained outcome",
        ));
    }
    Ok(Some(report(proof)))
}

pub(crate) fn finish(
    engine: &ChangeEngine,
    permit: &WriterPermit,
    manifest: &ChangeManifest,
    hash: &Blake3Hash,
    status: ChangeStatus,
) -> Result<ApplyReport> {
    let state = journal::load_journal(&engine.fs, manifest, hash)?;
    retain(engine, permit, manifest, hash, &state, status, true)
}
pub(crate) fn retain_terminal(
    engine: &ChangeEngine,
    permit: &WriterPermit,
    manifest: &ChangeManifest,
    hash: &Blake3Hash,
    state: &JournalState,
) -> Result<ApplyReport> {
    let status = state
        .status
        .ok_or_else(|| WikiError::invalid("missing terminal journal"))?;
    retain(engine, permit, manifest, hash, state, status, false)
}
fn retain(
    engine: &ChangeEngine,
    permit: &WriterPermit,
    manifest: &ChangeManifest,
    hash: &Blake3Hash,
    state: &JournalState,
    status: ChangeStatus,
    append_terminal: bool,
) -> Result<ApplyReport> {
    permit.require_root(engine.fs.root())?;
    let baseline = render_prepared_note(manifest, hash)?;
    let finalized = finalized_note(manifest, hash, status)?;
    let baseline_hash = Blake3Hash::digest(&baseline);
    let final_hash = Blake3Hash::digest(&finalized);
    let note_path = manifest_path(&manifest.change_id)?;
    let current = read_bounded(&engine.fs, &note_path, MAX_MANIFEST_BYTES + 65_536)?
        .ok_or_else(|| WikiError::invalid("missing outcome note"))?;
    let current_hash = Blake3Hash::digest(&current);
    if current_hash != baseline_hash && current_hash != final_hash {
        if matches!(state.status, None | Some(ChangeStatus::Prepared)) {
            return Err(WikiError::new(
                crate::domain::ErrorCode::ContentConflict,
                "staged outcome note was edited",
            ));
        }
        if !matches!(
            state.status,
            Some(ChangeStatus::Committed | ChangeStatus::Aborted)
        ) {
            return engine.conflict(
                permit,
                manifest,
                hash,
                "outcome note was edited",
                engine.observe(manifest)?,
            );
        }
        return Err(WikiError::new(
            crate::domain::ErrorCode::ContentConflict,
            "historical outcome note was edited; no terminal receipt available",
        ));
    }
    let mut transcript = state.frames.clone();
    if append_terminal {
        transcript.push(JournalFrame {
            version: 1,
            sequence: transcript.len() as u64,
            change_id: manifest.change_id.clone(),
            manifest_hash: hash.clone(),
            event: match status {
                ChangeStatus::Committed => ChangeEvent::Committed,
                ChangeStatus::Aborted => ChangeEvent::Aborted,
                _ => return Err(WikiError::invalid("invalid outcome status")),
            },
        });
    }
    // Validate before the note changes; the terminal frame is retained before journal append.
    let verified = journal::decode_journal(&encoded(&transcript)?, manifest, hash)?;
    if verified.status != Some(status) {
        return Err(WikiError::invalid("outcome transcript is not terminal"));
    }
    if current_hash == baseline_hash {
        let staged = engine.fs.stage(&note_path, &finalized, permit)?;
        journal::require_sync(engine.fs.replace(
            staged,
            &ExpectedState::Hash(baseline_hash.clone()),
            permit,
        )?)?;
    } else {
        journal::require_sync(engine.fs.sync_target(&note_path, permit)?)?;
    }
    let proof = TerminalProof {
        version: 1,
        change: PreparedChange {
            change_id: manifest.change_id.clone(),
            manifest_hash: hash.clone(),
        },
        status,
        snapshot: transcript.iter().rev().find_map(|f| match &f.event {
            ChangeEvent::Indexed { snapshot } => Some(snapshot.clone()),
            _ => None,
        }),
        baseline_note_hash: baseline_hash,
        finalized_note_hash: final_hash,
        journal: transcript,
    };
    let checksum = Blake3Hash::digest(
        serde_json::to_vec(&proof).map_err(|e| WikiError::invalid(e.to_string()))?,
    );
    let bytes = serde_json::to_vec(&Receipt {
        proof: proof.clone(),
        checksum,
    })
    .map_err(|e| WikiError::invalid(e.to_string()))?;
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(WikiError::invalid("outcome receipt exceeds limit"));
    }
    let receipt_path = path(&manifest.change_id)?;
    let staged = engine.fs.stage(&receipt_path, &bytes, permit)?;
    journal::require_sync(engine.fs.replace(staged, &ExpectedState::Absent, permit)?)?;
    if append_terminal {
        journal::append_event(
            &engine.fs,
            permit,
            manifest,
            hash,
            match status {
                ChangeStatus::Committed => ChangeEvent::Committed,
                ChangeStatus::Aborted => ChangeEvent::Aborted,
                _ => unreachable!(),
            },
        )?;
    }
    Ok(report(&proof))
}

/// Reestablish receipt durability when recovery observes a rename whose directory sync failed.
pub(crate) fn sync_receipt(
    fs: &VaultFs,
    permit: &WriterPermit,
    id: &crate::domain::RecordId,
) -> Result<()> {
    journal::require_sync(fs.sync_target(&path(id)?, permit)?)
}

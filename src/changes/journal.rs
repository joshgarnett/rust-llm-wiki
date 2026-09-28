//! Bounded, protected framing. An incomplete final frame is the only tolerated damage.
use super::{
    prepare::{MAX_JOURNAL_BYTES, read_bounded, strict_json},
    types::*,
};
use crate::{
    domain::{Result, VaultRelativePath, WikiError},
    vault::{DirectorySync, VaultFs, WriterPermit},
};
use std::collections::BTreeSet;

const MAGIC: &[u8; 8] = b"LWJNL001";
const HEADER: usize = 44; // magic + u32 length + independent BLAKE3 header checksum
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

pub fn journal_path(id: &crate::domain::RecordId) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!(".wiki/state/changes/{id}.journal"))
}

pub fn encode_frame(frame: &JournalFrame) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(frame).map_err(|e| WikiError::invalid(e.to_string()))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(WikiError::invalid("journal frame exceeds limit"));
    }
    let mut bytes = Vec::with_capacity(HEADER + body.len() + 32);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(blake3::hash(&body).as_bytes());
    Ok(bytes)
}

pub fn decode_journal(
    bytes: &[u8],
    manifest: &ChangeManifest,
    hash: &crate::domain::Blake3Hash,
) -> Result<JournalState> {
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(WikiError::invalid("journal exceeds limit"));
    }
    if manifest.operations.len() > super::prepare::MAX_OPS {
        return Err(WikiError::invalid("manifest operation limit exceeded"));
    }
    super::prepare::topological_order(
        &manifest
            .operations
            .iter()
            .map(|op| op.apply_after.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut result = JournalState {
        frames: Vec::new(),
        status: None,
        safe_offset: 0,
        torn_tail: false,
    };
    let mut offset = 0;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        if rest.len() < HEADER {
            result.torn_tail = true;
            break;
        }
        if &rest[..8] != MAGIC || blake3::hash(&rest[..12]).as_bytes() != &rest[12..HEADER] {
            return Err(WikiError::invalid("corrupt journal header"));
        }
        let length = u32::from_le_bytes(rest[8..12].try_into().expect("four bytes")) as usize;
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(WikiError::invalid("invalid journal frame length"));
        }
        let total = HEADER + length + 32;
        if rest.len() < total {
            result.torn_tail = true;
            break;
        }
        let body = &rest[HEADER..HEADER + length];
        if blake3::hash(body).as_bytes() != &rest[HEADER + length..total] {
            return Err(WikiError::invalid("corrupt journal frame checksum"));
        }
        let frame: JournalFrame = strict_json(body)?;
        result.frames.push(frame);
        offset += total;
        result.safe_offset = offset as u64;
    }
    result.status = validate_frames(&result.frames, manifest, hash)?;
    Ok(result)
}

pub fn load_journal(
    fs: &VaultFs,
    manifest: &ChangeManifest,
    hash: &crate::domain::Blake3Hash,
) -> Result<JournalState> {
    let bytes = read_bounded(fs, &journal_path(&manifest.change_id)?, MAX_JOURNAL_BYTES)?
        .unwrap_or_default();
    decode_journal(&bytes, manifest, hash)
}

pub fn append_event(
    fs: &VaultFs,
    permit: &WriterPermit,
    manifest: &ChangeManifest,
    hash: &crate::domain::Blake3Hash,
    event: ChangeEvent,
) -> Result<JournalState> {
    permit.require_root(fs.root())?;
    let mut state = load_journal(fs, manifest, hash)?;
    let frame = JournalFrame {
        version: 1,
        sequence: state.frames.len() as u64,
        change_id: manifest.change_id.clone(),
        manifest_hash: hash.clone(),
        event,
    };
    state.frames.push(frame.clone());
    validate_frames(&state.frames, manifest, hash)?;
    let bytes = encode_frame(&frame)?;
    if state.safe_offset as usize + bytes.len() > MAX_JOURNAL_BYTES {
        return Err(WikiError::invalid("journal exceeds limit"));
    }
    // Validate first, then repair under the same writer authority before any append.
    if state.torn_tail {
        fs.truncate_synced(
            &journal_path(&manifest.change_id)?,
            state.safe_offset,
            permit,
        )?;
    }
    require_sync(fs.ensure_directory(&VaultRelativePath::new(".wiki/state/changes")?, permit)?)?;
    require_sync(fs.append_synced(&journal_path(&manifest.change_id)?, &bytes, permit)?)?;
    load_journal(fs, manifest, hash)
}

pub(crate) fn require_sync(sync: DirectorySync) -> Result<()> {
    if sync == DirectorySync::Unsupported {
        return Err(WikiError::new(
            crate::domain::ErrorCode::CapabilityUnavailable,
            "changes require supported directory durability",
        ));
    }
    Ok(())
}

fn validate_frames(
    frames: &[JournalFrame],
    manifest: &ChangeManifest,
    hash: &crate::domain::Blake3Hash,
) -> Result<Option<ChangeStatus>> {
    let mut status = None;
    let mut completed = BTreeSet::new();
    let mut intent = None;
    for (sequence, frame) in frames.iter().enumerate() {
        if frame.version != 1
            || frame.sequence != sequence as u64
            || frame.change_id != manifest.change_id
            || &frame.manifest_hash != hash
        {
            return Err(WikiError::invalid(
                "journal identity/version/sequence mismatch",
            ));
        }
        let legal = match (&frame.event, status) {
            (ChangeEvent::Prepared, None) => {
                status = Some(ChangeStatus::Prepared);
                true
            }
            (
                ChangeEvent::Applying,
                Some(
                    ChangeStatus::Prepared
                    | ChangeStatus::Applying
                    | ChangeStatus::FilesApplied
                    | ChangeStatus::Indexed,
                ),
            ) => {
                completed.clear();
                intent = None;
                status = Some(ChangeStatus::Applying);
                true
            }
            (ChangeEvent::Intent { op }, Some(ChangeStatus::Applying)) => {
                let valid = manifest.operations.get(*op).is_some_and(|operation| {
                    operation.apply_after.iter().all(|n| completed.contains(n))
                }) && !completed.contains(op)
                    && intent.is_none();
                if valid {
                    intent = Some(*op);
                }
                valid
            }
            (ChangeEvent::Done { op }, Some(ChangeStatus::Applying)) if intent == Some(*op) => {
                completed.insert(*op);
                intent = None;
                true
            }
            (ChangeEvent::FilesApplied, Some(ChangeStatus::Applying))
                if completed.len() == manifest.operations.len() && intent.is_none() =>
            {
                status = Some(ChangeStatus::FilesApplied);
                true
            }
            (
                ChangeEvent::Indexed { .. },
                Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed),
            ) => {
                status = Some(ChangeStatus::Indexed);
                true
            }
            (ChangeEvent::Committed, Some(ChangeStatus::Indexed)) => {
                status = Some(ChangeStatus::Committed);
                true
            }
            (ChangeEvent::Aborted, Some(ChangeStatus::Prepared)) => {
                status = Some(ChangeStatus::Aborted);
                true
            }
            (
                ChangeEvent::Conflict {
                    observations,
                    phase,
                },
                Some(
                    ChangeStatus::Prepared
                    | ChangeStatus::Applying
                    | ChangeStatus::FilesApplied
                    | ChangeStatus::Indexed,
                ),
            ) => {
                let mut seen = BTreeSet::new();
                let valid = !phase.is_empty()
                    && phase.len() <= 1024
                    && observations.iter().all(|observation| {
                        seen.insert(&observation.target)
                            && manifest.operations.iter().any(|operation| {
                                operation.target == observation.target
                                    && operation.before == observation.before
                                    && operation.after == observation.after
                            })
                    });
                if valid {
                    status = Some(ChangeStatus::Conflict);
                }
                valid
            }
            _ => false,
        };
        if !legal {
            return Err(WikiError::invalid(
                "illegal journal transition or operation order",
            ));
        }
    }
    Ok(status)
}

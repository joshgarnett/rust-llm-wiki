//! Explicit whole-cache reconstruction, reserved outside the derived cache.
//!
//! The witness grants one exact candidate reservation, never ownership of an
//! unclassified database. Retries preserve bounded small artifacts or delegate
//! authenticated retirement to the selector; they never reuse an old UUID.
use super::{file_types::CatalogSelection, selector};
use crate::{
    changes::operation_authority::{self as operations, Authority, Presence, Publication},
    domain::{Blake3Hash, ErrorCode, RecordId, Result, WikiError},
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, time::Duration};

const MAX_RECORD_BYTES: usize = 4096;
const MAX_ABANDONED: usize = 8;
const MAX_ABANDONED_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Abandoned {
    pub(crate) candidate: CatalogSelection,
    /// Observed size for reporting only. Every admission remeasures exact files.
    pub(crate) bytes: u64,
}

#[derive(Debug)]
pub(crate) struct Admission {
    pub(crate) candidate: Option<CatalogSelection>,
    pub(crate) resumed: bool,
    pub(crate) abandoned: Vec<Abandoned>,
}

pub(crate) enum CandidateDisposition {
    Retired,
    Preserved { bytes: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Witness {
    version: u32,
    vault_id: RecordId,
    starting_authority_revision: u64,
    starting_publication: Publication,
    candidate: CatalogSelection,
    abandoned: Vec<Abandoned>,
}

fn recovery(message: &str) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}
fn corrupt(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn successor_epoch(publication: &Publication) -> Result<u64> {
    publication
        .epoch
        .checked_add(1)
        .filter(|epoch| *epoch <= i64::MAX as u64)
        .ok_or_else(|| WikiError::new(ErrorCode::CapabilityUnavailable, "catalog epoch exhausted"))
}

impl Witness {
    fn validate(&self, vault: &RecordId) -> Result<()> {
        let floor = &self.starting_publication;
        if self.version != 1
            || &self.vault_id != vault
            || self.starting_authority_revision == 0
            || floor.epoch == 0
            || floor.epoch > i64::MAX as u64
            || floor.file_id.len() != 32
            || !floor
                .file_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(corrupt("catalog rebuild witness identity is invalid"));
        }
        self.candidate.validate(vault)?;
        if self.candidate.creation_epoch != successor_epoch(floor)?
            || self.candidate.file_id == floor.file_id
        {
            return Err(corrupt(
                "catalog rebuild witness candidate is not a successor",
            ));
        }
        if self.abandoned.len() > MAX_ABANDONED {
            return Err(budget("catalog rebuild abandoned entry limit exceeded"));
        }
        let mut identities = BTreeSet::from([self.candidate.file_id.as_str()]);
        for abandoned in &self.abandoned {
            abandoned.candidate.validate(vault)?;
            if abandoned.candidate.creation_epoch != self.candidate.creation_epoch
                || abandoned.candidate.file_id == floor.file_id
                || !identities.insert(abandoned.candidate.file_id.as_str())
            {
                return Err(corrupt("catalog rebuild abandoned identity is invalid"));
            }
        }
        Ok(())
    }

    fn require_start(&self, authority: &Authority) -> Result<()> {
        authority.require_publication(authority.publication())?;
        if authority.revision() != self.starting_authority_revision
            || authority.publication() != &self.starting_publication
        {
            return Err(recovery(
                "catalog rebuild witness starting authority changed",
            ));
        }
        Ok(())
    }

    fn acknowledged(&self, authority: &Authority) -> Result<bool> {
        authority.require_publication(authority.publication())?;
        if authority.publication().file_id != self.candidate.file_id {
            return Ok(false);
        }
        if authority.revision() <= self.starting_authority_revision
            || authority.publication().epoch < self.candidate.creation_epoch
        {
            return Err(recovery(
                "catalog rebuild acknowledgement precedes its reservation",
            ));
        }
        Ok(true)
    }

    fn bytes(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self)
            .map_err(|_| corrupt("catalog rebuild witness cannot be encoded"))?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(budget("catalog rebuild witness byte limit exceeded"));
        }
        Ok(bytes)
    }

    fn reserved_candidates(&self) -> Vec<CatalogSelection> {
        std::iter::once(self.candidate.clone())
            .chain(self.abandoned.iter().map(|entry| entry.candidate.clone()))
            .collect()
    }
}

fn load(fs: &VaultFs, vault: &RecordId) -> Result<Option<(Witness, Vec<u8>)>> {
    let Some(bytes) = selector::read_rebuild_record(fs)? else {
        return Ok(None);
    };
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(budget("catalog rebuild witness byte limit exceeded"));
    }
    let witness: Witness = serde_json::from_slice(&bytes)
        .map_err(|_| corrupt("catalog rebuild witness is malformed"))?;
    witness.validate(vault)?;
    Ok(Some((witness, bytes)))
}

fn authority(fs: &VaultFs, vault: &RecordId) -> Result<Authority> {
    operations::load(fs, vault, Presence::Required)?
        .ok_or_else(|| recovery("catalog rebuild requires outside-cache authority"))
}

/// Recompute bounded retained bytes from exact no-follow candidate names.
/// Missing files do not cause their UUID reservation to be reused or recreated.
fn remeasure(fs: &VaultFs, abandoned: &mut [Abandoned]) -> Result<u64> {
    let mut total = 0u64;
    for entry in abandoned {
        entry.bytes = selector::measure_abandoned_candidate(fs, &entry.candidate)?;
        total = total
            .checked_add(entry.bytes)
            .ok_or_else(|| budget("catalog rebuild abandoned byte count overflow"))?;
        if total > MAX_ABANDONED_BYTES {
            return Err(budget("catalog rebuild abandoned byte limit exceeded"));
        }
    }
    Ok(total)
}

/// Admit at most one new candidate for an explicit forced reconstruction.
/// The initial witness is durable before the caller creates any cache object.
pub(crate) fn prepare(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    force: bool,
    timeout: Duration,
) -> Result<Admission> {
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    let existing = load(fs, vault)?;
    if existing.is_none() && (!force || !selector::cache_root_absent(fs)?) {
        return Ok(Admission {
            candidate: None,
            resumed: false,
            abandoned: Vec::new(),
        });
    }
    if !force {
        return Err(recovery(
            "catalog rebuild witness requires explicit forced rebuild",
        ));
    }
    let authority = match existing.as_ref() {
        Some(_) => authority(fs, vault)?,
        None => match operations::load(fs, vault, Presence::LegacyMayBeAbsent)? {
            Some(authority) => authority,
            None => {
                // A vault before first normalized activation has no lost
                // acknowledged cache. Existing activation remains responsible.
                return Ok(Admission {
                    candidate: None,
                    resumed: false,
                    abandoned: Vec::new(),
                });
            }
        },
    };
    authority.require_publication(authority.publication())?;
    match existing {
        None => {
            if !selector::cache_root_absent(fs)? {
                return Err(recovery(
                    "catalog cache appeared before rebuild reservation",
                ));
            }
            let witness = Witness {
                version: 1,
                vault_id: vault.clone(),
                starting_authority_revision: authority.revision(),
                starting_publication: authority.publication().clone(),
                candidate: CatalogSelection::new(
                    vault.clone(),
                    successor_epoch(authority.publication())?,
                )?,
                abandoned: Vec::new(),
            };
            witness.validate(vault)?;
            selector::store_rebuild_record(
                fs,
                writer,
                &ExpectedState::Absent,
                Some(&witness.bytes()?),
            )?;
            Ok(Admission {
                candidate: Some(witness.candidate),
                resumed: false,
                abandoned: Vec::new(),
            })
        }
        Some((mut witness, original)) => {
            if witness.acknowledged(&authority)? {
                selector::resume_acknowledged_rebuild(fs, writer, vault, timeout)?;
                return Ok(Admission {
                    candidate: None,
                    resumed: true,
                    abandoned: finish(fs, writer, vault)?,
                });
            }
            witness.require_start(&authority)?;
            selector::require_unselected_rebuild(fs, vault)?;
            selector::validate_unselected_rebuild_namespace(fs, &witness.reserved_candidates())?;
            remeasure(fs, &mut witness.abandoned)?;
            if let CandidateDisposition::Preserved { bytes } =
                selector::inspect_unacknowledged_candidate(
                    fs,
                    writer,
                    vault,
                    &witness.candidate,
                    timeout,
                )?
            {
                if witness.abandoned.len() == MAX_ABANDONED {
                    return Err(budget("catalog rebuild abandoned entry limit exceeded"));
                }
                witness.abandoned.push(Abandoned {
                    candidate: witness.candidate.clone(),
                    bytes,
                });
            }
            // Stored sizes and a previous classifier observation are not authority.
            remeasure(fs, &mut witness.abandoned)?;
            let candidate =
                CatalogSelection::new(vault.clone(), successor_epoch(authority.publication())?)?;
            if candidate.file_id == witness.candidate.file_id
                || witness
                    .abandoned
                    .iter()
                    .any(|old| old.candidate.file_id == candidate.file_id)
            {
                return Err(corrupt(
                    "catalog rebuild retry reused a reserved file identity",
                ));
            }
            witness.candidate = candidate.clone();
            witness.validate(vault)?;
            selector::store_rebuild_record(
                fs,
                writer,
                &ExpectedState::Hash(Blake3Hash::digest(&original)),
                Some(&witness.bytes()?),
            )?;
            Ok(Admission {
                candidate: Some(candidate),
                resumed: false,
                abandoned: witness.abandoned,
            })
        }
    }
}

/// Narrow predecessor-absent publication permit, before authority acknowledgement.
pub(crate) fn authorize_publication(
    fs: &VaultFs,
    vault: &RecordId,
    candidate: &CatalogSelection,
    authority: &Authority,
) -> Result<bool> {
    let Some((mut witness, _)) = load(fs, vault)? else {
        return Ok(false);
    };
    if &witness.candidate != candidate {
        return Err(recovery(
            "catalog rebuild publication differs from reserved candidate",
        ));
    }
    witness.require_start(authority)?;
    selector::require_unselected_rebuild(fs, vault)?;
    selector::validate_unselected_rebuild_namespace(fs, &witness.reserved_candidates())?;
    remeasure(fs, &mut witness.abandoned)?;
    Ok(true)
}

/// Clear only this exact acknowledged witness after coherent physical selection.
/// Return the preserved inventory even though its witness has been removed.
pub(crate) fn finish(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
) -> Result<Vec<Abandoned>> {
    writer.require_root(fs.root())?;
    fs.require_storage_ready()?;
    let Some((mut witness, original)) = load(fs, vault)? else {
        return Ok(Vec::new());
    };
    let authority = authority(fs, vault)?;
    if !witness.acknowledged(&authority)? {
        return Err(recovery("catalog rebuild witness is not acknowledged"));
    }
    let (selected, header) = selector::maintenance_header(fs, vault, Duration::ZERO)?
        .ok_or_else(|| recovery("catalog rebuild acknowledged selector is absent"))?;
    if selected != witness.candidate {
        return Err(recovery(
            "catalog rebuild selected identity differs from witness",
        ));
    }
    authority.require_publication(&Publication {
        file_id: selected.file_id,
        epoch: header.snapshot.generation,
    })?;
    remeasure(fs, &mut witness.abandoned)?;
    selector::store_rebuild_record(
        fs,
        writer,
        &ExpectedState::Hash(Blake3Hash::digest(&original)),
        None,
    )?;
    Ok(witness.abandoned)
}

#[cfg(test)]
#[path = "missing_cache_tests.rs"]
mod tests;

//! Sealed historical output evidence. Never grants application or inverse authority.
use super::{
    journal, outcome,
    prepare::{MAX_MANIFEST_BYTES, manifest_path, read_bounded},
    types::*,
};
use crate::{domain::*, records::parse_note, vault::ExpectedState};

/// The engine verified the original manifest and retained terminal transcript.
/// This proves an old selected output hash, not the current target's contents.
#[derive(Debug)]
pub struct CommittedOutputProof {
    change: PreparedChange,
    target: VaultRelativePath,
    hash: Blake3Hash,
}
impl CommittedOutputProof {
    pub fn change(&self) -> &PreparedChange {
        &self.change
    }
    pub fn target(&self) -> &VaultRelativePath {
        &self.target
    }
    pub fn hash(&self) -> &Blake3Hash {
        &self.hash
    }
}
impl ChangeEngine {
    /// Payload-independent terminal history; unresolved work keeps strict loading.
    pub fn inspect_history(&self, id: &RecordId) -> Result<ChangeInspection> {
        self.require_binding()?;
        let (manifest, manifest_hash) = self.load_manifest_structure(id)?;
        let Some(report) = outcome::terminal_report(&self.fs, &manifest, &manifest_hash)? else {
            return self.inspect(id);
        };
        let journal = journal::load_journal(&self.fs, &manifest, &manifest_hash)?;
        let note = read_bounded(&self.fs, &manifest_path(id)?, MAX_MANIFEST_BYTES + 65_536)?
            .ok_or_else(|| WikiError::invalid("missing historical change note"))?;
        let note_status = parse_note(&note)
            .canonical
            .and_then(|record| record.string("wiki_status").map(str::to_owned))
            .ok_or_else(|| WikiError::invalid("missing historical note status"))?;
        Ok(ChangeInspection {
            prepared: PreparedChange {
                change_id: id.clone(),
                manifest_hash,
            },
            manifest,
            journal,
            status: report.status,
            observations: Vec::new(),
            note_status,
        })
    }
    /// Report unavailable retained payloads without treating missing bytes as
    /// fresh mutation authority or as proof that a user intentionally expired them.
    /// Existing payload bytes still require their exact original length/hash.
    pub fn missing_retained_payloads(&self, id: &RecordId) -> Result<Vec<VaultRelativePath>> {
        let history = self.inspect_history(id)?;
        let mut missing = std::collections::BTreeSet::new();
        for reference in history
            .manifest
            .operations
            .iter()
            .flat_map(|op| [&op.before_payload, &op.after_payload])
            .flatten()
        {
            let mut bytes = crate::storage::layout::raw_read(
                self.fs.root(),
                &crate::storage::layout::physical_relative(self.fs.root(), &reference.path)?,
                super::prepare::MAX_PAYLOAD_BYTES,
            )?;
            if bytes.is_none()
                && history.manifest.version == 1
                && crate::storage::layout::active(self.fs.root())?
            {
                let map_path = crate::storage::layout::payload_map_path(id)?;
                if let Some(encoded) =
                    crate::storage::layout::raw_read(self.fs.root(), &map_path, MAX_MANIFEST_BYTES)?
                {
                    let map: crate::storage::layout::PayloadMap =
                        crate::storage::layout::decode(&encoded)?;
                    if map.version != 1
                        || map.change_id != *id
                        || map.manifest_hash != history.prepared.manifest_hash
                        || !map.entries.contains(reference)
                    {
                        return Err(WikiError::invalid("retained payload map binding differs"));
                    }
                    bytes = crate::storage::layout::raw_read(
                        self.fs.root(),
                        &crate::storage::layout::object_path(&reference.hash)?,
                        super::prepare::MAX_PAYLOAD_BYTES,
                    )?;
                }
            }
            match bytes {
                Some(bytes)
                    if bytes.len() as u64 == reference.byte_len
                        && Blake3Hash::digest(&bytes) == reference.hash => {}
                Some(_) => {
                    return Err(WikiError::new(
                        ErrorCode::RecoveryRequired,
                        "retained payload integrity failure",
                    ));
                }
                None => {
                    missing.insert(reference.path.clone());
                }
            }
        }
        Ok(missing.into_iter().collect())
    }
    pub fn prove_committed_output(
        &self,
        change: &PreparedChange,
        target: &VaultRelativePath,
        after: &Blake3Hash,
    ) -> Result<CommittedOutputProof> {
        self.require_binding()?;
        let (manifest, hash) = self.load_manifest_structure(&change.change_id)?;
        let report = outcome::terminal_report(&self.fs, &manifest, &hash)?;
        if hash != change.manifest_hash
            || report.is_none_or(|report| report.status != ChangeStatus::Committed)
            || !manifest
                .operations
                .iter()
                .any(|op| &op.target == target && op.after == ExpectedState::Hash(after.clone()))
        {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "selected output lacks exact retained committed manifest and outcome",
            ));
        }
        Ok(CommittedOutputProof {
            change: change.clone(),
            target: target.clone(),
            hash: after.clone(),
        })
    }
}

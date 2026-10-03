//! Identity shared by normalized catalog construction and durable selection.
use crate::domain::{Blake3Hash, ErrorCode, RecordId, Result, WikiError};
use serde::{Deserialize, Serialize};

pub(crate) const CATALOG_FILE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogSelection {
    pub version: u32,
    pub vault_id: RecordId,
    pub file_id: String,
    pub creation_epoch: u64,
    pub creation_header_hash: Blake3Hash,
}

impl CatalogSelection {
    pub fn new(vault_id: RecordId, epoch: u64) -> Result<Self> {
        let file_id = uuid::Uuid::now_v7().simple().to_string();
        let creation_header_hash = Self::identity_hash(&vault_id, &file_id, epoch)?;
        let selection = Self {
            version: CATALOG_FILE_VERSION,
            vault_id,
            file_id,
            creation_epoch: epoch,
            creation_header_hash,
        };
        selection.validate(&selection.vault_id)?;
        Ok(selection)
    }

    pub fn validate(&self, vault_id: &RecordId) -> Result<()> {
        if self.version != CATALOG_FILE_VERSION
            || &self.vault_id != vault_id
            || self.file_id.len() != 32
            || !self
                .file_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.creation_epoch == 0
            || self.creation_epoch > i64::MAX as u64
            || self.creation_header_hash
                != Self::identity_hash(&self.vault_id, &self.file_id, self.creation_epoch)?
        {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "catalog selection identity is invalid",
            ));
        }
        Ok(())
    }

    fn identity_hash(vault_id: &RecordId, file_id: &str, epoch: u64) -> Result<Blake3Hash> {
        // Only immutable creation fields participate. Future in-place epochs
        // must not invalidate the selector's identity of the physical file.
        let bytes = serde_json::to_vec(&(CATALOG_FILE_VERSION, vault_id, file_id, epoch))
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(Blake3Hash::digest(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangeBinding {
    pub change_id: RecordId,
    pub manifest_hash: Blake3Hash,
}

#[derive(Debug, Clone)]
pub(crate) struct BuildIdentity {
    pub selection: CatalogSelection,
    pub origin: Option<ChangeBinding>,
    pub vector_cache_lost: bool,
    pub vector_loss_unknown: bool,
}

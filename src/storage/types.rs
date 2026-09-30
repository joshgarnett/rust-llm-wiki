use crate::domain::{Blake3Hash, RecordId, VaultRelativePath};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageOptions {
    pub retain_undo_changes: usize,
    pub max_files: usize,
    pub max_bytes: u64,
    pub expected_plan: Option<Blake3Hash>,
}
impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            retain_undo_changes: 20,
            max_files: 100_000,
            max_bytes: 1_073_741_824,
            expected_plan: None,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageTotals {
    pub files: u64,
    pub logical_bytes: u64,
    pub unique_content_bytes: u64,
    pub duplicate_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageFile {
    pub path: VaultRelativePath,
    pub bytes: u64,
    pub hash: Blake3Hash,
    pub class: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageInventory {
    pub vault_id: RecordId,
    pub layout_version: u32,
    pub complete: bool,
    pub totals: StorageTotals,
    pub files: Vec<StorageFile>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectedStorage {
    pub path: VaultRelativePath,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoragePlan {
    pub vault_id: RecordId,
    pub plan_hash: Blake3Hash,
    pub layout_version: u32,
    pub retain_undo_changes: usize,
    pub before: StorageTotals,
    pub candidates: Vec<StorageFile>,
    pub protected: Vec<ProtectedStorage>,
    pub blockers: Vec<String>,
    pub full_backup_required: bool,
    pub planned_copy_bytes: u64,
    pub planned_delete_bytes: u64,
    pub estimated_net_bytes: i64,
    pub estimate_excludes: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageReport {
    pub operation_id: RecordId,
    pub resumed: bool,
    pub layout_version: u32,
    pub before: StorageTotals,
    pub after: StorageTotals,
    pub deleted_files: u64,
    pub deleted_bytes: u64,
    pub copied_files: u64,
    pub copied_bytes: u64,
    pub protected: Vec<ProtectedStorage>,
    pub warnings: Vec<String>,
    pub backup: String,
}

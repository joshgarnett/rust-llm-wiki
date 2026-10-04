//! Operational import progress is backed up with `.wiki/state`, not rebuilt.
use crate::{
    changes::{PreparedChange, types::NamedChangeIdentity},
    domain::{Blake3Hash, RecordId, VaultRelativePath},
    sources::{import_manifest_types::ImportManifestItem, types::CaptureAllocation},
};
use serde::{Deserialize, Serialize};

pub(crate) const IMPORT_STATE_VERSION: u32 = 1;
pub(crate) const MAX_IMPORT_STATE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_IMPORT_INTENT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_IMPORT_RESULT_LINE_BYTES: usize = 65_536;
pub(crate) const MAX_IMPORT_RESULTS_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredImportManifest {
    pub hash: Blake3Hash,
    pub items: u64,
    pub bytes: u64,
    pub first_item_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportIntentRef {
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportPendingCapture {
    pub item: ImportManifestItem,
    pub allocation: CaptureAllocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportPendingGroup {
    pub group: u64,
    pub first_ordinal: u64,
    pub next_manifest_offset: u64,
    pub captures: Vec<ImportPendingCapture>,
    pub change: NamedChangeIdentity,
    pub intent: Option<ImportIntentRef>,
    /// Input-only anchor from a closed attempt; never canonical write authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_intent: Option<ImportIntentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportedItem {
    pub ordinal: u64,
    pub source_id: RecordId,
    pub revision_id: RecordId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportGroupResult {
    pub group: u64,
    pub change: PreparedChange,
    pub items: Vec<ImportedItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImportAttemptCloseReason {
    StaleBase,
    AmbiguousPrefix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ImportResultEvent {
    GroupCommitted {
        result: ImportGroupResult,
    },
    AttemptClosed {
        group: u64,
        change: NamedChangeIdentity,
        prepared: Option<PreparedChange>,
        reason: ImportAttemptCloseReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportResultFrame {
    pub previous: Blake3Hash,
    pub event: ImportResultEvent,
    pub checksum: Blake3Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportProgress {
    pub version: u32,
    pub vault_id: RecordId,
    pub key: String,
    pub manifest: StoredImportManifest,
    pub group_size: usize,
    pub next_ordinal: u64,
    pub manifest_offset: u64,
    pub groups_committed: u64,
    pub results_offset: u64,
    pub results_hash: Blake3Hash,
    pub last_group: Option<ImportGroupResult>,
    pub pending: Option<ImportPendingGroup>,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportStateEnvelope {
    pub progress: ImportProgress,
    pub checksum: Blake3Hash,
}

#[derive(Debug, Clone)]
pub(crate) struct ImportResultTail {
    pub frame: Option<ImportResultFrame>,
    pub next_offset: u64,
    pub torn_tail: bool,
}

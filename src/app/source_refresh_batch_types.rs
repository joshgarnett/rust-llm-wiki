//! Explicit bounded bulk refresh requests; existing Source identity is mandatory.
use crate::{changes::ChangeStatus, domain::*};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MAX_SOURCE_REFRESH_BATCH_ITEMS: usize = 16;
pub const MAX_SOURCE_REFRESH_BATCH_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_SOURCE_REFRESH_BATCH_INPUT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRefreshBatchRequest {
    pub items: Vec<SourceRefreshBatchItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRefreshBatchItem {
    pub source_id: RecordId,
    pub file: PathBuf,
    pub if_match: Blake3Hash,
    pub expected_revision: RecordId,
    pub input_hash: Blake3Hash,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceRefreshedItem {
    /// Zero-based position in the caller's request.
    pub ordinal: usize,
    pub source_id: RecordId,
    pub previous_revision_id: Option<RecordId>,
    pub revision_id: Option<RecordId>,
    pub reused: Option<bool>,
    pub no_op: Option<bool>,
    pub capture_state: Option<crate::sources::SourceCaptureState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceRefreshBatchOutcome {
    pub items: Vec<SourceRefreshedItem>,
    pub plan: super::PlanSummary,
    pub change: Option<crate::changes::PreparedChange>,
    pub status: Option<ChangeStatus>,
    pub snapshot: Option<ReadSnapshot>,
    /// Dry-run validates inputs only; selected identity, head and reuse are unresolved.
    pub dry_run: bool,
}

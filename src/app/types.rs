//! Offline application contracts, owned by the orchestrator.
use crate::{
    catalog::{CatalogDiagnostic, SyncReport},
    changes::*,
    domain::*,
    vault::{ExpectedState, VaultFs},
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_READ_BYTES: usize = 64 * 1024;
pub struct OfflineApp {
    pub(crate) fs: VaultFs,
    pub(crate) vault_id: RecordId,
    pub(crate) options: OperationOptions,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationOptions {
    pub dry_run: bool,
    pub stage_only: bool,
    pub offline: bool,
    pub lock_timeout_ms: u64,
}
impl Default for OperationOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            stage_only: false,
            offline: false,
            lock_timeout_ms: 5000,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordSelector {
    Id(RecordId),
    Path(VaultRelativePath),
}
#[derive(Debug, Clone)]
pub struct ReadRequest {
    pub selector: RecordSelector,
    pub range: Option<ByteSpan>,
    pub max_bytes: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct ReadOutcome {
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
    pub record: Option<CanonicalRecord>,
    pub metadata: Option<BTreeMap<String, Value>>,
    pub diagnostics: Vec<CatalogDiagnostic>,
    pub body: String,
    pub range: ByteSpan,
    pub truncated: bool,
    pub continuation: Option<ByteSpan>,
}
#[derive(Debug, Clone, Serialize)]
pub struct PlannedOperation {
    pub path: VaultRelativePath,
    pub before: ExpectedState,
    pub after: ExpectedState,
    pub byte_len: u64,
    pub apply_after: Vec<VaultRelativePath>,
}
#[derive(Debug, Clone, Serialize)]
pub struct PlanSummary {
    pub title: String,
    pub read_preconditions: Vec<ReadDependency>,
    pub operations: Vec<PlannedOperation>,
}
#[derive(Debug, Clone, Serialize)]
pub struct MutationOutcome {
    #[serde(skip)]
    pub source_capture: Option<crate::sources::SourceCaptureState>,
    pub plan: PlanSummary,
    pub change: Option<PreparedChange>,
    pub status: Option<ChangeStatus>,
    pub snapshot: Option<ReadSnapshot>,
    pub reused: bool,
    pub allocated_ids: BTreeMap<String, RecordId>,
}
#[derive(Debug, Clone, Serialize)]
pub struct InitOutcome {
    pub id: RecordId,
    pub path: String,
    pub created: bool,
    pub planned_directories: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ChangeDetails {
    pub prepared: PreparedChange,
    pub manifest: ChangeManifest,
    pub status: ChangeStatus,
    pub note_status: String,
    pub frames: Vec<JournalFrame>,
    pub observations: Vec<TargetObservation>,
    pub payloads: Vec<ChangePayload>,
    pub omitted_payloads: Vec<usize>,
    pub unavailable_payloads: Vec<VaultRelativePath>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ChangePayload {
    pub operation: usize,
    pub target: VaultRelativePath,
    pub before: Option<Vec<u8>>,
    pub proposed: Option<Vec<u8>>,
}
#[derive(Debug, Clone, Serialize)]
pub struct CheckOutcome {
    pub diagnostics: Vec<CatalogDiagnostic>,
    pub error_count: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct DoctorOutcome {
    pub check: CheckOutcome,
    pub cache_state: String,
    pub cache_error: Option<WikiError>,
    pub unresolved_changes: Vec<RecordId>,
    pub incomplete_preparations: Vec<RecordId>,
    pub provider_probe_performed: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct IndexOutcome {
    pub report: Option<SyncReport>,
    pub dry_run: bool,
    pub cache_state_unknown: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct RecoverOutcome {
    pub report: Option<RecoveryReport>,
    pub pending: Vec<RecordId>,
    pub dry_run: bool,
}

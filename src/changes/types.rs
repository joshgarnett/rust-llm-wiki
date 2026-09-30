//! Retained changeset format v1. Shared definitions are orchestrator-owned.
use crate::{
    domain::{Blake3Hash, ReadSnapshot, RecordId, Result, VaultRelativePath},
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadRef {
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
    pub byte_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginOperation {
    GraphImport,
    GraphResolve,
    GraphDecide,
    GraphReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeOrigin {
    pub operation: OriginOperation,
    pub packet_id: RecordId,
    pub response_hash: Blake3Hash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationRole {
    MutableRecord,
    ImmutableAsset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeOp {
    pub target: VaultRelativePath,
    pub before: ExpectedState,
    pub after: ExpectedState,
    pub before_payload: Option<PayloadRef>,
    pub after_payload: Option<PayloadRef>,
    pub role: OperationRole,
    /// Indices into the target-sorted operations; application uses a topological order.
    pub apply_after: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeManifest {
    pub version: u32,
    pub vault_id: RecordId,
    pub change_id: RecordId,
    pub title: String,
    pub created_at: String,
    pub origin: Option<ChangeOrigin>,
    pub inverse_of: Option<RecordId>,
    pub allocated_ids: BTreeMap<String, RecordId>,
    /// Unmodified records/assets whose exact observed state authorized this proposal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_preconditions: Vec<ReadDependency>,
    pub operations: Vec<ChangeOp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedChange {
    pub change_id: RecordId,
    pub manifest_hash: Blake3Hash,
}

#[derive(Debug, Clone)]
pub struct ExpectedWrite {
    pub target: VaultRelativePath,
    pub expected: ExpectedState,
    pub proposed: Option<Vec<u8>>,
    /// Target paths of prerequisite writes, resolved to indices during preparation.
    pub apply_after: Vec<VaultRelativePath>,
}

#[derive(Debug, Clone)]
pub struct ChangeDraft {
    pub title: String,
    pub origin: Option<ChangeOrigin>,
    pub inverse_of: Option<RecordId>,
    pub allocated_ids: BTreeMap<String, RecordId>,
    /// Original read states, separate from the validator's projected final dependencies.
    pub read_preconditions: Vec<ReadDependency>,
    pub operations: Vec<ExpectedWrite>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginPolicy {
    ReuseOrConflict,
    AllowNewResponse,
}

#[derive(Debug, Clone)]
pub struct JournalState {
    pub frames: Vec<JournalFrame>,
    pub status: Option<ChangeStatus>,
    pub safe_offset: u64,
    pub torn_tail: bool,
}

#[derive(Debug, Clone)]
pub struct ChangeInspection {
    pub prepared: PreparedChange,
    pub manifest: ChangeManifest,
    pub journal: JournalState,
    pub status: ChangeStatus,
    pub observations: Vec<TargetObservation>,
    /// Editable note metadata is a diagnostic, never proof of application intent.
    pub note_status: String,
}

#[derive(Debug, Clone)]
pub struct PreparationOutcome {
    pub change: ChangeInspection,
    pub reused: bool,
}

#[derive(Debug, Clone)]
pub struct ChangePlan {
    pub read_preconditions: Vec<ReadDependency>,
    pub operations: Vec<ExpectedWrite>,
    pub roles: Vec<OperationRole>,
    pub(crate) before: Vec<Option<Vec<u8>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    Prepared,
    Applying,
    FilesApplied,
    Indexed,
    Committed,
    Aborted,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolutionMode {
    Resume,
    Abandon,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConflictResolutionRequest {
    pub change: PreparedChange,
    pub mode: ConflictResolutionMode,
    pub conflict_sequence: u64,
    pub conflict_hash: Blake3Hash,
    pub observations: Vec<TargetObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChangeEvent {
    Prepared,
    Applying,
    Intent {
        op: usize,
    },
    Done {
        op: usize,
    },
    FilesApplied,
    Indexed {
        snapshot: ReadSnapshot,
    },
    Committed,
    Aborted,
    Conflict {
        phase: String,
        observations: Vec<TargetObservation>,
    },
    ResolutionAccepted {
        mode: ConflictResolutionMode,
        conflict_sequence: u64,
        conflict_hash: Blake3Hash,
        observations: Vec<TargetObservation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetObservation {
    pub target: VaultRelativePath,
    pub before: ExpectedState,
    pub after: ExpectedState,
    pub observed: ExpectedState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalFrame {
    pub version: u32,
    pub sequence: u64,
    pub change_id: RecordId,
    pub manifest_hash: Blake3Hash,
    pub event: ChangeEvent,
}

/// No implicit validator or publication backend is installed. P05 wires those separately.
pub struct ChangeEngine {
    pub(crate) fs: VaultFs,
    pub(crate) vault_id: RecordId,
}

/// Complete canonical scan bytes, including readable unadopted/invalid Markdown.
#[derive(Debug, Clone)]
pub struct ScanDocument {
    pub path: VaultRelativePath,
    pub bytes: Vec<u8>,
    pub hash: Blake3Hash,
}

/// Retained proposed target bytes; deletion is an explicit absence.
#[derive(Debug, Clone)]
pub struct ProposedTarget {
    pub path: VaultRelativePath,
    pub bytes: Option<Vec<u8>>,
}

/// Validation sees the whole vault and overlay, rather than only edited records.
#[derive(Debug, Clone)]
pub struct ValidationInput {
    pub vault_id: RecordId,
    pub documents: Vec<ScanDocument>,
    pub overlay: Vec<ProposedTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadDependency {
    pub path: VaultRelativePath,
    pub expected: ExpectedState,
}

/// Backend validation includes source payloads and any records it read outside the overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatedGraph {
    pub parser_fingerprint: Blake3Hash,
    pub control_manifest: Blake3Hash,
    pub dependencies: Vec<ReadDependency>,
}

/// Engine-verified retained payloads supplement actual current documents.
/// Constructed only inside the changes engine; no unchecked public constructor.
#[derive(Debug)]
pub struct RetainedGraphInput {
    pub(super) change_id: RecordId,
    pub(super) manifest_hash: Blake3Hash,
    pub(super) origin: ChangeOrigin,
    pub(super) allocated_ids: BTreeMap<String, RecordId>,
    pub(super) operations: Vec<RetainedGraphOperation>,
}
#[derive(Debug)]
pub struct RetainedGraphOperation {
    pub(super) path: VaultRelativePath,
    pub(super) role: OperationRole,
    pub(super) before: ExpectedState,
    pub(super) after: ExpectedState,
    pub(super) before_bytes: Option<Vec<u8>>,
    pub(super) after_bytes: Option<Vec<u8>>,
}
impl RetainedGraphInput {
    pub fn change_id(&self) -> &RecordId {
        &self.change_id
    }
    pub fn manifest_hash(&self) -> &Blake3Hash {
        &self.manifest_hash
    }
    pub fn origin(&self) -> &ChangeOrigin {
        &self.origin
    }
    pub fn allocated_ids(&self) -> &BTreeMap<String, RecordId> {
        &self.allocated_ids
    }
    pub fn operations(&self) -> &[RetainedGraphOperation] {
        &self.operations
    }
}
impl RetainedGraphOperation {
    pub fn role(&self) -> OperationRole {
        self.role
    }
    pub fn path(&self) -> &VaultRelativePath {
        &self.path
    }
    pub fn before(&self) -> &ExpectedState {
        &self.before
    }
    pub fn after(&self) -> &ExpectedState {
        &self.after
    }
    pub fn before_bytes(&self) -> Option<&[u8]> {
        self.before_bytes.as_deref()
    }
    pub fn after_bytes(&self) -> Option<&[u8]> {
        self.after_bytes.as_deref()
    }
}

/// Authenticated committed inverse ancestry; only the engine constructs this seal.
/// No deserializer, Clone, or caller-controlled authorization flag exists.
#[derive(Debug)]
pub struct RetainedGraphInverseInput {
    pub(super) anchor: RetainedGraphInput,
    pub(super) parent_change_id: RecordId,
    pub(super) parent_manifest_hash: Blake3Hash,
    pub(super) parent_operations: Option<Vec<RetainedGraphOperation>>,
    pub(super) inversion_depth: usize,
}
impl RetainedGraphInverseInput {
    pub fn anchor(&self) -> &RetainedGraphInput {
        &self.anchor
    }
    pub fn parent_change_id(&self) -> &RecordId {
        &self.parent_change_id
    }
    pub fn parent_manifest_hash(&self) -> &Blake3Hash {
        &self.parent_manifest_hash
    }
    pub fn parent_operations(&self) -> &[RetainedGraphOperation] {
        self.parent_operations
            .as_deref()
            .unwrap_or(&self.anchor.operations)
    }
    pub fn inversion_depth(&self) -> usize {
        self.inversion_depth
    }
}

/// A trusted application implementation must validate complete graph/reference policy.
/// There is no permissive default. The CLI installs the P05 validator before applying.
pub trait GraphValidator: Send + Sync {
    fn validate(&self, fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph>;
    /// Retained payloads grant no generic mutation exception; the application
    /// validates exact canonical decision policy against these verified bytes.
    fn validate_retained(
        &self,
        fs: &VaultFs,
        input: &ValidationInput,
        _retained: &RetainedGraphInput,
    ) -> Result<ValidatedGraph> {
        self.validate(fs, input)
    }
    /// Exact committed reversal grants no general proposition-mutation permission.
    fn validate_inverse(
        &self,
        fs: &VaultFs,
        input: &ValidationInput,
        _inverse: &RetainedGraphInverseInput,
    ) -> Result<ValidatedGraph> {
        self.validate(fs, input)
    }
}

/// Constructed only inside the engine after verified files-applied state and dependencies.
/// Catalog publishers cannot construct a permit or obtain one for an incomplete apply.
pub struct PublicationPermit<'a> {
    pub(crate) writer: &'a WriterPermit,
    pub(crate) vault_id: &'a RecordId,
    pub(crate) change: &'a PreparedChange,
    pub(crate) graph: &'a ValidatedGraph,
}
impl<'a> PublicationPermit<'a> {
    pub fn writer(&self) -> &'a WriterPermit {
        self.writer
    }
    pub fn vault_id(&self) -> &'a RecordId {
        self.vault_id
    }
    pub fn change(&self) -> &'a PreparedChange {
        self.change
    }
    pub fn graph(&self) -> &'a ValidatedGraph {
        self.graph
    }
}

/// Trusted backend publishes one atomic catalog generation under the engine-held lock.
/// Availability must be checked before canonical mutation; no default backend exists.
pub trait PublicationBackend: Send + Sync {
    fn check_available(&self) -> Result<()>;
    fn publish(
        &self,
        fs: &VaultFs,
        permit: &PublicationPermit<'_>,
        input: &ValidationInput,
    ) -> Result<ReadSnapshot>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyReport {
    pub change: PreparedChange,
    pub status: ChangeStatus,
    pub snapshot: Option<ReadSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryReport {
    pub changes: Vec<ApplyReport>,
    pub staged: Vec<PreparedChange>,
}

#[derive(Debug, Clone)]
pub struct InversePlan {
    pub draft: ChangeDraft,
    pub retained_paths: Vec<VaultRelativePath>,
}

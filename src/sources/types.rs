//! Source operation and verification contracts, owned by the orchestrator.
use crate::{
    changes::{ChangeDraft, ReadDependency, ScanDocument},
    domain::{
        Blake3Hash, ByteSpan, CanonicalRecord, CitationRef, EvidenceRef, ReadSnapshot, RecordId,
        Result, VaultRelativePath,
    },
    vault::VaultFs,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub struct SourceStore {
    pub(crate) fs: VaultFs,
}

/// A read-only canonical view; proposed payloads can be verified before activation.
pub struct SourceView<'a> {
    pub(crate) fs: &'a VaultFs,
    pub(crate) notes: super::lookup::SourceNotes,
    pub(crate) overlay: BTreeMap<VaultRelativePath, Option<Vec<u8>>>,
    /// Closed proof inputs forbid falling back to unmetered filesystem reads.
    pub(crate) closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceOrigin {
    LocalFile,
    Url,
    AgentReport,
}
impl SourceOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "local-file",
            Self::Url => "url",
            Self::AgentReport => "agent-report",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ExtractionInput {
    Utf8Preserve,
    Unsupported {
        extractor: String,
        fingerprint: Blake3Hash,
    },
    Supplied {
        extractor: String,
        fingerprint: Blake3Hash,
        content: Vec<u8>,
    },
}

#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub title: String,
    pub origin_kind: SourceOrigin,
    pub origin: String,
    pub original: Vec<u8>,
    pub extraction: ExtractionInput,
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvalidationInputs {
    pub source_ids: Vec<RecordId>,
    pub revision_ids: Vec<RecordId>,
    pub assertion_ids: Vec<RecordId>,
}

#[derive(Debug, Clone)]
pub struct SourcePlan {
    pub draft: Option<ChangeDraft>,
    pub source_id: RecordId,
    pub revision_id: RecordId,
    pub reused: bool,
    pub invalidation: InvalidationInputs,
    pub dependencies: Vec<ReadDependency>,
    /// The capture's text availability; withdrawal has no capture state.
    pub capture_state: Option<SourceCaptureState>,
}

/// Selected facts from one published index. These are discovery facts, not a
/// fresh global identity audit of externally edited canonical files.
#[derive(Debug, Clone)]
pub(crate) struct RefreshRecord {
    pub record: CanonicalRecord,
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
}

#[derive(Debug, Clone)]
pub(crate) struct RevisionSignature {
    pub original_hash: Blake3Hash,
    pub content_hash: Option<Blake3Hash>,
    pub extractor_fingerprint: Blake3Hash,
}

#[derive(Debug, Clone)]
pub(crate) struct MatchingRevision {
    pub revision: RefreshRecord,
    /// Position in the authenticated source's retained revision list.
    pub retained_ordinal: usize,
}

pub(crate) trait SourceRefreshLookup {
    fn snapshot(&self) -> &ReadSnapshot;
    fn vault_id(&self) -> &RecordId;
    /// Refuse multiple or malformed identity claims; do not silently adopt the
    /// only valid record while ignoring other readable declarations of its ID.
    fn unique_record(&self, id: &RecordId) -> Result<Option<RefreshRecord>>;
    fn record_at_path(&self, path: &VaultRelativePath) -> Result<Option<RefreshRecord>>;
    fn id_is_claimed(&self, id: &RecordId) -> Result<bool>;
    /// First matching retained ordinal; the planner checks the current head
    /// first and authenticates exact bytes before actually reusing a revision.
    fn matching_revision(
        &self,
        source: &RecordId,
        signature: &RevisionSignature,
    ) -> Result<Option<MatchingRevision>>;
    fn source_assertions(&self, source: &RecordId) -> Result<Vec<RecordId>>;
}

#[derive(Debug, Clone)]
pub(crate) struct SourceRefreshLimits {
    pub max_file_bytes: usize,
    pub max_canonical_bytes: usize,
    pub max_retained_revisions: usize,
}
impl Default for SourceRefreshLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 64 * 1024 * 1024,
            max_canonical_bytes: 256 * 1024 * 1024,
            max_retained_revisions: 100_000,
        }
    }
}

/// Exact selected before-images and dependencies accompany the ordinary draft.
/// Only the indexed apply validator can authorize their bounded publication.
pub(crate) struct IndexedSourceRefreshPlan {
    pub plan: SourcePlan,
    pub base_snapshot: ReadSnapshot,
    pub previous_revision: RecordId,
    pub captured: Vec<ScanDocument>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCaptureState {
    Complete,
    Empty,
    Unsupported,
}

impl SourceCaptureState {
    pub const fn extraction_status(self) -> &'static str {
        match self {
            Self::Complete | Self::Empty => "complete",
            Self::Unsupported => "unsupported",
        }
    }

    pub const fn citable(self) -> bool {
        matches!(self, Self::Complete)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationScope {
    Current,
    Historical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationState {
    Current,
    Historical,
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCitation {
    pub citation: CitationRef,
    pub quote: Vec<u8>,
    pub state: CitationState,
    pub dependencies: Vec<ReadDependency>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStance {
    Supports,
    Contradicts,
}
impl EvidenceStance {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvidenceRequest {
    pub assertion_id: RecordId,
    pub source_id: RecordId,
    pub revision_id: RecordId,
    pub quote: Vec<u8>,
    pub window: ByteSpan,
    pub stance: EvidenceStance,
    pub explanation: String,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct EvidencePlan {
    pub draft: ChangeDraft,
    pub evidence_id: RecordId,
    pub citation: EvidenceRef,
    pub dependencies: Vec<ReadDependency>,
}

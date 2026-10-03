//! Source operation and verification contracts, owned by the orchestrator.
use crate::{
    changes::{ChangeDraft, ReadDependency},
    domain::{Blake3Hash, ByteSpan, CitationRef, EvidenceRef, RecordId, VaultRelativePath},
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

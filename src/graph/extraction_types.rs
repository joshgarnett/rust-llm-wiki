//! Durable packet and source-local proposal interfaces, owned by the orchestrator.
use crate::{
    changes::{ChangeDraft, ChangeStatus, PreparedChange, ReadDependency},
    domain::*,
    sources::EvidenceStance,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt};

pub const PACKET_SCHEMA: &str = "lwiki.extraction-packet.v1";
pub const EXTRACTION_SCHEMA: &str = "lwiki.extraction.v1";
pub const EXTRACTION_STATE_SCHEMA: &str = "lwiki.extraction-state.v1";
pub const MAX_PACKET_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_WINDOWS: usize = 16;
pub const MAX_WINDOW_BYTES: usize = 12000;
pub const MAX_CANDIDATE_IDENTITIES: usize = 32;
pub const MAX_JSON_DEPTH: usize = 32;
pub const MAX_JSON_NODES: usize = 65536;
pub const MAX_EVIDENCE_PER_ASSERTION: usize = 16;
pub const MAX_TOTAL_EVIDENCE: usize = 512;
pub const MAX_UNRESOLVED: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PacketLocalId(RecordId);
impl PacketLocalId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        RecordId::new(value).map(Self)
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl fmt::Display for PacketLocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionLimits {
    pub max_mentions: usize,
    pub max_assertions: usize,
    pub max_output_bytes: usize,
}
impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_mentions: 64,
            max_assertions: 128,
            max_output_bytes: 262144,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketWindow {
    pub id: PacketLocalId,
    pub span: ByteSpan,
    pub text: String,
    pub headings: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateIdentity {
    pub reference: RecordRef,
    pub title: String,
    pub entity_type: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionPacket {
    pub schema: String,
    pub packet_id: RecordId,
    pub packet_fingerprint: Blake3Hash,
    pub source_id: RecordId,
    pub source_revision: RecordId,
    pub snapshot_hash: Blake3Hash,
    pub windows: Vec<PacketWindow>,
    pub registry_version: String,
    pub output_schema: Value,
    pub limits: ExtractionLimits,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_context: Option<Vec<CandidateIdentity>>,
}
#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub source_id: RecordId,
    pub revision_id: Option<RecordId>,
    pub windows: Vec<ByteSpan>,
    pub limits: ExtractionLimits,
    pub candidate_context: Vec<CandidateIdentity>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExtractionCoverage {
    pub source_bytes: u64,
    pub selected_bytes: u64,
    pub omitted_source_bytes: u64,
    pub windows: usize,
    pub unresolved: usize,
    pub pending_mentions: usize,
    pub rejected_mentions: usize,
    pub materialized_assertions: usize,
}
#[derive(Debug, Clone)]
pub struct PacketPlan {
    pub packet: ExtractionPacket,
    pub dependencies: Vec<ReadDependency>,
    pub draft: Option<ChangeDraft>,
    pub locator: DocumentLocator,
    pub coverage: ExtractionCoverage,
    pub reused: bool,
}
#[derive(Debug, Clone)]
pub struct VerifiedPacket {
    pub(crate) packet: ExtractionPacket,
    pub(crate) locator: DocumentLocator,
    pub(crate) dependencies: Vec<ReadDependency>,
}
impl VerifiedPacket {
    pub fn packet(&self) -> &ExtractionPacket {
        &self.packet
    }
    pub fn locator(&self) -> &DocumentLocator {
        &self.locator
    }
    pub fn dependencies(&self) -> &[ReadDependency] {
        &self.dependencies
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionMention {
    pub id: PacketLocalId,
    pub window_id: PacketLocalId,
    pub label: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub quote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<ByteSpan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtractionObject {
    Mention {
        mention_id: PacketLocalId,
    },
    Literal {
        #[serde(rename = "type")]
        literal_type: String,
        value: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionEvidence {
    pub window_id: PacketLocalId,
    pub stance: EvidenceStance,
    pub quote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<ByteSpan>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionAssertion {
    pub id: PacketLocalId,
    pub subject: PacketLocalId,
    pub predicate: String,
    pub object: ExtractionObject,
    pub negated: bool,
    pub modality: String,
    pub evidence: Vec<ExtractionEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionUnresolved {
    pub window_id: PacketLocalId,
    pub quote: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionResponse {
    pub schema: String,
    pub packet_id: RecordId,
    pub packet_fingerprint: Blake3Hash,
    pub mentions: Vec<ExtractionMention>,
    pub assertions: Vec<ExtractionAssertion>,
    pub unresolved: Vec<ExtractionUnresolved>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceSpan {
    pub window_id: PacketLocalId,
    pub span: ByteSpan,
    pub quote_hash: Blake3Hash,
}
#[derive(Debug, Clone)]
pub struct ValidatedExtraction {
    pub(crate) packet: VerifiedPacket,
    pub(crate) response: ExtractionResponse,
    pub(crate) raw_response: String,
    pub(crate) response_hash: Blake3Hash,
    pub(crate) mention_spans: BTreeMap<PacketLocalId, TraceSpan>,
    pub(crate) evidence_spans: BTreeMap<PacketLocalId, Vec<TraceSpan>>,
    pub(crate) dependencies: Vec<ReadDependency>,
}
impl ValidatedExtraction {
    pub fn packet(&self) -> &VerifiedPacket {
        &self.packet
    }
    pub fn response(&self) -> &ExtractionResponse {
        &self.response
    }
    pub fn response_hash(&self) -> &Blake3Hash {
        &self.response_hash
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionAllocations {
    pub assertions: BTreeMap<PacketLocalId, RecordId>,
    pub evidence: BTreeMap<PacketLocalId, Vec<RecordId>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MentionBinding {
    Pending,
    Resolved {
        entity_id: RecordId,
        decision_id: RecordId,
    },
    Rejected {
        decision_id: RecordId,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionArtifactV1 {
    pub schema: String,
    pub extraction_id: RecordId,
    pub packet_id: RecordId,
    pub packet_fingerprint: Blake3Hash,
    pub source_id: RecordId,
    pub source_revision: RecordId,
    pub snapshot_hash: Blake3Hash,
    pub response_hash: Blake3Hash,
    pub raw_response: String,
    pub mention_spans: BTreeMap<PacketLocalId, TraceSpan>,
    pub evidence_spans: BTreeMap<PacketLocalId, Vec<TraceSpan>>,
    pub allocations: ExtractionAllocations,
    pub bindings: BTreeMap<PacketLocalId, MentionBinding>,
    pub materialized_assertions: Vec<PacketLocalId>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ImportOutcome {
    pub extraction: DocumentLocator,
    pub prepared: Option<PreparedChange>,
    pub status: Option<ChangeStatus>,
    pub disposition: ImportDisposition,
    pub allocations: ExtractionAllocations,
    pub coverage: ExtractionCoverage,
    pub reused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportDisposition {
    RetainedChange,
    CanonicalRestored,
}
#[derive(Debug, Clone)]
pub struct VerifiedExtractionArtifact {
    pub(crate) artifact: ExtractionArtifactV1,
    pub(crate) response: ExtractionResponse,
    pub(crate) packet: VerifiedPacket,
    pub(crate) locator: DocumentLocator,
    pub(crate) dependencies: Vec<ReadDependency>,
}
impl VerifiedExtractionArtifact {
    pub fn artifact(&self) -> &ExtractionArtifactV1 {
        &self.artifact
    }
    pub fn response(&self) -> &ExtractionResponse {
        &self.response
    }
    pub fn packet(&self) -> &VerifiedPacket {
        &self.packet
    }
    pub fn locator(&self) -> &DocumentLocator {
        &self.locator
    }
    pub fn dependencies(&self) -> &[ReadDependency] {
        &self.dependencies
    }
}

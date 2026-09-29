use crate::{changes::ReadDependency, domain::*};
use serde::{Deserialize, Serialize};

pub const MAX_SUBMISSION_BYTES: usize = 262144;
pub(crate) const MAX_ARTIFACT_BYTES: usize = 1048576;
pub(crate) const MAX_PASSAGES: usize = 32;
pub(crate) const MAX_PASSAGE_BYTES: usize = 65536;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchScope {
    pub question: String,
    pub urls: Vec<String>,
    pub exclusions: Vec<String>,
    pub source_ids: Vec<RecordId>,
    pub offline: bool,
    pub max_rounds: u32,
    pub max_sources: u32,
    pub max_source_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchStage {
    CollectSources,
    Answer,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchPassage {
    pub passage_id: String,
    pub citation: CitationRef,
    pub quote: String,
    pub dependencies: Vec<ReadDependency>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchPacket {
    pub schema: String,
    pub vault_id: RecordId,
    pub run_id: RecordId,
    pub generation: u32,
    pub packet_fingerprint: Blake3Hash,
    pub scope_hash: Blake3Hash,
    pub scope: ResearchScope,
    pub stage: ResearchStage,
    pub round: u32,
    pub remaining_sources: u32,
    pub remaining_source_bytes: u64,
    pub instructions: String,
    pub tasks: Vec<String>,
    pub warnings: Vec<String>,
    pub gaps: Vec<String>,
    pub follow_up: Option<String>,
    pub passages: Vec<ResearchPassage>,
    pub response_schema: serde_json::Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmittedSource {
    pub key: String,
    pub title: String,
    /// Host-reported provenance; lwiki did not fetch or authenticate this origin.
    pub origin: String,
    pub content: String,
    #[serde(default)]
    pub provenance: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmittedClaim {
    pub text: String,
    pub passage_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubmissionContent {
    CollectSources {
        sources: Vec<SubmittedSource>,
        gaps: Vec<String>,
    },
    Answer {
        claims: Vec<SubmittedClaim>,
        gaps: Vec<String>,
        #[serde(default)]
        follow_up: Option<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchSubmission {
    pub schema: String,
    pub run_id: RecordId,
    pub packet_fingerprint: Blake3Hash,
    pub response: SubmissionContent,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchClaim {
    pub text: String,
    pub citations: Vec<CitationRef>,
    pub assessment: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchReport {
    pub schema: String,
    pub run_id: RecordId,
    pub question: String,
    pub claims: Vec<ResearchClaim>,
    pub gaps: Vec<String>,
    pub partial: bool,
    pub external_tool_usage: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactRef {
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiptRef {
    pub packet: Blake3Hash,
    pub submission: Blake3Hash,
    pub artifact: ArtifactRef,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResearchHead {
    pub schema: String,
    pub vault_id: RecordId,
    pub run_id: RecordId,
    pub scope: ResearchScope,
    pub scope_hash: Blake3Hash,
    pub generation: u32,
    pub round: u32,
    pub captured_sources: u32,
    pub captured_bytes: u64,
    pub packet: Option<ArtifactRef>,
    pub report: Option<ArtifactRef>,
    pub receipts: Vec<ReceiptRef>,
    pub gaps: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportReceipt {
    pub run_id: RecordId,
    pub packet_fingerprint: Blake3Hash,
    pub submission_hash: Blake3Hash,
    pub submission: ResearchSubmission,
    pub captured_sources: Vec<SourceSpanRef>,
    pub outputs: Vec<ArtifactRef>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResearchOutcome {
    pub run_id: RecordId,
    pub status: String,
    pub persisted: bool,
    pub ready_to_import: bool,
    pub reused: bool,
    pub freshness: String,
    pub imported_sources: Vec<SourceSpanRef>,
    pub network_used: bool,
    pub external_tool_usage: String,
    pub packet: Option<ResearchPacket>,
    pub report: Option<ResearchReport>,
    pub next_command: Option<String>,
}

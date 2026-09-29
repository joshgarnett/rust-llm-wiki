//! Caller-owned research scope and typed projections; none confer dispatch authority.
use super::frontier::StageLimits;
use crate::{
    changes::{PreparedChange, ReadDependency},
    config::providers::TrustedService,
    domain::*,
    graph::ExtractionLimits,
    jobs::*,
    providers::{
        dispatcher::Dispatcher,
        public_fetch::{FetchLimits, PublicFetchOptions},
    },
    retrieval::QueryPlan,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchLimits {
    pub rounds: u32,
    pub sources: u32,
    pub search_count: u8,
    pub search_pages: u8,
    pub fetch: FetchLimits,
    pub retrieval: QueryPlan,
    pub extraction: ExtractionLimits,
    pub stage: StageLimits,
    pub stage_output_tokens: u64,
}
impl Default for ResearchLimits {
    fn default() -> Self {
        Self {
            rounds: 3,
            sources: 15,
            search_count: 10,
            search_pages: 1,
            fetch: FetchLimits::default(),
            retrieval: QueryPlan::default(),
            extraction: ExtractionLimits::default(),
            stage: StageLimits::default(),
            stage_output_tokens: 4096,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchScope {
    pub version: u32,
    pub question: String,
    pub exclusions: Vec<String>,
    pub explicit_urls: Vec<String>,
    pub generation_profile: String,
    pub search_profile: Option<String>,
    pub limits: ResearchLimits,
    /// Only a caller-supplied explicit mode may apply generated page proposals.
    pub apply: bool,
}
pub struct ResearchRuntime<'a> {
    pub generation: &'a TrustedService,
    pub search: Option<&'a TrustedService>,
    pub dispatcher: &'a Dispatcher,
    pub public_fetch: &'a PublicFetchOptions,
    pub job_options: JobOptions,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchResumeAmendment {
    pub limits: LifetimeLimits,
    pub deadline_utc_ms: i64,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchPassage {
    pub citation: CitationRef,
    pub quote: String,
    pub dependencies: Vec<ReadDependency>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchGap {
    pub stage: TaskStage,
    pub task_key: Option<Blake3Hash>,
    pub origin: Option<String>,
    pub code: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchPlan {
    pub version: u32,
    pub scope: ResearchScope,
    pub spec: RunSpec,
    pub scope_bytes: Vec<u8>,
    pub descriptors: Vec<ResearchDescriptor>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchDescriptor {
    pub task: TaskSpec,
    pub bytes: Vec<u8>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchStatus {
    pub inspection: LedgerInspection,
    pub gaps: Vec<ResearchGap>,
    pub latest_report: Option<DurableOutputRef>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchReport {
    pub version: u32,
    pub run_id: RecordId,
    pub question: String,
    pub partial: bool,
    pub stop_reason: String,
    pub passages: Vec<ResearchPassage>,
    pub gaps: Vec<ResearchGap>,
    pub synthesis: Option<super::synthesis::Synthesis>,
    pub claim_assessments: Vec<super::synthesis::ClaimAssessment>,
    pub proposed_changes: Vec<PreparedChange>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResearchOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_code: Option<ErrorCode>,
    pub status: ResearchStatus,
    pub report: Option<ResearchReport>,
    pub prepared_changes: Vec<PreparedChange>,
    pub network_used: bool,
}

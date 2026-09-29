//! Opaque authenticated dispatcher boundary. Raw request/auth fields are private.
use crate::config::providers::{InstructionRole, OutputLimitField, ResponseMode};
use crate::{
    domain::*,
    jobs::{
        AttemptBound, AttemptRef, CancellationToken, Capability, JobClock, KnownOrUnknown,
        MaterializationPlan, Money, SendAuthorization, SpoolRef, Usage,
    },
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceRole {
    Embed,
    Generate,
    Search,
}
impl ServiceRole {
    pub fn capability(self) -> Capability {
        match self {
            Self::Embed => Capability::Embed,
            Self::Generate => Capability::Generate,
            Self::Search => Capability::Search,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "purpose", rename_all = "snake_case", deny_unknown_fields)]
pub enum DispatchPurpose {
    Task,
    Probe { role: ServiceRole },
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingInput {
    pub input_hash: Blake3Hash,
    pub utf8: String,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoteOperation {
    Embed {
        inputs: Vec<EmbeddingInput>,
        expected_dimensions: Option<u32>,
        representation_fingerprint: Blake3Hash,
    },
    Generate {
        instructions: String,
        data: String,
        output_schema: serde_json::Value,
        max_output_tokens: u64,
    },
    Search {
        query: String,
        count: u8,
        page: u8,
    },
    Fetch {
        url: String,
        limits: super::public_fetch::FetchLimits,
    },
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteInput {
    pub version: u32,
    pub operation: RemoteOperation,
}

/// Only sealed production encoders and privileged tests can construct this value.
/// Never accepts public arbitrary encoded bytes/hash/token-proof constructors.
pub(crate) struct PreparedWire {
    pub(super) role: ServiceRole,
    pub(super) purpose: DispatchPurpose,
    pub(super) method: String,
    pub(super) url: String,
    pub(super) headers: BTreeMap<String, String>,
    pub(super) body: Vec<u8>,
    pub(super) bound: AttemptBound,
    pub(super) input: RemoteInput,
    pub(super) contract: WireContract,
}

/// One-use request: no Clone/Deserialize/Debug and no public authenticated fields.
pub struct AuthenticatedRequest<'a> {
    pub(super) authorization: SendAuthorization,
    pub(super) prepared: &'a PreparedWire,
    pub(super) lease: super::credentials::CredentialLease,
    pub(super) tls_ca: Option<Arc<[u8]>>,
    pub(super) service: &'a crate::config::providers::TrustedService,
    pub(super) fs: crate::vault::VaultFs,
    pub(super) broker: Arc<super::credentials::CredentialBroker>,
    pub(super) credential_context: super::credentials::CredentialContext,
}
#[derive(Debug, Clone, Serialize)]
pub struct TransportSummary {
    pub attempt: AttemptRef,
    pub role: ServiceRole,
    pub endpoint_fingerprint: Blake3Hash,
    pub wire_hash: Blake3Hash,
    pub request_bytes: u64,
}
#[derive(Clone)]
pub struct TransportContext {
    pub(super) clock: Arc<dyn JobClock>,
    pub(super) cancel: CancellationToken,
    pub(super) deadline_utc_ms: i64,
    pub(super) timeout_ms: u64,
    pub(super) connect_timeout_ms: u64,
    pub(super) response_bytes: u64,
}
/// No raw body/headers Debug/Serialize. Bounded mock constructor lives in transport.rs.
pub struct TransportReply {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
    pub(super) observed_body_bytes: u64,
    pub(super) body_exceeded: bool,
    pub(super) headers_exceeded: bool,
    pub(super) terminal: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportFailureCode {
    Unavailable,
    Timeout,
    Cancelled,
    ClockRegression,
    InvalidResponse,
}
/// External mock errors never mint definitely-not-sent observations.
pub struct TransportFailure {
    pub(super) code: TransportFailureCode,
    pub(super) observed_body_bytes: u64,
    pub(super) not_entered: bool,
}
pub type TransportFuture<'a> = Pin<
    Box<dyn Future<Output = std::result::Result<TransportReply, TransportFailure>> + Send + 'a>,
>;
pub trait Transport: Send + Sync {
    fn execute<'a>(
        &'a self,
        request: AuthenticatedRequest<'a>,
        context: TransportContext,
    ) -> TransportFuture<'a>;
}
pub trait JitterSource: Send + Sync {
    fn sample_inclusive(&self, max_ms: u64) -> Result<u64>;
}
pub struct DispatchOptions {
    pub broker: Arc<super::credentials::CredentialBroker>,
    pub transport: Arc<dyn Transport>,
    pub jitter: Arc<dyn JitterSource>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchDisposition {
    NotSent,
    Rejected,
    OutcomeUnknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RetryDecision {
    Never,
    After { delay_ms: u64, reason: String },
    Pause { reason: String },
}
#[derive(Default)]
pub(crate) struct RetryState {
    pub(super) attempts: u32,
    pub(super) command_refresh_used: bool,
}
/// Validated knowledge output, never an unparsed provider error body.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValidatedOutput {
    Embeddings {
        vectors: Vec<Vec<f32>>,
        returned_model: Option<String>,
    },
    Generation {
        value: serde_json::Value,
        text: String,
        returned_model: Option<String>,
    },
    Probe {
        role: ServiceRole,
    },
    Search {
        leads: Vec<super::search_wire::SearchLead>,
    },
}
pub struct DispatchOutcome {
    pub attempt: AttemptRef,
    pub spool: SpoolRef,
    pub materialization: MaterializationPlan,
    pub output: ValidatedOutput,
}
pub struct DispatchFailure {
    pub error: WikiError,
    pub disposition: DispatchDisposition,
    pub attempt: Option<AttemptRef>,
    pub spool: Option<SpoolRef>,
    pub materialization: Option<MaterializationPlan>,
    pub retry: RetryDecision,
}
pub(crate) struct ObservedUsage {
    pub(super) usage: KnownOrUnknown<Usage>,
    pub(super) computed_cost: KnownOrUnknown<Money>,
    pub(super) provider_request_id: Option<String>,
    pub(super) returned_model: Option<String>,
    pub(super) contract_violation: Option<WireContractViolation>,
}

pub(super) enum BoundBasis {
    UnknownCompatible,
    OpenAiChatGpt41SnapshotV1,
    OpenAiEmbedding3SmallV1 { representation_revision: String },
    OpenAiEmbedding3LargeV1 { representation_revision: String },
}

// No Debug: schemas/prompts must not become accidental diagnostics.
pub(super) enum WireContract {
    #[cfg(test)]
    Fixture,
    Embedding(EmbeddingContract),
    Generation(GenerationContract),
    Search(super::search_wire::SearchContract),
}

pub(super) struct EmbeddingItemContract {
    pub(super) position: u32,
    pub(super) input_hash: Blake3Hash,
    pub(super) utf8_bytes: u64,
}

pub(super) struct EmbeddingContract {
    pub(super) basis: BoundBasis,
    // Every position retained, including identical text/hash entries.
    pub(super) items: Vec<EmbeddingItemContract>,
    pub(super) representation_fingerprint: Blake3Hash,
    // Parameter sent only for explicit profile Fixed; Auto sends none.
    pub(super) requested_dimensions: Option<u32>,
    // Fixed and/or established space dimension, validated consistent upfront.
    pub(super) expected_dimensions: Option<u32>,
    pub(super) maximum_dimensions: u32,
    pub(super) maximum_coordinates: u64,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GenerationSurface {
    #[default]
    ChatCompletions,
    Responses,
}
impl GenerationSurface {
    pub(super) fn is_chat(&self) -> bool {
        matches!(self, Self::ChatCompletions)
    }
}

pub(super) struct GenerationContract {
    pub(super) surface: GenerationSurface,
    pub(super) provider_schema: Option<Arc<serde_json::Value>>,
    pub(super) basis: BoundBasis,
    pub(super) instruction_role: InstructionRole,
    pub(super) output_limit_field: OutputLimitField,
    pub(super) response_mode: ResponseMode,
    pub(super) total_generated_token_limit: u64,
    pub(super) schema_fingerprint: Blake3Hash,
    // Exact input schema, unweakened; compiled locally in offline mode.
    pub(super) schema: Arc<serde_json::Value>,
    pub(super) validator: Arc<jsonschema::Validator>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WireContractViolation {
    ReturnedModelMismatch,
    UnexpectedBillableClass,
    ObservedTotalBoundExceeded,
}

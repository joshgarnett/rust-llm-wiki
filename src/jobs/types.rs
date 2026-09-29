//! Durable jobs and conservative accounting interfaces, owned by the orchestrator.
use crate::{
    changes::{ChangeDraft, PreparedChange, ReadDependency},
    domain::{
        Blake3Hash, ErrorCode, ReadSnapshot, RecordId, RecordRef, Result, VaultRelativePath,
        WikiError,
    },
    vault::{VaultFs, WriterPermit},
};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub const JOURNAL_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const JOURNAL_MAX_EVENTS: usize = 65_536;
pub const RUN_MAX_TASKS: usize = 4_096;
pub const EVENT_MAX_BYTES: usize = 256 * 1024;
pub const METADATA_MAX_BYTES: usize = 64 * 1024;
pub const GENERATION_SPOOL_MAX_BYTES: u64 = 1024 * 1024;
pub const OTHER_SPOOL_MAX_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_DEADLINE_MS: u64 = 900_000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Currency(String);
impl Currency {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != 3 || !value.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "currency must be three uppercase ASCII letters",
            ));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for Currency {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Money {
    currency: Currency,
    nanounits: u64,
}
impl Money {
    pub fn new(currency: Currency, nanounits: u64) -> Self {
        Self {
            currency,
            nanounits,
        }
    }
    pub fn currency(&self) -> &Currency {
        &self.currency
    }
    pub fn nanounits(&self) -> u64 {
        self.nanounits
    }
    pub fn checked_add(&self, other: &Self) -> Result<Self> {
        if self.currency != other.currency {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "currency mismatch",
            ));
        }
        let nanounits = self
            .nanounits
            .checked_add(other.nanounits)
            .ok_or_else(|| WikiError::new(ErrorCode::BudgetExceeded, "money overflow"))?;
        Ok(Self::new(self.currency.clone(), nanounits))
    }
}
/// Leaf budgets.rs implements checked decimal parsing; never floats or exponents.
pub trait MoneyParsing {
    fn parse_decimal(currency: Currency, decimal: &str) -> Result<Money>;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rate {
    pub price_nanounits: u64,
    pub per_units: NonZeroU64,
}
impl Rate {
    pub fn allowance_nanounits(&self, units: u64) -> Result<u64> {
        let product = u128::from(self.price_nanounits)
            .checked_mul(u128::from(units))
            .ok_or_else(|| {
                WikiError::new(ErrorCode::BudgetExceeded, "rate multiplication overflow")
            })?;
        let denominator = u128::from(self.per_units.get());
        let rounded = (product / denominator)
            .checked_add(u128::from(product % denominator != 0))
            .ok_or_else(|| WikiError::new(ErrorCode::BudgetExceeded, "rate rounding overflow"))?;
        u64::try_from(rounded)
            .map_err(|_| WikiError::new(ErrorCode::BudgetExceeded, "rate allowance overflow"))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillableClass {
    Input,
    CachedInput,
    Output,
    Reasoning,
    SearchResult,
    FetchByte,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "quality", rename_all = "snake_case", deny_unknown_fields)]
pub enum TokenBound {
    Exact {
        count: u64,
        tokenizer: String,
        fingerprint: Blake3Hash,
    },
    ProvenUpper {
        count: u64,
        method: String,
        fingerprint: Blake3Hash,
    },
    Estimate {
        count: u64,
        method: String,
    },
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "pricing", rename_all = "snake_case", deny_unknown_fields)]
pub enum PriceValidity {
    DispatchLocked {
        valid_from_utc_ms: i64,
        valid_until_utc_ms: i64,
    },
    EntireAttempt {
        valid_from_utc_ms: i64,
        valid_until_utc_ms: i64,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateCard {
    pub id: String,
    /// Nonzero pricing snapshot revision. The enclosing v1 DTO fixes its format;
    /// the complete id/revision/content fingerprint binds this quoted snapshot.
    pub version: u32,
    pub fingerprint: Blake3Hash,
    pub currency: Currency,
    pub validity: PriceValidity,
    pub request_fee_nanounits: u64,
    /// Missing rate is unknown, never zero. Missing cached rate may use a proven
    /// conservative applicable full-input rate; discount is never inferred.
    pub rates: BTreeMap<BillableClass, Rate>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Embed,
    Generate,
    Search,
    Fetch,
    Probe,
    TokenCount,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Planned,
    Running,
    Paused,
    Completed,
    Failed,
    Stopped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    Completed,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStage {
    Embed,
    Extract,
    InspectExisting,
    PlanFrontier,
    Discover,
    Capture,
    AssessGaps,
    Synthesize,
    StageChanges,
    Probe,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifetimeLimits {
    pub requests: u64,
    pub concurrency: u32,
    pub attempts_per_task: u32,
    pub request_bytes: Option<u64>,
    pub response_bytes: Option<u64>,
    pub billable_units: BTreeMap<BillableClass, u64>,
    pub max_cost: Option<Money>,
    pub requests_per_minute: Option<u32>,
    pub tokens_per_minute: Option<u64>,
}
impl Default for LifetimeLimits {
    fn default() -> Self {
        Self {
            requests: 60,
            concurrency: 2,
            attempts_per_task: 3,
            request_bytes: None,
            response_bytes: None,
            billable_units: BTreeMap::new(),
            max_cost: None,
            requests_per_minute: None,
            tokens_per_minute: None,
        }
    }
}
/// Explicit UTC deadline is populated at creation from clock + default 900 seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    pub version: u32,
    pub run_id: RecordId,
    pub vault_id: RecordId,
    pub title: String,
    pub created_at_utc_ms: i64,
    pub deadline_utc_ms: i64,
    pub scope: RunScope,
    pub config_fingerprint: Blake3Hash,
    pub input_fingerprint: Blake3Hash,
    pub limits: LifetimeLimits,
    pub tasks: Vec<TaskSpec>,
    /// New runs after incomplete history disclose uncertainty; no old spend reset.
    pub prior_accounting: PriorAccounting,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScope {
    pub operation: String,
    pub question: Option<String>,
    pub exclusions: Vec<String>,
    pub source_snapshot: Option<ReadSnapshot>,
    pub input_records: Vec<RecordRef>,
    pub read_preconditions: Vec<ReadDependency>,
    pub profile_fingerprints: BTreeMap<String, Blake3Hash>,
    /// Host/research strings are bounded data, never executable commands or URLs
    /// that confer trust. P20 owns the versioned scope payload schema.
    pub scope_payload_hash: Option<Blake3Hash>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PriorAccounting {
    None,
    Unknown {
        prior_run_ids: Vec<RecordId>,
        reason: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub key: Blake3Hash,
    pub stage: TaskStage,
    pub capability: Option<Capability>,
    pub priority: i32,
    pub dependencies: Vec<Blake3Hash>,
    pub input_hash: Blake3Hash,
    pub prompt_hash: Option<Blake3Hash>,
    pub schema_hash: Option<Blake3Hash>,
    pub model_hash: Option<Blake3Hash>,
    pub settings_hash: Blake3Hash,
    pub source_bindings: Vec<ReadDependency>,
    /// Complete immutable task descriptor, held as bounded local payload.
    pub input: BoundedPayloadRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundedPayloadRef {
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
    pub byte_len: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRef {
    pub run_id: RecordId,
    pub task_key: Blake3Hash,
    pub attempt_id: RecordId,
    pub number: u32,
    pub request_hash: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptBound {
    pub capability: Capability,
    pub profile_id: String,
    pub endpoint_fingerprint: Blake3Hash,
    pub config_fingerprint: Blake3Hash,
    pub input_hash: Blake3Hash,
    pub wire_hash: Blake3Hash,
    pub requested_model: Option<String>,
    pub requested_model_revision: Option<String>,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub timeout_ms: u64,
    /// Every possibly billable class is required by preflight, not guessed by jobs.
    pub applicable_classes: BTreeSet<BillableClass>,
    pub billable_bounds: BTreeMap<BillableClass, TokenBound>,
    pub rate_card: Option<RateCard>,
    pub quoted_allowance: Option<Money>,
    pub bounds_fingerprint: Blake3Hash,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Reserved,
    DispatchIntent,
    Received,
    OutputCommitted,
    Settled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingDisposition {
    Reserved,
    ReleasedNotSent,
    KnownSettled,
    UnknownReserved,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteExposure {
    NotStarted,
    PossiblyInFlight,
    TerminalConfirmed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum KnownOrUnknown<T> {
    Known(T),
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub billable_units: BTreeMap<BillableClass, KnownOrUnknown<u64>>,
    pub request_bytes: u64,
    pub response_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputDisposition {
    Validated,
    Rejected,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorCacheRef {
    pub space_hash: Blake3Hash,
    pub input_hash: Blake3Hash,
    pub vector_hash: Blake3Hash,
    pub dimensions: u32,
    pub membership_fingerprint: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableOutputRef {
    pub record: RecordRef,
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageReceipt {
    pub version: u32,
    pub receipt_id: RecordId,
    pub attempt: AttemptRef,
    pub capability: Capability,
    pub profile_id: String,
    pub endpoint_fingerprint: Blake3Hash,
    pub input_hash: Blake3Hash,
    pub requested_model: Option<String>,
    pub returned_model: Option<String>,
    pub provider_request_id: Option<String>,
    pub usage: KnownOrUnknown<Usage>,
    pub billing: BillingDisposition,
    pub reservation: Option<Money>,
    pub computed_cost: KnownOrUnknown<Money>,
    pub rate_card_fingerprint: Option<Blake3Hash>,
    pub output_disposition: OutputDisposition,
    pub outputs: Vec<DurableOutputRef>,
    pub cache_outputs: Vec<VectorCacheRef>,
    pub failure_code: Option<String>,
}
/// Safe provider identifiers; public acquisition provenance is separately typed
/// and protected by the spool metadata hash. No auth/helper/error-body contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition: Option<crate::providers::public_fetch::PublicCaptureMetadata>,
    pub provider_request_id: Option<String>,
    pub returned_model: Option<String>,
    pub status_code: Option<u16>,
    pub terminal_response: bool,
    pub usage: KnownOrUnknown<Usage>,
    pub computed_cost: KnownOrUnknown<Money>,
    pub failure_code: Option<String>,
}
pub struct ResponseSpoolInput {
    pub bytes: Vec<u8>,
    pub metadata: ResponseMetadata,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoolRef {
    pub attempt: AttemptRef,
    pub response: BoundedPayloadRef,
    pub metadata: BoundedPayloadRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventRef {
    pub event_id: RecordId,
    pub sequence: u64,
    pub checksum: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEvent {
    pub version: u32,
    pub run_id: RecordId,
    pub event_id: RecordId,
    pub sequence: u64,
    pub previous_checksum: Option<Blake3Hash>,
    pub occurred_at_utc_ms: i64,
    pub payload: EventPayload,
}
/// Checksum is over encoded LedgerEvent bytes, excluding this frame envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalFrame {
    pub event: LedgerEvent,
    pub checksum: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventPayload {
    Genesis {
        spec: RunSpec,
        spec_hash: Blake3Hash,
        run_note: DurableOutputRef,
    },
    TasksAdded {
        tasks: Vec<TaskSpec>,
    },
    TaskFinished {
        task_key: Blake3Hash,
        state: TaskState,
        outputs: Vec<DurableOutputRef>,
        cache_outputs: Vec<VectorCacheRef>,
        reason: Option<String>,
    },
    RunTransition {
        from: RunState,
        to: RunState,
        reason: StopReason,
    },
    Amendment {
        amendment: LimitAmendment,
    },
    Reserved {
        attempt: AttemptRef,
        bound: AttemptBound,
        allowance: Allowance,
        persistence: PersistenceAllowance,
    },
    DispatchIntent {
        attempt: AttemptRef,
    },
    SendAuthorized {
        attempt: AttemptRef,
    },
    ReleasedNotSent {
        attempt: AttemptRef,
        reason: NotSentReason,
    },
    Received {
        spool: SpoolRef,
    },
    OutcomeUnknown {
        attempt: AttemptRef,
        reason: String,
    },
    OutputsCommitted {
        attempt: AttemptRef,
        receipt: DurableOutputRef,
        outputs: Vec<DurableOutputRef>,
        cache_outputs: Vec<VectorCacheRef>,
        change: PreparedChange,
    },
    Settled {
        attempt: AttemptRef,
        billing: BillingDisposition,
        usage: KnownOrUnknown<Usage>,
        cost: KnownOrUnknown<Money>,
    },
    Reconciled {
        attempt: AttemptRef,
        terminal_confirmed: bool,
        usage: KnownOrUnknown<Usage>,
        cost: KnownOrUnknown<Money>,
        reason: String,
    },
    BoundViolated {
        attempt: AttemptRef,
        actual: Usage,
        actual_cost: KnownOrUnknown<Money>,
    },
    Checkpoint {
        completed_tasks: CompletedTaskSummary,
        frontier: Option<BoundedPayloadRef>,
        run_note: DurableOutputRef,
    },
    SpoolRemoved {
        attempt: AttemptRef,
    },
    ClockObserved {
        utc_high_water_ms: i64,
    },
}
/// Hash of the canonical JSON array of sorted completed task keys. The complete
/// map remains in the run plan and replay state, keeping checkpoint events bounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletedTaskSummary {
    pub count: u32,
    pub fingerprint: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowance {
    pub requests: u64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub billable_units: BTreeMap<BillableClass, u64>,
    pub cost: Option<Money>,
}
/// Admission also reserves worst-case mandatory remaining event/frame capacity.
/// Full-history ceilings cannot make a dispatched receipt impossible to record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistenceAllowance {
    pub journal_bytes: u64,
    pub event_slots: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitAmendment {
    pub requested_at_utc_ms: i64,
    pub reason: String,
    pub limits: LifetimeLimits,
    pub deadline_utc_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "reason",
    content = "detail",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StopReason {
    Started,
    Completed,
    Cancelled,
    Deadline,
    Budget,
    ClockRegression,
    OutcomeUnknown,
    ReconciliationRequired,
    InputsChanged,
    Failed(String),
    User(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotSentReason {
    CredentialsUnavailable,
    CancelledBeforeSend,
    TransportNotEntered,
    ConnectFailedBeforeWrite,
}
/// Dispatcher-only observation; deserialized event reasons never release allowances.
pub(crate) struct NotSentObservation {
    pub(crate) reason: NotSentReason,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecutionPolicy {
    pub offline: bool,
    pub dry_run: bool,
    pub retry_uncertain: bool,
}
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ClockReading {
    pub utc_ms: i64,
    pub monotonic_ms: u64,
}
pub trait JobClock: Send + Sync {
    fn read(&self) -> Result<ClockReading>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerCheckpoint {
    #[cfg(test)]
    BeforeSendAuthorityFlush,
    BeforeAppend,
    AfterAppend,
    AfterJournalSync,
    AfterSpoolBytesSync,
    AfterSpoolMetadataSync,
    BeforeReceived,
    AfterReceived,
    BeforeOutputsCommitted,
    AfterOutputsCommitted,
    BeforeSettlement,
    AfterSettlement,
    BeforeSpoolRemove,
    AfterSpoolRemove,
}
pub trait LedgerFault: Send + Sync {
    fn check(&self, point: LedgerCheckpoint) -> Result<()>;
}
#[derive(Clone)]
pub struct JobOptions {
    pub clock: Arc<dyn JobClock>,
    pub fault: Option<Arc<dyn LedgerFault>>,
    pub cancel: CancellationToken,
    pub policy: ExecutionPolicy,
    pub lock_timeout_ms: u64,
}

/// Opaque one-use authorities: no Clone, Deserialize, public fields or constructors.
pub struct Reservation {
    pub(super) attempt: AttemptRef,
    pub(super) bound: AttemptBound,
    pub(super) allowance: Allowance,
    pub(super) reserved_event: EventRef,
    pub(super) persistence: PersistenceAllowance,
    pub(super) genesis_hash: Blake3Hash,
}
pub struct DispatchPermit {
    pub(super) attempt: AttemptRef,
    pub(super) bound: AttemptBound,
    pub(super) intent_event: EventRef,
    pub(super) genesis_hash: Blake3Hash,
}
/// The private dispatcher/transport consumes this exactly once. Replay never returns it.
pub struct SendAuthorization {
    pub(super) attempt: AttemptRef,
    pub(super) bound: AttemptBound,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "retained opaque authorization provenance is asserted by ledger tests"
        )
    )]
    pub(super) send_event: EventRef,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "retained opaque authorization provenance is asserted by ledger tests"
        )
    )]
    pub(super) genesis_hash: Blake3Hash,
    pub(super) closing_reading: ClockReading,
    pub(super) clock: Arc<dyn JobClock>,
    pub(super) cancel: CancellationToken,
    pub(super) effective_limits: LifetimeLimits,
    pub(super) effective_deadline_utc_ms: i64,
}
impl Reservation {
    pub fn attempt(&self) -> &AttemptRef {
        &self.attempt
    }
}
impl DispatchPermit {
    pub fn attempt(&self) -> &AttemptRef {
        &self.attempt
    }
    pub fn bound(&self) -> &AttemptBound {
        &self.bound
    }
}
impl SendAuthorization {
    pub(crate) fn closing_reading(&self) -> ClockReading {
        self.closing_reading
    }
    /// Recheck the exact authority at the actual first network poll. Only the
    /// owning ledger's clock and original complete bound can extend this proof.
    pub(crate) fn check_before_entry(&mut self) -> Result<ClockReading> {
        self.check_before_entry_after(self.closing_reading)
    }
    /// A transport's later observed reading can only strengthen the existing
    /// floor; it cannot replace the ledger clock or weaken its closing proof.
    pub(crate) fn check_before_entry_after(&mut self, floor: ClockReading) -> Result<ClockReading> {
        let now = self.clock.read()?;
        if self.cancel.is_cancelled() {
            return Err(WikiError::new(
                ErrorCode::Cancelled,
                "job cancelled before transport entry",
            ));
        }
        if now.utc_ms < self.closing_reading.utc_ms.max(floor.utc_ms)
            || now.monotonic_ms < self.closing_reading.monotonic_ms.max(floor.monotonic_ms)
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "clock regressed before transport entry",
            ));
        }
        super::budgets::quote_bound(
            &self.bound,
            &self.effective_limits,
            now.utc_ms,
            self.effective_deadline_utc_ms,
        )?;
        self.closing_reading = now;
        Ok(now)
    }
    pub(crate) fn attempt(&self) -> &AttemptRef {
        &self.attempt
    }
    pub(crate) fn bound(&self) -> &AttemptBound {
        &self.bound
    }
}
pub struct JobLedger {
    pub(super) fs: VaultFs,
    pub(super) vault_id: RecordId,
    pub(super) run_id: RecordId,
    pub(super) options: JobOptions,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptInspection {
    pub attempt: AttemptRef,
    pub phase: AttemptPhase,
    pub billing: BillingDisposition,
    pub remote_exposure: RemoteExposure,
    pub bound: AttemptBound,
    pub allowance: Allowance,
    pub persistence: PersistenceAllowance,
    pub spool: Option<SpoolRef>,
    pub receipt: Option<DurableOutputRef>,
    pub outputs: Vec<DurableOutputRef>,
    pub cache_outputs: Vec<VectorCacheRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingCompleteness {
    CompleteHistory,
    MissingHistory,
    CorruptHistory,
    MarkdownOnly,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetInspection {
    pub dispatched_requests: u64,
    pub outstanding_requests: u64,
    /// Includes Reserved + possibly in-flight; terminal response releases its slot.
    pub concurrency_admitted: u32,
    pub remote_inflight: u32,
    pub journal_bytes_used: u64,
    pub journal_events_used: u64,
    pub persistence_reserved: PersistenceAllowance,
    pub settled: Allowance,
    pub outstanding: Allowance,
    pub known_costs: BTreeMap<Currency, u64>,
    pub unknown_attempts: Vec<RecordId>,
    pub guarantee_intact: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInspection {
    pub spec: TaskSpec,
    pub state: TaskState,
    pub outputs: Vec<DurableOutputRef>,
    pub cache_outputs: Vec<VectorCacheRef>,
    pub failure_code: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerInspection {
    pub spec: RunSpec,
    pub spec_hash: Blake3Hash,
    pub effective_limits: LifetimeLimits,
    pub effective_deadline_utc_ms: i64,
    pub state: RunState,
    pub tasks: BTreeMap<Blake3Hash, TaskInspection>,
    pub attempts: Vec<AttemptInspection>,
    pub budget: BudgetInspection,
    pub last_event: Option<EventRef>,
    pub run_note: Option<DurableOutputRef>,
    pub utc_high_water_ms: i64,
    pub completeness: AccountingCompleteness,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayReport {
    pub inspection: LedgerInspection,
    pub orphan_spools: Vec<SpoolRef>,
    pub conflicting_paths: Vec<VaultRelativePath>,
    pub missing_cache_outputs: Vec<VectorCacheRef>,
    pub reusable_outputs: Vec<DurableOutputRef>,
    pub warnings: Vec<String>,
}
/// Plan has no authorization; canonical changes execute through actual ChangeEngine.
pub struct MaterializationPlan {
    pub attempt: AttemptRef,
    pub receipt: UsageReceipt,
    pub draft: ChangeDraft,
}

/// Public/offline planning and inspection; leaves implement this trait or identical
/// inherent methods after root freezes. No method holds run lock while taking writer.
pub trait JobLedgerApi: Sized {
    fn new(fs: VaultFs, vault_id: RecordId, run_id: RecordId, options: JobOptions) -> Result<Self>;
    fn bootstrap_plan(fs: &VaultFs, spec: &RunSpec) -> Result<ChangeDraft>;
    fn create(&self, writer: &WriterPermit, spec: RunSpec) -> Result<LedgerInspection>;
    fn inspect(&self) -> Result<LedgerInspection>;
    fn replay(&self) -> Result<ReplayReport>;
    fn add_tasks(&self, tasks: Vec<TaskSpec>) -> Result<EventRef>;
    fn start(&self) -> Result<EventRef>;
    fn pause(&self, reason: StopReason) -> Result<EventRef>;
    fn stop(&self) -> Result<EventRef>;
    fn resume(&self, amendment: Option<LimitAmendment>) -> Result<EventRef>;
    fn ready_tasks(&self) -> Result<Vec<TaskSpec>>;
    fn finish_local_task(
        &self,
        task: &Blake3Hash,
        outputs: Vec<DurableOutputRef>,
        cache_outputs: Vec<VectorCacheRef>,
    ) -> Result<EventRef>;
    fn check_bound(&self, task: &Blake3Hash, bound: &AttemptBound) -> Result<Allowance>;
    fn reserve(&self, task: &Blake3Hash, bound: AttemptBound) -> Result<Reservation>;
    fn dispatch_intent(&self, reservation: Reservation) -> Result<DispatchPermit>;
    fn materialization_plan(&self, attempt: &AttemptRef) -> Result<MaterializationPlan>;
    fn outputs_committed(
        &self,
        attempt: &AttemptRef,
        change: &PreparedChange,
        receipt: DurableOutputRef,
        outputs: Vec<DurableOutputRef>,
        cache_outputs: Vec<VectorCacheRef>,
    ) -> Result<EventRef>;
    fn checkpoint_plan(&self) -> Result<ChangeDraft>;
    fn checkpoint_committed(&self, change: &PreparedChange) -> Result<EventRef>;
}
/// Trusted crate-private P16 dispatcher boundary; no HTTP/auth types in P15.
/// begin_send consumes the intent permit and rechecks state/cancel/deadline under
/// short lock immediately before possible send. execute consumes SendAuthorization.
/// record_response validates stored attempt/genesis/request binding and spool ceiling;
/// it deliberately accepts stored identity after execute consumed send authority.
/// replay/reconcile return data only, never Reservation/DispatchPermit/SendAuthorization.
pub(crate) trait DispatcherLedgerApi {
    fn begin_send(&self, permit: DispatchPermit) -> Result<SendAuthorization>;
    fn release_not_sent(&self, attempt: &AttemptRef, proof: NotSentObservation)
    -> Result<EventRef>;
    fn outcome_unknown(&self, attempt: &AttemptRef, safe_code: &str) -> Result<EventRef>;
    fn record_response(
        &self,
        attempt: &AttemptRef,
        response: ResponseSpoolInput,
    ) -> Result<SpoolRef>;
    fn settle(&self, attempt: &AttemptRef) -> Result<EventRef>;
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "verified reconciliation contract exercised by accounting tests; workflows consume it separately"
        )
    )]
    fn reconcile(
        &self,
        attempt: &AttemptRef,
        terminal_confirmed: bool,
        usage: KnownOrUnknown<Usage>,
        cost: KnownOrUnknown<Money>,
        reason: &str,
    ) -> Result<EventRef>;
    fn remove_spool_after_verified_commit(&self, attempt: &AttemptRef) -> Result<EventRef>;
}

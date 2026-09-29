//! Trusted task input -> one-use ledger authorization -> bounded transport.
use super::{
    credentials::{CredentialBroker, CredentialContext, SecretBytes},
    retry,
    transport::{NativeTransport, headers_valid},
    types::*,
};
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{ChangeEngine, prepare::read_bounded},
    config::providers::{AuthConfig, TrustedService},
    domain::*,
    jobs::{self, DispatcherLedgerApi, *},
    vault::{ExpectedState, VaultFs, WriterPermit},
};
#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) enum RetainedDecodeFailure {
    Recovery(WikiError),
    InvalidResponse(WikiError),
}
impl From<WikiError> for RetainedDecodeFailure {
    fn from(error: WikiError) -> Self {
        Self::Recovery(error)
    }
}
impl RetainedDecodeFailure {
    pub(crate) fn error(self) -> WikiError {
        match self {
            Self::Recovery(e) | Self::InvalidResponse(e) => e,
        }
    }
}

/// One current research task. A Fetch task has no credentialed service.
pub struct ResearchDispatchWork<'a> {
    pub task_key: Blake3Hash,
    pub service: Option<&'a TrustedService>,
    pub purpose: DispatchPurpose,
}
pub enum ResearchDispatchOutcome {
    Remote(std::result::Result<DispatchOutcome, Box<DispatchFailure>>),
    Public(std::result::Result<super::public_fetch::PublicFetchOutcome, Box<DispatchFailure>>),
}
pub struct ResearchDispatchResult {
    pub task_key: Blake3Hash,
    pub outcome: ResearchDispatchOutcome,
}
pub struct ResearchDispatchBatch {
    pub outcomes: Vec<ResearchDispatchResult>,
    pub stop: Option<WikiError>,
}
enum PreparedResearch<'a> {
    Remote(&'a TrustedService, DispatchPurpose, PreparedWire),
    Public(super::public_fetch::PreparedFetch),
}

pub struct Dispatcher {
    fs: VaultFs,
    options: DispatchOptions,
    network_used: std::sync::atomic::AtomicBool,
}
impl Dispatcher {
    pub fn new(fs: VaultFs, options: DispatchOptions) -> Self {
        Self {
            fs,
            options,
            network_used: std::sync::atomic::AtomicBool::new(false),
        }
    }
    /// Monotone activity for this dispatcher instance. CLI runtimes scope one
    /// dispatcher to one invocation; retained response reads do not set it.
    pub fn network_used(&self) -> bool {
        self.network_used.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn native(fs: VaultFs, broker: Arc<CredentialBroker>) -> Self {
        Self::new(
            fs,
            DispatchOptions {
                broker,
                transport: Arc::new(NativeTransport),
                jitter: Arc::new(retry::NativeJitter),
            },
        )
    }
    pub fn execute_public(
        &self,
        ledger: &JobLedger,
        key: &Blake3Hash,
        options: &super::public_fetch::PublicFetchOptions,
    ) -> std::result::Result<super::public_fetch::PublicFetchOutcome, Box<DispatchFailure>> {
        if ledger
            .inspect()
            .map_err(|e| failure(e, None))?
            .research
            .is_some()
        {
            let batch = self
                .execute_ready_batch(
                    ledger,
                    &[ResearchDispatchWork {
                        task_key: key.clone(),
                        service: None,
                        purpose: DispatchPurpose::Task,
                    }],
                    options,
                )
                .map_err(|e| failure(e, None))?;
            if let Some(result) = batch.outcomes.into_iter().next() {
                return match result.outcome {
                    ResearchDispatchOutcome::Public(out) => out,
                    ResearchDispatchOutcome::Remote(_) => unreachable!(),
                };
            }
            return Err(failure(
                batch
                    .stop
                    .unwrap_or_else(|| WikiError::invalid("research task was not reserved")),
                None,
            ));
        }
        super::public_fetch::dispatch(&self.fs, ledger, key, options, &self.network_used)
    }
    /// Decode a hash-bound response already retained by this ledger. This grants
    /// no send authority and does not require live source freshness: callers must
    /// independently revalidate dependencies before activating derived output.
    pub fn decode_retained(
        &self,
        ledger: &JobLedger,
        service: &TrustedService,
        task_key: &Blake3Hash,
        purpose: DispatchPurpose,
        attempt: &AttemptRef,
    ) -> Result<ValidatedOutput> {
        self.decode_retained_classified(ledger, service, task_key, purpose, attempt)
            .map_err(RetainedDecodeFailure::error)
    }
    pub(crate) fn decode_retained_classified(
        &self,
        ledger: &JobLedger,
        service: &TrustedService,
        task_key: &Blake3Hash,
        purpose: DispatchPurpose,
        attempt: &AttemptRef,
    ) -> std::result::Result<ValidatedOutput, RetainedDecodeFailure> {
        let (fs, vault_id, run_id, options) = ledger.dispatcher_bindings();
        if fs.root().path() != self.fs.root().path()
            || attempt.run_id != run_id
            || attempt.task_key != *task_key
        {
            return Err(RetainedDecodeFailure::Recovery(WikiError::new(
                ErrorCode::RecoveryRequired,
                "retained attempt binding differs",
            )));
        }
        let inspection = ledger.inspect()?;
        let task = inspection
            .tasks
            .get(task_key)
            .ok_or_else(|| WikiError::new(ErrorCode::RecordNotFound, "retained task missing"))?;
        let recorded = inspection
            .attempts
            .iter()
            .find(|a| &a.attempt == attempt)
            .ok_or_else(|| WikiError::new(ErrorCode::RecordNotFound, "retained attempt missing"))?;
        let spool = recorded.spool.as_ref().ok_or_else(|| {
            WikiError::new(ErrorCode::RecoveryRequired, "retained response missing")
        })?;
        let prepared = if recorded.bound.codec.is_some() {
            super::history::restore(&fs, &task.spec, &recorded.bound, purpose)?
        } else {
            let mut prepared = prepare_descriptor(&fs, service, &task.spec, purpose)?;
            // Restore the original optional proof before exact bound comparison.
            prepared.bound.profile_fingerprint = recorded.bound.profile_fingerprint.clone();
            prepared.bound.bounds_fingerprint = jobs::budgets::bound_fingerprint(&prepared.bound)?;
            prepared
        };
        if prepared.bound != recorded.bound || spool.attempt != *attempt {
            return Err(RetainedDecodeFailure::Recovery(WikiError::new(
                ErrorCode::FreshnessConflict,
                "retained wire contract differs",
            )));
        }
        let store = crate::vault::operational::RunStore::open_existing(&fs, &vault_id, &run_id)?;
        let guard = store.lock(Duration::from_millis(options.lock_timeout_ms), &mut || {
            Ok(())
        })?;
        let body = guard
            .read_spool(
                attempt,
                crate::vault::operational::SpoolPart::Body,
                recorded.bound.response_bytes,
            )?
            .ok_or_else(|| WikiError::new(ErrorCode::RecoveryRequired, "retained body missing"))?;
        let meta = guard
            .read_spool(
                attempt,
                crate::vault::operational::SpoolPart::Metadata,
                METADATA_MAX_BYTES as u64,
            )?
            .ok_or_else(|| {
                WikiError::new(ErrorCode::RecoveryRequired, "retained metadata missing")
            })?;
        if body.hash != spool.response.hash
            || body.bytes.len() as u64 != spool.response.byte_len
            || meta.hash != spool.metadata.hash
            || meta.bytes.len() as u64 != spool.metadata.byte_len
        {
            return Err(RetainedDecodeFailure::Recovery(WikiError::new(
                ErrorCode::RecoveryRequired,
                "retained response bytes differ",
            )));
        }
        let metadata: ResponseMetadata = crate::changes::prepare::strict_json(&meta.bytes)?;
        let status = metadata.status_code.ok_or_else(|| {
            WikiError::new(
                ErrorCode::ProviderResponse,
                "retained response is incomplete",
            )
        })?;
        if !metadata.terminal_response {
            return Err(RetainedDecodeFailure::Recovery(WikiError::new(
                ErrorCode::RecoveryRequired,
                "retained response is incomplete",
            )));
        }
        if !(200..300).contains(&status) || metadata.failure_code.is_some() {
            return Err(RetainedDecodeFailure::InvalidResponse(WikiError::new(
                ErrorCode::ProviderResponse,
                "retained response is rejected",
            )));
        }
        let reply = TransportReply {
            status,
            headers: Vec::new(),
            observed_body_bytes: body.bytes.len() as u64,
            body: body.bytes,
            body_exceeded: false,
            headers_exceeded: false,
            terminal: true,
        };
        if observe(&prepared, &reply).contract_violation.is_some() {
            return Err(RetainedDecodeFailure::InvalidResponse(WikiError::new(
                ErrorCode::ProviderResponse,
                "retained response violates sealed contract",
            )));
        }
        decode(&prepared, &reply).map_err(RetainedDecodeFailure::InvalidResponse)
    }
    /// Resume a generation response without authentication, DNS, HTTP or a new
    /// attempt. Rejected spools remain paid; caller materializes their receipt.
    pub fn recover_response(
        &self,
        ledger: &JobLedger,
        service: &TrustedService,
        task_key: &Blake3Hash,
        attempt: &AttemptRef,
    ) -> std::result::Result<DispatchOutcome, Box<DispatchFailure>> {
        let recovery = |error: WikiError, spool: Option<SpoolRef>| {
            let mut out = failure(error, Some(attempt.clone()));
            out.disposition = DispatchDisposition::OutcomeUnknown;
            out.spool = spool;
            out
        };
        let inspection = ledger.inspect().map_err(|e| recovery(e, None))?;
        let recorded = inspection
            .attempts
            .iter()
            .find(|a| &a.attempt == attempt)
            .ok_or_else(|| {
                recovery(
                    WikiError::new(ErrorCode::RecoveryRequired, "retained attempt missing"),
                    None,
                )
            })?;
        let spool = recorded.spool.clone().ok_or_else(|| {
            recovery(
                WikiError::new(ErrorCode::RecoveryRequired, "retained response missing"),
                None,
            )
        })?;
        match self.decode_retained_classified(
            ledger,
            service,
            task_key,
            DispatchPurpose::Task,
            attempt,
        ) {
            Ok(output) => {
                let materialization = if let Some(receipt_ref) = &recorded.receipt {
                    let receipt = jobs::checkpoint::receipt(&self.fs, receipt_ref)
                        .map_err(|e| recovery(e, Some(spool.clone())))?;
                    if receipt.output_disposition != OutputDisposition::Validated {
                        return Err(recovery(
                            WikiError::new(
                                ErrorCode::RecoveryRequired,
                                "retained receipt does not validate output",
                            ),
                            Some(spool),
                        ));
                    }
                    jobs::checkpoint::receipt_plan(
                        ledger,
                        attempt,
                        OutputDisposition::Validated,
                        recorded.outputs.clone(),
                        recorded.cache_outputs.clone(),
                        Vec::new(),
                    )
                } else {
                    ledger.materialization_plan(attempt)
                }
                .map_err(|e| recovery(e, Some(spool.clone())))?;
                Ok(DispatchOutcome {
                    attempt: attempt.clone(),
                    spool,
                    materialization,
                    output,
                })
            }
            Err(RetainedDecodeFailure::InvalidResponse(error)) => {
                if recorded.receipt.is_some() {
                    return Err(recovery(error, Some(spool)));
                }
                let mut out = failure(error, Some(attempt.clone()));
                out.disposition = DispatchDisposition::Rejected;
                out.spool = Some(spool);
                out.materialization = jobs::checkpoint::receipt_plan(
                    ledger,
                    attempt,
                    OutputDisposition::Rejected,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
                .ok();
                Err(out)
            }
            Err(RetainedDecodeFailure::Recovery(error)) => Err(recovery(error, Some(spool))),
        }
    }

    /// Prepare every selected candidate without network IO, reserve one stable
    /// prefix, then poll only its opaque one-attempt authorities. Results preserve
    /// reservation order even when responses finish in a different order.
    pub fn execute_ready_batch(
        &self,
        ledger: &JobLedger,
        work: &[ResearchDispatchWork<'_>],
        public_options: &super::public_fetch::PublicFetchOptions,
    ) -> Result<ResearchDispatchBatch> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "batch dispatch requires application thread",
            ));
        }
        let inspection = ledger.inspect()?;
        if inspection.research.is_none() {
            return Err(WikiError::invalid(
                "research batch requires research genesis",
            ));
        }
        let (fs, _, _, options) = ledger.dispatcher_bindings();
        if fs.root().path() != self.fs.root().path() {
            return Err(WikiError::new(
                ErrorCode::ProfileUntrusted,
                "dispatcher vault differs",
            ));
        }
        policy(&options, None, inspection.effective_deadline_utc_ms)?;
        let mut ordered: Vec<_> = work.iter().collect();
        for item in &ordered {
            if !inspection.tasks.contains_key(&item.task_key) {
                return Err(WikiError::new(
                    ErrorCode::RecordNotFound,
                    "batch task missing",
                ));
            }
        }
        ordered.sort_by(|a, b| {
            inspection.tasks[&a.task_key]
                .spec
                .priority
                .cmp(&inspection.tasks[&b.task_key].spec.priority)
                .then(a.task_key.cmp(&b.task_key))
        });
        if ordered.windows(2).any(|w| w[0].task_key == w[1].task_key) {
            return Err(WikiError::invalid("duplicate batch task"));
        }
        let mut prepared = Vec::with_capacity(ordered.len());
        let mut candidates = Vec::with_capacity(ordered.len());
        for item in ordered {
            let task = &inspection.tasks[&item.task_key].spec;
            let (bound, request) = if task.capability == Some(Capability::Fetch) {
                if item.service.is_some() || item.purpose != DispatchPurpose::Task {
                    return Err(WikiError::new(
                        ErrorCode::ProfileUntrusted,
                        "public task role differs",
                    ));
                }
                let request = super::public_fetch::prepare_fetch(&self.fs, ledger, &item.task_key)
                    .map_err(|e| e.error)?;
                (request.bound.clone(), PreparedResearch::Public(request))
            } else {
                let service = item.service.ok_or_else(|| {
                    WikiError::new(ErrorCode::ProfileUntrusted, "remote service missing")
                })?;
                let request = prepare_effective(&fs, &inspection, service, task, item.purpose)?;
                (
                    request.bound.clone(),
                    PreparedResearch::Remote(service, item.purpose, request),
                )
            };
            prepared.push((item.task_key.clone(), request));
            candidates.push((item.task_key.clone(), bound));
        }
        // This call is deliberately before creating/polling any send future.
        let wave = ledger.reserve_ready_batch(candidates)?;
        let concurrency = inspection.effective_limits.concurrency as usize;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| WikiError::new(ErrorCode::Internal, "dispatcher runtime unavailable"));
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                for reservation in wave.reservations {
                    let _ = ledger.release_not_sent(
                        reservation.attempt(),
                        NotSentObservation {
                            reason: NotSentReason::TransportNotEntered,
                        },
                    );
                }
                return Err(error);
            }
        };
        let mut futures = Vec::new();
        for ((key, request), reservation) in prepared.into_iter().zip(wave.reservations) {
            let future: std::pin::Pin<
                Box<dyn std::future::Future<Output = ResearchDispatchResult> + '_>,
            > = Box::pin(async move {
                let outcome = match request {
                    PreparedResearch::Remote(service, purpose, prepared) => {
                        ResearchDispatchOutcome::Remote(
                            self.execute_inner(
                                ledger,
                                service,
                                &key,
                                purpose,
                                Some((prepared, reservation)),
                            )
                            .await,
                        )
                    }
                    PreparedResearch::Public(prepared) => ResearchDispatchOutcome::Public(
                        super::public_fetch::send_reserved(
                            &self.fs,
                            ledger,
                            prepared,
                            reservation,
                            public_options,
                            &self.network_used,
                        )
                        .await,
                    ),
                };
                ResearchDispatchResult {
                    task_key: key,
                    outcome,
                }
            });
            futures.push(Some(future));
        }
        let mut outcomes: Vec<_> = (0..futures.len()).map(|_| None).collect();
        runtime.block_on(std::future::poll_fn(|cx| {
            let mut running = 0;
            let mut finished = true;
            for (index, future) in futures.iter_mut().enumerate() {
                if let Some(current) = future {
                    finished = false;
                    if running >= concurrency {
                        continue;
                    }
                    match current.as_mut().poll(cx) {
                        std::task::Poll::Ready(outcome) => {
                            outcomes[index] = Some(outcome);
                            *future = None;
                        }
                        std::task::Poll::Pending => running += 1,
                    }
                }
            }
            if finished || futures.iter().all(Option::is_none) {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        }));
        runtime.shutdown_timeout(Duration::from_millis(50));
        Ok(ResearchDispatchBatch {
            outcomes: outcomes.into_iter().flatten().collect(),
            stop: wave.stop,
        })
    }

    pub fn execute(
        &self,
        ledger: &JobLedger,
        service: &TrustedService,
        task: &Blake3Hash,
        purpose: DispatchPurpose,
    ) -> std::result::Result<DispatchOutcome, Box<DispatchFailure>> {
        if ledger
            .inspect()
            .map_err(|e| failure(e, None))?
            .research
            .is_some()
        {
            let batch = self
                .execute_ready_batch(
                    ledger,
                    &[ResearchDispatchWork {
                        task_key: task.clone(),
                        service: Some(service),
                        purpose,
                    }],
                    &super::public_fetch::PublicFetchOptions::default(),
                )
                .map_err(|e| failure(e, None))?;
            if let Some(result) = batch.outcomes.into_iter().next() {
                return match result.outcome {
                    ResearchDispatchOutcome::Remote(out) => out,
                    ResearchDispatchOutcome::Public(_) => unreachable!(),
                };
            }
            return Err(failure(
                batch
                    .stop
                    .unwrap_or_else(|| WikiError::invalid("research task was not reserved")),
                None,
            ));
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(failure(
                WikiError::new(
                    ErrorCode::Usage,
                    "synchronous dispatch requires an application thread",
                ),
                None,
            ));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                failure(
                    WikiError::new(ErrorCode::Internal, "dispatcher runtime unavailable"),
                    None,
                )
            })?;
        let result = runtime.block_on(self.execute_inner(ledger, service, task, purpose, None));
        // OS DNS work may be uncancellable; shutdown must never join it indefinitely.
        runtime.shutdown_timeout(Duration::from_millis(50));
        result
    }
    async fn execute_inner(
        &self,
        ledger: &JobLedger,
        service: &TrustedService,
        key: &Blake3Hash,
        purpose: DispatchPurpose,
        mut reserved: Option<(PreparedWire, Reservation)>,
    ) -> std::result::Result<DispatchOutcome, Box<DispatchFailure>> {
        let (fs, vault_id, _, options) = ledger.dispatcher_bindings();
        let mut retry_state = RetryState::default();
        let mut refresh_epoch = None;
        let mut retained = RetainedPaid::default();
        let result = async {
        if fs.root().path() != self.fs.root().path() {
            return Err(failure(
                WikiError::new(ErrorCode::ProfileUntrusted, "dispatcher vault differs"),
                None,
            ));
        }
        policy(&options, None, i64::MAX).map_err(|e| failure(e, None))?;

            loop {
                let inspection = ledger.inspect().map_err(|e| failure(e, None))?;
                let deadline = inspection.effective_deadline_utc_ms;
                policy(&options, None, deadline).map_err(|e| failure(e, None))?;
                let task = inspection
                    .tasks
                    .get(key)
                    .ok_or_else(|| {
                        failure(
                            WikiError::new(ErrorCode::RecordNotFound, "dispatch task missing"),
                            None,
                        )
                    })?
                    .spec
                    .clone();
                // A prior Received response cannot be silently skipped before its actual receipt is acknowledged.
                if inspection
                    .attempts
                    .iter()
                    .any(|a| a.attempt.task_key == *key && a.spool.is_some() && a.receipt.is_none())
                {
                    return Err(failure(
                        WikiError::new(
                            ErrorCode::RecoveryRequired,
                            "prior response receipt requires materialization",
                        ),
                        None,
                    ));
                }
                let research = inspection.research.as_ref();
                let prepared = prepare_effective(&fs, &inspection, service, &task, purpose)
                    .map_err(|e| failure(e, None))?;
                let bound = prepared.bound.clone();
                let now = policy(&options, None, deadline).map_err(|e| failure(e, None))?;
                jobs::budgets::quote_bound(
                    &bound,
                    &inspection.effective_limits,
                    now.utc_ms,
                    deadline,
                )
                .map_err(|e| failure(e, None))?;
                let reservation = if let Some((prior, reservation)) = reserved.take() {
                    if prior.bound != prepared.bound || prior.body != prepared.body || prior.url != prepared.url {
                        let _ = ledger.release_not_sent(reservation.attempt(), NotSentObservation { reason: NotSentReason::TransportNotEntered });
                        return Err(failure(WikiError::new(ErrorCode::FreshnessConflict, "reserved request changed"), Some(reservation.attempt().clone())));
                    }
                    reservation
                } else {
                    ledger.reserve(key, bound.clone()).map_err(|e| failure(e, None))?
                };
                let attempt = reservation.attempt().clone();
                let permit = match ledger.dispatch_intent(reservation) {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = ledger.release_not_sent(&attempt, NotSentObservation { reason: NotSentReason::TransportNotEntered });
                        return Err(failure(e, Some(attempt)));
                    }
                };
                let context = CredentialContext {
                    policy: options.policy,
                    cancel: options.cancel.clone(),
                    deadline_utc_ms: deadline,
                };
                let lease_result = if let Some(epoch) = refresh_epoch.take() {
                    self.options
                        .broker
                        .refresh_after_401(service, &fs, epoch, &context)
                } else {
                    self.options.broker.resolve(service, &fs, &context)
                };
                let lease = match lease_result {
                    Ok(value) => value,
                    Err(e) => {
                        let _ = ledger.release_not_sent(
                            &attempt,
                            NotSentObservation {
                                reason: NotSentReason::CredentialsUnavailable,
                            },
                        );
                        return Err(failure(e, Some(attempt)));
                    }
                };
                if let Err(e) = request_headers_bound(service, &lease) {
                    let _ = ledger.release_not_sent(
                        &attempt,
                        NotSentObservation {
                            reason: NotSentReason::TransportNotEntered,
                        },
                    );
                    return Err(failure(e, Some(attempt)));
                }
                let epoch = lease.epoch();
                let redactors = match redactors(service, &lease) {
                    Ok(redactors) => redactors,
                    Err(error) => {
                        let _ = ledger.release_not_sent(&attempt, NotSentObservation { reason: NotSentReason::TransportNotEntered });
                        return Err(failure(error, Some(attempt)));
                    }
                };
                // Trust/source proof is refreshed after helper IO, without a ledger/writer lock.
                if let Err(e) = prepare_effective(&fs, &inspection, service, &task, purpose).and_then(|fresh| {
                    if fresh.bound != bound
                        || fresh.body != prepared.body
                        || fresh.url != prepared.url
                        || crate::graph::packet::canonical_json(&fresh.input)?
                            != crate::graph::packet::canonical_json(&prepared.input)?
                    {
                        Err(WikiError::new(
                            ErrorCode::FreshnessConflict,
                            "prepared request changed",
                        ))
                    } else {
                        Ok(())
                    }
                }) {
                    let _ = ledger.release_not_sent(
                        &attempt,
                        NotSentObservation {
                            reason: NotSentReason::TransportNotEntered,
                        },
                    );
                    return Err(failure(e, Some(attempt)));
                }
                let mut authorization = match ledger.begin_send(permit) {
                    Ok(value) => value,
                    Err(e) => {
                        let _ = ledger.release_not_sent(&attempt, NotSentObservation { reason: NotSentReason::TransportNotEntered });
                        return Err(failure(e, Some(attempt)));
                    }
                };
                if let Err(e) = self
                    .options
                    .broker
                    .validate_lease(service, &fs, &lease, &context)
                {
                    let _ = ledger.release_not_sent(
                        &attempt,
                        NotSentObservation {
                            reason: NotSentReason::TransportNotEntered,
                        },
                    );
                    return Err(failure(e, Some(attempt)));
                }
                if let Err(error)=authorization.check_before_entry() {
                    let _=ledger.release_not_sent(&attempt,NotSentObservation { reason:NotSentReason::TransportNotEntered });
                    return Err(failure(error,Some(attempt)));
                }
                let connect_timeout_ms =
                    u64::from(service.service().connect_timeout_seconds.unwrap_or(10)) * 1000;
                let transport_context = TransportContext {
                    clock: options.clock.clone(),
                    cancel: options.cancel.clone(),
                    deadline_utc_ms: deadline,
                    timeout_ms: bound.timeout_ms,
                    connect_timeout_ms: connect_timeout_ms.min(bound.timeout_ms),
                    response_bytes: bound.response_bytes,
                };
                let request = AuthenticatedRequest {
                    authorization,
                    prepared: &prepared,
                    lease,
                    tls_ca: service.service().ca_bytes.clone(),
                    service,
                    fs: fs.clone(),
                    broker: self.options.broker.clone(),
                    credential_context: context,
                };
                retry_state.attempts = retry_state.attempts.checked_add(1).ok_or_else(|| {
                    failure(
                        WikiError::invalid("attempt counter overflow"),
                        Some(attempt.clone()),
                    )
                })?;
                let reply = match self
                    .options
                    .transport
                    .execute(request, transport_context)
                    .await
                {
                    Ok(reply) => {
                        self.network_used.store(true, std::sync::atomic::Ordering::Relaxed);
                        reply
                    },
                    Err(error) => {
                        if !error.not_entered || error.observed_body_bytes > 0 {
                            self.network_used.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        if error.not_entered
                            && error.observed_body_bytes == 0
                            && ledger
                                .release_not_sent(
                                    &attempt,
                                    NotSentObservation {
                                        reason: NotSentReason::TransportNotEntered,
                                    },
                                )
                                .is_ok()
                        {
                            return Err(failure(transport_error(error.code), Some(attempt)));
                        }
                        retained.attempt(&attempt, DispatchDisposition::OutcomeUnknown);
                        let _ = ledger.outcome_unknown(&attempt, "transport_outcome_unknown");
                        let mut out = failure(transport_error(error.code), Some(attempt.clone()));
                        out.disposition = DispatchDisposition::OutcomeUnknown;
                        // Preserve bytes actually observed even when no complete response exists.
                        if error.observed_body_bytes > 0 {
                            let response = ResponseSpoolInput {
                                bytes: Vec::new(),
                                metadata: ResponseMetadata {
            acquisition: None,
                                    provider_request_id: None,
                                    returned_model: None,
                                    status_code: None,
                                    terminal_response: false,
                                    usage: unknown_usage(&bound, error.observed_body_bytes),
                                    computed_cost: KnownOrUnknown::Unknown,
                                    failure_code: Some("incomplete_response".into()),
                                },
                            };
                            if let Ok(spool) = ledger.record_response(&attempt, response) {
                                out.spool = Some(spool);
                                out.materialization = ledger.materialization_plan(&attempt).ok();
                                retained.response(&out);
                            }
                        }
                        out.retry = if matches!(
                            error.code,
                            TransportFailureCode::Cancelled | TransportFailureCode::ClockRegression
                        ) {
                            RetryDecision::Never
                        } else {
                            retry::uncertain(options.policy.retry_uncertain)
                        };
                        if matches!(out.retry, RetryDecision::After { .. }) && (research.is_some() || error.observed_body_bytes == 0) {
                            if research.is_some() {
                                schedule_retry(ledger, &attempt, &mut out);
                                return Err(out);
                            }
                            continue;
                        }
                        let _ = pause_running(
                            ledger,
                            if error.code == TransportFailureCode::Cancelled {
                                StopReason::Cancelled
                            } else {
                                StopReason::OutcomeUnknown
                            },
                        );
                        return Err(out);
                    }
                };
                let status = reply.status;
                let valid_headers = headers_valid(&reply.headers) && !reply.headers_exceeded;
                let encoded = reply.headers.iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("content-encoding")
                        && !value.trim().eq_ignore_ascii_case("identity")
                });
                let oversized = reply.body_exceeded
                    || reply.observed_body_bytes > bound.response_bytes
                    || reply.body.len() as u64 > bound.response_bytes;
                let malformed_transport = !(100..600).contains(&status)
                    || !valid_headers
                    || encoded
                    || oversized
                    || reply.observed_body_bytes < reply.body.len() as u64;
                let observed = if malformed_transport {
                    ObservedUsage {
                        usage: unknown_usage(&bound, reply.observed_body_bytes),
                        computed_cost: KnownOrUnknown::Unknown,
                        provider_request_id: None,
                        returned_model: None,
                        contract_violation: None,
                    }
                } else {
                    observe(&prepared, &reply)
                };
                let wire_violation = observed.contract_violation.is_some();
                let provider_request_id =
                    clean_field(observed.provider_request_id, &redactors, 128);
                let returned_model = clean_field(observed.returned_model, &redactors, 256);
                let failure_code = if observed.contract_violation.is_some() {
                    Some("wire_contract_violation".into())
                } else if malformed_transport {
                    Some("response_bound".into())
                } else if !(200..300).contains(&status) {
                    Some(format!("http_{status}"))
                } else {
                    None
                };
                let body = if malformed_transport || !(200..300).contains(&status) {
                    Vec::new()
                } else {
                    reply.body.clone()
                };
                let metadata = ResponseMetadata {
            acquisition: None,
                    provider_request_id,
                    returned_model,
                    status_code: Some(status),
                    terminal_response: reply.terminal && !malformed_transport,
                    usage: observed.usage,
                    computed_cost: observed.computed_cost,
                    failure_code,
                };
                let spool = match ledger.record_response(
                    &attempt,
                    ResponseSpoolInput {
                        bytes: body,
                        metadata,
                    },
                ) {
                    Ok(value) => value,
                    Err(e) => {
                        let _ = ledger.outcome_unknown(&attempt, "response_persistence_failed");
                        let mut out = failure(e, Some(attempt));
                        out.disposition = DispatchDisposition::OutcomeUnknown;
                        return Err(out);
                    }
                };
                retained.attempt(&attempt, DispatchDisposition::Rejected);
                retained.spool = Some(spool.clone());
                let decoded = if malformed_transport || wire_violation {
                    Err(WikiError::new(
                        ErrorCode::ProviderResponse,
                        "response security or size bound failed",
                    ))
                } else if (200..300).contains(&status) {
                    decode(&prepared, &reply)
                } else {
                    Err(WikiError::new(
                        if matches!(status, 401 | 403) {
                            ErrorCode::ProviderAuth
                        } else if status == 429 {
                            ErrorCode::ProviderRateLimit
                        } else {
                            ErrorCode::ProviderResponse
                        },
                        "provider rejected request",
                    ))
                };
                if let Ok(output) = decoded {
                    let materialization = ledger.materialization_plan(&attempt).map_err(|e| {
                        let mut out = failure(e, Some(attempt.clone()));
                        out.disposition = DispatchDisposition::OutcomeUnknown;
                        out.spool = Some(spool.clone());
                        out
                    })?;
                    return Ok(DispatchOutcome {
                        attempt,
                        spool,
                        materialization,
                        output,
                    });
                }
                let error = decoded.err().unwrap();
                let mut out = failure(error, Some(attempt.clone()));
                out.disposition = DispatchDisposition::Rejected;
                out.spool = Some(spool);
                let plan = jobs::checkpoint::receipt_plan(
                    ledger,
                    &attempt,
                    OutputDisposition::Rejected,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
                .map_err(|e| {
                    let mut failure = failure(e, Some(attempt.clone()));
                    failure.disposition = DispatchDisposition::Rejected;
                    failure.spool = out.spool.clone();
                    failure
                })?;
                out.materialization = Some(plan);
                retained.response(&out);
                let mut retained_change = None;
                if let Err(e) = commit_receipt(
                    ledger,
                    &fs,
                    &vault_id,
                    &attempt,
                    out.materialization.as_ref().unwrap(),
                    options.lock_timeout_ms,
                    &mut retained_change,
                ) {
                    out.error = redacted(e);
                    if let Some(change) = retained_change {
                        out.error.details = serde_json::json!({"receipt_change_id": change.change_id, "receipt_manifest_hash": change.manifest_hash});
                    }
                    out.retry = RetryDecision::Never;
                    return Err(out);
                }
                if malformed_transport {
                    let _ = pause_running(ledger, StopReason::Budget);
                    return Err(out);
                }
                if research.is_none() && status == 401
                    && !retry_state.command_refresh_used
                    && matches!(&service.service().auth, AuthConfig::Command { .. })
                {
                    retry_state.command_refresh_used = true;
                    refresh_epoch = Some(epoch);
                    continue;
                }
                out.retry = retry::decide(
                    status,
                    &reply.headers,
                    if research.is_some() { attempt.number } else { retry_state.attempts },
                    options
                        .clock
                        .read()
                        .map_err(|e| failure(e, Some(attempt.clone())))?,
                    deadline,
                    bound.timeout_ms,
                    self.options.jitter.as_ref(),
                )
                .map_err(|e| failure(e, Some(attempt.clone())))?;
                if research.is_some() {
                    schedule_retry(ledger, &attempt, &mut out);
                    if matches!(out.retry, RetryDecision::Pause { .. }) {
                        let _ = pause_running(ledger, StopReason::Deadline);
                    }
                    return Err(out);
                }
                match &out.retry {
                    RetryDecision::After { delay_ms, .. } => {
                        if let Err(e) = wait(*delay_ms, &options, deadline).await {
                            let _ = pause_running(
                                ledger,
                                if e.code == ErrorCode::Cancelled {
                                    StopReason::Cancelled
                                } else {
                                    StopReason::Deadline
                                },
                            );
                            out.error = redacted(e);
                            out.retry = RetryDecision::Never;
                            return Err(out);
                        }
                    }
                    RetryDecision::Pause { .. } => {
                        let _ = pause_running(ledger, StopReason::Deadline);
                        return Err(out);
                    }
                    RetryDecision::Never => return Err(out),
                }
            }
        }
        .await;
        let mut result = result;
        if let Some((_, reservation)) = reserved {
            if let Err(out) = &mut result {
                out.attempt = Some(reservation.attempt().clone());
            }
            let _ = ledger.release_not_sent(
                reservation.attempt(),
                NotSentObservation {
                    reason: NotSentReason::TransportNotEntered,
                },
            );
        }
        result.map_err(|out| retained.finish(out))
    }
}
fn prepare_effective(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    service: &TrustedService,
    task: &TaskSpec,
    purpose: DispatchPurpose,
) -> Result<PreparedWire> {
    service.recheck(fs)?;
    let summary = service.summary();
    let valid = if let Some(research) = &inspection.research {
        research.active_tasks.contains(&task.key)
            && research.binding.config_fingerprint == summary.config_fingerprint
            && research.binding.services.iter().any(|binding| {
                binding.profile_id == summary.profile_id
                    && binding.capability == summary.capability
                    && binding.profile_fingerprint == summary.profile_fingerprint
                    && binding.endpoint_fingerprint == summary.endpoint_fingerprint
            })
    } else {
        inspection.spec.config_fingerprint == summary.config_fingerprint
            && inspection
                .spec
                .scope
                .profile_fingerprints
                .get(&summary.profile_id)
                == Some(&summary.profile_fingerprint)
    };
    if !valid {
        return Err(WikiError::new(
            ErrorCode::ProfileUntrusted,
            "run profile/config binding differs",
        ));
    }
    let mut prepared = prepare(fs, service, task, purpose)?;
    if inspection.research.is_some() {
        prepared.bound.profile_fingerprint = Some(summary.profile_fingerprint);
        prepared.bound.bounds_fingerprint = jobs::budgets::bound_fingerprint(&prepared.bound)?;
        super::history::retain(fs, task, &mut prepared)?;
    }
    Ok(prepared)
}
fn schedule_retry(ledger: &JobLedger, attempt: &AttemptRef, out: &mut DispatchFailure) {
    let RetryDecision::After { delay_ms, .. } = out.retry else {
        return;
    };
    let schedule = ledger.schedule_research_retry_after(&attempt.task_key, attempt, delay_ms);
    if let Err(error) = schedule {
        out.error = redacted(error);
        out.retry = RetryDecision::Never;
    }
}
#[derive(Default)]
struct RetainedPaid {
    attempts: Vec<AttemptRef>,
    disposition: Option<DispatchDisposition>,
    spool: Option<SpoolRef>,
    plan: Option<MaterializationPlan>,
}
impl RetainedPaid {
    fn attempt(&mut self, attempt: &AttemptRef, disposition: DispatchDisposition) {
        if !self.attempts.contains(attempt) {
            self.attempts.push(attempt.clone());
        }
        self.disposition = Some(disposition);
    }
    fn response(&mut self, out: &DispatchFailure) {
        if let Some(spool) = &out.spool {
            self.spool = Some(spool.clone());
        }
        if let Some(plan) = &out.materialization {
            self.plan = Some(MaterializationPlan {
                attempt: plan.attempt.clone(),
                receipt: plan.receipt.clone(),
                draft: plan.draft.clone(),
            });
        }
    }
    fn finish(self, mut out: Box<DispatchFailure>) -> Box<DispatchFailure> {
        if self.attempts.is_empty() {
            return out;
        }
        if out.attempt.is_none() {
            out.attempt = self.attempts.last().cloned();
            out.disposition = self
                .disposition
                .unwrap_or(DispatchDisposition::OutcomeUnknown);
        }
        if out.spool.is_none() {
            out.spool = self.spool;
        }
        if out.materialization.is_none() {
            out.materialization = self.plan;
        }
        // Only identities minted and verified by the ledger enter safe error details.
        if !out.error.details.is_object() {
            out.error.details = serde_json::json!({});
        }
        out.error.details["retained_paid_attempts"] =
            serde_json::to_value(self.attempts).unwrap_or(serde_json::Value::Null);
        out
    }
}
fn redacted(error: WikiError) -> WikiError {
    WikiError::new(error.code, "bounded provider operation failed")
}
fn failure(error: WikiError, attempt: Option<AttemptRef>) -> Box<DispatchFailure> {
    Box::new(DispatchFailure {
        error: redacted(error),
        disposition: DispatchDisposition::NotSent,
        attempt,
        spool: None,
        materialization: None,
        retry: RetryDecision::Never,
    })
}
fn transport_error(code: TransportFailureCode) -> WikiError {
    WikiError::new(
        match code {
            TransportFailureCode::Cancelled => ErrorCode::Cancelled,
            TransportFailureCode::Timeout => ErrorCode::ProviderUnavailable,
            TransportFailureCode::ClockRegression => ErrorCode::BudgetExceeded,
            _ => ErrorCode::ProviderUnavailable,
        },
        "transport outcome unknown",
    )
}
fn policy(
    options: &JobOptions,
    previous: Option<ClockReading>,
    deadline: i64,
) -> Result<ClockReading> {
    if options.policy.offline || options.policy.dry_run {
        return Err(WikiError::new(
            ErrorCode::OfflineUnavailable,
            "dispatch prohibited by execution policy",
        ));
    }
    if options.cancel.is_cancelled() {
        return Err(WikiError::new(ErrorCode::Cancelled, "dispatch cancelled"));
    }
    let now = options
        .clock
        .read()
        .map_err(|_| WikiError::new(ErrorCode::BudgetExceeded, "dispatch clock unavailable"))?;
    if now.utc_ms < 0
        || now.utc_ms > 253_402_300_799_999
        || previous
            .is_some_and(|old| now.utc_ms < old.utc_ms || now.monotonic_ms < old.monotonic_ms)
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "dispatch clock regressed",
        ));
    }
    if now.utc_ms >= deadline {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "dispatch deadline expired",
        ));
    }
    Ok(now)
}
async fn wait(delay_ms: u64, options: &JobOptions, deadline: i64) -> Result<()> {
    let first = policy(options, None, deadline)?;
    let wall = Instant::now();
    let mut last = first;
    loop {
        let now = policy(options, Some(last), deadline)?;
        last = now;
        if now.monotonic_ms - first.monotonic_ms >= delay_ms
            || wall.elapsed() >= Duration::from_millis(delay_ms)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn commit_receipt(
    ledger: &JobLedger,
    fs: &VaultFs,
    vault_id: &RecordId,
    attempt: &AttemptRef,
    plan: &MaterializationPlan,
    timeout: u64,
    retained_change: &mut Option<crate::changes::PreparedChange>,
) -> Result<()> {
    let op = plan
        .draft
        .operations
        .last()
        .ok_or_else(|| WikiError::invalid("receipt plan empty"))?;
    let bytes = op
        .proposed
        .as_ref()
        .ok_or_else(|| WikiError::invalid("receipt bytes missing"))?;
    let receipt = DurableOutputRef {
        record: RecordRef {
            vault_id: vault_id.clone(),
            record_id: plan.receipt.receipt_id.clone(),
            expected_kind: RecordKind::RunEvent,
        },
        path: op.target.clone(),
        hash: Blake3Hash::digest(bytes),
    };
    let writer = WriterPermit::acquire(fs.root(), Duration::from_millis(timeout))?;
    let engine = ChangeEngine::new(fs.clone())?;
    let catalog = Catalog::new(fs.clone(), vault_id.clone());
    let prepared = engine.prepare(&writer, plan.draft.clone())?.prepared;
    *retained_change = Some(prepared.clone());
    engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
    drop(writer);
    ledger.outputs_committed(attempt, &prepared, receipt, Vec::new(), Vec::new())?;
    ledger.settle(attempt)?;
    Ok(())
}
fn redactors(
    service: &TrustedService,
    lease: &super::credentials::CredentialLease,
) -> Result<Vec<SecretBytes>> {
    let prefix = match &service.service().auth {
        AuthConfig::Static { prefix, .. } | AuthConfig::Command { prefix, .. } => prefix,
    };
    let mut result = Vec::new();
    for (_, secret) in lease.headers() {
        result.push(SecretBytes::new(secret.as_bytes().to_vec())?);
        if let Ok(value) = std::str::from_utf8(secret.as_bytes())
            && let Some(token) = value.strip_prefix(prefix)
            && !token.is_empty()
        {
            result.push(SecretBytes::new(token.as_bytes().to_vec())?);
        }
    }
    Ok(result)
}
fn clean_field(value: Option<String>, redactors: &[SecretBytes], max: usize) -> Option<String> {
    value.filter(|value| {
        !value.is_empty()
            && value.len() <= max
            && !value.chars().any(char::is_control)
            && !redactors.iter().any(|secret| {
                std::str::from_utf8(secret.as_bytes())
                    .is_ok_and(|text| !text.is_empty() && value.contains(text))
            })
    })
}
fn unknown_usage(bound: &AttemptBound, observed: u64) -> KnownOrUnknown<Usage> {
    KnownOrUnknown::Known(Usage {
        request_bytes: bound.request_bytes,
        response_bytes: observed,
        billable_units: bound
            .applicable_classes
            .iter()
            .map(|c| (*c, KnownOrUnknown::Unknown))
            .collect(),
    })
}
fn prepare(
    fs: &VaultFs,
    service: &TrustedService,
    task: &TaskSpec,
    purpose: DispatchPurpose,
) -> Result<PreparedWire> {
    service.recheck(fs)?;
    for dep in &task.source_bindings {
        let actual = read_bounded(fs, &dep.path, crate::changes::prepare::MAX_PAYLOAD_BYTES)?
            .map_or(ExpectedState::Absent, |b| {
                ExpectedState::Hash(Blake3Hash::digest(b))
            });
        if actual != dep.expected {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "task source changed",
            ));
        }
    }
    prepare_descriptor(fs, service, task, purpose)
}
fn prepare_descriptor(
    fs: &VaultFs,
    service: &TrustedService,
    task: &TaskSpec,
    purpose: DispatchPurpose,
) -> Result<PreparedWire> {
    if task.key != jobs::tasks::task_key(task)? {
        return Err(WikiError::invalid("task identity differs"));
    }
    let bytes = read_bounded(fs, &task.input.path, 256 * 1024)?
        .ok_or_else(|| WikiError::new(ErrorCode::FreshnessConflict, "task input missing"))?;
    if bytes.len() as u64 != task.input.byte_len || Blake3Hash::digest(&bytes) != task.input.hash {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "task input changed",
        ));
    }
    let value = super::wire_json::parse(&bytes, 256 * 1024, 8192, 40)?;
    let input: RemoteInput =
        serde_json::from_value(value).map_err(|_| WikiError::invalid("invalid remote input"))?;
    #[cfg(not(test))]
    if crate::graph::packet::canonical_json(&input)? != bytes {
        return Err(WikiError::invalid(
            "remote input descriptor must use canonical JSON",
        ));
    }
    if input.version != 1 {
        return Err(WikiError::invalid("unsupported remote input"));
    }
    let role = match input.operation {
        RemoteOperation::Embed { .. } => ServiceRole::Embed,
        RemoteOperation::Generate { .. } => ServiceRole::Generate,
        RemoteOperation::Search { .. } => ServiceRole::Search,
        RemoteOperation::Fetch { .. } => {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "public fetch uses execute_public",
            ));
        }
    };
    let capability = match purpose {
        DispatchPurpose::Task => role.capability(),
        DispatchPurpose::Probe { role: r } if r == role => Capability::Probe,
        _ => {
            return Err(WikiError::new(
                ErrorCode::ProfileUntrusted,
                "probe role differs",
            ));
        }
    };
    if task.capability != Some(capability) || service.summary().capability != role.capability() {
        return Err(WikiError::new(
            ErrorCode::ProfileUntrusted,
            "task capability differs",
        ));
    }
    #[cfg(not(test))]
    {
        match role {
            ServiceRole::Embed => super::embedding_wire::prepare(service, task, &input, purpose),
            ServiceRole::Generate => {
                super::generation_wire::prepare(service, task, &input, purpose)
            }
            ServiceRole::Search => super::search_wire::prepare(service, task, &input, purpose),
        }
    }
    #[cfg(test)]
    {
        if role == ServiceRole::Search {
            return super::search_wire::prepare(service, task, &input, purpose);
        }
        let fingerprints = super::wire::task_fingerprints(service, &input)?;
        if task.input_hash == fingerprints.input
            && task.prompt_hash == fingerprints.prompt
            && task.schema_hash == fingerprints.schema
            && task.model_hash.as_ref() == Some(&fingerprints.model)
            && task.settings_hash == fingerprints.settings
        {
            if crate::graph::packet::canonical_json(&input)? != bytes {
                return Err(WikiError::invalid(
                    "remote input descriptor must use canonical JSON",
                ));
            }
            return match role {
                ServiceRole::Embed => {
                    super::embedding_wire::prepare(service, task, &input, purpose)
                }
                ServiceRole::Generate => {
                    super::generation_wire::prepare(service, task, &input, purpose)
                }
                ServiceRole::Search => unreachable!("handled above"),
            };
        }
        fixture_prepare(service, task, input, role, purpose, capability)
    }
}
#[cfg(test)]
pub(super) fn input_fingerprint(input: &RemoteInput) -> Blake3Hash {
    Blake3Hash::digest(serde_json::to_vec(input).unwrap())
}
#[cfg(test)]
pub(super) fn settings_fingerprint(service: &TrustedService) -> Blake3Hash {
    Blake3Hash::digest(service.summary().profile_fingerprint.as_str().as_bytes())
}
#[cfg(test)]
pub(super) fn fixture_prepare(
    service: &TrustedService,
    task: &TaskSpec,
    input: RemoteInput,
    role: ServiceRole,
    purpose: DispatchPurpose,
    capability: Capability,
) -> Result<PreparedWire> {
    let model_fp = Blake3Hash::digest(
        serde_json::to_vec(&(
            service.service().model.clone(),
            service.service().revision.clone(),
        ))
        .unwrap(),
    );
    let (schema, prompt) = match &input.operation {
        RemoteOperation::Generate {
            instructions,
            data,
            output_schema,
            ..
        } => (
            Some(Blake3Hash::digest(
                serde_json::to_vec(output_schema).unwrap(),
            )),
            Some(Blake3Hash::digest(
                serde_json::to_vec(&(instructions, data)).unwrap(),
            )),
        ),
        _ => (None, None),
    };
    if task.input_hash != input_fingerprint(&input)
        || task.settings_hash != settings_fingerprint(service)
        || task.model_hash != Some(model_fp)
        || task.schema_hash != schema
        || task.prompt_hash != prompt
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "task wire/settings proof differs",
        ));
    }
    let body=match &input.operation {
        RemoteOperation::Embed {inputs,..}=>{
            if inputs.is_empty()||inputs.len()>usize::from(service.service().max_batch_items.unwrap_or(32))||inputs.iter().any(|i|Blake3Hash::digest(i.utf8.as_bytes())!=i.input_hash) {return Err(WikiError::invalid("embedding inputs invalid"));}
            serde_json::to_vec(&serde_json::json!({"model":service.service().model,"input":inputs.iter().map(|i|&i.utf8).collect::<Vec<_>>(),"encoding_format":"float"})).unwrap()
        }
        RemoteOperation::Generate {instructions,data,max_output_tokens,..}=>serde_json::to_vec(&serde_json::json!({"model":service.service().model,"messages":[{"role":"system","content":instructions},{"role":"user","content":data}],"stream":false,"n":1,"max_completion_tokens":max_output_tokens})).unwrap(),
        RemoteOperation::Search { .. } | RemoteOperation::Fetch { .. } => {
            return Err(WikiError::new(ErrorCode::Usage, "fixture encoder only supports model operations"));
        }
    };
    if body.len() as u64 > service.service().max_batch_bytes.unwrap_or(256 * 1024) {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "request body exceeds cap",
        ));
    }
    let summary = service.summary();
    let headers = BTreeMap::from([("Content-Type".into(), "application/json".into())]);
    let mut all_headers = service.service().headers.clone();
    all_headers.extend(headers);
    let wire_hash = Blake3Hash::digest(
        serde_json::to_vec(&(
            "lwiki.wire.v1",
            "POST",
            &service.service().url,
            &all_headers,
            &body,
            &summary.profile_fingerprint,
        ))
        .unwrap(),
    );
    let applicable_classes = match role {
        ServiceRole::Embed => BTreeSet::from([BillableClass::Input, BillableClass::CachedInput]),
        _ => BTreeSet::from([
            BillableClass::Input,
            BillableClass::CachedInput,
            BillableClass::Output,
            BillableClass::Reasoning,
        ]),
    };
    let mut bound = AttemptBound {
        codec: None,
        profile_fingerprint: None,
        capability,
        profile_id: summary.profile_id,
        endpoint_fingerprint: summary.endpoint_fingerprint,
        config_fingerprint: summary.config_fingerprint,
        input_hash: task.input_hash.clone(),
        wire_hash,
        requested_model: service.service().model.clone(),
        requested_model_revision: service.service().revision.clone(),
        request_bytes: body.len() as u64,
        response_bytes: if role == ServiceRole::Generate {
            1024 * 1024
        } else {
            8 * 1024 * 1024
        },
        timeout_ms: u64::from(service.service().timeout_seconds.unwrap_or(
            if role == ServiceRole::Generate {
                120
            } else {
                60
            },
        )) * 1000,
        billable_bounds: applicable_classes
            .iter()
            .map(|c| (*c, TokenBound::Unknown))
            .collect(),
        applicable_classes,
        rate_card: service.service().rate_card.clone(),
        quoted_allowance: None,
        bounds_fingerprint: Blake3Hash::digest([]),
    };
    bound.bounds_fingerprint = jobs::budgets::bound_fingerprint(&bound)?;
    Ok(PreparedWire {
        role,
        purpose,
        method: "POST".into(),
        url: service.service().url.clone(),
        headers: all_headers,
        body,
        bound,
        input,
        contract: WireContract::Fixture,
    })
}
fn observe(prepared: &PreparedWire, reply: &TransportReply) -> ObservedUsage {
    match &prepared.contract {
        WireContract::Embedding(_) => super::embedding_wire::observe(prepared, reply),
        WireContract::Generation(_) => super::generation_wire::observe(prepared, reply),
        WireContract::Search(_) => super::search_wire::observe(prepared, reply),
        #[cfg(test)]
        WireContract::Fixture => fixture_observe(&prepared.bound, reply),
    }
}
#[cfg(test)]
fn fixture_observe(bound: &AttemptBound, reply: &TransportReply) -> ObservedUsage {
    #[cfg(test)]
    {
        let value: serde_json::Value =
            serde_json::from_slice(&reply.body).unwrap_or(serde_json::Value::Null);
        let mut usage = match unknown_usage(bound, reply.observed_body_bytes) {
            KnownOrUnknown::Known(v) => v,
            _ => unreachable!(),
        };
        for (class, key) in [
            (BillableClass::Input, "input_tokens"),
            (BillableClass::CachedInput, "cached_input_tokens"),
            (BillableClass::Output, "output_tokens"),
            (BillableClass::Reasoning, "reasoning_tokens"),
        ] {
            if let Some(n) = value
                .get("usage")
                .and_then(|u| u.get(key))
                .and_then(|v| v.as_u64())
            {
                usage.billable_units.insert(class, KnownOrUnknown::Known(n));
            }
        }
        let cost = bound
            .rate_card
            .as_ref()
            .and_then(|card| {
                let mut n = card.request_fee_nanounits;
                for class in &bound.applicable_classes {
                    let KnownOrUnknown::Known(units) = usage.billable_units.get(class)? else {
                        return None;
                    };
                    n = n.checked_add(card.rates.get(class)?.allowance_nanounits(*units).ok()?)?;
                }
                Some(Money::new(card.currency.clone(), n))
            })
            .map_or(KnownOrUnknown::Unknown, KnownOrUnknown::Known);
        ObservedUsage {
            usage: KnownOrUnknown::Known(usage),
            computed_cost: cost,
            provider_request_id: value.get("id").and_then(|v| v.as_str()).map(str::to_owned),
            returned_model: value
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            contract_violation: None,
        }
    }
}
fn decode(prepared: &PreparedWire, reply: &TransportReply) -> Result<ValidatedOutput> {
    let result = match &prepared.contract {
        WireContract::Embedding(_) => super::embedding_wire::decode(prepared, reply),
        WireContract::Generation(_) => super::generation_wire::decode(prepared, reply),
        WireContract::Search(_) => super::search_wire::decode(prepared, reply),
        #[cfg(test)]
        WireContract::Fixture => fixture_decode(prepared.purpose, &reply.body),
    };
    result.map_err(|_| {
        WikiError::new(
            ErrorCode::ProviderResponse,
            "provider output violates sealed wire contract",
        )
    })
}
#[cfg(test)]
fn fixture_decode(purpose: DispatchPurpose, body: &[u8]) -> Result<ValidatedOutput> {
    #[cfg(test)]
    {
        let value: serde_json::Value = serde_json::from_slice(body)
            .map_err(|_| WikiError::new(ErrorCode::ProviderResponse, "invalid response JSON"))?;
        if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return Err(WikiError::new(
                ErrorCode::ProviderResponse,
                "invalid fixture output",
            ));
        }
        Ok(match purpose {
            DispatchPurpose::Probe { role } => ValidatedOutput::Probe { role },
            _ => ValidatedOutput::Embeddings {
                vectors: vec![vec![1.0]],
                returned_model: None,
            },
        })
    }
}

fn request_headers_bound(
    service: &TrustedService,
    lease: &super::credentials::CredentialLease,
) -> Result<()> {
    let headers = &service.service().headers;
    let mut count = 4usize;
    let mut total = service.service().url.len() + 128;
    for (name, value) in headers {
        count = count
            .checked_add(1)
            .ok_or_else(|| WikiError::invalid("request headers overflow"))?;
        total = total
            .checked_add(name.len())
            .and_then(|n| n.checked_add(value.len()))
            .ok_or_else(|| WikiError::invalid("request headers overflow"))?;
        if name.len() > 128 || value.len() > 16384 {
            return Err(WikiError::new(
                ErrorCode::ProviderAuth,
                "request header exceeds bound",
            ));
        }
    }
    for (name, value) in lease.headers() {
        count = count
            .checked_add(1)
            .ok_or_else(|| WikiError::invalid("request headers overflow"))?;
        total = total
            .checked_add(name.len())
            .and_then(|n| n.checked_add(value.as_bytes().len()))
            .ok_or_else(|| WikiError::invalid("request headers overflow"))?;
        if name.len() > 128 || value.as_bytes().len() > 16384 {
            return Err(WikiError::new(
                ErrorCode::ProviderAuth,
                "request header exceeds bound",
            ));
        }
    }
    if count > 64 || total > 65536 {
        return Err(WikiError::new(
            ErrorCode::ProviderAuth,
            "request headers exceed aggregate bound",
        ));
    }
    Ok(())
}

fn pause_running(ledger: &JobLedger, reason: StopReason) -> Result<()> {
    if ledger.inspect()?.state == RunState::Running {
        if reason == StopReason::Cancelled {
            ledger.stop()?;
        } else {
            ledger.pause(reason)?;
        }
    }
    Ok(())
}

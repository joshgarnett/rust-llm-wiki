//! Short-lock durable admission. No lock spans canonical application or transport.
use super::{budgets, checkpoint, events, replay, tasks, types::*};
use crate::vault::operational::{RunFile, RunLedgerGuard, RunStore, SpoolPart};
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{ChangeDraft, ChangeEngine, ChangeStatus, PreparedChange},
    domain::*,
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    version: u32,
    run_id: RecordId,
    spec_hash: Blake3Hash,
    genesis_hash: Blake3Hash,
    last_event: EventRef,
    journal_length: u64,
}
pub(super) struct Loaded {
    pub state: replay::State,
    pub frames: Vec<JournalFrame>,
    pub length: u64,
    pub head: ExpectedState,
    pub torn_tail: bool,
    pub physical_length: u64,
}
pub(super) fn read(fs: &VaultFs, path: &VaultRelativePath) -> Result<Option<Vec<u8>>> {
    crate::changes::prepare::read_bounded(fs, path, crate::changes::prepare::MAX_PAYLOAD_BYTES)
}
fn fail(code: ErrorCode, message: &str) -> WikiError {
    WikiError::new(code, message)
}
fn json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    crate::changes::prepare::strict_json(bytes)
        .map_err(|_| events::corrupt("invalid operational JSON"))
}
fn bytes(value: &impl Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| WikiError::invalid("job data encoding failed"))
}
fn reference(
    vault: &RecordId,
    run: &RecordId,
    path: VaultRelativePath,
    hash: Blake3Hash,
    kind: RecordKind,
) -> DurableOutputRef {
    DurableOutputRef {
        record: RecordRef {
            vault_id: vault.clone(),
            record_id: run.clone(),
            expected_kind: kind,
        },
        path,
        hash,
    }
}
fn attempt<'a>(loaded: &'a Loaded, r: &AttemptRef) -> Result<&'a AttemptInspection> {
    loaded
        .state
        .inspection
        .attempts
        .iter()
        .find(|a| &a.attempt == r)
        .ok_or_else(|| events::corrupt("attempt identity differs from stored history"))
}
impl JobLedger {
    fn io_store(&self) -> Result<RunStore> {
        RunStore::open_existing(&self.fs, &self.vault_id, &self.run_id)
    }
    fn fault(&self, point: LedgerCheckpoint) -> Result<()> {
        if let Some(f) = &self.options.fault {
            f.check(point)?
        }
        Ok(())
    }
    fn local_write(&self) -> Result<()> {
        if self.options.policy.dry_run {
            return Err(fail(ErrorCode::Usage, "dry run cannot modify job state"));
        }
        Ok(())
    }
    fn paid_gate(&self) -> Result<()> {
        self.local_write()?;
        if self.options.policy.offline {
            return Err(fail(
                ErrorCode::OfflineUnavailable,
                "offline policy blocks remote attempt admission",
            ));
        }
        if self.options.cancel.is_cancelled() {
            return Err(fail(ErrorCode::Cancelled, "job cancelled before dispatch"));
        }
        Ok(())
    }
    pub(super) fn with<T>(
        &self,
        bind: bool,
        f: impl FnOnce(&RunLedgerGuard<'_>, &mut Loaded) -> Result<T>,
    ) -> Result<T> {
        let store = self.io_store()?;
        let first = self.options.clock.read()?;
        let mut check = || {
            let current = self.options.clock.read()?;
            if current.monotonic_ms < first.monotonic_ms {
                return Err(fail(
                    ErrorCode::BudgetExceeded,
                    "monotonic clock regressed while waiting for run lock",
                ));
            }
            Ok(())
        };
        let guard = store.lock(
            Duration::from_millis(self.options.lock_timeout_ms),
            &mut check,
        )?;
        let mut loaded = self.load(&guard, bind)?;
        f(&guard, &mut loaded).map_err(|mut e| {
            let old = std::mem::replace(&mut e.details, serde_json::Value::Null);
            e.details = serde_json::json!({"run_id":self.run_id,"context":old});
            e
        })
    }
    fn load(&self, g: &RunLedgerGuard<'_>, bind: bool) -> Result<Loaded> {
        let before = g.read_journal()?.ok_or_else(|| {
            events::corrupt("operational journal missing; old lifetime budget cannot resume")
        })?;
        let decoded = events::decode(&before.bytes, &self.run_id)?;
        if decoded.frames.is_empty() {
            return Err(events::corrupt("complete operational genesis missing"));
        }
        let head = g
            .read_checkpoint(METADATA_MAX_BYTES as u64)?
            .ok_or_else(|| {
                events::corrupt("durable history head missing; old budget cannot resume")
            })?;
        let h: Head = json(&head.bytes)?;
        let genesis = &decoded.frames[0];
        if h.version != 1 || h.run_id != self.run_id || h.genesis_hash != genesis.checksum {
            return Err(events::corrupt("durable history head/genesis differs"));
        }
        let end = usize::try_from(h.last_event.sequence)
            .ok()
            .and_then(|n| decoded.frames.get(n))
            .ok_or_else(|| events::corrupt("history suffix missing beneath durable head"))?;
        let anchored_len = decoded
            .frames
            .iter()
            .take(h.last_event.sequence as usize + 1)
            .try_fold(0u64, |sum, f| {
                sum.checked_add(events::encode(&f.event)?.1.len() as u64)
                    .ok_or_else(|| events::corrupt("head length overflow"))
            })?;
        if events::event_ref(end) != h.last_event
            || h.journal_length != anchored_len
            || h.journal_length > decoded.safe_offset
        {
            return Err(events::corrupt(
                "history head no longer matches complete prefix",
            ));
        }
        let mut state = replay::replay(&decoded.frames, decoded.safe_offset)?;
        if state.inspection.spec_hash != h.spec_hash
            || state.inspection.spec.vault_id != self.vault_id
        {
            return Err(events::corrupt("genesis vault/spec binding differs"));
        }
        for frame in &decoded.frames {
            if let EventPayload::Checkpoint {
                completed_tasks,
                run_note,
                ..
            } = &frame.event.payload
                && let Err(error) = checkpoint::verify_summary_proof(
                    &self.fs,
                    run_note,
                    completed_tasks,
                    &decoded.frames,
                )
            {
                if bind {
                    return Err(error);
                }
                state.inspection.warnings.push(format!("canonical checkpoint proof unavailable; operational accounting only, no canonical authority: {}", error.message));
            }
        }
        for a in &mut state.inspection.attempts {
            if let Some(spool) = &a.spool {
                let metadata =
                    g.read_spool(&a.attempt, SpoolPart::Metadata, METADATA_MAX_BYTES as u64)?;
                if let Some(metadata) = metadata {
                    if metadata.hash != spool.metadata.hash
                        || metadata.bytes.len() as u64 != spool.metadata.byte_len
                    {
                        return Err(events::corrupt("response metadata hash differs"));
                    }
                    let m: ResponseMetadata = json(&metadata.bytes)?;
                    validate_metadata(&m)?;
                    if m.terminal_response {
                        a.remote_exposure = RemoteExposure::TerminalConfirmed
                    }
                    state.received_meta.insert(a.attempt.attempt_id.clone(), m);
                } else {
                    state
                        .inspection
                        .warnings
                        .push("response metadata absent; no response authority inferred".into())
                }
            }
        }
        enrich_observations(&mut state, &decoded.frames)?;
        replay::recount(&mut state)?;
        if bind {
            let run = state
                .inspection
                .run_note
                .as_ref()
                .ok_or_else(|| events::corrupt("run-note binding missing"))?;
            checkpoint::output(&self.fs, run)?;
        }
        Ok(Loaded {
            state,
            frames: decoded.frames,
            length: decoded.safe_offset,
            head: ExpectedState::Hash(head.hash),
            torn_tail: decoded.torn_tail,
            physical_length: before.bytes.len() as u64,
        })
    }
    fn sync_head(&self, g: &RunLedgerGuard<'_>, loaded: &mut Loaded) -> Result<()> {
        let last = loaded
            .frames
            .last()
            .ok_or_else(|| events::corrupt("genesis missing"))?;
        let h = Head {
            version: 1,
            run_id: self.run_id.clone(),
            spec_hash: loaded.state.inspection.spec_hash.clone(),
            genesis_hash: loaded.frames[0].checksum.clone(),
            last_event: events::event_ref(last),
            journal_length: loaded.length,
        };
        let hash = g.secure_replace(RunFile::Checkpoint, loaded.head.clone(), &bytes(&h)?)?;
        loaded.head = ExpectedState::Hash(hash);
        Ok(())
    }
    fn append(
        &self,
        g: &RunLedgerGuard<'_>,
        loaded: &mut Loaded,
        payload: EventPayload,
    ) -> Result<EventRef> {
        self.local_write()?;
        if loaded.torn_tail {
            g.truncate_torn_tail(loaded.physical_length, loaded.length)?;
            loaded.torn_tail = false;
            loaded.physical_length = loaded.length;
        }
        let now = self.options.clock.read()?.utc_ms;
        let event = events::new_event(
            &self.run_id,
            loaded.state.inspection.last_event.as_ref(),
            now,
            payload,
        )?;
        let (frame, encoded) = events::encode(&event)?;
        let mut proposed = loaded.frames.clone();
        proposed.push(frame.clone());
        let new_len = loaded
            .length
            .checked_add(encoded.len() as u64)
            .ok_or_else(|| events::corrupt("journal length overflow"))?;
        let mut next = replay::replay(&proposed, new_len)?;
        next.received_meta = loaded.state.received_meta.clone();
        for a in &mut next.inspection.attempts {
            if next
                .received_meta
                .get(&a.attempt.attempt_id)
                .is_some_and(|m| m.terminal_response)
            {
                a.remote_exposure = RemoteExposure::TerminalConfirmed
            }
        }
        enrich_observations(&mut next, &proposed)?;
        replay::recount(&mut next)?;
        let b = &next.inspection.budget;
        if !events::history_capacity(
            new_len,
            proposed.len() as u64,
            &b.persistence_reserved,
            next.inspection.state,
            b.guarantee_intact,
            JOURNAL_MAX_BYTES,
            JOURNAL_MAX_EVENTS as u64,
        ) {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "complete-history capacity reserved for pending receipts",
            ));
        }
        self.fault(LedgerCheckpoint::BeforeAppend)?;
        let length = g.append_journal(loaded.length, &encoded)?;
        self.fault(LedgerCheckpoint::AfterAppend)?;
        self.fault(LedgerCheckpoint::AfterJournalSync)?;
        loaded.length = length;
        loaded.physical_length = length;
        loaded.frames = proposed;
        loaded.state = next;
        self.sync_head(g, loaded)?;
        Ok(events::event_ref(&frame))
    }
    fn active(&self, g: &RunLedgerGuard<'_>, loaded: &mut Loaded) -> Result<i64> {
        self.paid_gate()?;
        self.refresh_observations(g, loaded)?;
        self.repair_committed_tasks(g, loaded)?;
        let now = self.options.clock.read()?.utc_ms;
        let i = &loaded.state.inspection;
        if now < i.utc_high_water_ms {
            if i.state == RunState::Running {
                self.append(
                    g,
                    loaded,
                    EventPayload::RunTransition {
                        from: RunState::Running,
                        to: RunState::Paused,
                        reason: StopReason::ClockRegression,
                    },
                )?;
            }
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "UTC clock regressed; explicit reconciliation required",
            ));
        }
        if now >= i.effective_deadline_utc_ms {
            if i.state == RunState::Running {
                self.append(
                    g,
                    loaded,
                    EventPayload::RunTransition {
                        from: RunState::Running,
                        to: RunState::Paused,
                        reason: StopReason::Deadline,
                    },
                )?;
            }
            return Err(fail(ErrorCode::BudgetExceeded, "run deadline expired"));
        }
        if i.state != RunState::Running || !i.budget.guarantee_intact {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "run is not admitting attempts",
            ));
        }
        self.bind_inputs(i)?;
        Ok(now)
    }
    /// Flushes can cross a deadline or observe cancellation. Durable intent is
    /// retained even when no caller receives the corresponding authority.
    fn authority_gate(
        &self,
        g: &RunLedgerGuard<'_>,
        loaded: &mut Loaded,
        bound: &AttemptBound,
        before: ClockReading,
    ) -> Result<()> {
        let now = self.options.clock.read()?;
        let i = &loaded.state.inspection;
        let rejected = if self.options.cancel.is_cancelled() {
            Some((
                RunState::Stopped,
                StopReason::Cancelled,
                fail(
                    ErrorCode::Cancelled,
                    "job cancelled during durable authority recording",
                ),
            ))
        } else if now.monotonic_ms < before.monotonic_ms || now.utc_ms < i.utc_high_water_ms {
            Some((
                RunState::Paused,
                StopReason::ClockRegression,
                fail(
                    ErrorCode::BudgetExceeded,
                    "clock regressed during durable authority recording",
                ),
            ))
        } else if now.utc_ms >= i.effective_deadline_utc_ms {
            Some((
                RunState::Paused,
                StopReason::Deadline,
                fail(
                    ErrorCode::BudgetExceeded,
                    "run deadline expired during durable authority recording",
                ),
            ))
        } else {
            budgets::quote_bound(
                bound,
                &i.effective_limits,
                now.utc_ms,
                i.effective_deadline_utc_ms,
            )
            .err()
            .map(|error| (RunState::Paused, StopReason::ReconciliationRequired, error))
        };
        if let Some((to, reason, error)) = rejected {
            if i.state == RunState::Running {
                self.append(
                    g,
                    loaded,
                    EventPayload::RunTransition {
                        from: RunState::Running,
                        to,
                        reason,
                    },
                )?;
            }
            return Err(error);
        }
        if i.state != RunState::Running || !i.budget.guarantee_intact {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "durable attempt cannot return dispatch authority",
            ));
        }
        Ok(())
    }
    fn bind_inputs(&self, i: &LedgerInspection) -> Result<()> {
        for dep in &i.spec.scope.read_preconditions {
            let actual = read(&self.fs, &dep.path)?.map_or(ExpectedState::Absent, |b| {
                ExpectedState::Hash(Blake3Hash::digest(b))
            });
            if actual != dep.expected {
                return Err(fail(
                    ErrorCode::FreshnessConflict,
                    "run source/config binding changed",
                ));
            }
        }
        for task in i.tasks.values() {
            tasks::bind(&self.fs, &task.spec)?;
            for output in &task.outputs {
                checkpoint::output(&self.fs, output)?;
                if output.record.vault_id != self.vault_id {
                    return Err(events::corrupt("task output belongs to another vault"));
                }
            }
        }
        Ok(())
    }
    fn admission(
        &self,
        loaded: &Loaded,
        key: &Blake3Hash,
        bound: &AttemptBound,
        now: i64,
    ) -> Result<Allowance> {
        let i = &loaded.state.inspection;
        let task = i
            .tasks
            .get(key)
            .ok_or_else(|| WikiError::invalid("unknown task key"))?;
        let previous = i
            .attempts
            .iter()
            .filter(|a| &a.attempt.task_key == key)
            .collect::<Vec<_>>();
        let uncertain = previous
            .iter()
            .any(|a| a.billing == BillingDisposition::UnknownReserved);
        let retry_ready = task.state == TaskState::Running
            && !previous.is_empty()
            && previous.iter().all(|a| {
                a.phase == AttemptPhase::Settled || a.billing == BillingDisposition::UnknownReserved
            })
            && task.outputs.is_empty();
        if task.state != TaskState::Pending && !retry_ready {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "task is not ready for another attempt",
            ));
        }
        if uncertain && !self.options.policy.retry_uncertain {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "uncertain retry requires explicit retry policy and new reservation",
            ));
        }
        if task
            .spec
            .dependencies
            .iter()
            .any(|d| i.tasks[d].state != TaskState::Completed)
            || task.spec.capability != Some(bound.capability)
            || task.spec.input_hash != bound.input_hash
            || bound.config_fingerprint != i.spec.config_fingerprint
            || !i
                .spec
                .scope
                .profile_fingerprints
                .contains_key(&bound.profile_id)
        {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "attempt does not match ready task/config bindings",
            ));
        }
        if previous.len() >= i.effective_limits.attempts_per_task as usize
            || i.budget.concurrency_admitted >= i.effective_limits.concurrency
        {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "attempt/concurrency ceiling reached",
            ));
        }
        let allowance =
            budgets::quote_bound(bound, &i.effective_limits, now, i.effective_deadline_utc_ms)?;
        budgets::fits(
            &i.budget.settled,
            &i.budget.outstanding,
            &allowance,
            &i.effective_limits,
        )?;
        self.rate_check(loaded, &allowance, now)?;
        Ok(allowance)
    }
    fn rate_check(&self, loaded: &Loaded, proposed: &Allowance, now: i64) -> Result<()> {
        let i = &loaded.state.inspection;
        let cutoff = now.saturating_sub(60_000);
        let mut requests = 0u64;
        let mut units = 0u64;
        for f in &loaded.frames {
            if f.event.occurred_at_utc_ms > cutoff
                && let EventPayload::Reserved { allowance, .. } = &f.event.payload
            {
                requests = requests
                    .checked_add(allowance.requests)
                    .ok_or_else(|| events::corrupt("rate count overflow"))?;
                for (class, n) in &allowance.billable_units {
                    if matches!(
                        class,
                        BillableClass::Input
                            | BillableClass::CachedInput
                            | BillableClass::Output
                            | BillableClass::Reasoning
                    ) {
                        units = units
                            .checked_add(*n)
                            .ok_or_else(|| events::corrupt("rate token overflow"))?
                    }
                }
            }
        }
        if i.effective_limits
            .requests_per_minute
            .is_some_and(|n| requests.saturating_add(proposed.requests) > u64::from(n))
        {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "run requests-per-minute ceiling reached",
            ));
        }
        for (class, n) in &proposed.billable_units {
            if matches!(
                class,
                BillableClass::Input
                    | BillableClass::CachedInput
                    | BillableClass::Output
                    | BillableClass::Reasoning
            ) {
                units = units
                    .checked_add(*n)
                    .ok_or_else(|| events::corrupt("rate token overflow"))?
            }
        }
        if i.effective_limits
            .tokens_per_minute
            .is_some_and(|n| units > n)
        {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "run tokens-per-minute ceiling reached",
            ));
        }
        Ok(())
    }
}
fn validate_metadata(m: &ResponseMetadata) -> Result<()> {
    for value in [&m.provider_request_id, &m.returned_model, &m.failure_code]
        .into_iter()
        .flatten()
    {
        if value.len() > 256 || value.chars().any(char::is_control) {
            return Err(WikiError::invalid(
                "response metadata must be bounded safe identifiers",
            ));
        }
    }
    if m.status_code.is_some_and(|s| !(100..=599).contains(&s)) {
        return Err(WikiError::invalid("invalid response status"));
    }
    Ok(())
}
impl JobLedgerApi for JobLedger {
    fn new(fs: VaultFs, vault_id: RecordId, run_id: RecordId, options: JobOptions) -> Result<Self> {
        let engine = ChangeEngine::new(fs.clone())?;
        if engine.vault_id() != &vault_id || options.lock_timeout_ms > 30_000 {
            return Err(WikiError::invalid(
                "ledger vault binding or lock timeout invalid",
            ));
        }
        Ok(Self {
            fs,
            vault_id,
            run_id,
            options,
        })
    }
    fn bootstrap_plan(fs: &VaultFs, spec: &RunSpec) -> Result<ChangeDraft> {
        validate_spec(fs, spec)?;
        let path = checkpoint::run_path(&spec.run_id)?;
        if read(fs, &path)?.is_some() {
            return Err(fail(
                ErrorCode::ContentConflict,
                "run already exists; initialization never overwrites it",
            ));
        }
        let i = initial_inspection(spec)?;
        let mut d = checkpoint::draft("Create durable planned run", spec);
        d.read_preconditions = spec.scope.read_preconditions.clone();
        d.operations.push(checkpoint::write(
            path,
            ExpectedState::Absent,
            checkpoint::run_bytes(&i)?,
        ));
        Ok(d)
    }
    fn create(&self, writer: &WriterPermit, spec: RunSpec) -> Result<LedgerInspection> {
        self.local_write()?;
        writer.require_root(self.fs.root())?;
        if spec.run_id != self.run_id || spec.vault_id != self.vault_id {
            return Err(WikiError::invalid(
                "creation spec belongs to another ledger",
            ));
        }
        validate_spec(&self.fs, &spec)?;
        self.check_prior(&spec)?;
        let run_path = checkpoint::run_path(&self.run_id)?;
        let private = VaultRelativePath::new(format!(".wiki/state/jobs/{}", self.run_id))?;
        let namespace = self.fs.root().resolve(&private)?;
        let existing = namespace.exists();
        let canonical = read(&self.fs, &run_path)?;
        if !existing && canonical.is_some() {
            return Err(events::corrupt(
                "Markdown-only run cannot recreate old accounting history",
            ));
        }
        let store = if existing {
            let journal = read(
                &self.fs,
                &VaultRelativePath::new(format!("{}/journal.bin", private))?,
            )?;
            if journal.is_none() && canonical.is_some() {
                return Err(events::corrupt(
                    "existing run journal missing; no budget reset",
                ));
            }
            if journal.is_none() {
                RunStore::bootstrap(&self.fs, writer, &self.vault_id, &self.run_id)?
            } else {
                self.io_store()?
            }
        } else {
            RunStore::bootstrap(&self.fs, writer, &self.vault_id, &self.run_id)?
        };
        let mut check = || Ok(());
        let g = store.lock(
            Duration::from_millis(self.options.lock_timeout_ms),
            &mut check,
        )?;
        let journal = g
            .read_journal()?
            .ok_or_else(|| events::corrupt("bootstrap journal missing"))?;
        let loaded = if journal.bytes.is_empty() {
            if canonical.is_some() || g.read_checkpoint(METADATA_MAX_BYTES as u64)?.is_some() {
                return Err(events::corrupt(
                    "existing history cannot be replaced by new genesis",
                ));
            }
            let i = initial_inspection(&spec)?;
            let run_bytes = checkpoint::run_bytes(&i)?;
            let run_note = reference(
                &self.vault_id,
                &self.run_id,
                run_path.clone(),
                Blake3Hash::digest(&run_bytes),
                RecordKind::Run,
            );
            let event = events::new_event(
                &self.run_id,
                None,
                spec.created_at_utc_ms,
                EventPayload::Genesis {
                    spec: spec.clone(),
                    spec_hash: events::spec_hash(&spec)?,
                    run_note,
                },
            )?;
            let (frame, encoded) = events::encode(&event)?;
            self.fault(LedgerCheckpoint::BeforeAppend)?;
            let length = g.append_journal(0, &encoded)?;
            self.fault(LedgerCheckpoint::AfterJournalSync)?;
            let state = replay::replay(std::slice::from_ref(&frame), length)?;
            let mut l = Loaded {
                state,
                frames: vec![frame],
                length,
                head: ExpectedState::Absent,
                torn_tail: false,
                physical_length: length,
            };
            self.sync_head(&g, &mut l)?;
            l
        } else {
            self.load_pristine(&g, &spec)?
        };
        if loaded.state.inspection.spec != spec {
            return Err(fail(
                ErrorCode::ContentConflict,
                "existing run genesis/spec differs",
            ));
        }
        if !loaded.state.inspection.attempts.is_empty() {
            return Err(fail(
                ErrorCode::ContentConflict,
                "existing run is not bootstrap-retryable",
            ));
        }
        let run_bytes = checkpoint::run_bytes(&initial_inspection(&spec)?)?;
        let mut d = checkpoint::draft("Create durable planned run", &spec);
        d.read_preconditions = spec.scope.read_preconditions.clone();
        d.operations.push(checkpoint::write(
            run_path,
            ExpectedState::Absent,
            run_bytes,
        ));
        let frame = &loaded.frames[0];
        d.operations.push(checkpoint::write(
            checkpoint::event_path(&self.run_id, &frame.event.event_id)?,
            ExpectedState::Absent,
            checkpoint::event_bytes(&frame.event)?,
        ));
        drop(g);
        let engine = ChangeEngine::new(self.fs.clone())?;
        let prepared = find_or_prepare(&engine, writer, &d)?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        engine.apply(writer,&prepared,&CatalogGraphValidator,&catalog).map_err(|mut e|{e.details=serde_json::json!({"run_id":self.run_id,"change_id":prepared.change_id,"manifest_hash":prepared.manifest_hash,"context":e.details});e})?;
        loaded
            .state
            .inspection
            .run_note
            .as_ref()
            .map(|r| checkpoint::output(&self.fs, r))
            .transpose()?;
        Ok(loaded.state.inspection)
    }
    fn inspect(&self) -> Result<LedgerInspection> {
        match self.with(true, |_, l| Ok(l.state.inspection.clone())) {
            Ok(i) => Ok(i),
            Err(e)
                if matches!(
                    e.code,
                    ErrorCode::FreshnessConflict | ErrorCode::ContentConflict
                ) =>
            {
                self.with(false, |_, l| {
                    let mut i = l.state.inspection.clone();
                    i.warnings.push(format!(
                        "canonical run binding conflicts; no new authority: {}",
                        e.message
                    ));
                    Ok(i)
                })
            }
            Err(e) if matches!(e.code, ErrorCode::RecoveryRequired | ErrorCode::Internal) => {
                match self.with(false, |_, l| {
                    let mut i = l.state.inspection.clone();
                    i.warnings.push(format!(
                        "canonical binding unavailable; operational accounting only: {}",
                        e.message
                    ));
                    Ok(i)
                }) {
                    Ok(i) => Ok(i),
                    Err(_) => self.incomplete(&e.message),
                }
            }
            Err(e) => Err(e),
        }
    }
    fn replay(&self) -> Result<ReplayReport> {
        let result=self.with(false, |g, l| {
            let canonical_conflict = match self.recover_checkpoint(g,l) {
                Ok(()) => None,
                Err(e) if matches!(e.code, ErrorCode::ContentConflict | ErrorCode::FreshnessConflict) => Some(e.message),
                Err(e) => return Err(e),
            };
            let mut report = ReplayReport {
                inspection: l.state.inspection.clone(),
                orphan_spools: vec![],
                conflicting_paths: vec![],
                missing_cache_outputs: vec![],
                reusable_outputs: vec![],
                warnings: vec![],
            };
            if let Some(conflict) = canonical_conflict {
                report.conflicting_paths.push(checkpoint::run_path(&self.run_id)?);
                report.warnings.push(format!("canonical run binding conflicts; retained accounting preserved: {conflict}"));
            }
            let mut check = || Ok(());
            for r in g.enumerate_spool_ids(RUN_MAX_TASKS * 16, &mut check)? {
                let body = g.read_spool(&r, SpoolPart::Body, OTHER_SPOOL_MAX_BYTES)?;
                let metadata = g.read_spool(&r, SpoolPart::Metadata, METADATA_MAX_BYTES as u64)?;
                if let (Some(body), Some(metadata)) = (body, metadata) {
                    let a = attempt(l, &r)?;
                    if body.bytes.len() as u64 > a.bound.response_bytes {
                        return Err(events::corrupt("orphan response exceeds admission bound"));
                    }
                    let spool = spool_ref(
                        &r,
                        body.hash,
                        body.bytes.len() as u64,
                        metadata.hash,
                        metadata.bytes.len() as u64,
                    )?;
                    let m: ResponseMetadata = json(&metadata.bytes)?;
                    validate_metadata(&m)?;
                    if a.spool.is_none() && a.phase == AttemptPhase::DispatchIntent {
                        report.orphan_spools.push(spool.clone());
                        if !self.options.policy.dry_run {
                            l.state
                                .received_meta
                                .insert(r.attempt_id.clone(), m.clone());
                            self.append(g, l, EventPayload::Received { spool })?;
                        }
                    }
                }
            }
            if !self.options.policy.dry_run{self.refresh_observations(g,l)?;self.recover_committed_receipts(g,l)?;self.sync_head(g,l)?;}
            let fresh=self.bind_inputs(&l.state.inspection).is_ok();
            if !fresh{report.warnings.push("source/task/config inputs changed; retained outputs are stale and not reusable".into());}
            for a in &l.state.inspection.attempts {
                if let Some(receipt) = &a.receipt
                    && checkpoint::output(&self.fs, receipt).is_err()
                {
                    report.conflicting_paths.push(receipt.path.clone())
                }
                for output in &a.outputs {
                    if checkpoint::output(&self.fs, output).is_ok() && fresh {
                        report.reusable_outputs.push(output.clone())
                    } else {
                        report.conflicting_paths.push(output.path.clone())
                    }
                }
                report.missing_cache_outputs.extend(a.cache_outputs.clone());
            }
            for task in l.state.inspection.tasks.values() {
                for output in &task.outputs {
                    if checkpoint::output(&self.fs, output).is_ok() && fresh {
                        report.reusable_outputs.push(output.clone())
                    } else {
                        report.conflicting_paths.push(output.path.clone())
                    }
                }
                report
                    .missing_cache_outputs
                    .extend(task.cache_outputs.clone());
            }
            report.inspection = l.state.inspection.clone();
            report.reusable_outputs.sort_by(|a, b| a.path.cmp(&b.path));
            report.reusable_outputs.dedup();
            report
                .missing_cache_outputs
                .sort_by(|a, b| a.vector_hash.cmp(&b.vector_hash));
            report.missing_cache_outputs.dedup();
            if !report.missing_cache_outputs.is_empty() {
                report.warnings.push(
                    "vector cache references are unverified/missing; no regeneration authorized"
                        .into(),
                )
            }
            Ok(report)
        });
        match result {
            Ok(r) => Ok(r),
            Err(e) if matches!(e.code, ErrorCode::RecoveryRequired | ErrorCode::Internal) => {
                Ok(ReplayReport {
                    inspection: self.incomplete(&e.message)?,
                    orphan_spools: vec![],
                    conflicting_paths: vec![],
                    missing_cache_outputs: vec![],
                    reusable_outputs: vec![],
                    warnings: vec!["incomplete accounting cannot resume".into()],
                })
            }
            Err(e) => Err(e),
        }
    }
    fn add_tasks(&self, new: Vec<TaskSpec>) -> Result<EventRef> {
        self.local_write()?;
        self.with(true, |g, l| {
            if !matches!(
                l.state.inspection.state,
                RunState::Running | RunState::Planned
            ) {
                return Err(WikiError::invalid(
                    "stopped/paused run cannot add new tasks",
                ));
            }
            for t in &new {
                tasks::bind(&self.fs, t)?;
            }
            self.append(g, l, EventPayload::TasksAdded { tasks: new })
        })
    }
    fn start(&self) -> Result<EventRef> {
        self.local_write()?;
        self.with(true, |g, l| {
            self.control_gate(l)?;
            self.bind_inputs(&l.state.inspection)?;
            self.append(
                g,
                l,
                EventPayload::RunTransition {
                    from: RunState::Planned,
                    to: RunState::Running,
                    reason: StopReason::Started,
                },
            )
        })
    }
    fn pause(&self, reason: StopReason) -> Result<EventRef> {
        self.local_write()?;
        self.with(true, |g, l| {
            self.append(
                g,
                l,
                EventPayload::RunTransition {
                    from: l.state.inspection.state,
                    to: RunState::Paused,
                    reason,
                },
            )
        })
    }
    fn stop(&self) -> Result<EventRef> {
        self.local_write()?;
        self.with(true, |g, l| {
            self.append(
                g,
                l,
                EventPayload::RunTransition {
                    from: l.state.inspection.state,
                    to: RunState::Stopped,
                    reason: StopReason::Cancelled,
                },
            )
        })
    }
    fn resume(&self, amendment: Option<LimitAmendment>) -> Result<EventRef> {
        self.local_write()?;
        self.with(true, |g, l| {
            if !matches!(
                l.state.inspection.state,
                RunState::Paused | RunState::Stopped
            ) {
                return Err(WikiError::invalid(
                    "only paused/stopped runs may explicitly resume",
                ));
            }
            self.bind_inputs(&l.state.inspection)?;
            if let Some(amendment) = amendment {
                validate_amendment(l, &amendment, self.options.clock.read()?.utc_ms)?;
                self.append(g, l, EventPayload::Amendment { amendment })?;
            }
            self.control_gate(l)?;
            if l.state.inspection.budget.remote_inflight > 0 {
                return Err(fail(
                    ErrorCode::RecoveryRequired,
                    "possible remote requests require reconciliation before resume",
                ));
            }
            self.append(
                g,
                l,
                EventPayload::RunTransition {
                    from: l.state.inspection.state,
                    to: RunState::Running,
                    reason: StopReason::Started,
                },
            )
        })
    }
    fn ready_tasks(&self) -> Result<Vec<TaskSpec>> {
        self.with(true, |_, l| {
            self.bind_inputs(&l.state.inspection)?;
            Ok(tasks::ready(&l.state.inspection.tasks))
        })
    }
    fn finish_local_task(
        &self,
        key: &Blake3Hash,
        outputs: Vec<DurableOutputRef>,
        cache_outputs: Vec<VectorCacheRef>,
    ) -> Result<EventRef> {
        self.local_write()?;
        self.with(true, |g, l| {
            let task = l
                .state
                .inspection
                .tasks
                .get(key)
                .ok_or_else(|| WikiError::invalid("unknown local task"))?;
            if !matches!(
                l.state.inspection.state,
                RunState::Running | RunState::Paused | RunState::Stopped
            ) || task.spec.capability.is_some()
                || !tasks::ready(&l.state.inspection.tasks)
                    .iter()
                    .any(|t| &t.key == key)
            {
                return Err(WikiError::invalid("local task is not ready"));
            }
            if !cache_outputs.is_empty() {
                return Err(fail(
                    ErrorCode::CapabilityUnavailable,
                    "no P15 vector adapter verifies cache-only completion",
                ));
            }
            tasks::bind(&self.fs, &task.spec)?;
            for r in &outputs {
                if r.record.vault_id != self.vault_id {
                    return Err(WikiError::invalid("output from another vault"));
                }
                checkpoint::output(&self.fs, r)?;
            }
            self.append(
                g,
                l,
                EventPayload::TaskFinished {
                    task_key: key.clone(),
                    state: TaskState::Completed,
                    outputs,
                    cache_outputs,
                    reason: None,
                },
            )
        })
    }
    fn check_bound(&self, key: &Blake3Hash, bound: &AttemptBound) -> Result<Allowance> {
        self.with(true, |_, l| {
            self.admission(l, key, bound, self.options.clock.read()?.utc_ms)
        })
    }
    fn reserve(&self, key: &Blake3Hash, bound: AttemptBound) -> Result<Reservation> {
        self.paid_gate()?;
        self.with(true, |g, l| {
            let now = self.active(g, l)?;
            let allowance = self.admission(l, key, &bound, now)?;
            let attempt = AttemptRef {
                run_id: self.run_id.clone(),
                task_key: key.clone(),
                attempt_id: RecordId::new(format!("attempt_{}", uuid::Uuid::now_v7()))?,
                number: l
                    .state
                    .inspection
                    .attempts
                    .iter()
                    .filter(|a| &a.attempt.task_key == key)
                    .count() as u32
                    + 1,
                request_hash: bound.wire_hash.clone(),
            };
            let persistence = events::persistence();
            let before_flush = self.options.clock.read()?;
            let reserved_event = self.append(
                g,
                l,
                EventPayload::Reserved {
                    attempt: attempt.clone(),
                    bound: bound.clone(),
                    allowance: allowance.clone(),
                    persistence: persistence.clone(),
                },
            )?;
            self.authority_gate(g, l, &bound, before_flush)?;
            Ok(Reservation {
                attempt,
                bound,
                allowance,
                reserved_event,
                persistence,
                genesis_hash: l.frames[0].checksum.clone(),
            })
        })
    }
    fn dispatch_intent(&self, r: Reservation) -> Result<DispatchPermit> {
        self.paid_gate()?;
        self.with(true, |g, l| {
            let now = self.active(g, l)?;
            let a = attempt(l, &r.attempt)?;
            if a.phase != AttemptPhase::Reserved
                || a.bound != r.bound
                || a.allowance != r.allowance
                || a.persistence != r.persistence
                || l.frames[0].checksum != r.genesis_hash
                || !l
                    .frames
                    .iter()
                    .any(|f| events::event_ref(f) == r.reserved_event)
            {
                return Err(events::corrupt("reservation authority binding differs"));
            }
            budgets::quote_bound(
                &r.bound,
                &l.state.inspection.effective_limits,
                now,
                l.state.inspection.effective_deadline_utc_ms,
            )?;
            let before_flush = self.options.clock.read()?;
            let intent_event = self.append(
                g,
                l,
                EventPayload::DispatchIntent {
                    attempt: r.attempt.clone(),
                },
            )?;
            self.authority_gate(g, l, &r.bound, before_flush)?;
            Ok(DispatchPermit {
                attempt: r.attempt,
                bound: r.bound,
                intent_event,
                genesis_hash: r.genesis_hash,
            })
        })
    }
    fn materialization_plan(&self, r: &AttemptRef) -> Result<MaterializationPlan> {
        self.with(true, |_, l| {
            checkpoint::receipt_plan_locked(
                self,
                l,
                r,
                OutputDisposition::Unknown,
                vec![],
                vec![],
                vec![],
            )
        })
    }
    fn outputs_committed(
        &self,
        r: &AttemptRef,
        change: &PreparedChange,
        receipt: DurableOutputRef,
        outputs: Vec<DurableOutputRef>,
        cache_outputs: Vec<VectorCacheRef>,
    ) -> Result<EventRef> {
        self.local_write()?;
        self.with(true, |g, l| {
            self.ack_outputs(g, l, r, change, receipt, outputs, cache_outputs)
        })
    }
    fn checkpoint_plan(&self) -> Result<ChangeDraft> {
        self.with(true, |_, l| checkpoint::checkpoint_plan(&self.fs, l))
    }
    fn checkpoint_committed(&self, change: &PreparedChange) -> Result<EventRef> {
        self.local_write()?;
        self.with(false, |g, l| self.ack_checkpoint(g, l, change))
    }
}
fn initial_inspection(spec: &RunSpec) -> Result<LedgerInspection> {
    let path = checkpoint::run_path(&spec.run_id)?;
    let placeholder = reference(
        &spec.vault_id,
        &spec.run_id,
        path,
        Blake3Hash::digest([]),
        RecordKind::Run,
    );
    let event = events::new_event(
        &spec.run_id,
        None,
        spec.created_at_utc_ms,
        EventPayload::Genesis {
            spec: spec.clone(),
            spec_hash: events::spec_hash(spec)?,
            run_note: placeholder,
        },
    )?;
    let (frame, _) = events::encode(&event)?;
    let mut i = replay::replay(&[frame], 0)?.inspection;
    i.last_event = None;
    i.run_note = None;
    i.budget.journal_events_used = 0;
    Ok(i)
}
fn validate_spec(fs: &VaultFs, spec: &RunSpec) -> Result<()> {
    let engine = ChangeEngine::new(fs.clone())?;
    if spec.version != 1
        || &spec.vault_id != engine.vault_id()
        || spec.title.is_empty()
        || spec.title.len() > 16_384
        || spec.scope.operation.is_empty()
        || spec.scope.operation.len() > 128
        || spec.created_at_utc_ms < 0
        || spec.deadline_utc_ms <= spec.created_at_utc_ms
        || spec.deadline_utc_ms > 253_402_300_799_999
        || spec.input_fingerprint != tasks::input_fingerprint(spec)?
    {
        return Err(WikiError::invalid(
            "invalid immutable run spec/input fingerprint",
        ));
    }
    budgets::validate_limits(&spec.limits)?;
    let tasks = tasks::initial(spec.tasks.clone())?;
    for task in tasks.values() {
        tasks::bind(fs, &task.spec)?;
        if task.spec.input_hash != task.spec.input.hash {
            return Err(WikiError::invalid(
                "task input hash must bind complete descriptor bytes",
            ));
        }
        if task.spec.capability.is_some() && spec.scope.profile_fingerprints.is_empty() {
            return Err(WikiError::invalid(
                "remote tasks require declared profile identities",
            ));
        }
    }
    if spec.scope.read_preconditions.len() > crate::changes::prepare::MAX_OPS {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "run dependency ceiling exceeded",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for dep in &spec.scope.read_preconditions {
        if !seen.insert(&dep.path) {
            return Err(WikiError::invalid("duplicate run read precondition"));
        }
        let actual = read(fs, &dep.path)?.map_or(ExpectedState::Absent, |b| {
            ExpectedState::Hash(Blake3Hash::digest(b))
        });
        if actual != dep.expected {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "initial run binding changed",
            ));
        }
    }
    for r in &spec.scope.input_records {
        if r.vault_id != spec.vault_id {
            return Err(WikiError::invalid("cross-vault input reference"));
        }
        let matches = spec
            .scope
            .read_preconditions
            .iter()
            .filter_map(|d| {
                read(fs, &d.path)
                    .ok()
                    .flatten()
                    .and_then(|b| crate::records::parse_note(&b).canonical)
                    .filter(|c| c.id() == &r.record_id && c.kind() == r.expected_kind)
            })
            .count();
        if matches != 1 {
            return Err(WikiError::invalid(
                "input record must have one exact guarded canonical binding",
            ));
        }
    }
    Ok(())
}
fn existing_ref(l: &Loaded, p: impl Fn(&EventPayload) -> bool) -> Result<EventRef> {
    l.frames
        .iter()
        .rev()
        .find(|f| p(&f.event.payload))
        .map(events::event_ref)
        .ok_or_else(|| events::corrupt("durable transition reference missing"))
}
fn spool_ref(
    r: &AttemptRef,
    response_hash: Blake3Hash,
    response_len: u64,
    metadata_hash: Blake3Hash,
    metadata_len: u64,
) -> Result<SpoolRef> {
    Ok(SpoolRef {
        attempt: r.clone(),
        response: BoundedPayloadRef {
            path: VaultRelativePath::new(format!(
                ".wiki/state/requests/{}/response.bin",
                r.attempt_id
            ))?,
            hash: response_hash,
            byte_len: response_len,
        },
        metadata: BoundedPayloadRef {
            path: VaultRelativePath::new(format!(
                ".wiki/state/requests/{}/metadata.json",
                r.attempt_id
            ))?,
            hash: metadata_hash,
            byte_len: metadata_len,
        },
    })
}
fn validate_amendment(l: &Loaded, a: &LimitAmendment, now: i64) -> Result<()> {
    budgets::validate_limits(&a.limits)?;
    let prior = &l.state.inspection.effective_limits;
    if a.requested_at_utc_ms != now
        || a.reason.is_empty()
        || a.reason.len() > 256
        || a.deadline_utc_ms <= now
        || a.deadline_utc_ms < l.state.inspection.effective_deadline_utc_ms
        || a.limits.requests < prior.requests
        || a.limits.concurrency < prior.concurrency
        || a.limits.attempts_per_task < prior.attempts_per_task
    {
        return Err(WikiError::invalid(
            "amendment must explicitly preserve/raise lifetime limits and deadline",
        ));
    }
    for (old, new) in [
        (prior.request_bytes, a.limits.request_bytes),
        (prior.response_bytes, a.limits.response_bytes),
    ] {
        if old.is_none() && new.is_some() || old.zip(new).is_some_and(|(old, new)| new < old) {
            return Err(WikiError::invalid("amendment cannot lower lifetime bounds"));
        }
    }
    if a.limits
        .billable_units
        .keys()
        .any(|key| !prior.billable_units.contains_key(key))
    {
        return Err(WikiError::invalid(
            "new hard class ceilings cannot prove previously unbounded usage",
        ));
    }
    for (old, new) in [
        (
            prior.requests_per_minute.map(u64::from),
            a.limits.requests_per_minute.map(u64::from),
        ),
        (prior.tokens_per_minute, a.limits.tokens_per_minute),
    ] {
        if old.is_none() && new.is_some() || old.zip(new).is_some_and(|(old, new)| new < old) {
            return Err(WikiError::invalid(
                "amendment cannot add/lower historical rate ceilings",
            ));
        }
    }
    for (class, old) in &prior.billable_units {
        if a.limits
            .billable_units
            .get(class)
            .is_some_and(|new| new < old)
        {
            return Err(WikiError::invalid("amendment cannot lower class ceiling"));
        }
    }
    if prior.max_cost.is_none() && a.limits.max_cost.is_some()
        || prior
            .max_cost
            .as_ref()
            .zip(a.limits.max_cost.as_ref())
            .is_some_and(|(old, new)| {
                old.currency() != new.currency() || new.nanounits() < old.nanounits()
            })
    {
        return Err(WikiError::invalid(
            "amendment cannot lower/change cost ceiling",
        ));
    }
    budgets::fits(
        &l.state.inspection.budget.settled,
        &l.state.inspection.budget.outstanding,
        &budgets::zero(),
        &a.limits,
    )
}
fn find_or_prepare(
    engine: &ChangeEngine,
    writer: &WriterPermit,
    draft: &ChangeDraft,
) -> Result<PreparedChange> {
    for id in engine.change_ids()? {
        let i = engine.inspect(&id)?;
        if i.manifest.allocated_ids.get("run") != draft.allocated_ids.get("run")
            || i.manifest.operations.len() != draft.operations.len()
            || i.manifest.title != draft.title
            || matches!(i.status, ChangeStatus::Aborted | ChangeStatus::Conflict)
        {
            continue;
        }
        let same = draft.operations.iter().all(|op| {
            i.manifest.operations.iter().any(|existing| {
                existing.target == op.target
                    && existing.before == op.expected
                    && existing.after
                        == op.proposed.as_ref().map_or(ExpectedState::Absent, |b| {
                            ExpectedState::Hash(Blake3Hash::digest(b))
                        })
            })
        });
        if same {
            for op in &i.manifest.operations {
                if let Some(payload) = &op.after_payload {
                    let bytes = read(engine.fs(), &payload.path)?
                        .ok_or_else(|| events::corrupt("retained canonical payload missing"))?;
                    if Blake3Hash::digest(&bytes) != payload.hash {
                        return Err(events::corrupt("retained canonical payload corrupt"));
                    }
                }
            }
            return Ok(i.prepared);
        }
    }
    Ok(engine.prepare(writer, draft.clone())?.prepared)
}
impl JobLedger {
    fn load_pristine(&self, g: &RunLedgerGuard<'_>, spec: &RunSpec) -> Result<Loaded> {
        if g.read_checkpoint(METADATA_MAX_BYTES as u64)?.is_some() {
            return self.load(g, false);
        }
        let before = g
            .read_journal()?
            .ok_or_else(|| events::corrupt("genesis retry journal missing"))?;
        let decoded = events::decode(&before.bytes, &self.run_id)?;
        if decoded.frames.len() != 1
            || !matches!(&decoded.frames[0].event.payload,EventPayload::Genesis{spec:existing,..}if existing==spec)
            || read(&self.fs, &checkpoint::run_path(&self.run_id)?)?.is_some()
        {
            return Err(events::corrupt(
                "missing head cannot reconstruct non-pristine old authority",
            ));
        }
        if decoded.torn_tail {
            g.truncate_torn_tail(before.bytes.len() as u64, decoded.safe_offset)?;
        }
        let state = replay::replay(&decoded.frames, decoded.safe_offset)?;
        let mut l = Loaded {
            state,
            frames: decoded.frames,
            length: decoded.safe_offset,
            head: ExpectedState::Absent,
            torn_tail: false,
            physical_length: decoded.safe_offset,
        };
        self.sync_head(g, &mut l)?;
        Ok(l)
    }
    fn incomplete(&self, reason: &str) -> Result<LedgerInspection> {
        let canonical = read(&self.fs, &checkpoint::run_path(&self.run_id)?)?
            .ok_or_else(|| events::corrupt("run and complete accounting unavailable"))?;
        let plan = checkpoint::decode_run(&canonical)?;
        if plan.version != 1
            || plan.spec.run_id != self.run_id
            || plan.spec.vault_id != self.vault_id
            || plan.spec_hash != events::spec_hash(&plan.spec)?
        {
            return Err(events::corrupt("restored canonical run binding invalid"));
        }
        let mut i = initial_inspection(&plan.spec)?;
        i.effective_limits = plan.effective_limits;
        i.effective_deadline_utc_ms = plan.effective_deadline_utc_ms;
        i.tasks = plan.tasks;
        i.state = RunState::Paused;
        i.last_event = None;
        i.run_note = Some(reference(
            &self.vault_id,
            &self.run_id,
            checkpoint::run_path(&self.run_id)?,
            Blake3Hash::digest(&canonical),
            RecordKind::Run,
        ));
        i.budget.guarantee_intact = false;
        let path = VaultRelativePath::new(format!(".wiki/state/jobs/{}/journal.bin", self.run_id))?;
        i.completeness = if read(&self.fs, &path)?.is_none() {
            AccountingCompleteness::MarkdownOnly
        } else {
            AccountingCompleteness::CorruptHistory
        };
        i.warnings.push(format!(
            "prior lifetime spend is unknown: {reason}; old enforced budget cannot resume"
        ));
        Ok(i)
    }
    fn check_prior(&self, spec: &RunSpec) -> Result<()> {
        let mut entries = 0;
        let paths = self.fs.root().scan_markdown_budgeted(&mut || {
            entries += 1;
            if entries > 65536 {
                return Err(fail(
                    ErrorCode::BudgetExceeded,
                    "prior accounting discovery ceiling exceeded",
                ));
            }
            Ok(())
        })?;
        let mut prior_ids = std::collections::BTreeSet::new();
        for path in paths {
            if !path.as_str().starts_with("runs/") || !path.as_str().ends_with("/run.md") {
                continue;
            }
            let Some(bytes) = read(&self.fs, &path)? else {
                continue;
            };
            // Owned fixed layout supplies only a possible prior-accounting namespace,
            // never typed authority from malformed/future record prose.
            if let Some(name) = path
                .as_str()
                .strip_prefix("runs/")
                .and_then(|s| s.strip_suffix("/run.md"))
                && !name.contains('/')
                && let Ok(id) = RecordId::new(name)
            {
                prior_ids.insert(id);
            }
            if let Some(record) = crate::records::parse_note(&bytes).canonical
                && record.kind() == RecordKind::Run
            {
                prior_ids.insert(record.id().clone());
            }
        }
        let mut check = || Ok(());
        prior_ids.extend(RunStore::discover_existing(
            &self.fs,
            &self.vault_id,
            4096,
            &mut check,
        )?);
        let mut unknown = Vec::new();
        for prior_id in prior_ids {
            if prior_id == spec.run_id {
                continue;
            }
            let prior = Self::new(
                self.fs.clone(),
                self.vault_id.clone(),
                prior_id.clone(),
                self.options.clone(),
            )?;
            let complete = prior
                .with(true, |_, loaded| {
                    Ok(loaded.state.inspection.budget.unknown_attempts.is_empty())
                })
                .unwrap_or(false);
            if !complete {
                unknown.push(prior_id);
            }
        }
        if !unknown.is_empty() {
            match &spec.prior_accounting {
                PriorAccounting::Unknown {
                    prior_run_ids,
                    reason,
                } if !reason.is_empty() && unknown.iter().all(|id| prior_run_ids.contains(id)) => {}
                _ => {
                    return Err(fail(
                        ErrorCode::RecoveryRequired,
                        "new run must disclose retained runs with unknown prior accounting",
                    ));
                }
            }
        }
        Ok(())
    }
    fn control_gate(&self, l: &Loaded) -> Result<()> {
        if self.options.cancel.is_cancelled() {
            return Err(fail(
                ErrorCode::Cancelled,
                "cancelled run cannot restart automatically",
            ));
        }
        let now = self.options.clock.read()?.utc_ms;
        if now < l.state.inspection.utc_high_water_ms
            || now >= l.state.inspection.effective_deadline_utc_ms
            || !l.state.inspection.budget.guarantee_intact
        {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "clock/deadline or invalidated budget blocks restart",
            ));
        }
        Ok(())
    }
    fn ack_checkpoint(
        &self,
        g: &RunLedgerGuard<'_>,
        l: &mut Loaded,
        change: &PreparedChange,
    ) -> Result<EventRef> {
        let engine = ChangeEngine::new(self.fs.clone())?;
        let committed = engine.inspect(&change.change_id)?;
        if &committed.prepared != change || committed.status != ChangeStatus::Committed {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "checkpoint changeset not committed",
            ));
        }
        let path = checkpoint::run_path(&self.run_id)?;
        let actual =
            read(&self.fs, &path)?.ok_or_else(|| events::corrupt("checkpoint run missing"))?;
        let plan = checkpoint::decode_run(&actual)?;
        if plan.spec != l.state.inspection.spec || plan.spec_hash != l.state.inspection.spec_hash {
            return Err(events::corrupt("checkpoint changed immutable genesis"));
        }
        let through = plan
            .checkpoint
            .as_ref()
            .ok_or_else(|| events::corrupt("checkpoint has no operational binding"))?;
        let n = usize::try_from(through.sequence)
            .map_err(|_| events::corrupt("checkpoint sequence overflow"))?;
        if l.frames
            .get(n)
            .is_none_or(|f| events::event_ref(f) != *through)
        {
            return Err(events::corrupt(
                "checkpoint does not match retained operational prefix",
            ));
        }
        let prefix = replay::replay(&l.frames[..=n], 0)?.inspection;
        let expected = checkpoint::run_bytes(&prefix)?;
        if actual != expected {
            return Err(fail(
                ErrorCode::ContentConflict,
                "checkpoint bytes differ from exact projected operational state",
            ));
        }
        let run = reference(
            &self.vault_id,
            &self.run_id,
            path,
            Blake3Hash::digest(&actual),
            RecordKind::Run,
        );
        checkpoint::committed(&self.fs, change, std::slice::from_ref(&run))?;
        for f in &l.frames[..=n] {
            let path = checkpoint::event_path(&self.run_id, &f.event.event_id)?;
            if read(&self.fs, &path)?.as_ref() != Some(&checkpoint::event_bytes(&f.event)?) {
                return Err(events::corrupt("checkpoint event mirror missing/edited"));
            }
        }
        if l.state.inspection.run_note.as_ref() == Some(&run) {
            return existing_ref(
                l,
                |p| matches!(p,EventPayload::Checkpoint{run_note,..}if run_note==&run),
            );
        }
        self.append(
            g,
            l,
            EventPayload::Checkpoint {
                completed_tasks: events::completed_task_summary(
                    prefix
                        .tasks
                        .iter()
                        .filter(|(_, t)| t.state == TaskState::Completed)
                        .map(|(key, _)| key.clone())
                        .collect(),
                )?,
                frontier: None,
                run_note: run,
            },
        )
    }
}
impl DispatcherLedgerApi for JobLedger {
    fn begin_send(&self, p: DispatchPermit) -> Result<SendAuthorization> {
        self.paid_gate()?;
        self.with(true, |g, l| {
            let now = self.active(g, l)?;
            let a = attempt(l, &p.attempt)?;
            if a.phase != AttemptPhase::DispatchIntent
                || a.bound != p.bound
                || l.frames[0].checksum != p.genesis_hash
                || !l
                    .frames
                    .iter()
                    .any(|f| events::event_ref(f) == p.intent_event)
                || l.state.sent.contains(&p.attempt.attempt_id)
            {
                return Err(events::corrupt(
                    "dispatch authority has already been consumed or differs",
                ));
            }
            budgets::quote_bound(
                &p.bound,
                &l.state.inspection.effective_limits,
                now,
                l.state.inspection.effective_deadline_utc_ms,
            )?;
            let before_flush = self.options.clock.read()?;
            let send_event = self.append(
                g,
                l,
                EventPayload::SendAuthorized {
                    attempt: p.attempt.clone(),
                },
            )?;
            self.authority_gate(g, l, &p.bound, before_flush)?;
            Ok(SendAuthorization {
                attempt: p.attempt,
                bound: p.bound,
                send_event,
                genesis_hash: p.genesis_hash,
            })
        })
    }
    fn release_not_sent(&self, r: &AttemptRef, proof: NotSentObservation) -> Result<EventRef> {
        self.local_write()?;
        self.with(false, |g, l| {
            let a = attempt(l, r)?;
            if a.billing == BillingDisposition::ReleasedNotSent {
                return existing_ref(
                    l,
                    |p| matches!(p,EventPayload::ReleasedNotSent{attempt,..}if attempt==r),
                );
            }
            self.append(
                g,
                l,
                EventPayload::ReleasedNotSent {
                    attempt: r.clone(),
                    reason: proof.reason,
                },
            )
        })
    }
    fn outcome_unknown(&self, r: &AttemptRef, code: &str) -> Result<EventRef> {
        self.local_write()?;
        safe_code(code)?;
        self.with(false, |g, l| {
            let a = attempt(l, r)?;
            if a.phase != AttemptPhase::DispatchIntent {
                return Err(WikiError::invalid("only unresolved intent can become unknown"));
            }
            if l.frames.iter().any(|f| matches!(&f.event.payload, EventPayload::OutcomeUnknown { attempt, .. } if attempt == r)) {
                return existing_ref(l, |p| matches!(p, EventPayload::OutcomeUnknown { attempt, .. } if attempt == r));
            }
            self.append(g, l, EventPayload::OutcomeUnknown { attempt: r.clone(), reason: code.into() })
        })
    }
    fn record_response(&self, r: &AttemptRef, response: ResponseSpoolInput) -> Result<SpoolRef> {
        self.local_write()?;
        validate_metadata(&response.metadata)?;
        let metadata = bytes(&response.metadata)?;
        if metadata.len() > METADATA_MAX_BYTES {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "response metadata ceiling exceeded",
            ));
        }
        self.with(false, |g, l| {
            let a = attempt(l, r)?;
            if !l.state.sent.contains(&r.attempt_id) {
                return Err(events::corrupt("response lacks complete one-use send binding"));
            }
            if response.bytes.len() as u64 > a.bound.response_bytes
                || response.bytes.len() as u64 > if a.bound.capability == Capability::Generate { GENERATION_SPOOL_MAX_BYTES } else { OTHER_SPOOL_MAX_BYTES }
            {
                let mut actual = observation_usage(a, &response.metadata);
                actual.response_bytes = actual.response_bytes.max(response.bytes.len() as u64);
                if !violation_recorded(&l.frames, r) {
                    self.append(g, l, EventPayload::BoundViolated { attempt:r.clone(), actual:actual.clone(), actual_cost:response.metadata.computed_cost.clone() })?;
                }
                self.append(g, l, EventPayload::Reconciled { attempt:r.clone(), terminal_confirmed:response.metadata.terminal_response,
                    usage:KnownOrUnknown::Known(actual), cost:response.metadata.computed_cost.clone(), reason:"oversized_response_not_spooled".into() })?;
                return Err(fail(ErrorCode::BudgetExceeded, "paid response exceeds admitted spool bound; cost retained, no oversized body stored"));
            }
            let spool = spool_ref(
                r,
                Blake3Hash::digest(&response.bytes),
                response.bytes.len() as u64,
                Blake3Hash::digest(&metadata),
                metadata.len() as u64,
            )?;
            if let Some(existing) = &a.spool {
                if existing == &spool {
                    let result = existing.clone();
                    self.refresh_observations(g, l)?;
                    return Ok(result);
                }
                return Err(fail(
                    ErrorCode::ContentConflict,
                    "different response already retained for attempt",
                ));
            }
            if a.phase != AttemptPhase::DispatchIntent {
                return Err(events::corrupt(
                    "response cannot recreate cleaned settled spool",
                ));
            }
            g.ensure_attempt_dir(r)?;
            replace_spool(
                g,
                r,
                SpoolPart::Body,
                &response.bytes,
                a.bound.response_bytes,
            )?;
            self.fault(LedgerCheckpoint::AfterSpoolBytesSync)?;
            replace_spool(
                g,
                r,
                SpoolPart::Metadata,
                &metadata,
                METADATA_MAX_BYTES as u64,
            )?;
            self.fault(LedgerCheckpoint::AfterSpoolMetadataSync)?;
            l.state
                .received_meta
                .insert(r.attempt_id.clone(), response.metadata.clone());
            self.fault(LedgerCheckpoint::BeforeReceived)?;
            self.append(
                g,
                l,
                EventPayload::Received {
                    spool: spool.clone(),
                },
            )?;
            self.fault(LedgerCheckpoint::AfterReceived)?;
            self.refresh_observations(g, l)?;
            Ok(spool)
        })
    }
    fn settle(&self, r: &AttemptRef) -> Result<EventRef> {
        self.local_write()?;
        self.with(false, |g, l| {
            let a = attempt(l, r)?;
            if a.phase == AttemptPhase::Settled {
                return existing_ref(
                    l,
                    |p| matches!(p,EventPayload::Settled{attempt,..}if attempt==r),
                );
            }
            if a.phase != AttemptPhase::OutputCommitted {
                return Err(events::corrupt(
                    "settlement requires canonical receipt/output commit",
                ));
            }
            let receipt = checkpoint::receipt(
                &self.fs,
                a.receipt
                    .as_ref()
                    .ok_or_else(|| events::corrupt("settlement receipt missing"))?,
            )?;
            for output in &a.outputs {
                checkpoint::output(&self.fs, output)?;
            }
            if receipt.attempt != *r
                || receipt.outputs != a.outputs
                || receipt.cache_outputs != a.cache_outputs
            {
                return Err(events::corrupt("settlement receipt bindings differ"));
            }
            self.fault(LedgerCheckpoint::BeforeSettlement)?;
            let event = self.append(
                g,
                l,
                EventPayload::Settled {
                    attempt: r.clone(),
                    billing: receipt.billing,
                    usage: receipt.usage,
                    cost: receipt.computed_cost,
                },
            )?;
            self.fault(LedgerCheckpoint::AfterSettlement)?;
            Ok(event)
        })
    }
    fn reconcile(
        &self,
        r: &AttemptRef,
        terminal: bool,
        usage: KnownOrUnknown<Usage>,
        cost: KnownOrUnknown<Money>,
        reason: &str,
    ) -> Result<EventRef> {
        self.local_write()?;
        safe_code(reason)?;
        self.with(false, |g, l| {
            let a = attempt(l, r)?;
            if a.phase == AttemptPhase::Reserved || a.billing == BillingDisposition::ReleasedNotSent {
                return Err(WikiError::invalid("reconciliation requires possible-send attempt"));
            }
            let metadata = ResponseMetadata {
                provider_request_id: None,
                returned_model: None,
                status_code: None,
                terminal_response: terminal,
                usage: usage.clone(),
                computed_cost: cost.clone(),
                failure_code: None,
            };
            if observation_violation(a, &metadata) {
                let actual = observation_usage(a, &metadata);
                self.append(g, l, EventPayload::BoundViolated { attempt: r.clone(), actual, actual_cost: cost.clone() })?;
            }
            if l.frames.iter().any(|f| matches!(&f.event.payload, EventPayload::Reconciled { attempt, terminal_confirmed, usage: u, cost: c, reason: why } if attempt == r && *terminal_confirmed == terminal && u == &usage && c == &cost && why == reason)) {
                return existing_ref(l, |p| matches!(p, EventPayload::Reconciled { attempt, terminal_confirmed, usage: u, cost: c, reason: why } if attempt == r && *terminal_confirmed == terminal && u == &usage && c == &cost && why == reason));
            }
            self.append(g, l, EventPayload::Reconciled { attempt: r.clone(), terminal_confirmed: terminal, usage, cost, reason: reason.into() })
        })
    }
    fn remove_spool_after_verified_commit(&self, r: &AttemptRef) -> Result<EventRef> {
        self.local_write()?;
        self.with(true, |g, l| {
            let a = attempt(l, r)?;
            if a.phase != AttemptPhase::Settled {
                return Err(events::corrupt("cleanup requires settlement"));
            }
            if a.spool.is_none() {
                return existing_ref(
                    l,
                    |p| matches!(p,EventPayload::SpoolRemoved{attempt,..}if attempt==r),
                );
            }
            let receipt = a
                .receipt
                .as_ref()
                .ok_or_else(|| events::corrupt("cleanup receipt missing"))?;
            checkpoint::output(&self.fs, receipt)?;
            for output in &a.outputs {
                checkpoint::output(&self.fs, output)?;
            }
            let spool = a.spool.as_ref().unwrap().clone();
            self.fault(LedgerCheckpoint::BeforeSpoolRemove)?;
            g.remove_spool_file(r, SpoolPart::Body, &spool.response.hash)?;
            g.remove_spool_file(r, SpoolPart::Metadata, &spool.metadata.hash)?;
            g.remove_empty_attempt_dir(r)?;
            self.fault(LedgerCheckpoint::AfterSpoolRemove)?;
            self.append(g, l, EventPayload::SpoolRemoved { attempt: r.clone() })
        })
    }
}
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Dispatcher-only accounting boundary is consumed by P16 and exercised by private ledger tests"
    )
)]
fn replace_spool(
    g: &RunLedgerGuard<'_>,
    r: &AttemptRef,
    part: SpoolPart,
    bytes: &[u8],
    max: u64,
) -> Result<()> {
    let expected = match g.read_spool(r, part, max)? {
        Some(before) if before.bytes == bytes => return Ok(()),
        Some(_) => {
            return Err(fail(
                ErrorCode::ContentConflict,
                "existing response spool bytes differ",
            ));
        }
        None => ExpectedState::Absent,
    };
    g.secure_replace(RunFile::Spool { attempt: r, part }, expected, bytes)?;
    Ok(())
}
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Dispatcher-only accounting boundary is consumed by P16 and exercised by private ledger tests"
    )
)]
fn safe_code(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err(WikiError::invalid(
            "safe failure/reconciliation code required",
        ));
    }
    Ok(())
}
fn observation_violation(a: &AttemptInspection, m: &ResponseMetadata) -> bool {
    let units = match &m.usage {
        KnownOrUnknown::Known(u) => budgets::usage_violates(&a.bound, u),
        _ => false,
    };
    let cost = match (&m.computed_cost, &a.allowance.cost) {
        (KnownOrUnknown::Known(actual), Some(cap)) => {
            actual.currency() != cap.currency() || actual.nanounits() > cap.nanounits()
        }
        _ => false,
    };
    units || cost
}
fn observation_usage(a: &AttemptInspection, m: &ResponseMetadata) -> Usage {
    match &m.usage {
        KnownOrUnknown::Known(u) => u.clone(),
        KnownOrUnknown::Unknown => Usage {
            billable_units: a
                .bound
                .applicable_classes
                .iter()
                .map(|c| (*c, KnownOrUnknown::Unknown))
                .collect(),
            request_bytes: a.bound.request_bytes,
            response_bytes: a.spool.as_ref().map_or(0, |s| s.response.byte_len),
        },
    }
}
fn violation_recorded(frames: &[JournalFrame], r: &AttemptRef) -> bool {
    frames
        .iter()
        .any(|f| matches!(&f.event.payload,EventPayload::BoundViolated{attempt,..}if attempt==r))
}
fn enrich_observations(state: &mut replay::State, frames: &[JournalFrame]) -> Result<()> {
    let metadata = state.received_meta.clone();
    for (id, m) in metadata {
        let Some(a) = state
            .inspection
            .attempts
            .iter()
            .find(|a| a.attempt.attempt_id == id)
            .cloned()
        else {
            continue;
        };
        if !state.observed_costs.contains_key(&id)
            && let KnownOrUnknown::Known(cost) = &m.computed_cost
        {
            state.observed_costs.insert(id.clone(), cost.clone());
        }
        if !state.observed_usage.contains_key(&id)
            && let KnownOrUnknown::Known(usage) = &m.usage
        {
            state.observed_usage.insert(id.clone(), usage.clone());
        }
        if observation_violation(&a, &m) && !violation_recorded(frames, &a.attempt) {
            state.inspection.budget.guarantee_intact = false;
            state.inspection.state = RunState::Paused;
            state.inspection.warnings.push(
                "durable response exposes a bound violation pending journal reconciliation".into(),
            );
        }
    }
    replay::recount(state)
}
impl JobLedger {
    fn refresh_observations(&self, g: &RunLedgerGuard<'_>, l: &mut Loaded) -> Result<()> {
        for (id, m) in l.state.received_meta.clone() {
            let r = l
                .state
                .inspection
                .attempts
                .iter()
                .find(|a| a.attempt.attempt_id == id)
                .ok_or_else(|| events::corrupt("metadata without attempt"))?
                .attempt
                .clone();
            let recorded=l.frames.iter().any(|f|matches!(&f.event.payload,EventPayload::Reconciled{attempt,terminal_confirmed,usage,cost,..}if attempt==&r&&*terminal_confirmed==m.terminal_response&&usage==&m.usage&&cost==&m.computed_cost));
            if !recorded {
                self.append(
                    g,
                    l,
                    EventPayload::Reconciled {
                        attempt: r.clone(),
                        terminal_confirmed: m.terminal_response,
                        usage: m.usage.clone(),
                        cost: m.computed_cost.clone(),
                        reason: "durable_response_observed".into(),
                    },
                )?;
            }
            let a = attempt(l, &r)?;
            if observation_violation(a, &m) && !violation_recorded(&l.frames, &r) {
                let actual = observation_usage(a, &m);
                self.append(
                    g,
                    l,
                    EventPayload::BoundViolated {
                        attempt: r,
                        actual,
                        actual_cost: m.computed_cost,
                    },
                )?;
            }
        }
        Ok(())
    }
}
impl JobLedger {
    #[expect(
        clippy::too_many_arguments,
        reason = "Exact attempt/change/receipt/output/cache bindings are explicit at durable acknowledgment"
    )]
    fn ack_outputs(
        &self,
        g: &RunLedgerGuard<'_>,
        l: &mut Loaded,
        r: &AttemptRef,
        change: &PreparedChange,
        receipt: DurableOutputRef,
        outputs: Vec<DurableOutputRef>,
        cache_outputs: Vec<VectorCacheRef>,
    ) -> Result<EventRef> {
        let mut all = outputs.clone();
        all.push(receipt.clone());
        checkpoint::committed(&self.fs, change, &all)?;
        let actual = checkpoint::receipt(&self.fs, &receipt)?;
        if actual.receipt_id != checkpoint::receipt_id(r)?
            || actual.attempt != *r
            || actual.outputs != outputs
            || actual.cache_outputs != cache_outputs
        {
            return Err(events::corrupt(
                "receipt does not bind exact attempt/output references",
            ));
        }
        let a = attempt(l, r)?;
        let event = if matches!(
            a.phase,
            AttemptPhase::OutputCommitted | AttemptPhase::Settled
        ) {
            if a.receipt.as_ref() != Some(&receipt)
                || a.outputs != outputs
                || a.cache_outputs != cache_outputs
            {
                return Err(events::corrupt(
                    "different output acknowledgment for attempt",
                ));
            }
            existing_ref(
                l,
                |p| matches!(p,EventPayload::OutputsCommitted{attempt,..}if attempt==r),
            )?
        } else {
            let expected = checkpoint::receipt_plan_locked(
                self,
                l,
                r,
                actual.output_disposition,
                outputs.clone(),
                cache_outputs.clone(),
                vec![],
            )?
            .receipt;
            if actual != expected {
                return Err(events::corrupt(
                    "receipt differs from exact bound response metadata",
                ));
            }
            self.fault(LedgerCheckpoint::BeforeOutputsCommitted)?;
            let event = self.append(
                g,
                l,
                EventPayload::OutputsCommitted {
                    attempt: r.clone(),
                    receipt,
                    outputs: outputs.clone(),
                    cache_outputs: cache_outputs.clone(),
                    change: change.clone(),
                },
            )?;
            self.fault(LedgerCheckpoint::AfterOutputsCommitted)?;
            event
        };
        self.repair_task_finish(g, l, r, &actual)?;
        Ok(event)
    }
    fn repair_task_finish(
        &self,
        g: &RunLedgerGuard<'_>,
        l: &mut Loaded,
        r: &AttemptRef,
        receipt: &UsageReceipt,
    ) -> Result<()> {
        if receipt.output_disposition == OutputDisposition::Validated
            && receipt.cache_outputs.is_empty()
            && l.state
                .inspection
                .tasks
                .get(&r.task_key)
                .is_some_and(|task| task.state == TaskState::Running)
        {
            let a = attempt(l, r)?;
            if a.receipt.is_none() || a.outputs != receipt.outputs {
                return Err(events::corrupt(
                    "task completion has no verified output acknowledgment",
                ));
            }
            for out in &receipt.outputs {
                checkpoint::output(&self.fs, out)?;
            }
            self.append(
                g,
                l,
                EventPayload::TaskFinished {
                    task_key: r.task_key.clone(),
                    state: TaskState::Completed,
                    outputs: receipt.outputs.clone(),
                    cache_outputs: vec![],
                    reason: None,
                },
            )?;
        }
        Ok(())
    }
    fn repair_committed_tasks(&self, g: &RunLedgerGuard<'_>, l: &mut Loaded) -> Result<()> {
        let candidates = l
            .state
            .inspection
            .attempts
            .iter()
            .filter(|a| {
                a.receipt.is_some()
                    && matches!(
                        a.phase,
                        AttemptPhase::OutputCommitted | AttemptPhase::Settled
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        for a in candidates {
            let receipt = checkpoint::receipt(&self.fs, a.receipt.as_ref().unwrap())?;
            if receipt.attempt != a.attempt
                || receipt.outputs != a.outputs
                || receipt.cache_outputs != a.cache_outputs
            {
                return Err(events::corrupt(
                    "replayed receipt does not bind stored outputs",
                ));
            }
            self.repair_task_finish(g, l, &a.attempt, &receipt)?;
        }
        Ok(())
    }
    fn recover_committed_receipts(&self, g: &RunLedgerGuard<'_>, l: &mut Loaded) -> Result<()> {
        let attempts = l
            .state
            .inspection
            .attempts
            .iter()
            .filter(|a| a.phase == AttemptPhase::Received)
            .map(|a| a.attempt.clone())
            .collect::<Vec<_>>();
        for r in attempts {
            let id = checkpoint::receipt_id(&r)?;
            let path = checkpoint::event_path(&self.run_id, &id)?;
            let Some(bytes) = read(&self.fs, &path)? else {
                continue;
            };
            let reference = reference(
                &self.vault_id,
                &id,
                path.clone(),
                Blake3Hash::digest(&bytes),
                RecordKind::RunEvent,
            );
            let receipt = checkpoint::receipt(&self.fs, &reference)?;
            let engine = ChangeEngine::new(self.fs.clone())?;
            let mut found = None;
            for id in engine.change_ids()? {
                let inspected = engine.inspect(&id)?;
                if inspected.status == ChangeStatus::Committed
                    && inspected.manifest.operations.iter().any(|op| {
                        op.target == path && op.after == ExpectedState::Hash(reference.hash.clone())
                    })
                {
                    found = Some(inspected.prepared);
                    break;
                }
            }
            let change = found
                .ok_or_else(|| events::corrupt("receipt has no verified committed changeset"))?;
            self.ack_outputs(
                g,
                l,
                &r,
                &change,
                reference,
                receipt.outputs,
                receipt.cache_outputs,
            )?;
        }
        self.repair_committed_tasks(g, l)
    }
}
impl JobLedger {
    fn recover_checkpoint(&self, g: &RunLedgerGuard<'_>, l: &mut Loaded) -> Result<()> {
        let bound = l
            .state
            .inspection
            .run_note
            .as_ref()
            .ok_or_else(|| events::corrupt("run binding missing"))?;
        if checkpoint::output(&self.fs, bound).is_ok() {
            return Ok(());
        }
        if self.options.policy.dry_run {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "checkpoint acknowledgment needed; dry run cannot reconcile",
            ));
        }
        let path = checkpoint::run_path(&self.run_id)?;
        let actual = read(&self.fs, &path)?.ok_or_else(|| {
            fail(
                ErrorCode::ContentConflict,
                "run note missing; retained accounting cannot recreate it",
            )
        })?;
        let hash = Blake3Hash::digest(actual);
        let engine = ChangeEngine::new(self.fs.clone())?;
        for id in engine.change_ids()? {
            let inspected = engine.inspect(&id)?;
            if inspected.status == ChangeStatus::Committed
                && inspected
                    .manifest
                    .operations
                    .iter()
                    .any(|op| op.target == path && op.after == ExpectedState::Hash(hash.clone()))
            {
                self.ack_checkpoint(g, l, &inspected.prepared)?;
                return Ok(());
            }
        }
        Err(fail(
            ErrorCode::ContentConflict,
            "unfamiliar run edit has no exact committed checkpoint proof",
        ))
    }
}

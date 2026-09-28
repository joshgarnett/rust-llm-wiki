//! Pure complete-history state reconstruction. Replayed intent is data, never send authority.
use super::{budgets, events, tasks, types::*};
use crate::domain::*;
use std::collections::BTreeMap;
pub(super) struct State {
    pub inspection: LedgerInspection,
    pub sent: std::collections::BTreeSet<RecordId>,
    pub send_times: Vec<(i64, AttemptRef)>,
    pub received_meta: BTreeMap<RecordId, ResponseMetadata>,
    pub settled: std::collections::BTreeSet<RecordId>,
    pub actual: BTreeMap<RecordId, Allowance>,
    pub observed_costs: BTreeMap<RecordId, Money>,
    pub observed_usage: BTreeMap<RecordId, Usage>,
}
pub(super) fn replay(frames: &[JournalFrame], bytes: u64) -> Result<State> {
    let Some(JournalFrame {
        event:
            LedgerEvent {
                payload:
                    EventPayload::Genesis {
                        spec,
                        spec_hash,
                        run_note,
                    },
                ..
            },
        ..
    }) = frames.first()
    else {
        return Err(events::corrupt(
            "complete operational genesis unavailable; old budget cannot resume",
        ));
    };
    if spec.version != 1
        || spec.input_fingerprint != tasks::input_fingerprint(spec)?
        || spec_hash != &events::spec_hash(spec)?
        || spec.vault_id != run_note.record.vault_id
        || spec.run_id != run_note.record.record_id
        || run_note.record.expected_kind != RecordKind::Run
    {
        return Err(events::corrupt("genesis spec/run binding invalid"));
    }
    budgets::validate_limits(&spec.limits)?;
    let inspection = LedgerInspection {
        spec: spec.clone(),
        spec_hash: spec_hash.clone(),
        effective_limits: spec.limits.clone(),
        effective_deadline_utc_ms: spec.deadline_utc_ms,
        state: RunState::Planned,
        tasks: tasks::initial(spec.tasks.clone())?,
        attempts: vec![],
        budget: BudgetInspection {
            dispatched_requests: 0,
            outstanding_requests: 0,
            concurrency_admitted: 0,
            remote_inflight: 0,
            journal_bytes_used: bytes,
            journal_events_used: frames.len() as u64,
            persistence_reserved: PersistenceAllowance {
                journal_bytes: 0,
                event_slots: 0,
            },
            settled: budgets::zero(),
            outstanding: budgets::zero(),
            known_costs: BTreeMap::new(),
            unknown_attempts: vec![],
            guarantee_intact: true,
        },
        last_event: None,
        run_note: Some(run_note.clone()),
        utc_high_water_ms: spec.created_at_utc_ms,
        completeness: AccountingCompleteness::CompleteHistory,
        warnings: vec![],
    };
    let mut state = State {
        inspection,
        sent: Default::default(),
        send_times: vec![],
        received_meta: Default::default(),
        settled: Default::default(),
        actual: BTreeMap::new(),
        observed_costs: BTreeMap::new(),
        observed_usage: BTreeMap::new(),
    };
    for (index, frame) in frames.iter().enumerate() {
        if index > 0 && matches!(frame.event.payload, EventPayload::Genesis { .. }) {
            return Err(events::corrupt("duplicate genesis"));
        }
        apply(&mut state, &frame.event)?;
        state.inspection.last_event = Some(events::event_ref(frame));
    }
    recount(&mut state)?;
    Ok(state)
}
fn bad() -> WikiError {
    events::corrupt("illegal durable job transition or binding")
}
fn attempt_mut<'a>(s: &'a mut State, r: &AttemptRef) -> Result<&'a mut AttemptInspection> {
    s.inspection
        .attempts
        .iter_mut()
        .find(|a| &a.attempt == r)
        .ok_or_else(bad)
}
fn apply(s: &mut State, e: &LedgerEvent) -> Result<()> {
    if e.run_id != s.inspection.spec.run_id {
        return Err(bad());
    }
    let now = e.occurred_at_utc_ms;
    if !(0..=253_402_300_799_999).contains(&now) {
        return Err(bad());
    }
    if let Some(r) = events::attempt_of(&e.payload) {
        if r.run_id != e.run_id {
            return Err(bad());
        }
        if events::uses_persistence(&e.payload) {
            let a = attempt_mut(s, r)?;
            if a.persistence.event_slots == 0 {
                return Err(bad());
            }
            a.persistence.event_slots -= 1;
            a.persistence.journal_bytes = a.persistence.journal_bytes.saturating_sub(
                serde_json::to_vec(e).map_err(|_| bad())?.len() as u64 + events::FRAME_OVERHEAD,
            );
        }
    }
    if let Some(r) = events::attempt_of(&e.payload) {
        match &e.payload {
            EventPayload::Settled { usage, cost, .. }
            | EventPayload::Reconciled { usage, cost, .. } => observe(s, r, usage, cost),
            EventPayload::BoundViolated {
                actual,
                actual_cost,
                ..
            } => observe(s, r, &KnownOrUnknown::Known(actual.clone()), actual_cost),
            _ => {}
        }
    }
    match &e.payload {
        EventPayload::Genesis { .. } => {}
        EventPayload::TasksAdded { tasks: new } => {
            for spec in new {
                if s.inspection
                    .tasks
                    .insert(
                        spec.key.clone(),
                        TaskInspection {
                            spec: spec.clone(),
                            state: TaskState::Pending,
                            outputs: vec![],
                            cache_outputs: vec![],
                            failure_code: None,
                        },
                    )
                    .is_some()
                {
                    return Err(bad());
                }
            }
            tasks::validate(&s.inspection.tasks)?;
        }
        EventPayload::TaskFinished {
            task_key,
            state,
            outputs,
            cache_outputs,
            reason,
        } => {
            let task = s.inspection.tasks.get_mut(task_key).ok_or_else(bad)?;
            if !matches!(task.state, TaskState::Pending | TaskState::Running)
                || !matches!(state, TaskState::Completed | TaskState::Failed)
            {
                return Err(bad());
            }
            task.state = *state;
            task.outputs = outputs.clone();
            task.cache_outputs = cache_outputs.clone();
            task.failure_code = reason.clone();
        }
        EventPayload::RunTransition { from, to, .. } => {
            if s.inspection.state != *from
                || !matches!(
                    (from, to),
                    (RunState::Planned, RunState::Running | RunState::Stopped)
                        | (RunState::Paused, RunState::Stopped)
                        | (
                            RunState::Running,
                            RunState::Paused
                                | RunState::Stopped
                                | RunState::Completed
                                | RunState::Failed,
                        )
                        | (RunState::Paused | RunState::Stopped, RunState::Running)
                )
            {
                return Err(bad());
            }
            s.inspection.state = *to;
        }
        EventPayload::Amendment { amendment } => {
            budgets::validate_limits(&amendment.limits)?;
            s.inspection.effective_limits = amendment.limits.clone();
            s.inspection.effective_deadline_utc_ms = amendment.deadline_utc_ms;
        }
        EventPayload::Reserved {
            attempt,
            bound,
            allowance,
            persistence,
        } => {
            recount(s)?;
            budgets::fits(
                &s.inspection.budget.settled,
                &s.inspection.budget.outstanding,
                allowance,
                &s.inspection.effective_limits,
            )?;
            if s.inspection.budget.concurrency_admitted >= s.inspection.effective_limits.concurrency
            {
                return Err(bad());
            }
            if s.inspection.state != RunState::Running
                || s.inspection
                    .attempts
                    .iter()
                    .any(|a| a.attempt.attempt_id == attempt.attempt_id)
                || attempt.request_hash != bound.wire_hash
            {
                return Err(bad());
            }
            let task = s
                .inspection
                .tasks
                .get_mut(&attempt.task_key)
                .ok_or_else(bad)?;
            if matches!(task.state, TaskState::Completed | TaskState::Failed)
                || task.spec.capability != Some(bound.capability)
                || task.spec.input_hash != bound.input_hash
                || bound.config_fingerprint != s.inspection.spec.config_fingerprint
                || !s
                    .inspection
                    .spec
                    .scope
                    .profile_fingerprints
                    .contains_key(&bound.profile_id)
            {
                return Err(bad());
            }
            let number = s
                .inspection
                .attempts
                .iter()
                .filter(|a| a.attempt.task_key == attempt.task_key)
                .count() as u32
                + 1;
            if number != attempt.number || number > s.inspection.effective_limits.attempts_per_task
            {
                return Err(bad());
            }
            let expected = budgets::quote_bound(
                bound,
                &s.inspection.effective_limits,
                now,
                s.inspection.effective_deadline_utc_ms,
            )?;
            if &expected != allowance || persistence != &events::persistence() {
                return Err(bad());
            }
            task.state = TaskState::Running;
            s.inspection.attempts.push(AttemptInspection {
                attempt: attempt.clone(),
                phase: AttemptPhase::Reserved,
                billing: BillingDisposition::Reserved,
                remote_exposure: RemoteExposure::NotStarted,
                bound: bound.clone(),
                allowance: allowance.clone(),
                persistence: persistence.clone(),
                spool: None,
                receipt: None,
                outputs: vec![],
                cache_outputs: vec![],
            });
        }
        EventPayload::DispatchIntent { attempt } => {
            let a = attempt_mut(s, attempt)?;
            if a.phase != AttemptPhase::Reserved || a.billing != BillingDisposition::Reserved {
                return Err(bad());
            }
            a.phase = AttemptPhase::DispatchIntent;
            a.billing = BillingDisposition::UnknownReserved;
            a.remote_exposure = RemoteExposure::PossiblyInFlight;
        }
        EventPayload::SendAuthorized { attempt } => {
            if now < s.inspection.utc_high_water_ms
                || s.sent.contains(&attempt.attempt_id)
                || attempt_mut(s, attempt)?.phase != AttemptPhase::DispatchIntent
            {
                return Err(bad());
            }
            let recent = s
                .send_times
                .iter()
                .filter(|(time, _)| *time > now.saturating_sub(60000))
                .collect::<Vec<_>>();
            let limits = &s.inspection.effective_limits;
            if limits
                .requests_per_minute
                .is_some_and(|n| recent.len() >= n as usize)
            {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "actual send requests-per-minute ceiling reached",
                ));
            }
            let mut units = 0u64;
            for r in recent
                .iter()
                .map(|entry| &entry.1)
                .chain(std::iter::once(attempt))
            {
                let a = s
                    .inspection
                    .attempts
                    .iter()
                    .find(|a| &a.attempt == r)
                    .ok_or_else(bad)?;
                for (class, n) in &a.allowance.billable_units {
                    if matches!(
                        class,
                        BillableClass::Input
                            | BillableClass::CachedInput
                            | BillableClass::Output
                            | BillableClass::Reasoning
                    ) {
                        units = units.checked_add(*n).ok_or_else(bad)?;
                    }
                }
            }
            if limits.tokens_per_minute.is_some_and(|n| units > n) {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "actual send tokens-per-minute ceiling reached",
                ));
            }
            s.sent.insert(attempt.attempt_id.clone());
            s.send_times.push((now, attempt.clone()));
        }
        EventPayload::ReleasedNotSent { attempt, .. } => {
            let a = attempt_mut(s, attempt)?;
            if !matches!(
                a.phase,
                AttemptPhase::Reserved | AttemptPhase::DispatchIntent
            ) || a.spool.is_some()
            {
                return Err(bad());
            }
            a.phase = AttemptPhase::Settled;
            a.billing = BillingDisposition::ReleasedNotSent;
            a.remote_exposure = RemoteExposure::TerminalConfirmed;
            a.persistence = PersistenceAllowance {
                journal_bytes: 0,
                event_slots: 0,
            };
            s.inspection
                .tasks
                .get_mut(&attempt.task_key)
                .ok_or_else(bad)?
                .state = TaskState::Pending;
        }
        EventPayload::Received { spool } => {
            let a = attempt_mut(s, &spool.attempt)?;
            if a.phase != AttemptPhase::DispatchIntent || a.spool.is_some() {
                return Err(bad());
            }
            a.phase = AttemptPhase::Received;
            a.spool = Some(spool.clone());
        }
        EventPayload::OutcomeUnknown { attempt, .. } => {
            let a = attempt_mut(s, attempt)?;
            if a.phase != AttemptPhase::DispatchIntent {
                return Err(bad());
            }
            a.billing = BillingDisposition::UnknownReserved;
        }
        EventPayload::OutputsCommitted {
            attempt,
            receipt,
            outputs,
            cache_outputs,
            ..
        } => {
            let a = attempt_mut(s, attempt)?;
            if a.phase != AttemptPhase::Received {
                return Err(bad());
            }
            a.phase = AttemptPhase::OutputCommitted;
            a.receipt = Some(receipt.clone());
            a.outputs = outputs.clone();
            a.cache_outputs = cache_outputs.clone();
        }
        EventPayload::Settled {
            attempt,
            billing,
            usage,
            cost,
        } => {
            if !s.settled.insert(attempt.attempt_id.clone()) {
                return Err(bad());
            }
            let a = attempt_mut(s, attempt)?;
            if a.phase != AttemptPhase::OutputCommitted
                || !matches!(
                    billing,
                    BillingDisposition::KnownSettled | BillingDisposition::UnknownReserved
                )
            {
                return Err(bad());
            }
            if *billing == BillingDisposition::KnownSettled
                && budgets::actual_allowance(&a.bound, usage, cost).is_none()
            {
                return Err(bad());
            }
            a.phase = AttemptPhase::Settled;
            a.billing = *billing;
            a.persistence = if a.spool.is_some() {
                PersistenceAllowance {
                    event_slots: 1,
                    journal_bytes: EVENT_MAX_BYTES as u64,
                }
            } else {
                PersistenceAllowance {
                    event_slots: 0,
                    journal_bytes: 0,
                }
            };
            if *billing == BillingDisposition::KnownSettled {
                let actual = budgets::actual_allowance(&a.bound, usage, cost).ok_or_else(bad)?;
                s.actual.insert(attempt.attempt_id.clone(), actual);
            }
        }
        EventPayload::Reconciled {
            attempt,
            terminal_confirmed,
            ..
        } => {
            let prior = attempt_mut(s, attempt)?.clone();
            if *terminal_confirmed {
                attempt_mut(s, attempt)?.remote_exposure = RemoteExposure::TerminalConfirmed;
            }
            if prior.phase == AttemptPhase::Settled {
                let effective_usage = s
                    .observed_usage
                    .get(&attempt.attempt_id)
                    .cloned()
                    .map(KnownOrUnknown::Known)
                    .unwrap_or(KnownOrUnknown::Unknown);
                let effective_cost = s
                    .observed_costs
                    .get(&attempt.attempt_id)
                    .cloned()
                    .map(KnownOrUnknown::Known)
                    .unwrap_or(KnownOrUnknown::Unknown);
                if let Some(actual) =
                    budgets::actual_allowance(&prior.bound, &effective_usage, &effective_cost)
                {
                    attempt_mut(s, attempt)?.billing = BillingDisposition::KnownSettled;
                    s.actual.insert(attempt.attempt_id.clone(), actual);
                } else {
                    attempt_mut(s, attempt)?.billing = BillingDisposition::UnknownReserved;
                    s.actual.remove(&attempt.attempt_id);
                }
            }
        }
        EventPayload::BoundViolated { .. } => {
            s.inspection.budget.guarantee_intact = false;
            s.inspection.state = RunState::Paused;
        }
        EventPayload::Checkpoint { run_note, .. } => s.inspection.run_note = Some(run_note.clone()),
        EventPayload::SpoolRemoved { attempt } => {
            let a = attempt_mut(s, attempt)?;
            a.spool = None;
            a.persistence = PersistenceAllowance {
                journal_bytes: 0,
                event_slots: 0,
            };
        }
        EventPayload::ClockObserved { utc_high_water_ms } => {
            if *utc_high_water_ms < s.inspection.utc_high_water_ms {
                return Err(bad());
            }
            s.inspection.utc_high_water_ms = *utc_high_water_ms;
        }
    }
    s.inspection.utc_high_water_ms = s.inspection.utc_high_water_ms.max(now);
    Ok(())
}
pub(super) fn observe(
    s: &mut State,
    r: &AttemptRef,
    usage: &KnownOrUnknown<Usage>,
    cost: &KnownOrUnknown<Money>,
) {
    if let KnownOrUnknown::Known(cost) = cost {
        s.observed_costs.insert(r.attempt_id.clone(), cost.clone());
    }
    if let KnownOrUnknown::Known(usage) = usage {
        let merged = s
            .observed_usage
            .entry(r.attempt_id.clone())
            .or_insert_with(|| usage.clone());
        merged.request_bytes = usage.request_bytes;
        merged.response_bytes = usage.response_bytes;
        for (class, value) in &usage.billable_units {
            if matches!(value, KnownOrUnknown::Known(_))
                || !merged.billable_units.contains_key(class)
            {
                merged.billable_units.insert(*class, value.clone());
            }
        }
    }
}
fn add_cost(map: &mut BTreeMap<Currency, u64>, cost: &Money) -> Result<()> {
    let n = map.entry(cost.currency().clone()).or_default();
    *n = n.checked_add(cost.nanounits()).ok_or_else(bad)?;
    Ok(())
}
fn single_cost(map: BTreeMap<Currency, u64>) -> Option<Money> {
    if map.len() == 1 {
        map.into_iter().next().map(|(c, n)| Money::new(c, n))
    } else {
        None
    }
}
pub(super) fn recount(s: &mut State) -> Result<()> {
    let b = &mut s.inspection.budget;
    let mut settled_costs = BTreeMap::new();
    let mut outstanding_costs = BTreeMap::new();
    b.dispatched_requests = 0;
    b.outstanding_requests = 0;
    b.concurrency_admitted = 0;
    b.remote_inflight = 0;
    b.settled = budgets::zero();
    b.outstanding = budgets::zero();
    b.unknown_attempts.clear();
    b.known_costs.clear();
    b.persistence_reserved = PersistenceAllowance {
        journal_bytes: 0,
        event_slots: 0,
    };
    for a in &s.inspection.attempts {
        if let Some(cost) = s.observed_costs.get(&a.attempt.attempt_id) {
            add_cost(&mut b.known_costs, cost)?;
        }
        if a.billing == BillingDisposition::ReleasedNotSent {
            continue;
        }
        if a.phase == AttemptPhase::Reserved {
            b.outstanding_requests = b.outstanding_requests.checked_add(1).ok_or_else(bad)?
        } else {
            b.dispatched_requests = b.dispatched_requests.checked_add(1).ok_or_else(bad)?
        }
        if a.remote_exposure != RemoteExposure::TerminalConfirmed {
            b.concurrency_admitted = b.concurrency_admitted.checked_add(1).ok_or_else(bad)?
        }
        if a.remote_exposure == RemoteExposure::PossiblyInFlight {
            b.remote_inflight = b.remote_inflight.checked_add(1).ok_or_else(bad)?
        }
        if a.billing == BillingDisposition::KnownSettled {
            let actual = s.actual.get(&a.attempt.attempt_id).ok_or_else(bad)?;
            budgets::add(&mut b.settled, actual)?;
            if let Some(cost) = &actual.cost {
                add_cost(&mut settled_costs, cost)?;
            }
        } else {
            let mut held = a.allowance.clone();
            if let Some(actual) = s.observed_usage.get(&a.attempt.attempt_id) {
                held.request_bytes = held.request_bytes.max(actual.request_bytes);
                held.response_bytes = held.response_bytes.max(actual.response_bytes);
                for (class, value) in &actual.billable_units {
                    if let KnownOrUnknown::Known(n) = value {
                        let prior = held.billable_units.entry(*class).or_default();
                        *prior = (*prior).max(*n);
                    }
                }
            }
            if let Some(cost) = s.observed_costs.get(&a.attempt.attempt_id) {
                match &held.cost {
                    Some(prior) if prior.currency() == cost.currency() => {
                        held.cost = Some(Money::new(
                            cost.currency().clone(),
                            prior.nanounits().max(cost.nanounits()),
                        ))
                    }
                    None => held.cost = Some(cost.clone()),
                    _ => {}
                }
            }
            budgets::add(&mut b.outstanding, &held)?;
            if let Some(cost) = &held.cost {
                add_cost(&mut outstanding_costs, cost)?;
            }
            if a.billing == BillingDisposition::UnknownReserved {
                b.unknown_attempts.push(a.attempt.attempt_id.clone());
            }
        }
        b.persistence_reserved.journal_bytes = b
            .persistence_reserved
            .journal_bytes
            .checked_add(a.persistence.journal_bytes)
            .ok_or_else(bad)?;
        b.persistence_reserved.event_slots = b
            .persistence_reserved
            .event_slots
            .checked_add(a.persistence.event_slots)
            .ok_or_else(bad)?;
    }
    b.settled.cost = single_cost(settled_costs);
    b.outstanding.cost = single_cost(outstanding_costs);
    Ok(())
}

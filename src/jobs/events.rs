//! Independent header/body checksums and complete-history event binding.
use super::types::*;
use crate::{changes::prepare::strict_json, domain::*};
use std::collections::BTreeSet;
const MAGIC: &[u8; 8] = b"LWJOB001";
pub(super) const FRAME_OVERHEAD: u64 = 76;
pub(super) const TERMINAL_EVENTS: u32 = 12;
pub(super) const CONTROL_EVENTS: u64 = 2;
pub(super) struct Decoded {
    pub frames: Vec<JournalFrame>,
    pub safe_offset: u64,
    pub torn_tail: bool,
}
pub fn event_ref(frame: &JournalFrame) -> EventRef {
    EventRef {
        event_id: frame.event.event_id.clone(),
        sequence: frame.event.sequence,
        checksum: frame.checksum.clone(),
    }
}
pub fn encode(event: &LedgerEvent) -> Result<(JournalFrame, Vec<u8>)> {
    let body = bounded_json(event, EVENT_MAX_BYTES - FRAME_OVERHEAD as usize)?;
    if body.is_empty() || body.len() > EVENT_MAX_BYTES - FRAME_OVERHEAD as usize {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "job event exceeds frame ceiling",
        ));
    }
    let hash = blake3::hash(&body);
    let frame = JournalFrame {
        event: event.clone(),
        checksum: Blake3Hash::digest(&body),
    };
    let mut bytes = Vec::with_capacity(body.len() + 76);
    bytes.extend(MAGIC);
    bytes.extend((body.len() as u32).to_le_bytes());
    bytes.extend(blake3::hash(&bytes).as_bytes());
    bytes.extend(body);
    bytes.extend(hash.as_bytes());
    Ok((frame, bytes))
}
pub(super) fn decode(bytes: &[u8], run_id: &RecordId) -> Result<Decoded> {
    if bytes.len() as u64 > JOURNAL_MAX_BYTES {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "job journal exceeds ceiling",
        ));
    }
    let mut frames = Vec::new();
    let mut seen = BTreeSet::new();
    let mut offset = 0;
    let mut torn = false;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        if rest.len() < 44 {
            torn = true;
            break;
        }
        if &rest[..8] != MAGIC || blake3::hash(&rest[..12]).as_bytes() != &rest[12..44] {
            return Err(corrupt("journal header checksum mismatch"));
        }
        let len = u32::from_le_bytes(rest[8..12].try_into().unwrap()) as usize;
        if len == 0 || len > EVENT_MAX_BYTES - FRAME_OVERHEAD as usize {
            return Err(corrupt("journal event length invalid"));
        }
        let total = 44 + len + 32;
        if rest.len() < total {
            torn = true;
            break;
        }
        let body = &rest[44..44 + len];
        if blake3::hash(body).as_bytes() != &rest[44 + len..total] {
            return Err(corrupt("journal body checksum mismatch"));
        }
        let event: LedgerEvent =
            strict_json(body).map_err(|_| corrupt("invalid strict job event JSON"))?;
        let checksum = Blake3Hash::digest(body);
        if event.version != 1
            || &event.run_id != run_id
            || event.sequence != frames.len() as u64
            || !seen.insert(event.event_id.clone())
            || event.previous_checksum != frames.last().map(|f: &JournalFrame| f.checksum.clone())
        {
            return Err(corrupt(
                "journal sequence, identity or checksum chain differs",
            ));
        }
        if frames.len() >= JOURNAL_MAX_EVENTS {
            return Err(corrupt("journal event ceiling exceeded"));
        }
        frames.push(JournalFrame { event, checksum });
        offset += total;
    }
    if frames
        .first()
        .is_some_and(|f| !matches!(f.event.payload, EventPayload::Genesis { .. }))
    {
        return Err(corrupt("complete genesis missing"));
    }
    Ok(Decoded {
        frames,
        safe_offset: offset as u64,
        torn_tail: torn,
    })
}
pub(super) fn corrupt(message: &str) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}
pub(super) fn new_event(
    run: &RecordId,
    prior: Option<&EventRef>,
    utc_ms: i64,
    payload: EventPayload,
) -> Result<LedgerEvent> {
    if !(0..=253_402_300_799_999).contains(&utc_ms) {
        return Err(WikiError::new(
            ErrorCode::ConfigInvalid,
            "clock outside canonical UTC range",
        ));
    }
    Ok(LedgerEvent {
        version: 1,
        run_id: run.clone(),
        event_id: RecordId::new(format!("run_event_{}", uuid::Uuid::now_v7()))?,
        sequence: prior.map_or(0, |p| p.sequence + 1),
        previous_checksum: prior.map(|p| p.checksum.clone()),
        occurred_at_utc_ms: utc_ms,
        payload,
    })
}
pub(super) fn persistence() -> PersistenceAllowance {
    PersistenceAllowance {
        journal_bytes: u64::from(TERMINAL_EVENTS) * EVENT_MAX_BYTES as u64,
        event_slots: TERMINAL_EVENTS,
    }
}
pub(super) fn spec_hash(spec: &RunSpec) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(bounded_json(
        spec,
        EVENT_MAX_BYTES - FRAME_OVERHEAD as usize,
    )?))
}
pub(super) fn attempt_of(payload: &EventPayload) -> Option<&AttemptRef> {
    match payload {
        EventPayload::Reserved { attempt, .. }
        | EventPayload::DispatchIntent { attempt }
        | EventPayload::SendAuthorized { attempt }
        | EventPayload::ReleasedNotSent { attempt, .. }
        | EventPayload::OutcomeUnknown { attempt, .. }
        | EventPayload::OutputsCommitted { attempt, .. }
        | EventPayload::Settled { attempt, .. }
        | EventPayload::Reconciled { attempt, .. }
        | EventPayload::BoundViolated { attempt, .. }
        | EventPayload::SpoolRemoved { attempt } => Some(attempt),
        EventPayload::Received { spool } => Some(&spool.attempt),
        _ => None,
    }
}
pub(super) fn event_type(payload: &EventPayload) -> &'static str {
    match payload {
        EventPayload::Genesis { .. } => "genesis",
        EventPayload::TasksAdded { .. } => "tasks_added",
        EventPayload::ResearchRebound { .. } => "research_rebound",
        EventPayload::ResearchFrontierAdmitted { .. } => "research_frontier_admitted",
        EventPayload::ResearchRoundAssessed { .. } => "research_round_assessed",
        EventPayload::ResearchRetryScheduled { .. } => "research_retry_scheduled",
        EventPayload::TaskFinished { .. } => "task_finished",
        EventPayload::RunTransition { .. } => "run_transition",
        EventPayload::Amendment { .. } => "amendment",
        EventPayload::Reserved { .. } => "reserved",
        EventPayload::DispatchIntent { .. } => "dispatch_intent",
        EventPayload::SendAuthorized { .. } => "send_authorized",
        EventPayload::ReleasedNotSent { .. } => "released_not_sent",
        EventPayload::Received { .. } => "received",
        EventPayload::OutcomeUnknown { .. } => "outcome_unknown",
        EventPayload::OutputsCommitted { .. } => "outputs_committed",
        EventPayload::Settled { .. } => "settled",
        EventPayload::Reconciled { .. } => "reconciled",
        EventPayload::BoundViolated { .. } => "bound_violated",
        EventPayload::Checkpoint { .. } => "checkpoint",
        EventPayload::SpoolRemoved { .. } => "spool_removed",
        EventPayload::ClockObserved { .. } => "clock_observed",
    }
}
/// Optional accounting corrections cannot consume reserved receipt/cleanup space.
pub(super) fn uses_persistence(payload: &EventPayload) -> bool {
    matches!(
        payload,
        EventPayload::DispatchIntent { .. }
            | EventPayload::SendAuthorized { .. }
            | EventPayload::ReleasedNotSent { .. }
            | EventPayload::Received { .. }
            | EventPayload::OutputsCommitted { .. }
            | EventPayload::Settled { .. }
            | EventPayload::SpoolRemoved { .. }
    )
}
/// Sorted complete keys remain in canonical RunPlan; the event binds their fixed-size digest.
pub fn completed_task_summary(mut keys: Vec<Blake3Hash>) -> Result<CompletedTaskSummary> {
    if keys.len() > RUN_MAX_TASKS {
        return Err(WikiError::invalid(
            "completed task count exceeds run ceiling",
        ));
    }
    keys.sort();
    if keys.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(WikiError::invalid("completed task keys are not unique"));
    }
    Ok(CompletedTaskSummary {
        count: keys.len() as u32,
        fingerprint: Blake3Hash::digest(
            serde_json::to_vec(&keys).map_err(|_| WikiError::invalid("completed key encoding"))?,
        ),
    })
}

fn bounded_json(value: &impl serde::Serialize, max: usize) -> Result<Vec<u8>> {
    struct Limited {
        bytes: Vec<u8>,
        max: usize,
        exceeded: bool,
    }
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > self.max)
            {
                self.exceeded = true;
                return Err(std::io::Error::other("bounded job JSON ceiling"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Limited {
        bytes: Vec::new(),
        max,
        exceeded: false,
    };
    if serde_json::to_writer(&mut sink, value).is_err() {
        return Err(WikiError::new(
            if sink.exceeded {
                ErrorCode::BudgetExceeded
            } else {
                ErrorCode::RecordInvalid
            },
            "bounded job JSON encoding refused",
        ));
    }
    Ok(sink.bytes)
}

pub(super) fn history_capacity(
    length: u64,
    count: u64,
    reserved: &PersistenceAllowance,
    state: RunState,
    guarantee_intact: bool,
    max_bytes: u64,
    max_events: u64,
) -> bool {
    let controls = if state == RunState::Running && guarantee_intact {
        CONTROL_EVENTS
    } else {
        0
    };
    length
        .checked_add(reserved.journal_bytes)
        .and_then(|n| n.checked_add(controls * EVENT_MAX_BYTES as u64))
        .is_some_and(|n| n <= max_bytes)
        && count
            .checked_add(u64::from(reserved.event_slots))
            .and_then(|n| n.checked_add(controls))
            .is_some_and(|n| n <= max_events)
}

//! Monotone research epochs and frontier admission share the accounting journal.
use super::{checkpoint, events, ledger, tasks, types::*};
use crate::{
    changes::ReadDependency,
    domain::*,
    graph::packet::canonical_json,
    providers::{
        public_fetch,
        types::{RemoteInput, RemoteOperation},
    },
    sources::{CitationScope, SourceView},
    vault::{
        ExpectedState, VaultFs,
        operational::{RunLedgerGuard, SpoolPart},
    },
};
use std::collections::{BTreeMap, BTreeSet};

fn invalid(message: &str) -> WikiError {
    WikiError::invalid(message)
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn hash(payload: &EventPayload) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(canonical_json(payload)?))
}
fn transition_id(payload: &EventPayload) -> Result<&Blake3Hash> {
    match payload {
        EventPayload::ResearchRebound { amendment_id, .. } => Ok(amendment_id),
        EventPayload::ResearchFrontierAdmitted { admission_id, .. } => Ok(admission_id),
        EventPayload::ResearchRoundAssessed { assessment_id, .. } => Ok(assessment_id),
        _ => Err(invalid("research transition required")),
    }
}
/// A local report records current verified evidence and retained historical work.
/// It grants no remote authority and cannot relax source bindings for paid work.
fn local_report_control(
    fs: &VaultFs,
    i: &LedgerInspection,
    payload: &EventPayload,
) -> Result<bool> {
    let EventPayload::ResearchFrontierAdmitted {
        round: 0,
        origins,
        task_origins,
        parent_outputs,
        tasks: new,
        ..
    } = payload
    else {
        return Ok(false);
    };
    if !origins.is_empty()
        || !task_origins.is_empty()
        || !parent_outputs.is_empty()
        || new.is_empty()
    {
        return Ok(false);
    }
    for task in new {
        if !is_local_report_task(fs, i, task)? {
            return Ok(false);
        }
    }
    Ok(true)
}
pub(super) fn is_local_report_task(
    fs: &VaultFs,
    i: &LedgerInspection,
    task: &TaskSpec,
) -> Result<bool> {
    if i.research.is_none()
        || task.stage != TaskStage::StageChanges
        || task.capability.is_some()
        || !task.dependencies.is_empty()
        || task.prompt_hash.is_some()
        || task.schema_hash.is_some()
        || task.model_hash.is_some()
        || task.settings_hash != Blake3Hash::digest(b"lwiki.research-report.v1")
    {
        return Ok(false);
    }
    let bytes = payload_bytes(fs, &task.input, EVENT_MAX_BYTES)?;
    let report: crate::research::ResearchReport = crate::changes::prepare::strict_json(&bytes)?;
    if report.version != 1
        || report.run_id != i.spec.run_id
        || i.spec.scope.question.as_ref() != Some(&report.question)
        || canonical_json(&report)? != bytes
        || task.input_hash != task.input.hash
        || task.input.path.as_str()
            != format!(
                "runs/{}/inputs/{}.json",
                i.spec.run_id,
                task.input.hash.hex()
            )
    {
        return Ok(false);
    }
    if crate::research::report::authenticate_local_report(fs, i, &report)? != task.source_bindings {
        return Err(invalid("local report dependency proofs differ"));
    }
    let view = SourceView::from_fs_bounded(fs, 64 * 1024 * 1024, 4096)?;
    for passage in &report.passages {
        let proof = view.verify(&passage.citation, CitationScope::Current)?;
        if proof.quote != passage.quote.as_bytes()
            || proof.dependencies != passage.dependencies
            || proof
                .dependencies
                .iter()
                .any(|dep| !task.source_bindings.contains(dep))
        {
            return Err(invalid("partial report quotation proof differs"));
        }
    }
    if let Some(synthesis) = &report.synthesis {
        for (section_index, section) in synthesis.sections.iter().enumerate() {
            for (claim_index, claim) in section.claims.iter().enumerate() {
                if !report.claim_assessments.iter().any(|assessment| {
                    assessment.section_index == section_index
                        && assessment.claim_index == claim_index
                        && assessment.status == crate::research::synthesis::ClaimStatus::Unassessed
                        && assessment.provenance_verified == !claim.citations.is_empty()
                }) {
                    return Err(invalid("partial report claim assessment differs"));
                }
                for citation in &claim.citations {
                    let proof = view.verify(citation, CitationScope::Current)?;
                    if proof
                        .dependencies
                        .iter()
                        .any(|dep| !task.source_bindings.contains(dep))
                    {
                        return Err(invalid("partial report citation dependency missing"));
                    }
                }
            }
        }
    }
    tasks::bind(fs, task)?;
    Ok(true)
}
fn binding_valid(binding: &BindingEpochV1, vault: &RecordId) -> Result<()> {
    if binding.version != 1
        || binding.services.len() > 16
        || binding.read_preconditions.len() > crate::changes::prepare::MAX_OPS
        || binding.input_records.len() > crate::changes::prepare::MAX_OPS
        || binding.input_records.iter().any(|r| &r.vault_id != vault)
        || binding
            .read_preconditions
            .iter()
            .map(|d| &d.path)
            .collect::<BTreeSet<_>>()
            .len()
            != binding.read_preconditions.len()
        || binding
            .input_records
            .iter()
            .map(|r| &r.record_id)
            .collect::<BTreeSet<_>>()
            .len()
            != binding.input_records.len()
    {
        return Err(invalid(
            "invalid research binding version, bounds or identities",
        ));
    }
    let mut prior = None;
    for service in &binding.services {
        let key = (service.capability, service.profile_id.as_str());
        if service.profile_id.is_empty()
            || service.profile_id.len() > 128
            || service.profile_id.chars().any(char::is_control)
            || !matches!(
                service.capability,
                Capability::Generate | Capability::Search | Capability::Embed
            )
            || prior.is_some_and(|p| p >= key)
        {
            return Err(invalid(
                "research service bindings must be unique and sorted by capability/profile",
            ));
        }
        prior = Some(key);
    }
    Ok(())
}
pub(super) fn service_binding<'a>(
    i: &'a LedgerInspection,
    profile: &str,
    capability: Capability,
) -> Option<&'a ServiceBindingV1> {
    i.research
        .as_ref()?
        .binding
        .services
        .iter()
        .find(|s| s.profile_id == profile && s.capability == capability)
}
pub(super) fn task_is_active(i: &LedgerInspection, key: &Blake3Hash) -> bool {
    i.research
        .as_ref()
        .is_none_or(|r| r.active_tasks.contains(key))
}
pub(super) fn current_read_preconditions(i: &LedgerInspection) -> &[ReadDependency] {
    i.research
        .as_ref()
        .map_or(&i.spec.scope.read_preconditions, |r| {
            &r.binding.read_preconditions
        })
}
pub(super) fn bound_is_current(
    i: &LedgerInspection,
    key: &Blake3Hash,
    bound: &AttemptBound,
) -> bool {
    let Some(task) = i.tasks.get(key) else {
        return false;
    };
    if !task_is_active(i, key)
        || task.spec.capability != Some(bound.capability)
        || task.spec.input_hash != bound.input_hash
    {
        return false;
    }
    let Some(research) = &i.research else {
        return bound.config_fingerprint == i.spec.config_fingerprint
            && i.spec
                .scope
                .profile_fingerprints
                .contains_key(&bound.profile_id);
    };
    if bound.config_fingerprint != research.binding.config_fingerprint {
        return false;
    }
    if bound.capability == Capability::Fetch {
        return bound.profile_id == public_fetch::PUBLIC_PROFILE
            && bound
                .profile_fingerprint
                .as_ref()
                .is_none_or(|f| f == &public_fetch::settings_fingerprint())
            && task.spec.settings_hash == public_fetch::settings_fingerprint()
            && research.task_origins.contains_key(key);
    }
    service_binding(i, &bound.profile_id, bound.capability).is_some_and(|s| {
        bound.profile_fingerprint.as_ref() == Some(&s.profile_fingerprint)
            && bound.endpoint_fingerprint == s.endpoint_fingerprint
            && task.spec.settings_hash == Blake3Hash::digest(s.profile_fingerprint.as_str())
    })
}
fn task_binding_valid(task: &TaskSpec, binding: &BindingEpochV1) -> Result<()> {
    match task.capability {
        None => {}
        Some(Capability::Fetch)
            if task.stage == TaskStage::Capture
                && task.settings_hash == public_fetch::settings_fingerprint()
                && task.model_hash.is_none()
                && task.prompt_hash.is_none()
                && task.schema_hash.is_none() => {}
        Some(role)
            if binding.services.iter().any(|s| {
                s.capability == role
                    && task.settings_hash == Blake3Hash::digest(s.profile_fingerprint.as_str())
            }) => {}
        _ => {
            return Err(invalid(
                "task settings do not identify a research service role",
            ));
        }
    }
    Ok(())
}
pub(super) fn initial(spec: &RunSpec) -> Result<Option<ResearchStateV1>> {
    let Some(genesis) = &spec.scope.research else {
        return Ok(None);
    };
    if genesis.version != 1
        || genesis.initial_binding.number != 0
        || genesis.limits.rounds == 0
        || genesis.limits.rounds > 16
        || genesis.limits.sources == 0
        || genesis.limits.sources > 256
        || genesis.scope.byte_len == 0
        || genesis.scope.byte_len > EVENT_MAX_BYTES as u64
        || spec.scope.scope_payload_hash.as_ref() != Some(&genesis.scope.hash)
        || genesis.initial_binding.config_fingerprint != spec.config_fingerprint
        || genesis.initial_binding.source_snapshot != spec.scope.source_snapshot
        || genesis.initial_binding.input_records != spec.scope.input_records
        || genesis.initial_binding.read_preconditions != spec.scope.read_preconditions
    {
        return Err(invalid("invalid research genesis"));
    }
    binding_valid(&genesis.initial_binding, &spec.vault_id)?;
    tasks::initial(spec.tasks.clone())?;
    for task in &spec.tasks {
        task_binding_valid(task, &genesis.initial_binding)?;
        if matches!(
            task.capability,
            Some(Capability::Fetch | Capability::Search)
        ) {
            return Err(invalid("initial acquisition requires frontier admission"));
        }
    }
    Ok(Some(ResearchStateV1 {
        version: 1,
        binding: genesis.initial_binding.clone(),
        binding_event: None,
        active_tasks: spec.tasks.iter().map(|t| t.key.clone()).collect(),
        frontier_revision: 0,
        rounds_started: 0,
        origins: BTreeMap::new(),
        task_origins: BTreeMap::new(),
        rounds: vec![],
        support_groups: BTreeSet::new(),
        no_progress_rounds: 0,
        transitions: BTreeMap::new(),
        retry_not_before: BTreeMap::new(),
    }))
}
fn add_tasks(i: &mut LedgerInspection, new: &[TaskSpec]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for spec in new {
        if spec.input_hash != spec.input.hash || !seen.insert(&spec.key) {
            return Err(invalid("duplicate frontier task"));
        }
        if let Some(old) = i.tasks.get(&spec.key) {
            if old.spec != *spec {
                return Err(conflict(
                    "task identity already has a different complete definition",
                ));
            }
        } else {
            i.tasks.insert(
                spec.key.clone(),
                TaskInspection {
                    spec: spec.clone(),
                    state: TaskState::Pending,
                    outputs: vec![],
                    cache_outputs: vec![],
                    failure_code: None,
                },
            );
        }
    }
    tasks::validate(&i.tasks)
}
fn active_valid(i: &LedgerInspection, r: &ResearchStateV1) -> Result<()> {
    for key in &r.active_tasks {
        let task = i
            .tasks
            .get(key)
            .ok_or_else(|| invalid("active task missing"))?;
        task_binding_valid(&task.spec, &r.binding)?;
        if task
            .spec
            .dependencies
            .iter()
            .any(|d| !r.active_tasks.contains(d))
        {
            return Err(invalid(
                "active research task dependencies must remain active",
            ));
        }
        if task.spec.capability == Some(Capability::Fetch) && !r.task_origins.contains_key(key) {
            return Err(invalid("capture task has no admitted origin"));
        }
    }
    Ok(())
}
fn owns_output(i: &LedgerInspection, key: &Blake3Hash, output: &DurableOutputRef) -> bool {
    task_is_active(i, key)
        && output.record.vault_id == i.spec.vault_id
        && i.tasks.get(key).is_some_and(|t| {
            t.state == TaskState::Completed
                && t.outputs.contains(output)
                && (t.spec.capability.is_none()
                    || i.attempts.iter().any(|a| {
                        a.attempt.task_key == *key
                            && a.phase == AttemptPhase::Settled
                            && a.receipt.is_some()
                            && a.outputs.contains(output)
                    }))
        })
}
fn groups(citations: &[CitationRef]) -> Vec<Blake3Hash> {
    citations
        .iter()
        .map(|c| match c {
            CitationRef::Source(r) => r.quote_hash.clone(),
            CitationRef::Assertion(r) => r.quote_hash.clone(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
/// Pure validation is shared by append and complete-history replay. Mutations are
/// installed only after every invariant passes; no transition resets accounting.
pub(super) fn apply(
    i: &mut LedgerInspection,
    payload: &EventPayload,
    event: &EventRef,
) -> Result<()> {
    if let EventPayload::ResearchRetryScheduled {
        version,
        epoch,
        task_key,
        attempt,
        not_before_utc_ms,
    } = payload
    {
        let r = i
            .research
            .as_ref()
            .ok_or_else(|| invalid("research retry requires research genesis"))?;
        let task = i
            .tasks
            .get(task_key)
            .ok_or_else(|| invalid("retry task missing"))?;
        let prior = i
            .attempts
            .iter()
            .rev()
            .find(|a| a.attempt.task_key == *task_key)
            .ok_or_else(|| invalid("retry prior attempt missing"))?;
        if *version != 1
            || *epoch != r.binding.number
            || !r.active_tasks.contains(task_key)
            || prior.attempt != *attempt
            || !matches!(task.state, TaskState::Pending | TaskState::Running)
            || !task.outputs.is_empty()
            || !bound_is_current(i, task_key, &prior.bound)
            || (prior.phase != AttemptPhase::Settled
                && prior.billing != BillingDisposition::UnknownReserved)
            || *not_before_utc_ms < i.utc_high_water_ms
            || *not_before_utc_ms >= i.effective_deadline_utc_ms
            || r.retry_not_before
                .get(task_key)
                .is_some_and(|old| old > not_before_utc_ms)
        {
            return Err(conflict("research retry attempt, epoch or time differs"));
        }
        i.research
            .as_mut()
            .unwrap()
            .retry_not_before
            .insert(task_key.clone(), *not_before_utc_ms);
        return Ok(());
    }
    let id = transition_id(payload)?;
    let payload_hash = hash(payload)?;
    let mut r = i
        .research
        .clone()
        .ok_or_else(|| invalid("research event without research genesis"))?;
    if let Some(old) = r.transitions.get(id) {
        return if old.payload_hash == payload_hash {
            Err(invalid("duplicate research transition frame"))
        } else {
            Err(conflict("research transition ID has conflicting payload"))
        };
    }
    if canonical_json(payload)?.len() > EVENT_MAX_BYTES {
        return Err(budget("research event too large"));
    }
    let mut next = i.clone();
    match payload {
        EventPayload::ResearchRebound {
            version,
            expected_epoch,
            prior_revision,
            binding,
            active_tasks,
            tasks: new,
            reason,
            ..
        } => {
            if *version != 1
                || *expected_epoch != r.binding.number
                || *prior_revision != r.frontier_revision
                || r.binding.number.checked_add(1) != Some(binding.number)
            {
                return Err(conflict("research epoch/revision differs"));
            }
            if !matches!(i.state, RunState::Paused | RunState::Stopped)
                || i.attempts.iter().any(|a| {
                    a.phase != AttemptPhase::Settled
                        || a.remote_exposure != RemoteExposure::TerminalConfirmed
                })
            {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "research rebind requires paused quiescent attempts",
                ));
            }
            if reason.is_empty()
                || reason.len() > 4096
                || active_tasks.windows(2).any(|w| w[0] >= w[1])
            {
                return Err(invalid("rebind reason or sorted membership invalid"));
            }
            binding_valid(binding, &i.spec.vault_id)?;
            if new.iter().any(|task| {
                !i.tasks.contains_key(&task.key)
                    && matches!(
                        task.capability,
                        Some(Capability::Search | Capability::Fetch)
                    )
            }) {
                return Err(invalid(
                    "new acquisition requires atomic frontier admission",
                ));
            }
            add_tasks(&mut next, new)?;
            r.binding = binding.clone();
            r.binding_event = Some(event.clone());
            r.active_tasks = active_tasks.iter().cloned().collect();
        }
        EventPayload::ResearchFrontierAdmitted {
            version,
            epoch,
            prior_revision,
            round,
            origins,
            tasks: new,
            task_origins,
            parent_outputs,
            ..
        } => {
            if *version != 1 || *epoch != r.binding.number || *prior_revision != r.frontier_revision
            {
                return Err(conflict("research frontier epoch/revision differs"));
            }
            if !matches!(
                i.state,
                RunState::Planned | RunState::Running | RunState::Paused | RunState::Stopped
            ) {
                return Err(invalid("ended research run cannot admit frontier"));
            }
            let limits = &i
                .spec
                .scope
                .research
                .as_ref()
                .ok_or_else(|| invalid("research genesis missing"))?
                .limits;
            if *round == 0 {
                if !origins.is_empty()
                    || !task_origins.is_empty()
                    || new.iter().any(|t| {
                        matches!(t.capability, Some(Capability::Fetch | Capability::Search))
                    })
                {
                    return Err(invalid("round zero admits control work only"));
                }
            } else if *round > r.rounds_started {
                if *round != r.rounds_started + 1
                    || *round > limits.rounds
                    || r.no_progress_rounds >= 2
                    || (r.rounds_started > 0
                        && !r.rounds.iter().any(|a| a.round == r.rounds_started))
                {
                    return Err(budget(
                        "research round admission ceiling or assessment barrier",
                    ));
                }
                r.rounds_started = *round;
            } else if *round != r.rounds_started || r.rounds.iter().any(|a| a.round == *round) {
                return Err(invalid("frontier cannot extend a closed research round"));
            }
            for output in parent_outputs {
                if !i.tasks.keys().any(|key| owns_output(i, key, output)) {
                    return Err(invalid(
                        "frontier parent is not a completed active task output",
                    ));
                }
            }
            let mut seen_origins = BTreeSet::new();
            for origin in origins {
                let normalized = public_fetch::validate_url(&origin.url, None)?;
                if origin.round != *round
                    || normalized.as_str() != origin.url
                    || Blake3Hash::digest(normalized.as_str()) != origin.key
                    || !seen_origins.insert(&origin.key)
                    || r.origins.contains_key(&origin.key)
                {
                    return Err(invalid(
                        "origin must be new, normalized and bound to admitted round",
                    ));
                }
                r.origins.insert(origin.key.clone(), origin.clone());
            }
            if r.origins.len() > limits.sources as usize {
                return Err(budget("research source ceiling reached"));
            }
            add_tasks(&mut next, new)?;
            for task in new {
                r.active_tasks.insert(task.key.clone());
            }
            let mut seen = BTreeSet::new();
            for origin in task_origins {
                let task = next
                    .tasks
                    .get(&origin.task_key)
                    .ok_or_else(|| invalid("origin task missing"))?;
                if !new.iter().any(|t| t.key == origin.task_key)
                    || task.spec.capability != Some(Capability::Fetch)
                    || !r.origins.contains_key(&origin.origin_key)
                    || !seen.insert(&origin.task_key)
                {
                    return Err(invalid("capture origin binding invalid"));
                }
                if let Some(old) = r.task_origins.get(&origin.task_key)
                    && old != origin
                {
                    return Err(conflict("capture origin cannot change"));
                }
                if let Some(parent) = &origin.parent_capture
                    && (!i.tasks.get(parent).is_some_and(|t| {
                        t.state == TaskState::Completed
                            && t.spec.capability == Some(Capability::Fetch)
                    }) || !task.spec.dependencies.contains(parent)
                        || !r.active_tasks.contains(parent)
                        || r.task_origins
                            .get(parent)
                            .is_none_or(|p| p.origin_key != origin.origin_key))
                {
                    return Err(invalid(
                        "redirect requires completed preceding hop of same origin",
                    ));
                }
                r.task_origins
                    .insert(origin.task_key.clone(), origin.clone());
            }
        }
        EventPayload::ResearchRoundAssessed {
            version,
            epoch,
            prior_revision,
            round,
            task_key,
            output,
            citations,
            support_groups,
            ..
        } => {
            if *version != 1 || *epoch != r.binding.number || *prior_revision != r.frontier_revision
            {
                return Err(conflict("research assessment epoch/revision differs"));
            }
            if *round == 0
                || *round != r.rounds_started
                || r.rounds.iter().any(|a| a.round == *round)
                || !i.tasks.get(task_key).is_some_and(|t| {
                    t.spec.stage == TaskStage::AssessGaps
                        && t.spec.capability == Some(Capability::Generate)
                })
                || !owns_output(i, task_key, output)
                || citations.len() > 4096
                || groups(citations) != *support_groups
            {
                return Err(invalid("round assessment output/support binding invalid"));
            }
            let added = support_groups
                .iter()
                .filter(|g| !r.support_groups.contains(*g))
                .count() as u32;
            r.support_groups.extend(support_groups.iter().cloned());
            r.no_progress_rounds = if added == 0 {
                r.no_progress_rounds
                    .checked_add(1)
                    .ok_or_else(|| budget("assessment count overflow"))?
            } else {
                0
            };
            r.rounds.push(ResearchRoundV1 {
                round: *round,
                epoch: *epoch,
                task_key: task_key.clone(),
                output: output.clone(),
                support_groups: support_groups.clone(),
                added_groups: added,
            });
        }
        _ => return Err(invalid("research transition required")),
    }
    active_valid(&next, &r)?;
    r.frontier_revision = r
        .frontier_revision
        .checked_add(1)
        .ok_or_else(|| budget("research revision overflow"))?;
    r.transitions.insert(
        id.clone(),
        ResearchTransitionV1 {
            payload_hash,
            event: event.clone(),
        },
    );
    next.research = Some(r);
    *i = next;
    Ok(())
}

pub(super) fn validate_live_genesis(fs: &VaultFs, spec: &RunSpec) -> Result<()> {
    initial(spec)?;
    if let Some(genesis) = &spec.scope.research {
        payload_bytes(fs, &genesis.scope, EVENT_MAX_BYTES)?;
        live_binding(fs, &genesis.initial_binding)?;
    }
    Ok(())
}
fn payload_bytes(fs: &VaultFs, payload: &BoundedPayloadRef, limit: usize) -> Result<Vec<u8>> {
    let bytes = crate::changes::prepare::read_bounded(fs, &payload.path, limit)?
        .ok_or_else(|| conflict("research payload missing"))?;
    if bytes.len() as u64 != payload.byte_len || Blake3Hash::digest(&bytes) != payload.hash {
        return Err(conflict("research payload hash/length changed"));
    }
    Ok(bytes)
}
fn live_binding(fs: &VaultFs, binding: &BindingEpochV1) -> Result<()> {
    let mut records = BTreeMap::new();
    for dep in &binding.read_preconditions {
        let bytes = ledger::read(fs, &dep.path)?;
        let actual = bytes.as_ref().map_or(ExpectedState::Absent, |b| {
            ExpectedState::Hash(Blake3Hash::digest(b))
        });
        if actual != dep.expected {
            return Err(conflict("research source binding changed"));
        }
        if let Some(bytes) = bytes
            && let Some(record) = crate::records::parse_note(&bytes).canonical
        {
            records
                .entry(record.id().clone())
                .or_insert_with(Vec::new)
                .push(record.kind());
        }
    }
    for record in &binding.input_records {
        if records
            .get(&record.record_id)
            .is_none_or(|k| k.len() != 1 || k[0] != record.expected_kind)
        {
            return Err(conflict(
                "research input record requires exact guarded canonical ownership",
            ));
        }
    }
    Ok(())
}
fn fetch_descriptor(fs: &VaultFs, task: &TaskSpec) -> Result<(String, public_fetch::FetchLimits)> {
    let bytes = payload_bytes(fs, &task.input, EVENT_MAX_BYTES)?;
    let input: RemoteInput = crate::changes::prepare::strict_json(&bytes)?;
    if input.version != 1
        || canonical_json(&input)? != bytes
        || public_fetch::input_fingerprint(&input)? != task.input_hash
    {
        return Err(invalid(
            "research fetch descriptor is not canonical and bound",
        ));
    }
    let RemoteOperation::Fetch { url, limits } = input.operation else {
        return Err(invalid("capture requires fetch descriptor"));
    };
    limits.validate()?;
    Ok((public_fetch::validate_url(&url, None)?.to_string(), limits))
}
pub(super) fn validate_live_bound(
    fs: &VaultFs,
    i: &LedgerInspection,
    key: &Blake3Hash,
    bound: &AttemptBound,
) -> Result<()> {
    if !bound_is_current(i, key, bound) {
        return Err(conflict(
            "attempt is not bound to current active research service",
        ));
    }
    if i.research.is_some() && bound.capability == Capability::Fetch {
        let task = &i
            .tasks
            .get(key)
            .ok_or_else(|| invalid("capture task missing"))?
            .spec;
        let (url, limits) = fetch_descriptor(fs, task)?;
        let wire_hash = Blake3Hash::digest(canonical_json(&(
            "lwiki.public-wire.v1",
            "GET",
            url.as_str(),
            &limits,
            public_fetch::settings_fingerprint(),
        ))?);
        if bound.wire_hash != wire_hash
            || bound.endpoint_fingerprint != Blake3Hash::digest(&url)
            || bound.response_bytes != limits.compressed_bytes
            || bound.timeout_ms != limits.timeout_ms
            || bound.request_bytes != 0
            || bound.requested_model.is_some()
            || bound.requested_model_revision.is_some()
            || !bound.applicable_classes.is_empty()
            || !bound.billable_bounds.is_empty()
        {
            return Err(conflict(
                "public fetch bound differs from admitted URL/limits",
            ));
        }
    }
    Ok(())
}
fn validated_output(
    fs: &VaultFs,
    i: &LedgerInspection,
    key: &Blake3Hash,
    output: &DurableOutputRef,
) -> Result<()> {
    if !owns_output(i, key, output) {
        return Err(conflict("research parent output ownership differs"));
    }
    checkpoint::output(fs, output)?;
    let task = &i.tasks[key];
    if task.spec.capability.is_some() {
        let a = i
            .attempts
            .iter()
            .rev()
            .find(|a| {
                a.attempt.task_key == *key
                    && a.phase == AttemptPhase::Settled
                    && a.outputs.contains(output)
            })
            .ok_or_else(|| invalid("research parent settlement missing"))?;
        let receipt = checkpoint::receipt(
            fs,
            a.receipt
                .as_ref()
                .ok_or_else(|| invalid("research parent receipt missing"))?,
        )?;
        if receipt.attempt != a.attempt
            || receipt.output_disposition != OutputDisposition::Validated
            || receipt.outputs != a.outputs
        {
            return Err(conflict("research parent receipt not validated"));
        }
    }
    Ok(())
}
fn live_origins(
    fs: &VaultFs,
    guard: &RunLedgerGuard<'_>,
    before: &LedgerInspection,
    after: &LedgerInspection,
    origins: &[ResearchTaskOriginV1],
) -> Result<()> {
    let research = after
        .research
        .as_ref()
        .ok_or_else(|| invalid("research state missing"))?;
    for origin in origins {
        let task = &after.tasks[&origin.task_key].spec;
        let (url, limits) = fetch_descriptor(fs, task)?;
        if let Some(parent) = &origin.parent_capture {
            let old = &before.tasks[parent];
            let attempt = before
                .attempts
                .iter()
                .rev()
                .find(|a| {
                    a.attempt.task_key == *parent
                        && a.phase == AttemptPhase::Settled
                        && a.outputs == old.outputs
                })
                .ok_or_else(|| invalid("redirect preceding hop settlement missing"))?;
            let receipt = checkpoint::receipt(
                fs,
                attempt
                    .receipt
                    .as_ref()
                    .ok_or_else(|| invalid("redirect receipt missing"))?,
            )?;
            if receipt.attempt != attempt.attempt
                || receipt.output_disposition != OutputDisposition::Validated
                || receipt.outputs != old.outputs
            {
                return Err(conflict("redirect preceding receipt differs"));
            }
            let spool = attempt
                .spool
                .as_ref()
                .ok_or_else(|| conflict("redirect protected capture missing"))?;
            let meta = guard
                .read_spool(
                    &attempt.attempt,
                    SpoolPart::Metadata,
                    METADATA_MAX_BYTES as u64,
                )?
                .ok_or_else(|| conflict("redirect protected metadata missing"))?;
            if meta.hash != spool.metadata.hash
                || meta.bytes.len() as u64 != spool.metadata.byte_len
            {
                return Err(conflict("redirect metadata changed"));
            }
            let metadata: ResponseMetadata = crate::changes::prepare::strict_json(&meta.bytes)?;
            let capture = metadata
                .acquisition
                .ok_or_else(|| invalid("redirect acquisition metadata missing"))?;
            capture.validate()?;
            let raw = guard
                .read_spool(
                    &attempt.attempt,
                    SpoolPart::Body,
                    capture.limits.compressed_bytes,
                )?
                .ok_or_else(|| conflict("redirect protected response missing"))?;
            if raw.hash != spool.response.hash
                || raw.bytes.len() as u64 != spool.response.byte_len
                || raw.hash != capture.original_hash
            {
                return Err(conflict("redirect protected response changed"));
            }

            let (parent_url, parent_limits) = fetch_descriptor(fs, &old.spec)?;
            if !metadata.terminal_response
                || !matches!(capture.status, 301 | 302 | 303 | 307 | 308)
                || capture.requested_url != parent_url
                || capture.limits != parent_limits
                || limits != parent_limits
            {
                return Err(invalid("redirect preceding response or limits invalid"));
            }
            let location = capture
                .selected_headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("location"))
                .map(|(_, v)| v)
                .ok_or_else(|| invalid("redirect Location missing"))?;
            let destination = url::Url::parse(&capture.requested_url)
                .map_err(|_| invalid("redirect base invalid"))?
                .join(location)
                .map_err(|_| invalid("redirect destination invalid"))?;
            if public_fetch::validate_url(destination.as_str(), Some(&capture.requested_url))?
                .as_str()
                != url
            {
                return Err(conflict("capture URL differs from authenticated redirect"));
            }
            let mut depth = 1u8;
            let mut cursor = research
                .task_origins
                .get(parent)
                .and_then(|o| o.parent_capture.as_ref());
            while let Some(key) = cursor {
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| budget("redirect depth overflow"))?;
                if depth > limits.redirects {
                    return Err(budget("redirect depth exceeded"));
                }
                cursor = research
                    .task_origins
                    .get(key)
                    .and_then(|o| o.parent_capture.as_ref());
            }
            if depth > limits.redirects {
                return Err(budget("redirect depth exceeded"));
            }
        } else if research
            .origins
            .get(&origin.origin_key)
            .is_none_or(|o| o.url != url)
        {
            return Err(conflict("capture URL differs from admitted source origin"));
        }
    }
    Ok(())
}
impl JobLedger {
    /// Resume only local publication of an already completed synthesis. Exact
    /// committed own writes may satisfy its old page proofs; paid bindings stay
    /// unchanged and remain subject to ordinary admission.
    pub(crate) fn resume_research_publication(
        &self,
        key: &Blake3Hash,
        epoch: u32,
        requested: Option<(LifetimeLimits, i64, String)>,
        services: &[&crate::config::providers::TrustedService],
    ) -> Result<EventRef> {
        self.local_write()?;
        self.with(true, |guard, loaded| {
            let i = &loaded.state.inspection;
            let research = i
                .research
                .as_ref()
                .ok_or_else(|| invalid("research genesis missing"))?;
            if !matches!(i.state, RunState::Paused | RunState::Stopped)
                || research.binding.number != epoch
                || !research.active_tasks.contains(key)
                || i.tasks.get(key).is_none_or(|task| {
                    task.spec.stage != TaskStage::Synthesize
                        || task.state != TaskState::Completed
                        || task.spec.capability != Some(Capability::Generate)
                })
                || i.tasks.values().any(|task| {
                    task_is_active(i, &task.spec.key)
                        && task.spec.capability.is_some()
                        && task.state != TaskState::Completed
                })
                || i.attempts.iter().any(|attempt| {
                    attempt.phase != AttemptPhase::Settled
                        || attempt.remote_exposure != RemoteExposure::TerminalConfirmed
                })
                || services.len() != research.binding.services.len()
            {
                return Err(conflict(
                    "research local publication cannot resume this state",
                ));
            }
            let mut proven = BTreeMap::new();
            for service in services {
                service.recheck(&self.fs)?;
                let summary = service.summary();
                if summary.config_fingerprint != research.binding.config_fingerprint
                    || proven
                        .insert((summary.capability, summary.profile_id.clone()), summary)
                        .is_some()
                {
                    return Err(conflict(
                        "research publication service configuration changed",
                    ));
                }
            }
            for expected in &research.binding.services {
                let actual = proven
                    .get(&(expected.capability, expected.profile_id.clone()))
                    .ok_or_else(|| conflict("research publication service role missing"))?;
                if actual.profile_fingerprint != expected.profile_fingerprint
                    || actual.endpoint_fingerprint != expected.endpoint_fingerprint
                {
                    return Err(conflict("research publication service binding changed"));
                }
            }
            let substitutions =
                crate::research::report::publication_substitutions(&self.fs, i, key)?;
            let mut observed = i.clone();
            let substitute = |reads: &mut Vec<ReadDependency>| {
                for read in reads {
                    if let Some((before, after)) = substitutions.get(&read.path)
                        && &read.expected == before
                    {
                        read.expected = after.clone();
                    }
                }
            };
            substitute(
                &mut observed
                    .research
                    .as_mut()
                    .unwrap()
                    .binding
                    .read_preconditions,
            );
            for task in observed.tasks.values_mut() {
                substitute(&mut task.spec.source_bindings);
            }
            self.bind_inputs(&observed, &loaded.frames)?;
            if let Some((limits, deadline_utc_ms, reason)) = requested {
                let now = self.options.clock.read()?.utc_ms;
                let amendment = LimitAmendment {
                    requested_at_utc_ms: now,
                    reason,
                    limits,
                    deadline_utc_ms,
                };
                ledger::validate_amendment(loaded, &amendment, now)?;
                self.append(guard, loaded, EventPayload::Amendment { amendment })?;
            }
            self.control_gate(loaded)?;
            self.append(
                guard,
                loaded,
                EventPayload::RunTransition {
                    from: loaded.state.inspection.state,
                    to: RunState::Running,
                    reason: StopReason::Started,
                },
            )
        })
    }
    /// The caller already owns the vault writer permit. Hold the run guard
    /// through explicit page apply so a concurrent epoch retirement cannot
    /// authorize publication from an older synthesis.
    pub(crate) fn with_research_publication<T>(
        &self,
        key: &Blake3Hash,
        expected_epoch: u32,
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.local_write()?;
        self.with(false, |_, loaded| {
            let inspection = &loaded.state.inspection;
            let research = inspection
                .research
                .as_ref()
                .ok_or_else(|| invalid("research publication requires genesis"))?;
            let task = inspection
                .tasks
                .get(key)
                .ok_or_else(|| conflict("research synthesis task missing"))?;
            if research.binding.number != expected_epoch
                || !research.active_tasks.contains(key)
                || task.state != TaskState::Completed
                || task.spec.stage != TaskStage::Synthesize
                || task.spec.capability != Some(Capability::Generate)
            {
                return Err(conflict("research synthesis publication authority changed"));
            }
            if self.options.cancel.is_cancelled() {
                return Err(WikiError::new(
                    ErrorCode::Cancelled,
                    "research publication cancelled",
                ));
            }
            let substitutions =
                crate::research::report::publication_substitutions(&self.fs, inspection, key)?;
            let substitute = |reads: &mut Vec<ReadDependency>| {
                for read in reads {
                    if let Some((before, after)) = substitutions.get(&read.path)
                        && &read.expected == before
                    {
                        read.expected = after.clone();
                    }
                }
            };
            let mut observed_binding = research.binding.clone();
            substitute(&mut observed_binding.read_preconditions);
            live_binding(&self.fs, &observed_binding)?;
            let mut observed_task = task.spec.clone();
            substitute(&mut observed_task.source_bindings);
            tasks::bind(&self.fs, &observed_task)?;
            publish()
        })
    }
    /// Caller-requested replacement of current source/service bindings. Trusted
    /// services recheck private configuration without resolving credentials.
    pub fn rebind_research(
        &self,
        payload: EventPayload,
        services: &[&crate::config::providers::TrustedService],
    ) -> Result<EventRef> {
        if !matches!(payload, EventPayload::ResearchRebound { .. }) {
            return Err(invalid("rebind event required"));
        }
        self.research_transition(payload, services)
    }
    pub fn admit_research_frontier(&self, payload: EventPayload) -> Result<EventRef> {
        if !matches!(payload, EventPayload::ResearchFrontierAdmitted { .. }) {
            return Err(invalid("frontier event required"));
        }
        self.research_transition(payload, &[])
    }
    pub fn assess_research_round(&self, payload: EventPayload) -> Result<EventRef> {
        if !matches!(payload, EventPayload::ResearchRoundAssessed { .. }) {
            return Err(invalid("assessment event required"));
        }
        self.research_transition(payload, &[])
    }
    fn research_transition(
        &self,
        payload: EventPayload,
        services: &[&crate::config::providers::TrustedService],
    ) -> Result<EventRef> {
        self.local_write()?;
        self.with(false, |guard, loaded| {
            if matches!(payload, EventPayload::ResearchRebound { .. }) {
                self.refresh_observations(guard, loaded)?;
            }
            let i = &loaded.state.inspection;
            let r = i
                .research
                .as_ref()
                .ok_or_else(|| invalid("research genesis required"))?;
            if let Some(old) = r.transitions.get(transition_id(&payload)?) {
                return if old.payload_hash == hash(&payload)? {
                    Ok(old.event.clone())
                } else {
                    Err(conflict("research transition ID has conflicting payload"))
                };
            }
            let genesis = i
                .spec
                .scope
                .research
                .as_ref()
                .ok_or_else(|| invalid("research genesis required"))?;
            payload_bytes(&self.fs, &genesis.scope, EVENT_MAX_BYTES)?;
            // Preview uses the actual next event identity only for validation;
            // append reconstructs and authenticates the final EventRef itself.
            let event = events::new_event(
                &self.run_id,
                i.last_event.as_ref(),
                self.options.clock.read()?.utc_ms,
                payload.clone(),
            )?;
            let (frame, _) = events::encode(&event)?;
            let mut proposed = i.clone();
            apply(&mut proposed, &payload, &events::event_ref(&frame))?;
            let partial_report = local_report_control(&self.fs, i, &payload)?;
            let new_binding = &proposed
                .research
                .as_ref()
                .ok_or_else(|| invalid("research state missing"))?
                .binding;
            if !partial_report {
                live_binding(&self.fs, new_binding)?;
            }
            match &payload {
                EventPayload::ResearchRebound { binding, .. } => {
                    if services.len() != binding.services.len() {
                        return Err(conflict("complete trusted service set required for rebind"));
                    }
                    let mut proven = BTreeMap::new();
                    for service in services {
                        service.recheck(&self.fs)?;
                        let summary = service.summary();
                        if summary.config_fingerprint != binding.config_fingerprint
                            || proven
                                .insert((summary.capability, summary.profile_id.clone()), summary)
                                .is_some()
                        {
                            return Err(conflict("trusted service configuration differs"));
                        }
                    }
                    for expected in &binding.services {
                        let got = proven
                            .get(&(expected.capability, expected.profile_id.clone()))
                            .ok_or_else(|| conflict("trusted service role missing"))?;
                        if got.profile_fingerprint != expected.profile_fingerprint
                            || got.endpoint_fingerprint != expected.endpoint_fingerprint
                        {
                            return Err(conflict("trusted service profile/endpoint differs"));
                        }
                    }
                }
                EventPayload::ResearchFrontierAdmitted {
                    parent_outputs,
                    task_origins,
                    ..
                } => {
                    for output in parent_outputs {
                        let key = i
                            .tasks
                            .keys()
                            .find(|k| owns_output(i, k, output))
                            .ok_or_else(|| conflict("parent output missing"))?;
                        validated_output(&self.fs, i, key, output)?;
                    }
                    live_origins(&self.fs, guard, i, &proposed, task_origins)?;
                }
                EventPayload::ResearchRoundAssessed {
                    task_key,
                    output,
                    citations,
                    ..
                } => {
                    validated_output(&self.fs, i, task_key, output)?;
                    let view = SourceView::from_fs_bounded(&self.fs, 64 * 1024 * 1024, 4096)?;
                    for citation in citations {
                        view.verify(citation, CitationScope::Current)?;
                    }
                }
                _ => return Err(invalid("research transition required")),
            }
            for (key, task) in &proposed.tasks {
                if task_is_active(&proposed, key) && (!partial_report || matches!(&payload,
                    EventPayload::ResearchFrontierAdmitted { tasks, .. } if tasks.iter().any(|new| &new.key == key))) {
                    tasks::bind(&self.fs, &task.spec)?;
                }
            }
            self.append(guard, loaded, payload.clone())
        })
    }
    /// Construct the retry timestamp while owning the admission lock, avoiding
    /// a zero-delay timestamp becoming stale while waiting for another writer.
    pub(crate) fn schedule_research_retry_after(
        &self,
        key: &Blake3Hash,
        attempt: &AttemptRef,
        delay_ms: u64,
    ) -> Result<EventRef> {
        self.paid_gate()?;
        self.with(false, |guard, loaded| {
            if let Some(frame) = loaded.frames.iter().rev().find(|frame| {
                matches!(&frame.event.payload, EventPayload::ResearchRetryScheduled { attempt: old, .. } if old == attempt)
            }) { return Ok(events::event_ref(frame)); }
            let now = self.active(guard, loaded)?;
            let i = &loaded.state.inspection;
            let r = i.research.as_ref().ok_or_else(|| invalid("research retry requires research genesis"))?;
            let prior = i.attempts.iter().find(|a| a.attempt == *attempt)
                .ok_or_else(|| invalid("retry attempt missing"))?;
            if &attempt.task_key != key || (!(prior.phase == AttemptPhase::Settled
                && prior.remote_exposure == RemoteExposure::TerminalConfirmed) && !self.options.policy.retry_uncertain) {
                return Err(WikiError::new(ErrorCode::RecoveryRequired, "retry attempt is not safely terminal"));
            }
            let delay = i64::try_from(delay_ms).map_err(|_| budget("retry delay overflow"))?;
            let not_before = now.checked_add(delay).ok_or_else(|| budget("retry time overflow"))?;
            if not_before >= i.effective_deadline_utc_ms { return Err(budget("retry exceeds run deadline")); }
            let payload = EventPayload::ResearchRetryScheduled { version: 1, epoch: r.binding.number,
                task_key: key.clone(), attempt: attempt.clone(), not_before_utc_ms: not_before };
            self.append(guard, loaded, payload)
        })
    }
    #[cfg(test)]
    pub(crate) fn schedule_research_retry(&self, payload: EventPayload) -> Result<EventRef> {
        let EventPayload::ResearchRetryScheduled {
            ref attempt,
            not_before_utc_ms,
            ..
        } = payload
        else {
            return Err(invalid("research retry event required"));
        };
        self.local_write()?;
        self.with(false, |guard, loaded| {
            for frame in loaded.frames.iter().rev() {
                if let EventPayload::ResearchRetryScheduled { attempt: old, .. } =
                    &frame.event.payload
                    && old == attempt
                {
                    return if frame.event.payload == payload {
                        Ok(events::event_ref(frame))
                    } else {
                        Err(conflict("attempt already has a different retry schedule"))
                    };
                }
            }
            let i = &loaded.state.inspection;
            let prior = i
                .attempts
                .iter()
                .find(|a| a.attempt == *attempt)
                .ok_or_else(|| invalid("retry attempt missing"))?;
            if !(prior.phase == AttemptPhase::Settled
                && prior.remote_exposure == RemoteExposure::TerminalConfirmed)
                && !self.options.policy.retry_uncertain
            {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "nonterminal retry requires explicit uncertain policy",
                ));
            }
            let now = self.options.clock.read()?.utc_ms;
            if not_before_utc_ms < now {
                return Err(conflict("retry schedule is earlier than admission clock"));
            }
            self.append(guard, loaded, payload.clone())
        })
    }
    /// Reserve a deterministic eligible prefix under one run lock. Every
    /// successful append is returned even if a later admission or clock fails.
    pub fn reserve_ready_batch(
        &self,
        candidates: Vec<(Blake3Hash, AttemptBound)>,
    ) -> Result<ReservationWave> {
        self.paid_gate()?;
        self.with(true, |guard, loaded| {
            let now = self.active(guard, loaded)?;
            let mut eligible: Vec<_> = loaded
                .state
                .inspection
                .tasks
                .values()
                .filter(|task| {
                    batch_eligible(
                        loaded,
                        &task.spec.key,
                        now,
                        self.options.policy.retry_uncertain,
                    )
                })
                .map(|t| &t.spec)
                .collect();
            eligible.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.key.cmp(&b.key)));
            if candidates.len() > eligible.len()
                || candidates
                    .iter()
                    .zip(&eligible)
                    .any(|((key, _), task)| key != &task.key)
            {
                return Err(conflict(
                    "reservation candidates must be the complete stable ready prefix",
                ));
            }
            for (key, bound) in &candidates {
                validate_live_bound(&self.fs, &loaded.state.inspection, key, bound)?;
            }
            let mut wave = ReservationWave {
                reservations: vec![],
                stop: None,
            };
            for (key, bound) in candidates {
                let result = (|| -> Result<Reservation> {
                    let now = self.active(guard, loaded)?;
                    let allowance = self.admission(loaded, &key, &bound, now)?;
                    let attempt = AttemptRef {
                        run_id: self.run_id.clone(),
                        task_key: key.clone(),
                        attempt_id: RecordId::new(format!("attempt_{}", uuid::Uuid::now_v7()))?,
                        number: loaded
                            .state
                            .inspection
                            .attempts
                            .iter()
                            .filter(|a| a.attempt.task_key == key)
                            .count() as u32
                            + 1,
                        request_hash: bound.wire_hash.clone(),
                    };
                    let persistence = events::persistence();
                    let before = self.options.clock.read()?;
                    let reserved_event = self.append(
                        guard,
                        loaded,
                        EventPayload::Reserved {
                            attempt: attempt.clone(),
                            bound: bound.clone(),
                            allowance: allowance.clone(),
                            persistence: persistence.clone(),
                        },
                    )?;
                    let reservation = Reservation {
                        attempt,
                        bound,
                        allowance,
                        reserved_event,
                        persistence,
                        genesis_hash: loaded.frames[0].checksum.clone(),
                    };
                    // Keep this opaque reservation even if the flush crossed the
                    // deadline: intent/send still independently refuse it.
                    if let Err(error) =
                        self.authority_gate(guard, loaded, &reservation.bound, before)
                    {
                        wave.stop = Some(error);
                    }
                    Ok(reservation)
                })();
                match result {
                    Ok(reservation) => wave.reservations.push(reservation),
                    Err(error) => wave.stop = Some(error),
                }
                if wave.stop.is_some() {
                    break;
                }
            }
            Ok(wave)
        })
    }
}
fn batch_eligible(
    loaded: &ledger::Loaded,
    key: &Blake3Hash,
    now: i64,
    retry_uncertain: bool,
) -> bool {
    let i = &loaded.state.inspection;
    let Some(task) = i.tasks.get(key) else {
        return false;
    };
    if !task_is_active(i, key)
        || task.spec.capability.is_none()
        || task
            .spec
            .dependencies
            .iter()
            .any(|d| !task_is_active(i, d) || i.tasks[d].state != TaskState::Completed)
    {
        return false;
    }
    if let Some(r) = &i.research {
        if r.retry_not_before.get(key).is_some_and(|time| *time > now) {
            return false;
        }
        if task.state == TaskState::Running && !r.retry_not_before.contains_key(key) {
            return false;
        }
    }
    let prior: Vec<_> = i
        .attempts
        .iter()
        .filter(|a| a.attempt.task_key == *key)
        .collect();
    if prior.len() >= i.effective_limits.attempts_per_task as usize {
        return false;
    }
    if task.state == TaskState::Pending {
        return prior.iter().all(|a| a.phase == AttemptPhase::Settled);
    }
    task.state == TaskState::Running
        && task.outputs.is_empty()
        && !prior.is_empty()
        && prior.iter().all(|a| {
            (a.phase == AttemptPhase::Settled
                || retry_uncertain && a.billing == BillingDisposition::UnknownReserved)
                && (a.billing != BillingDisposition::UnknownReserved
                    || retry_uncertain
                    || (a.remote_exposure == RemoteExposure::TerminalConfirmed
                        && loaded
                            .state
                            .received_meta
                            .get(&a.attempt.attempt_id)
                            .is_some_and(|m| {
                                m.terminal_response
                                    && matches!(
                                        m.status_code,
                                        Some(401 | 429 | 500 | 502 | 503 | 504)
                                    )
                            })))
        })
}

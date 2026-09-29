# Astra P20 accounting/resume design review

Status: architectural review of the current working tree and P20-plan; no implementation or runtime qualification. Only this report was edited. Contracts: providers-jobs §§7–10, P20, CLI mutation/JSONL/cancellation, handoff M4.

## Summary (under 400 words)

Approve the direction, conditional on the changes below. One immutable genesis and one lifetime ledger must survive every research epoch. Retiring a task changes scheduling eligibility, never its attempts, receipts, outputs, consumed requests, unknown holds, or persistence reservations. Current reuse and historical retention are separate decisions.

Use an explicit versioned effective-binding epoch plus an active-task set. A paused/stopped run can atomically replace current source/config/service proofs and retire stale tasks, including their dependent closure; unchanged completed tasks remain eligible only after fresh proof. Preserve all task specifications and old binding epochs. Ordinary task admission and every dispatch authority boundary must require current membership/binding. Recovery must authenticate against the attempt's original binding, even after retirement.

Prefer a conservative rebind barrier: no Reserved attempt, possible in-flight attempt, or live unsent authority may cross it. Proven never-sent reservations can be released; possibly sent requests require the existing reconciliation rules. Terminal responses with unknown billing may remain held and do not prevent rebinding. An epoch is never a timeout refund or a way around remote-inflight reconciliation.

Make round/frontier/origin admission one journal event under the run lock, with an expected prior frontier revision and an idempotency hash. Counters are monotone across epochs. Model strings cannot create this authority. Reserve each ready wave in stable priority/key order under one scheduling critical section, then perform bounded parallel sends outside it. Retrying requests re-enter this ordered scheduler.

Use typed `(profile_id, capability)` service bindings: current provider fingerprints already contain capability, but the run's profile-name-only map cannot represent generation and search together. Preserve old on-disk bytes and fail closed on unknown extension versions.

P20's existing-ledger extraction adapter must factor P18 rather than call its whole-run API. Retain descriptors and stage outputs outside disposable cache. Old paid responses must remain recoverable when the current model/config no longer matches; an undecodable historical response remains protected with conservative accounting, not a rejected schema response or a reason to create another run.

## Source-derived blockers

- `jobs/ledger.rs:600` binds genesis read preconditions and **every** task; callers include admission, start/resume and ready tasks. `:675`, `jobs/replay.rs:240`, and `providers/dispatcher.rs:334` bind new attempts to genesis config/profile identities. Updating only `resume` cannot work.
- `jobs/checkpoint.rs:399` puts genesis preconditions in every Validated receipt draft. `ledger.rs:148` binds sources on remote completion. These must distinguish current publication from historical accounting. `repair_task_finish` at `:2370` may complete an old task from a verified receipt; that must never reactivate it.
- `config/providers.rs:429` hashes `(profile, capability, service_id, service.fingerprint)`; `RunScope.profile_fingerprints` at `jobs/types.rs:263` is only keyed by profile name. `AttemptBound` has config/endpoint/model but no profile fingerprint.
- `jobs/tasks.rs:9` excludes priority and dependencies from task identity. Epoch alone must not force identical work to receive a new key; changed source/model/settings/descriptor proof does. Frontier admission must reject a duplicate key whose *full* TaskSpec differs, especially changed dependencies/priority.
- `dispatcher.rs:309` combines preparation/reservation/retry/send. Parallel calls race for the final slot. `SendAuthorization::check_before_entry_after` (`types.rs:740`) checks the captured clock/limits/cancellation, not a subsequently changed effective epoch.
- `graph/api_extract.rs:143,179,227,429,491` creates/completes its own run. `graph/generation_cache.rs:62` stores descriptors under cache. `research/acquire.rs:425` is fresh-work orchestration, repeatedly calling `add_tasks`; use `public_fetch::{retained_capture,recover_response}` for old responses.

## Minimal shared interfaces and durable events

Names are proposals; freeze equivalent invariants before assigning workers.

```rust
ServiceBindingV1 {
    profile_id: String, capability: Capability,
    profile_fingerprint: Blake3Hash,
    endpoint_fingerprint: Blake3Hash,
}
BindingEpochV1 {
    version: u32, number: u32,
    config_fingerprint: Blake3Hash,
    source_snapshot: Option<ReadSnapshot>,
    input_records: Vec<RecordRef>, read_preconditions: Vec<ReadDependency>,
    services: Vec<ServiceBindingV1>, // sorted, unique (profile, capability)
}
ResearchGenesisV1 {
    version: u32, scope: BoundedPayloadRef, // hash == scope_payload_hash
    initial_binding: BindingEpochV1,
    limits: ResearchAdmissionLimits, // rounds/sources, bounded scope consistency
}
ResearchStateV1 {
    binding: BindingEpochV1, binding_event: EventRef,
    active_tasks: BTreeSet<Blake3Hash>, frontier_revision: u64,
    rounds_started: u32, origins: BTreeMap<OriginKey, OriginAdmission>,
    // bounded durable round assessments/support-group state
}
```

Add an optional versioned research genesis extension to RunScope (absent for legacy runs), and derived research state to inspection/checkpoint projections. Initial tasks are active. Keep retirement separate from TaskState: old Running/Completed states still explain historical recovery. Epoch history and attempt-to-epoch mapping can be reconstructed from event order; avoid changing legacy AttemptRef/receipt identity. New opaque reservations carry the epoch event hash; replay records the binding effective at each reservation. Old nonresearch runs retain their current binding rules.

Add these bounded events/methods:

1. `ResearchRebound { expected_epoch, expected_frontier_revision, amendment_id, next_binding, active_keys, new_tasks, reason }` through `rebind_research(...)`. Only caller-requested resume may append it in Paused/Stopped state. Validate the complete replacement proof, new descriptors, dependency closure, full duplicate-task equality, trusted service summaries and barrier under the lock; publish membership/epoch atomically. Retired keys stay immutable and cannot become current merely because late recovery finishes them. Repeated identical amendment returns the existing event; same ID/different bytes conflicts. Rebind cannot change scope, research limits, deadline or any accounting counter. Resume may then transition to Running using existing control gates. A failed transition after successful rebind is safely resumable.
2. `ResearchFrontierAdmitted { epoch, prior_revision, admission_id, round, new_origins, tasks, parent_outputs }` through `admit_research_frontier(...)`. Under the lock validate current epoch/revision, exact parent output hashes, one next round (or an addition to its already admitted frontier), source/round limits, bounded normalized URLs, unique origin keys, full task definitions and dependency closure. Atomically add tasks and monotone origin/round records. Redirect tasks reference the original admitted origin, spend request slots, and cannot create an uncounted origin. Failed/unsupported origins stay consumed. Exact replay is idempotent; conflicting replay fails. Generic `add_tasks` must not bypass this gate for research remote work. A descriptor may be retained before admission; an orphan descriptor conveys no dispatch authority.
3. `ResearchRoundAssessed { epoch, round, assessment_task, output, support_groups, ... }` or equivalent authenticated frontier transition. Record each round's assessment once, only after its canonical output/receipt is verified. Two successive rounds without new verified support stop scheduling. Preserve assessments across epochs; revalidate current support before reuse. Never reset source/round counters on rebind.
4. If research-limit raises are supported, a distinct caller amendment event validates bounded monotone raises, records reason, and cannot alter consumed counts. No generated schema accepts amendments. Existing `LimitAmendment` remains the only lifetime-budget/deadline route.

All admission/replay/ready/local-finish/current-publication checks use active membership and dependency closure. Keep historical output hash integrity checks separate from mutable-source freshness checks. `complete_run` requires all active tasks complete **and all historical attempts safely settled**; unknown billing can remain reserved as today. Retired unfinished tasks alone must not block completion. Missing/corrupt old accounting still blocks the whole ledger.

For roles, do not concatenate strings or fabricate duplicate user profiles. Dispatcher resolves the exact capability binding and compares trusted config/profile/endpoint fingerprints. Consider adding an optional profile-fingerprint proof to the versioned reservation payload, or authenticate it through a stored task-to-service binding; comparing only profile-name presence in ledger replay is insufficient. Public Fetch has its own fixed noncredential binding. Old retained attempts use original bound/descriptor/epoch, never the new service implicitly.

## Dispatch and recovery details

`execute_ready_batch` should privately prepare candidates, then call one ledger `reserve_ready_batch(expected_epoch, expected_frontier_revision, ordered_candidates)` critical section. The ledger recomputes ready eligibility and stable `(priority,key)` ordering; it admits a deterministic affordable prefix subject to concurrency/rate/lifetime limits. Sequential durable reservation frames within the lock are sufficient: a crash leaves an authenticated prefix, not an unjournaled batch. Return opaque reserved work; perform credential resolution, intent/authorization and network work afterward. Concurrent schedulers either observe the prefix or receive a stale-plan conflict. A caller must not select a lower-priority subset to win the final slot. Local work and unreconciled/retry-pending work need explicit scheduler eligibility, not an accidental `Pending`-only filter.

Refactor the existing retry loop so retries join later ordered waves with their Retry-After/backoff eligibility and new reservations. Do not retain a private retry race inside each parallel worker. Preserve every worker's receipt/spool on cancellation or sibling failure. Extend the private prepared-work union to Fetch if capture runs in parallel.

Check epoch/membership again at intent and begin-send. The recommended quiescence barrier blocks rebinding while DispatchIntent/SendAuthorized could still execute, so a send token cannot outlive its epoch. A design permitting rebind with those authorities instead needs cross-process epoch validation at actual transport entry, plus a race-free fence through entry; an ordinary earlier `inspect()` is inadequate. Never declare a reservation not sent merely because it belongs to an old epoch.

Historical response decoding needs the immutable original wire contract, not the current TrustedService. Persist the nonsecret codec/model/schema/settings facts required for offline decoding, or provide a historical decoder authenticated by original descriptor/bound/response hashes. This grants no network/helper authority. Without a decoder, retain protected raw spool and an Unknown disposition receipt; do not misclassify local/current-config mismatch as invalid provider output. Current Validated publication uses the attempt's applicable epoch/task dependencies and current SourceView; stale outputs remain historical. Previously committed receipts are never rewritten. Rebinding can proceed with terminal-confirmed unknown holds; outstanding possibly-in-flight work retains the existing reconciliation stop.

`execute_existing_task(ledger, task_key, verified_packet, ...)` must retain P18's exact receipt/output/import idempotency, return staged proposals, and never create/complete the containing research run. Canonical generation-stage records and descriptors must survive `.wiki/cache` deletion. Report publication retains verified citation dependencies through the changeset; valid bytes establish provenance, never entailment or acceptance.

## Schema/replay requirements and acceptance

Use `default + skip_serializing_if=None` for absent legacy extensions so old genesis/task/spec/checkpoint hashes remain byte-identical. New research events carry a strict extension version; update event routing, schema registry, projection validation, complete-history replay and Markdown-only fallback together. Reject unknown versions and malformed/unsorted/duplicate bindings. Validate transitions and limit arithmetic in replay as well as live methods. Existing ceilings remain 4,096 lifetime tasks and 256 KiB per event (`jobs/types.rs:22–23`); never dump an unbounded active set/frontier into one event. Count new control events against journal capacity without spending reserved receipt/cleanup space. A run-note projection cannot authorize a rebind or recover missing operational accounting.

Required hard cases beyond the P20 named workflow tests:

- Source edit and model/config edit retire dependent work; unchanged completed work reuses exactly; genesis hash, prior task/receipt hashes and lifetime counters remain unchanged. Historical stale sources do not block unrelated active work; stale keys cannot reserve, finish current work, or reactivate through receipt repair.
- One profile serves Generate and Search, while wrong role/fingerprint/config/endpoint fails before helper/reservation/send. Restoring current config never changes old receipt identity.
- Rebind races Reserved, intent and send authorization; barrier wins safely or rebind fails. Unknown nonterminal work prevents resume; terminal unknown billing permits new work only with the full hold still counted.
- Two processes race the final source, round and request slot; duplicate frontier admission is idempotent, same ID/different payload conflicts, and the stable smallest eligible task wins the request slot. Retries and redirects cannot bypass counters/order.
- Kill before/after rebound/frontier/reservation-prefix/receipt-ack writes; replay preserves exact accepted events and accounting. A late old receipt cannot make its output current. Current-model change never discards an old protected spool.
- Failed fetch consumes its origin; redirect consumes requests; mirrored bytes do not manufacture new support. Epoch changes do not reset no-progress history, rounds, origins, deadline or lifetime task ceiling.
- Cache deletion preserves descriptors/stage results/old accounting; dry-run leaves the complete tree unchanged; budget stop renders deterministic partial output without another model call. Explicit-URL flow needs no Search profile.

Validation performed: source/contract inspection only, no Cargo/tests/network calls. These recommendations do not qualify implementation, provider compatibility, native crash safety, or model quality.

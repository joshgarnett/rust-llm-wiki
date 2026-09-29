# P20 design packet (read-only planning)

Status: proposal only; P17/P18/P19 integration is not yet accepted. No source, Cargo, Git, schema or CLI edits are authorized by this packet. Root freezes shared interfaces and leases implementation after dependencies pass.

## Summary (under 400 words)

Use one existing `JobLedger` for the entire research run. Persist immutable question, exclusions, explicit URLs, role/profile bindings, retrieval/extraction settings and research limits; put its hash in `RunScope.scope_payload_hash`. The run note's `lwiki.run-plan.v1` projection lists stage keys, dependencies, status and durable outputs derived from ledger history. It is a projection, never a second accounting authority.

Execute stable `(priority,key)` ready tasks through inspect-existing, generation frontier, admitted search pages, separately admitted capture/redirect hops, packet-bound extraction, generation gap assessment, generation synthesis, then local staging. Default inspection uses verified lexical/context retrieval; no implicit embedding/query-expansion call. Explicit URLs require generation for model stages but no search profile or subscription. Model strings cannot dispatch, alter limits, choose filesystem paths or apply changes.

All paid stages use the actual production `Dispatcher`. Preserve its raw response, valid/rejected receipt and settlement before advancing. Retain canonical stage output records for frontier/gaps/synthesis and capture provenance for redirects. Resume repairs journals/changesets, adopts protected orphan responses, then reuses only matching input/prompt/schema/model/settings/source proofs. P19's `fetch_url` is fresh-work orchestration; resume uses `retained_capture`, `recover_response` and committed output references instead.

Verify every citation through current `SourceView` and retain byte/hash/ownership dependencies on report publication. Citation validity proves origin, not entailment: model claims stay explicitly unassessed, unsupported claims are gaps/proposals, and assertions are never autoaccepted. Reports include coverage/omissions and unanswered questions; staged changes apply only under explicit caller apply mode. Budget/cancellation/unknown-outcome stops preserve deterministic partial reports without a final model call.

Core root deltas: factor P18 extraction into an existing-ledger task path; support explicit versioned source/profile rebinding without resetting history; bind generation/search services by role as well as profile; and atomically admit research rounds/sources. Current `bind_inputs` validates all historical task dependencies and immutable genesis configuration, blocking changed-source/model continuation. Role-specific profile fingerprints currently collide under one profile-ID key, preventing one configured profile from serving both generation and search in a single run.

## Proposed public API packet

Root-owned `src/research/types.rs` defines strict versioned DTOs and references existing jobs/domain types:

```rust
ResearchLimits { rounds: u8, sources: u16, search_count: u8,
  search_pages: u8, fetch: FetchLimits, retrieval: QueryPlan,
  extraction: ExtractionLimits, stage_output_tokens: u64 }
// Default rounds=3, sources=15, search_count=10; requests/deadline/money
// remain existing LifetimeLimits and RunSpec, default60 requests/15minutes.
ResearchScope { version: u32, question: String, exclusions: Vec<String>,
  explicit_urls: Vec<String>, generation_profile: String,
  search_profile: Option<String>, limits: ResearchLimits, apply: bool }
ResearchStageBinding { version: u32, scope_hash: Blake3Hash, epoch: u32,
  round: u8, stage: TaskStage, parent_outputs: Vec<DurableOutputRef>,
  citation_pool: Vec<CitationRef>, input_dependencies: Vec<ReadDependency> }
ResearchRuntime<'a> { generation: &'a TrustedService,
  search: Option<&'a TrustedService>, dispatcher: &'a Dispatcher,
  public_fetch: &'a PublicFetchOptions, job_options: JobOptions }
ResearchPlan { scope: ResearchScope, spec: RunSpec,
  initial_descriptors: Vec<(TaskSpec, Vec<u8>)> }
ResearchStatus { inspection: LedgerInspection, rounds_used: u8,
  sources_used: u16, gaps: Vec<ResearchGap>, latest_report: Option<DurableOutputRef> }
ResearchOutcome { status: ResearchStatus, report: Option<ResearchReport>,
  prepared_changes: Vec<PreparedChange>, network_used: bool }
```

Application entry points: pure `plan(scope, bindings, snapshot, now)`, `create(plan, writer)`, `run(run_id,runtime)`, `resume(run_id,runtime,amendments)`, read-only `status(run_id)`, `report(run_id,format)`. Dry-run returns a read-only plan with unknown remote work and no retention/ID/directory/index refresh. Offline can read or explicitly publish local partial outputs but cannot schedule remote work. Runtime receives injectable production dispatcher/DNS/connection/clock/cancel/fault boundaries; mocks never introduce a separate execution branch.

## Stage mapping and proof obligations

| TaskStage | Capability / execution | Durable result |
|---|---|---|
| InspectExisting | None; bounded verified lexical/context and source passages | Snapshot, citation pool, coverage/omissions |
| PlanFrontier | Generate; strict `lwiki.research-frontier.v1` | `{queries,urls,reason}`; bounded untrusted leads |
| Discover | Search; one Brave GET page per task | Canonical lead record; snippets are never evidence |
| Capture | Fetch; one public GET/redirect per task | Immutable sources or explicit gaps; protected URL/media/time provenance |
| Extract | Generate; existing packet/export/schema/import logic, same ledger | Raw stage output/receipt and staged extraction proposals; no auto-resolution/review |
| AssessGaps | Generate; strict `lwiki.research-gaps.v1` | Covered known evidence IDs, gaps, next queries/URLs, stop; stable source-support groups |
| Synthesize | Generate; strict `lwiki.research-synthesis.v1` | Citation-checked sections/claims, unanswered questions, bounded proposals |
| StageChanges | None; expected-hash changesets | Immutable report and prepared change references; explicit apply only |

Generation descriptors are existing canonical `RemoteInput::Generate`; encode stage binding as bounded data, not a new provider operation. Bind schema/prompt/settings/model fingerprints using `wire::task_fingerprints`. Stage-output identity is deterministic from task/attempt/response hash. Materialize stage output plus an actual `checkpoint::receipt_plan` carrying its exact hash, then acknowledge, settle and complete. Unknown/rejected responses never become validated task completion. Local stage outputs use `finish_local_task` after actual changeset publication.

Schemas reject unknown fields, control/oversized query/URL/text arrays, unknown citations/record IDs and invented path fields. `covered_evidence_ids` means real known current evidence-record IDs; direct captured source passages live in the separately verified citation pool and may exist without evidence notes. Count new supported material by verified `(revision,span,quote_hash)` / original-content support groups, never search snippets, model claims, mirrored origins or mutable titles. Persist each assessed round result; stop after two consecutive rounds add no new supported group. Every distinct requested origin consumes the source cap before dispatch, including failed/unsupported acquisition; redirect hops consume request limits but do not manufacture another independent source.

`ResearchProposal` should be a narrow tagged union: proposed page creation/update with bounded title/body, citations and optional existing `RecordRef`; references to prepared extraction proposals. Forbid raw target paths, commands, accepted-status switches, credentials and limit changes. Root chooses paths/IDs and preserves expected states. Generated prose is labeled unassessed even with byte-valid citations; unsupported/unverifiable claims are excluded from accepted factual assertions and remain visible in gaps/proposals.

## Required root-owned deltas before worker leases

1. Register research modules, the three exact generation-stage schemas plus run-plan schema, and CLI `plan/run/resume/status/report` arguments/envelopes. JSONL emits existing presentation events and terminal envelope; durable ledger remains authority. No progress output exposes private spool/provider bodies.
2. Expose/factor P18 `execute_existing_task(ledger, task, verified_packet, service, dispatcher)` and durable input/output/receipt materialization primitives. It must never create another run, refresh lifetime budgets, complete the research run early, or auto-review/accept proposals. Current `generation_cache::plan_task` can supply the Extract descriptor; existing private import/retention helpers need a bounded adapter.
3. Add ledger-atomic research-frontier admission, e.g. `admit_research_frontier(epoch,round,origin_keys,tasks)`, with root-owned immutable research-limit proof. Record one bounded event binding new task keys/origins. Check round/source/task/concurrency-related counters under the existing run lock; repeated identical admission is idempotent. No root writer lock is held across HTTP.
4. Define binding revalidation/amendment events and effective bindings separate from immutable genesis: current source snapshot/preconditions, config/profile fingerprints, active task keys and retired stale keys. Revalidate trusted service/source proofs explicitly on resume; settle/reconcile pending old attempts first. `admission`, `resume`, `bind_inputs` and run completion must use the effective active proof set while preserving all old tasks/outputs/receipts and lifetime allowances. Changed proofs create new task keys; matching completed work remains reusable. A new model/config cannot silently pass old genesis checks, and stale historical tasks cannot globally block new authorized work.
5. Freeze role-aware service binding identity. `ProviderConfig::authorize` fingerprints `(profile,capability,service,fingerprint)`, while `RunScope.profile_fingerprints` stores only one value per profile name and Dispatcher checks that value. Both generation and search commonly select the same profile, so their differing fingerprints cannot fit. Add bounded unique typed service bindings keyed by `(profile_id,service_role)` (a serialized list avoids ambiguous map-string separators), and validate each task/prepared service against that role. Preserve existing single-capability clients without demanding duplicate user profiles. Public-fetch fixed settings remain an independent Fetch binding with no trusted search service dependency.
6. Persist round/source/no-progress counters through the above history and canonical stage outputs; limit raises/deadline extensions require recorded caller amendments, never model proposals. Keep request/byte/money counters in the original JobLedger. Immutable research-scope/task payloads must survive SQLite/cache deletion; do not retain them solely under `.wiki/cache/generation`.
7. Supply an opaque dispatcher batch entry for bounded parallel work, e.g. `execute_ready_batch(ledger,ordered_work,services,public_options)`. Root privately prepares and reserves in stable priority/key order before polling parallel network futures, then retains every paid outcome. Existing synchronous `execute` combines reservation and sending internally; invoking it from parallel threads makes final-slot selection scheduling-dependent and cannot satisfy reservation-before-parallel-execution. No public pre-encoded request/SendAuthorization constructor is introduced. Retries remain separate admitted attempts and cancellation releases only proven unentered work.

## Acceptance packet and limits

Implement all ten P20 named tests using disposable vaults, actual library calls and concrete CLI subprocesses with injected mock providers. Include full explicit-URL path with no search subscription; unknown IDs/paths/schema and unsupported entailment; paid-stage capture/import/report receipt chain; two rounds without new evidence; source/model epoch change under the same run; 60-request/source/round/deadline lifetime accounting across resume; metadata-before-Received and receipt/apply restart boundaries; unknown-attempt cancellation with preserved reports; cache deletion; deterministic budget partial output with no synthesis call; full tree-hash equality for dry-run. Root obtains Astra review of rebind/admission/citation/report invariants before acceptance. These fixtures establish local correctness, not live provider interoperability, native other-platform crash safety or model-quality/entailment guarantees.

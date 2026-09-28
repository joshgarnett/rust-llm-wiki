# P15 independent accounting/storage review

Independent `gpt-6-sol` fallback because Astra is unavailable at the runtime thread limit; root performs a separate invariant review and retains every gate. Base accepted P10 `24d7911f9b112b9a931c73c7cc2201b4232721df`. This report is the only reviewer write lease. No source/test/Cargo/Git changes, nested delegation, providers/helpers or production access. Status: **final changed-source rereview complete, findings source-closed conditional on actual gates; focused runtime evidence attributed below; full gates pending, not acceptance**.

Selected reads: P15 package; providers/jobs sections1 and7–10; run/run-event schema and recoverable storage/cancellation contracts; jobs types/leaves and private vault operational helper. Findings below are source-derived; the reviewer has not executed reproductions or tests.

## R1 — enforce monetary/usage violations independently and across recovery

Initial blocking source evidence: `record_response` and `reconcile` compare known computed cost/currency against the admitted allowance only inside a Known usage branch. A response with usage Unknown but cost Known above allowance leaves `guarantee_intact` true and its smaller allowance reserved. Known monetary overspend cannot be conditional on token/usage completeness.

The same invariant is lost across response persistence faults: Received is appended before Reconciled/BoundViolated. AfterReceived interruption leaves no violation event; orphan spool replay appends Received/Reconciled with Unknown usage/cost without checking the retained actual metadata. Repeated identical `record_response` returns an existing spool before repairing those observations. Even fully known overbound usage can therefore avoid the required stop through recovery.

Requested fix: one canonical observed-response check for money and units, used on initial response, idempotent retry and orphan/Received recovery. Persist overspend/guarantee invalidation before further admission; keep unknown unit reservations separate from observed money. Regressions: Unknown usage with known overcost/wrong currency, overbound metadata at AfterReceived and complete orphan boundaries, reopen/retry/admission must remain stopped. Root/worker notified; resolution pending.

Class-completeness variant: `usage_violates` checks undeclared classes only for Known counts; an undeclared Unknown class is ignored. `actual_allowance` then iterates declared classes only and can settle known while the provider reports an extra unknown billable class. Presence of an undeclared billable class must invalidate completeness independently of its numeric count. Sent to root as part of R1.

## R2 — repair task completion after output acknowledgment interruption

Initial blocking source evidence: `outputs_committed` appends OutputsCommitted, exposes AfterOutputsCommitted fault, then appends TaskFinished for validated canonical outputs. A crash leaves the attempt OutputCommitted and its task Running. Idempotent acknowledgement returns its existing event before repairing TaskFinished; replay does not repair it. After settlement, retry admission sees Running plus settled attempts and empty *task* outputs, so can admit another paid attempt despite already committed validated output. Descendants also remain unready.

Requested fix: repeat/replay verified acknowledgment must complete the task exactly once without another dispatch, using the exact committed receipt/output association. Test interruption, reopen, reack/settle, completed task/dependency readiness and rejected paid retry. Root/worker notified; resolution pending.

## R3 — accounting persistence must survive source drift

Source policy concern sent to root: the shared checkpoint draft copies immutable run scope read guards into every receipt/checkpoint changeset. A source edit after dispatch blocks even receipt-only Unknown/Rejected paid-response retention through storage's base guard check. Accounting-only persistence should retain the real paid receipt/checkpoint while stale inputs block new dispatch or derived knowledge activation. Derived output proposals can retain their own guards. Root policy and meaningful drift-after-send fixture pending; no runtime failure claimed yet.

## R4 — prior history disclosure must validate its head

Initial blocking source evidence: `check_prior` calls history complete when the old journal decodes to a nonempty event list and a head file merely exists. A valid shorter journal prefix plus unchanged current head fails ordinary old-run loading, but passes this new-run disclosure shortcut. New-run creation can then omit prior-accounting uncertainty despite unknown missing charges.

Requested fix: reuse exact genesis/spec/head-prefix/length validation for prior history, or conservatively classify a mismatch unknown. Test valid-prefix truncation retaining the newer head: old run cannot resume and new run requires explicit prior unknown disclosure. Root/worker notified; resolution pending.

## R5 — amendments cannot invent retrospective hard unit caps

Initial blocking source evidence: amendments reject None-to-Some byte/money ceilings, but check unit ceilings only for classes already limited. New class limits can be added after Unknown/Estimate historical units were admitted; those allowances omit that class, so lifetime fits treats it as zero. For example a formerly unbounded Input class becomes capped at0 despite retained unknown input usage. Request/token minute limits also lack preserve/raise validation.

Requested fix: reject a restrictive cap introduced from unlimited history, or require complete retrospective proof; maintain the recorded explicit preserve/raise policy for rate limits as well. Unknown history must not acquire a claimed hard unit guarantee. Root/worker notified; resolution pending.

## R6 — retain an append path for reconciliation after cleanup

Initial blocking source evidence: SpoolRemoved on KnownSettled clears the attempt's persistence slots. Replay's event preamble requires a remaining attempt slot for every later Reconciled/BoundViolated event. The reconciliation API accepts settled known attempts, but after normal verified cleanup can append neither a legitimate billing correction nor a subsequently discovered overbound charge. A failed BoundViolated append leaves the old guarantee intact.

Requested fix: reconciliation/guarantee corrections after terminal cleanup must remain appendable under checked global/control capacity, without assuming still-reserved terminal-attempt space. Preserve immutable receipts and record amendments. Test known settlement/cleanup followed by provider correction and overbound reconciliation; unknown settlement/cleanup followed by fully known reconciliation should release no-longer-needed terminal persistence allowance. Root/worker notified; resolution pending.

Related capacity audit: preterminal optional Reconciled observations consume the same12 slots as mandatory paid-response/receipt/settlement/cleanup events. Distinct optional observations must not exhaust mandatory receipt capacity while journal space remains. Fixed control headroom must also be usable for final stop/pause rather than always reserved again afterward. Sent to root for persistence-policy review.

Follow-up on the current urgent_control fix: it releases the two control slots only for the urgent event itself. The next mandatory ordinary attempt event re-reserves both slots. At exact event capacity P+R+2=maximum, an urgent stop consumes one control slot; a terminal attempt event consumes one reserved slot but its capacity check becomes P+R+3. Paid receipt persistence can therefore still fail after a stop/violation. Control headroom consumption must persist in the resulting paused/stopped state or be tracked explicitly, and restart must recheck available headroom. Root/worker notified; this arithmetic boundary remains open pending fix/test.

## R7 — distinguish admission rate windows from dispatch rate windows

Source policy question: rate checks count Reserved timestamps only and do not run at intent/send authorization. With requests/minute1 and concurrency2, reserve A in minute0 and B in minute1, then authorize both sends in minute2. Actual sends can exceed the advertised dispatch-minute control. Token windows have the same shift. Require send-time held-capacity enforcement, or explicitly define/lower the advertised contract to admission rate only if that is an accepted decision. No runtime burst test or live provider claim; root classification pending.

## R8 — checkpoint event size must cover allowed task counts

Source capacity finding: RUN_MAX_TASKS4096 but checkpoint acknowledgment copies every completed task hash into one Checkpoint event. 4096 JSON hashes exceed256KiB before event/framing fields. Tasks can be added in bounded batches and completed locally without reaching journal size/event ceilings. The canonical checkpoint can therefore commit before its operational acknowledgment fails encoding, leaving a new canonical run hash unmatched by the old operational binding.

Requested fix: compact/hash/chunk the completion summary or preflight the exact acknowledgment encoding before canonical commit; a valid allowed task population must not create an unacknowledgeable checkpoint. The existing exact prefix/run-byte binding may make the redundant list unnecessary. This is an exact size/source argument, not an executed4096-task workload. Root notified; classification/fix pending.

## R9 — canonical mirror backlogs must be chunkable

Root requested a separate receipt-capacity audit. Current receipt_plan_locked writes supplied guarded outputs plus the deterministic receipt only; it no longer requires mirroring the complete unrelated history, so mandatory receipt persistence is structurally independent of mirror backlog. Runtime coverage remains pending.

checkpoint_plan still puts every unmirrored journal event into one changeset. Valid operational history permits65536 frames, while storage MAX_OPS is10000; optional observations can exceed the latter without exhausting the former. A backlog above10000 therefore cannot be staged through the public checkpoint planner. Bounded batches should advance the canonical RunPlan only through an exactly acknowledged fully mirrored prefix, then repeat, while retaining the full operational ledger. Root/worker notified; chunking policy/regression pending. The4096 task bound alone does not exceed MAX_OPS, and this finding does not claim that it does.

## In-progress source rereview

The descriptions above retain the initial findings. Current source structurally addresses R1–R5/R7 and most of R6: observations independently check cost and class completeness, enrich interrupted/orphan response state before admission and journal the stop; verified output acknowledgments repair TaskFinished; accounting-only receipt/checkpoint plans omit mutable source guards; prior disclosure loads the complete prior head/history; amendments reject new restrictive unit/rate ceilings; optional corrections do not consume terminal-attempt slots; SendAuthorized replay checks actual send-time RPM/TPM before durable authority returns. R6's control-slot follow-up above remains open. The source contains focused regressions, but final execution is pending. They are not yet runtime-closed findings.

R8 now uses a fixed-size CompletedTaskSummary count/fingerprint over sorted complete keys. Genesis encoding is checked by initial_inspection/events::encode before bootstrap_plan returns and before create applies canonical Markdown; large initial specs cannot strand a canonical run. A task population may reach4096 through bounded TasksAdded events. The full-size summary/checkpoint regression and final source stability remain pending.

## Frozen candidate rereview

Root snapshot `/tmp/lwiki-p15-candidate-hashes.json` binds the source reviewed in this pass. R6's follow-up is now source-closed: history_capacity reserves control headroom only for a resulting Running/intact-guarantee state; pause/stop/broken-bound state retains the release for later mandatory paid lifecycle events. A Running transition must fit the full control reserve again. Optional corrections do not consume terminal-attempt slots. The small-cap regression spends one urgent control slot, then every reserved receipt slot, and refuses restart throughout.

Reviewer independently compared all19 snapshot files with current SHA256s; zero mismatches at this pass. Principal frozen hashes (not the later fix/gate hashes):

```text
704f7a3d97556b1b6cfe527588ea71fe87c68381c83b00016d12332b4e90971b src/jobs/ledger.rs
3c6ae6d6fbcf9d66bc6f24dbc7499ebc9cc6b6a88ade28cd51349769db0a34c8 src/jobs/types.rs
22611f0c44f0131fa152a3b0325ff6005d6d9a5bbfbe9b4eabaeb24e573575f3 src/jobs/events.rs
da5410da77527b62f6a36a5431d12f339b31c39dc8e17d3e8ebba2f73a625d3c src/jobs/checkpoint.rs
d6f74de9c980213b2221d6a466de1c935ce83f0e68ba2b1f1ae1b6e55aca76bf src/jobs/replay.rs
d94bcebc42744bb45ea0027004eadfc576fcb7cd1a458413fa10afc38bc5b3c3 src/jobs/budgets.rs
57e8f84074b4b24ec934d78908027e587a0b3f83d666e706318a12ac9dec151f src/jobs/accounting_tests.rs
9175f47da59f10025d772291d00efd47494ff6132425e6d9ec0a1c591a3bbb38 tests/job_accounting.rs
332cb222de29279d8d5e23258dc46a272f58954e089330469c3c74dde9b6b823 src/vault/operational.rs
```

R9 is now source-closed: checkpoint batches include at most256 missing mirrors and16MiB combined payload, construct the exact fully mirrored prefix RunPlan and acknowledge only that prefix. Complete operational history is retained. The real two-batch changeset/ack fixture preserves lifetime dispatch counts and attempt identity. Receipt staging remains independent of that backlog. These source closures await actual final runtime gates.

D34 explicitly keeps a violated bound permanently non-admitting; raising caps does not repair the historical guarantee. Unknown charges and remote exposure remain independent. Postpaid APIs now request operational binding only while canonical plans/new paid authority remain strict. The additional R10 below identifies a remaining unconditional canonical dependency in that intended separation.

## R10 — operational paid-response retention still requires historical canonical checkpoint bytes

Blocking frozen source evidence: load(bind=false) unconditionally calls checkpoint::verify_summary_proof for every historical Checkpoint before its final bind branch. That helper requires either the original canonical run bytes or a retained committed changeset payload. Thus canonical byte loss can still block operational accounting even when complete checksummed journal/genesis/head remain intact.

Source-derived reproduction: create/start; commit and acknowledge a checkpoint; authorize a paid attempt; edit/delete canonical run.md and discard retained changes while preserving `.wiki/state/jobs` journal/head; record_response/reconcile now fail during load before retaining paid metadata. The current canonical-run-drift regression contains no earlier checkpoint, so does not exercise this path. Operational-only accounting must preserve charges from complete history without requiring historical canonical payload availability; strict canonical/new-authority/ack verification can retain those checks. Root/worker notified, fix/regression pending. No reviewer execution claimed.

## R11 — complete prior history can still contain undisclosed unknown spend

Frozen check_prior tests only whether loading a discovered prior run succeeds. It does not inspect UnknownReserved paid attempts. Source-derived reproduction: run A reaches dispatch_intent and loses its permit with complete journal/head and unchanged canonical run; create run B with PriorAccounting::None. The prior load succeeds, so no disclosure is required despite a possible earlier charge. Section10 requires a new run to disclose previous unknown spend, not only missing accounting history. Explicit prior IDs must cover complete histories whose paid charges remain unknown. Root/worker notified; fix/regression pending.

Discovery variant sent to root: only valid canonical runs/*/run.md envelopes are scanned. A deleted/corrupt old canonical run with retained private journal/head is omitted altogether. A bounded union of canonical and private run identities can preserve prior-accounting disclosure; exact vault/run/genesis validation and conservative unknown classification are needed. Root policy/fix pending. No reviewer execution or global cross-run dollar limit claimed.

## Final changed-source disposition

R10 is source-closed in the final candidate: unavailable historical canonical checkpoint proof produces an operational-only warning for bind=false; bind=true remains strict. Inspection can expose complete journal/head accounting with warnings, while new paid authority/current canonical plans stay blocked. The focused test first commits/acks a checkpoint, dispatches, deletes canonical run/retained changes, then retains paid response/cost and confirms no canonical recreation or new authority.

R11 is source-closed: prior discovery unions possible IDs from exact owned canonical run layout, valid run envelopes and bounded private jobs namespaces. Each prior load must prove its complete history and have no unresolved unknown charges; otherwise the new run requires explicit prior IDs/reason. Tests cover complete-history unknown intent, deleted canonical note, and malformed/future owned restoration without private history. Private discovery rechecks vault binding, rejects malformed/symlink/non-directory namespace entries and caps enumeration at4096. It grants no typed authority from a directory name.

D35 now treats positive RateCard.version as pricing revision, with full-content fingerprint and unchanged complete class/rate/validity/currency checks; zero rejects. Oversized paid bodies record observed bytes/cost and invalidate admission without storing the oversized body. Stopped/paused local completion is limited to already planned ready nonremote tasks with exact input/output proof; no task expansion or paid restart follows. R1–R11 have no remaining concrete source blocker in this bounded rereview. Runtime fault/bootstrap/schema/full-dry and integration gates must still pass; this is not acceptance.

Final reviewed SHA256s, superseding the earlier frozen candidate for changed files:

```text
61219082007de49cd8bf21c911458cabcd45757a6853546686b685266b06ffc7 src/jobs/ledger.rs
43851bfd86806d877ce303ded035c919fd6cd553a51fdb3864ee562786053345 src/jobs/types.rs
22611f0c44f0131fa152a3b0325ff6005d6d9a5bbfbe9b4eabaeb24e573575f3 src/jobs/events.rs
da5410da77527b62f6a36a5431d12f339b31c39dc8e17d3e8ebba2f73a625d3c src/jobs/checkpoint.rs
508c38b9f5c3811e5424d2ae36cf8cc4ecf5c4165cf0e920609147f5f1d9522b src/jobs/replay.rs
c51945ae6fcd19597b33d7b60f164ab38240c7f93d2aaac38a8adf64aa54162a src/jobs/budgets.rs
2b7fc4ed2f4cb0ef59e52d5c2dbb1ad028dd1add022ace6b1bae5f1648d1f0f9 src/jobs/accounting_tests.rs
82c544f5ad3ce3dd03c8566b94294e1441e0cb1f2fe4ac71d600a6c6f63711d8 tests/job_accounting.rs
8ae076e1b0b3385945b748d6f92a0de3aa7070759879df26516d30692c8a6182 src/vault/operational.rs
3fabdbe054008ddd23b49e062de94b89cff0ad1eecf9a96c46e1cd49eb2effcc src/vault/operational_tests.rs
40c5bbf96d95a771019d0f91c1e203ef90c897d3e19ebd1b663e36b46ec4c525 schemas/run-v1.json
d248b9a6fd5f25f558697115582821bd8f1a2fb0b38be0929b91fcdab2352117 schemas/run-event-v1.json
846626ad8e0179037e0400052359f2f33d0bd6312b28050fc2e45bac3425243a schemas/usage-receipt-v1.json
```

## Boundaries inspected so far

Checked integer nanounit parsing/rate multiplication/upward rounding, currency checks, complete applicable class/rate lookup and validity intervals are used before admission. Admission counts settled plus outstanding plus proposed money/requests/bytes/units and serializes concurrency under a fixed cross-process run lock. Unknown billing and possible remote exposure are separate; phase/output completion alone does not recreate send authority. Reservation/intent/send values are opaque, noncloneable and non-deserializable; private dispatcher APIs bind genesis and stored events, with head publication before returning authority. Replay returns data only.

Journal framing validates independent header/body checksums, exact sequence/event/previous checksum identities and complete genesis, bounded64MiB/65536events/256KiBframes. The separate head binds a complete prefix and catches suffix rollback while untouched; coordinated journal+head rollback cannot be detected and is not claimed. Pending attempts reserve terminal persistence space. Metadata and sensitive spools bind vault/run/full attempt ownership and exact hash/length, use Unix0600/0700 and reject symlinks/unfamiliar bytes. Cleanup follows committed receipt/output verification and must tolerate partial cleanup without deleting foreign payloads. Canonical change checks are read-only under ledger guard; the guard is dropped before actual vault application.

AttemptBound/profile/rate/token-proof fingerprints are opaque assertions of the trusted future P16 preflight, not independently verified tokenizer/provider promises. Public DTO estimates or self-computed hashes do not establish hard dollars, model compatibility or actual endpoint trust. P15 introduces no transport. Markdown-only/corrupt history cannot issue old-budget authority; external qualification and actual native fault evidence remain separate.

## Runtime attribution and limits

No reviewer Cargo/tests have been run. Worker/root accounting tests, cross-process admission and native kill/fault gates are forthcoming. Root attributes four earlier private-helper tests and128 before/after injected I/O points; the new cleanup/read-checkpoint branches require its P15 rerun. These earlier helper checks do not certify the complete new ledger state machine. Final report requires actual logs and matching stable hashes after fixes. No power-loss, live-provider or other-platform safety claim.


## Final post-sync authority gate rereview

Reviewed the frozen `src/jobs/ledger.rs` SHA256 `835cd8522e95ac0327c3903b26528d0ffddcdf83f13d40ca6b7758989fd66d05`, superseding the ledger hash above. Shared types remain `43851bfd86806d877ce303ded035c919fd6cd553a51fdb3864ee562786053345`. The test file now hashes `4abb8d8d34f991a053d0c2b4a3e6377ea93e415a7d41e2068cfc274c370165a0`; its later unused-import removal does not change the focused regression logic.

Reservation, DispatchPermit and SendAuthorization are constructed only after the corresponding event append, journal sync and exact head sync return successfully, followed by the new fallible authority_gate. It samples UTC/monotonic time and cancellation again, compares the UTC high-water/deadline, and recalculates validity/remaining timeout using the **same immutable bound and RateCard**. No fresh pricing/profile/provider trust is introduced. Cancellation stops the run; clock/deadline/quote rejection pauses it. The transition uses the resulting non-Running capacity rule, retaining released control headroom for mandatory paid lifecycle events. Failed clock reads, quote checks or transition persistence propagate an error before capsule construction. Refusal does not invent a NotSent proof or refund: Reserved remains Reserved; recorded intent/send retains UnknownReserved and possible remote exposure. Replay remains data-only and cannot reconstruct the lost capsule.

No new concrete source blocker was found in this bounded rereview. This gate closes the local durable-I/O boundary; no transport or live-provider behavior was exercised or claimed. Full private/native/helper/integration acceptance remains with root.

Reviewer read actual root/worker logs without running Cargo:

- `/tmp/lwiki-p15-authority-final.log`: one parent regression, `final_durable_authority_gate_retains_accounting_without_returning_capsules`, passed in9.10s after4.59s compile. Its source loops over three capsule stages and six policy changes (18 cases): cancellation, clock regression, expired run deadline, DispatchLocked price expiry, EntireAttempt coverage expiry, and insufficient remaining timeout. Each checks error, durable paused/stopped state, retained phase/billing/exposure and rejected new admission. The log contains an unused-import warning subsequently removed; it is not a Clippy pass.
- `/tmp/lwiki-p15-public-gated.log`:13 external accounting tests passed in5.95s; one explicit subprocess child helper was ignored in the parent target. This log is not the full private/native/bootstrap/cleanup gate.

The earlier old private run's unrealistic byte-cap fixture failure is not treated as a passed gate. The corrected fixture, stronger before/after NativeIo fault counter, private state-machine/schema suite,13 SIGKILL cases and integrated P11/P15 checks still require root's final actual outcomes. No reviewer source/test/Cargo/Git changes or native execution were performed.

## Root runtime closure

Final targeted43 parents,19 explicit subprocess invocations and574 actual I/O calls/1148 before-after faults all passed. Integrated acceptedP11+P15-only final131 parents, strict lint/fmt/build, CLI3schema smoke, seed34 and whitespace passed after equivalent style/precise private-boundary expectations. Root separately reviewed the changed invariants and accepts R1–R11/final-sync authority closure. Exact hashes, command/log attribution and limits are in P15-checks.json; the independent reviewer performed source review and focused evidence inspection, not these root/worker full runs. P16 trust/transport and external platform/provider qualification remain required.

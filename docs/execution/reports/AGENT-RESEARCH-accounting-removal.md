# Research accounting removal

2026-09-29. Implemented under root's bounded lease for `src/jobs/**`, `src/providers/dispatcher.rs`, and `src/providers/public_fetch.rs`. Root owns replacement research, CLI, schemas, all other provider interfaces, tests outside the leased accounting fixture, builds, and Git. No nested delegation, Cargo/build/test execution, network, or Git operations occurred. Source is ready for root integration; runtime acceptance remains pending.

## Removed interfaces and code

- Deleted `jobs/research.rs`, `jobs/research_tests.rs`, `jobs/capture_history.rs`, and `providers/public_fetch.rs`.
- Removed `RunScope.research`, `LedgerInspection.research`, `RunPlanV1.research`, `ServiceBindingV1`, `BindingEpochV1`, all `Research*` ledger state structs, and `ResearchRebound`, `ResearchFrontierAdmitted`, `ResearchRoundAssessed`, `ResearchRetryScheduled` event variants and replay/event encoding branches.
- Removed research frontier admission, epoch retirement/rebinding, retry scheduling and current-binding adapters. Removed research-only failure/completion exceptions and historical capture Source-header proof. Generic outputs now use the ordinary exact `checkpoint::output` verification at all former capture-aware call sites.
- Removed dispatcher `ResearchDispatchWork`, `ResearchDispatchOutcome`, `ResearchDispatchResult`, `ResearchDispatchBatch`, `PreparedResearch`, `execute_ready_batch`, `execute_public`, research reservation reuse, research retry returns, and search/fetch wire branches. Ordinary dispatch uses its existing prepare/reserve/intent/auth/send/spool/receipt/retry flow.
- `Capability` now contains `Embed`, `Generate`, `Probe`, `TokenCount`; removed `Search`, `Fetch`. `TaskStage` now contains `Embed`, `Extract`, `Probe`; removed `InspectExisting`, `PlanFrontier`, `Discover`, `Capture`, `AssessGaps`, `Synthesize`, `StageChanges`. `BillableClass` now contains `Input`, `CachedInput`, `Output`, `Reasoning`; removed `SearchResult`, `FetchByte`.
- Removed `ResponseMetadata.acquisition` and its validator. Root must remove outside initializers and search/fetch/provider/config/schema callers. Removed the reservation-bound getter whose only consumer was research batching.

## Preserved accounting invariants

The old research module also contained the ordinary immutable task/config/profile checks. Those were extracted as `jobs::tasks::bound_is_current` (`tasks.rs:151`) rather than deleted: task capability/input must match the bound, config fingerprint must match the run, and profile ID must belong to the run's declared profiles. Admission, intent, final send and journal replay retain those checks. Dispatcher preparation still checks exact trusted profile/config fingerprints.

Source/run read preconditions and complete descriptor bytes remain checked by `bind_inputs` and `tasks::bind`. Validated receipt plans retain run source preconditions and task source bindings. Generic output verification, actual paid response storage, receipt acknowledgement, settlement, unknown holds, reconciliation, spool cleanup, one-use send authorization and final clock/price/cancellation checks remain present. The ordinary opt-in uncertain-retry behavior is retained; removing research does not create automatic uncertain resends.

The generic accounting test `undeclared_unknown_billable_class_breaks_completeness` remains meaningful: its removed `FetchByte` input became `CachedInput`, which is absent from that fixture's declared Input/Output/Reasoning classes. No test assertion was weakened. No new test was added under the fixture-only test lease.

## Shared receipt helper

Exported `jobs::settle_receipt(fs, ledger, plan)` from `jobs/checkpoint.rs:16` through `jobs/mod.rs`. Its intentionally narrow shared use is probe or rejected/unknown receipt publication with no derived outputs or cache writes. Direct graph API extraction retains its existing output-aware publication path.

The helper rejects dry-run, wrong vault/run/attempt/receipt identity, derived outputs/cache references, and operations targeting another path. It does not apply the caller's draft. Under the vault writer lock it recovers pending changes, replays the ledger to authenticate any apply-before-acknowledgement receipt, and compares an existing acknowledged receipt in full before idempotent settlement. If no receipt exists, it regenerates the receipt from retained accounting, compares it to the requested receipt, prepares/applies that fresh receipt-only draft, acknowledges, then settles. A conflicting receipt is an error, never a swallowed ContentConflict. This avoids the deleted research caller's duplicate create-only receipt publication and preserves a crash recovery route.

Root should replace `research::acquire::settle_receipt` imports/calls with `jobs::settle_receipt`. The helper intentionally does not complete the enclosing run, execute work, or delete a spool.

## Static verification and root gates

- `rustfmt --edition 2024` parsed/formatted the changed source successfully; final formatting used `skip_children=true`.
- Scoped `rg` found no remaining research, acquisition, public-fetch or search references in `src/jobs` or `src/providers/dispatcher.rs`.
- Examined former generic fallback predicates, retained begin-send/admission/replay checks, output verification, probe receipt use, and the undeclared-class fixture. No runtime pass is claimed.
- Root integration must remove outside old enum/metadata initializers and registrations before compiling. Run generic accounting/crash recovery, probe malformed-paid-output, direct API extraction and embeddings gates. Specifically test `jobs::settle_receipt` twice with the same plan, after apply-before-acknowledgement interruption, and with a conflicting receipt; one durable receipt/charge and no resend are required. Dry-run helper rejection must leave the tree unchanged. Run strict lint to detect declarations used only by removed research code.

## Returned source SHA-256

```text
83d2b05166dd1dd486799ccc297db46186eb00ebba77f2bbd2aa84dc79c3fa4b  src/jobs/types.rs
7ca88efb9b9a25124498943513e29c08eb60b7fa8d4b2b854702fd579b5c53b1  src/jobs/tasks.rs
22611f0c44f0131fa152a3b0325ff6005d6d9a5bbfbe9b4eabaeb24e573575f3  src/jobs/events.rs
a4bf5c297b27ea2b260aec84c26ff5e6d4e253fec6873dbb80471f6344202162  src/jobs/checkpoint.rs
0ece4b585b0ff893ab56259f13adaaf77627a77bb6e35df35432f6749e20042f  src/jobs/replay.rs
fba100325ce2992dae84c84071f48fd4e03189c8ad74474d16e1f00230737f09  src/jobs/ledger.rs
a334570dd318285d79bb5f06688c9a5821bebd5c9c0ba7686133735d9e694490  src/jobs/mod.rs
3c869267f1b8c8bc3ff7d150548ae47dd663a3b4470aca297d9aa4ff9cb682a9  src/jobs/accounting_tests.rs
6be61ee75bd98e8c774b9c6c88eb888897697982911ff9b9e1422154bc0ea815  src/providers/dispatcher.rs
```

All implementation/report leases returned to root. No source mutation remains active.

## Authorized regression follow-up

Root subsequently leased `src/jobs` accounting tests and `tests/provider_dispatch.rs` for four focused regressions. No production source was changed in this follow-up. Tests are authored and rustfmt-parsed, **not executed**; root owns compile/runtime acceptance.

- `receipt_helper_reuses_original_plan_without_second_commit_or_charge` (`accounting_tests.rs:163`): settles a rejected receipt, reopens the ledger, resubmits the original create-only plan, and requires unchanged receipt bytes, changeset inventory, budget and last event. It counts exactly one receipt file, one OutputsCommitted event and one Settled event, with one known 60-nanounit charge.
- `receipt_helper_recovers_apply_before_ack_without_republishing` (`:204`): injects the existing `BeforeOutputsCommitted` fault after canonical apply, proves the ledger remains Received without an acknowledgement while receipt bytes exist, reopens, and requires recovery/settlement without another changeset or receipt.
- `receipt_helper_rejects_altered_accounting_before_and_after_publication` (`:251`): changes the receipt charge to zero and expects ContentConflict both before publication and against an acknowledged receipt. No altered receipt is written; the actual known charge survives.
- `malformed_paid_generation_keeps_original_failure_and_one_receipt_after_reopen` (`tests/provider_dispatch.rs:193`): uses the existing canonical production wire fixture and an in-memory transport returning invalid model JSON with complete usage. A complete rate card gives exactly 25 nanounits including the one-time fee. It requires the original safe ProviderResponse failure, then invokes the receipt helper with the exact already-committed plan, reopens, repeats and recovers the rejected spool. Error code/message/safe reason remain stable; there is one transport call, one attempt, one receipt, unchanged changeset inventory/budget/receipt bytes, and no provider body text in the error. No live endpoint is contacted.

The focused static audit found no generic source/config binding omission introduced by removal. `tasks::bound_is_current` preserves the original non-research predicate; admission, intent, final-send and replay call it. `bind_inputs` still verifies run dependencies and complete task descriptors/source bindings; dispatcher `prepare` still checks source hashes and trusted service/config identity. Validated receipt plans still carry run and task dependencies. Capture-specific historical Source tolerance was removed only with capture functionality. Removing optional research/acquisition fields that previously serialized absent values does not change existing generic `None` encodings. This is a static equivalence finding, not a runtime qualification.

Updated test hashes:

```text
ba01a80208ad709d890e593ecd3428f4660b8561638d67f7b42c009f1457f6cf  src/jobs/accounting_tests.rs
3672b99fa0ce9558473b8bbaf19a4f80fd9c64ae7f047efc45ff34e8d1893f88  tests/provider_dispatch.rs
```

Follow-up test/report leases returned; source quiescent.

## Generic historical codec follow-up

Root's compile warning exposed an important omission from the initial removal audit: the sole production `history::retain` call disappeared with the research conditional. Read-only comparison against `ff3a014` confirms that this call was conditional on `inspection.research.is_some()`; ordinary embeddings, direct API extraction and probes did not call it on that baseline either. The correction therefore restores the retained-decoder lifecycle for the remaining operations by extending retention to the generic preparation path. The earlier source/config predicate equivalence finding remains supported, but was not sufficient to establish retention completeness.

`dispatcher::prepare_effective` now records the trusted profile fingerprint, recomputes the bound fingerprint and retains the original sealed codec after exact run/config/profile and source/descriptor checks. Execution policy is checked before this function; immutable retention occurs before reservation, credential resolution and send. Re-preparation after credential IO verifies the same snapshot and exact bound. The snapshot cannot grant send authority: one-use ledger authorization, lease/trust revalidation, source checks, pricing checks and final clock/cancellation gates remain in their original generic order.

The run-input-directory restriction was research-specific. All codecs now use `.wiki/state/provider-codecs/<hash>.json`; the canonical snapshot still binds the complete TaskSpec, descriptor path/hash/length, purpose, role, normalized original allowance and decoding contract. Exclusive absent-state publication, bounded reads, exact byte comparison on reuse and required directory synchronization remain intact. Existing history tests were updated to validate the generic location and reject foreign paths, altered contracts, noncanonical data and unknown versions.

The new public regression `production_codecs_reopen_embeddings_api_and_probes_without_current_config_or_auth` exercises four real production encoder/decoder paths: embeddings, direct generation, embedding probe and generation probe. Descriptor locations match the actual application layouts. Its in-memory transport requires the codec to exist before entry; after reopening offline and invalidating the provider configuration, decoding must return the original output without changing codec bytes, budget or attempt count. Missing descriptors and altered codec bytes fail with RecoveryRequired; an online execute against the still-unacknowledged paid response must refuse without another transport or credential call. This is authored coverage, not an execution result. The two unreachable p16c fixture match arms were removed, and alternate descriptor fixture paths create their parent directories.

Static comparison of `execute` against `ff3a014` found the remaining deletions confined to research batching, research scheduling, public fetch/search variants and acquisition metadata. The surviving generic behavior still checks outstanding unacknowledged spools before preparing a new attempt, reserves before credential acquisition, refreshes source/trust after helper IO, durably accounts paid invalid outputs, preserves uncertain holds and does not automatically retry incomplete observed response bodies. No other concrete generic deletion omission was identified in this bounded review.

Limitation: original descriptors remain required for retained decoding. Direct API extraction descriptors currently reside in `.wiki/cache/generation/inputs`; deleting that cache prevents decoding until the authenticated original descriptor is restored. This fails closed and cannot authorize a replacement request or release a paid hold. Moving those descriptors is outside this correction and was explicitly left unchanged by root.

No Cargo, runtime tests, network calls or Git mutations were performed. Rustfmt parsed all five changed Rust files. Root owns compilation and runtime qualification. Source is quiescent and leases returned.

```text
10ab562fb4341a29eabd992222aa8def7dbb227228343abb6770a60acf18b118  src/providers/dispatcher.rs
6511ab5b93091f1901c6f0c656f23e62d78666fed6119926c19de62fc3ef60f8  src/providers/history.rs
07da7436d07ab4f3a3f907bcc3e2b0a68023d0b7c14c626d481655afeab18f04  src/providers/history_tests.rs
5a079f792f10398bed7caf7bf64e8c84be364b4e635dbfdfb726629362d4da5d  tests/provider_dispatch.rs
9403b08757e88c15101bde19fd0d28660e56b85560b923bd03e5c2ae4ad746f9  tests/fixtures/p16c/common.rs
```

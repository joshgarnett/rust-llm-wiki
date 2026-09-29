# Astra P20 authority review

Status: bounded independent source review with four reviewer-authored regressions. Three consequential findings were fixed by root during review and pass the focused ledger target. **This is not P20 integration acceptance.** Runner/report orchestration was still being written. HEAD was `809e119aefa781ad7737c575d43acbaae104ad2b`; inspected dirty-file hashes are below. Final capture: 2026-09-29 04:14 UTC.

Only this report and, under a later explicit root lease, `src/jobs/research_tests.rs` were edited. No production source, Cargo files, schemas, commits, network calls, or nested delegation. The production corrections below were root-authored, independently inspected, and exercised by reviewer-authored tests.

## Findings and resolution

### R1 — P1: settled accounting did not imply terminal historical remote work — closed for inspected source and focused regression

At `src/jobs/ledger.rs:135` and `src/jobs/replay.rs:216`, research completion originally required historical `AttemptPhase::Settled` without checking `RemoteExposure`. `Settled` deliberately does not establish remote terminality. Minimal reproduction: enable explicit uncertain retries and concurrency at least two; observe a partial/nonterminal response, publish an Unknown receipt and settle it; schedule a retry, settle a successful response and finish the task. All attempts are settled and the task is completed, but the first request remains possibly in flight. Completion previously admitted this state.

Root's narrow fix requires both Settled and TerminalConfirmed for every historical research attempt in the live completion method and replay transition. Terminal responses with unknown billing retain their complete holds and can still complete. `research_completion_requires_terminal_history_and_preserves_unknown_holds` (`research_tests.rs:798`) exercises real reservations, journal/spool writes, ChangeEngine receipt publication, settlement, retry scheduling and task completion. It verifies live completion refusal, independent append/replay refusal, unchanged state after refusal, then successful completion after explicit terminal reconciliation with unchanged unknown holds and both requests retained.

### R2 — P1: individual reservation bypassed deterministic research priority — closed for inspected source and focused regression

`JobLedgerApi::reserve` previously admitted an arbitrary ready research task through `admission`, bypassing the stable-prefix check in `reserve_ready_batch`. Minimal reproduction: admit two ready remote tasks with distinct priorities and call individual `reserve` for the lower-priority task first; it could take the final allowance even though the batch API refused that subset.

Root routes research individual reservations through the batch method (`ledger.rs:1306`). If the flush admits a token and its closing gate fails, the token is still returned. `research_individual_reserve_enforces_prefix_and_retains_postflush_token` (`research_tests.rs:643`) proves the non-prefix refusal leaves inspection unchanged; then advances the clock at journal sync, receives the already-durable token through individual reserve, confirms intent refusal and releases the proven-unsent allowance. Existing final-slot and batch-postflush regressions also pass.

### R3 — P1: rebinding could introduce new Search work outside round admission — closed for inspected source and focused regression

`ResearchRebound` previously accepted new task definitions through `add_tasks` with only current-service validation. A new Search task could therefore enter a paused run at `rounds_started=0`, or after the no-progress stop, without `ResearchFrontierAdmitted`. Genesis and round-zero frontier admission already rejected this route, but rebinding provided another one.

Root rejects newly introduced Search/Fetch keys during rebind (`research.rs:398`); unchanged existing keys remain subject to normal membership, dependency and binding checks. `research_rebind_cannot_admit_new_acquisition_outside_rounds` (`research_tests.rs:686`) exercises the shared pure transition validator with valid task identities and Search role proof at both zero and two no-progress rounds. It asserts the specific acquisition fence and unchanged prior state. Fetch also exercises the explicit new guard; unlike Search, an entirely new Fetch key without a prior origin mapping was already refused by the later active-set check. Live trusted-service rebind is covered by earlier tests, not this pure-transition regression.

### Additional reviewed delta: explicit failed-origin state

Root's `fail_research_task` requires active remote work, safely settled/terminal attempts, no outputs and nonvalidated receipts (or proven-unsent release). The replay failure gate checks settled terminal history and empty outputs. `research_failed_gap_requires_terminal_settlement_without_refunding_hold` (`research_tests.rs:860`) proves a Reserved attempt and a settled nonterminal Unknown response cannot fail the task; terminal reconciliation then permits idempotent failure, retains the full unknown hold, and does not allow run completion with an active failed task. Historical failure does not erase source/round consumption.

## Boundary review and remaining evidence obligations

- The rebind barrier requires paused/stopped state plus settled terminal history. Reserved, intent, and live send authority cannot cross it. Intent and begin-send independently verify effective binding and active membership. This provides the cross-process epoch fence without pretending that the final in-memory clock check rereads an epoch. No new native multi-process/SIGKILL sweep was performed here.
- Rebind preserves immutable genesis, prior task definitions, attempts, accounting, deadline, origins, rounds and no-progress groups. Full duplicate TaskSpec equality includes dependencies and priority. Typed `(capability, profile)` bindings permit a shared Generate/Search profile; dispatcher preparation checks trusted config/profile/endpoint before reservation. Existing focused tests exercise these properties; they are not a whole-workflow source/model-change qualification.
- Ordered batch reservation recomputes the eligible prefix under the ledger lock, persists retry eligibility, and preserves returned reservations when a later admission/closing gate stops. Research transport performs one attempt per wave. The initial early-return token-loss suspicion was withdrawn after reading the existing dispatcher finalizer at `dispatcher.rs:1042`: it preserves the AttemptRef and releases an unconsumed reserved token. A full dispatcher cancellation/sibling-failure test after postflush gate failure is still useful integration coverage; the new test directly covers the ledger authority.
- Historical codecs retain original model/schema/decoding bounds, complete task identity and descriptor hash outside cache, and restore without current source/config checks or credentials. Missing/mismatched codec data is a recovery failure with protected spool, not invalid provider output. Saved history tests exercise production generation/embedding/Search codecs; the current target compiled these modules but did not rerun those seven tests.
- Public Fetch resolves the actual hostname, rejects mixed/nonpublic destinations, supplies pinned addresses to a no-proxy/no-redirect client and runs final authorization before polling transport. Redirect admission reads protected receipt/metadata/body hashes, checks exact Location target, same original source slot, equal hop limits and redirect depth. Network telemetry is set only after actual connector entry. These source checks and prior mocks do not establish real DNS/TLS compatibility or hostile-network qualification.
- Validated receipt plans use effective current source dependencies, task source bindings and active membership; rejected/unknown receipts cannot activate outputs. A prepared first receipt cannot normally cross rebind because the attempt must first settle and conflicting receipt publication is immutable. **The run-note read precondition is not itself an epoch fence:** rebind only appends journal state. A trusted caller that reused an already-settled receipt with additional new output operations could prepare before retirement and apply afterward while canonical run/source hashes remain unchanged. No inspected current stage/extraction caller established that pattern, so this is a runner/report publication review obligation, not a claimed reachable P1. Review report/local-stage publication and all deferred changesets against operational rebinding before final acceptance.
- Round assessment checks current citations and hashes support groups, but the final caller must prove the submitted citation list belongs to its validated stage output. Protected redirects, cache deletion, paid-response recovery after source/model changes, retained unknown spools, and final partial-report behavior still require the integrated runner cases.
- Optional research/profile-proof/codec fields use omitted serialization when absent. The three schemas parse as JSON and preserve optional legacy extension membership, strict objects and version 1 constants. Exact legacy byte compatibility beyond the existing omission test and complete schema-registry validation remain integration checks. New control events use ordinary complete-history capacity accounting without debiting per-attempt cleanup reservations; no new near-capacity fault sweep was run.

## Actual checks

Reviewer run: `rustfmt --edition 2024 --config skip_children=true src/jobs/research_tests.rs`; `cargo test --locked --offline --lib jobs::research_tests` — **20 passed, 0 failed**, 13.71 seconds compilation, 5.91 seconds tests, no warnings. Log: `/private/tmp/lwiki-p20-authority-regressions-test.log`. Source/test diff checks passed. Cargo and test-file leases returned to root. This is a focused target on an actively changing shared checkout, not a quiescent whole-tree build/test claim.

Previously saved logs were read and match the worker reports: ledger 16/16 (`/private/tmp/lwiki-p20-ledger-epochs-test.log`), dispatch 7/7 (`/private/tmp/lwiki-p20-dispatch-batches-test-third.log`), history 7/7 (`/private/tmp/lwiki-p20-historical-codec-test.log`). The dispatch report explicitly excludes its later two receipt-planning error-classification edits from that saved runtime evidence. The reviewer target compiled the current library but did not execute dispatcher/history targets. No live providers, helpers, production credentials, native other-platform checks, power-loss safety or model-quality claims follow.

### SHA-256 evidence boundary

```text
2e4e60141278743621c305d1ba36bbd88bbed329581b391245510abb6841631b  src/jobs/types.rs
05bf0feecd9c588ecd8e16eb48a8d23c869f5f7dcc9e6f8b16f5d4ca590b19f6  src/jobs/research.rs
e3a08222cf2a88e7e9367475561a044c4ce809c431594d2f18ef6daf51156705  src/jobs/research_tests.rs
06c09c141d6897e00c3ceb1120c6246f5979cee48c0f6c66e3b5df31f7927049  src/jobs/ledger.rs
f551bcbc53a9e7477b7cae4307862ec37aee7f2694a0133a45e0f6ffd458ecb0  src/jobs/replay.rs
254f3dfe10d81bbd8904c8c838ecc9d42dd91f08910c7b5acd685917720c3a86  src/jobs/events.rs
ae7fba2215082043e88d4998686787975dd3897a89759971af938cca384b4556  src/jobs/checkpoint.rs
7477f5f01c7c2c05c1796274cd34dbc27274a06b5deca5f29d77a53e78d67ea3  src/providers/dispatcher.rs
1e3581ea5cf398757c4a8e8c4ec1eb5bd3b6d2b1157df98f92fd31e7ceaa151b  src/providers/public_fetch.rs
7751ef4c10112909bca92cab4807cf5734e516e11d3e682bd8ed100198be53a3  src/providers/history.rs
e1174b5dfdd3394c8f45e908a1d909878b051a84967200d9e9e75fdf9be016d8  src/providers/history_tests.rs
cd2f9de4e53be650d47f779ecfed8f76ed61187a8aa4b6898ff7c43e5a29a2ff  schemas/run-v1.json
5e1ab748cc69c92ee983c5194d6348980748995b04b3c66e33564b720c0f04e5  schemas/run-event-v1.json
598f14a969b41a8119c3e8ae4d823bd76cbd43e1919b7a2f806f3f24506772e8  schemas/research-run-plan-v1.json
55ea24dcbc709e37abf9765da21245f296900b79d30196771c66b6a7c5435a66  /private/tmp/lwiki-p20-authority-regressions-test.log
c616e855ce23c5b5384d33d8517ead7ec29359f12d969c7e671325f064c927ec  /private/tmp/lwiki-p20-ledger-epochs-test.log
f9b1bc21c6cb8d4ca5c740ac92eaeb6055f25a919d0a2fd2d9cfbc0de4100dfa  /private/tmp/lwiki-p20-dispatch-batches-test-third.log
28568429734df2a15d3df4d4d29f5c7e330d04d5ce220a0a4c8ae6d216139809  /private/tmp/lwiki-p20-historical-codec-test.log
```

Later root changes and the new runner require a final diff review and integration evidence; these hashes do not qualify subsequent mutations.

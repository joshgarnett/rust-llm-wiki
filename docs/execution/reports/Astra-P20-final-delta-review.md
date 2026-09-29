# P20 final delta — independent invariant review

## Summary

Independent Astra review, 2026-09-29. Base HEAD `809e119aefa781ad7737c575d43acbaae104ad2b`; acceptance/commit remain root-owned. This reviewer did not author the capture-history helper and edits only this report.

**The bounded P20 review and its integrated local runtime gate are closed.** R12 prior-accounting discovery, R13 historical Capture Source proof, R14 stale active dependency rebind and R15 post-export Current checking are source-approved and qualified by the final frozen-tree evidence below. The lint-only follow-up preserves those invariants. No consequential P1/P2 remains in this review scope.

Independently recomputed all 278 current source hashes and their fingerprint: `e6befa4647750e301323f39662aa7ad904fe9e8f62a3f3e6f59fbcd641bb445e`. They match both integration manifests and P20-checks.json. All saved log hashes match. Actual results: 181 public parent tests across 17 targets, including all 23 workflows; private ledger 21, capture history 6, providers 63, post-export gate 1 and API recovery 1; skill tests 4 execute the maintained 32-step recipe. Strict Clippy and formatting pass. The saved native debug artifact hash also matches.

No Cargo was run by this reviewer. This approves P20's recorded integration gate, not P21's whole-repository fault/doctest/release/corpus/completion qualification. The exact R15 threaded lock interleaving remains unexecuted; its production gate is tested with real historical/current packets. No live-provider, host-installation, other-OS or power-loss claim follows. Earlier pending statements below describe historical checkpoints and are superseded by the final reconciliation.

## R13 source closure and proof trace

- `capture_history::output` first retains strict exact-current output verification and same-vault identity. Fallback applies only to canonical Source path, Capture/Fetch, and the actual replayed Running/Completed task. Running requires no already-recorded outputs; Completed requires exact task/attempt output equality.
- The unique owner is the same run/task/input, Fetch, OutputCommitted/Settled attempt. Immutable current receipt must be Validated and match the full attempt/output/cache vectors. The unique authenticated OutputsCommitted frame must name that exact receipt and complete vectors.
- The exact recorded PreparedChange must inspect as Committed in this vault. Source and receipt must each be an operation with the exact target/hash. `verify_payload` independently checks retained proposed path derived from change/index/side/target, length and hash; parsed original Source must match ID/kind. Another change containing identical Source bytes, receipt absence, changed payload/manifest, or cross-run/vault/task references cannot satisfy this chain.
- `ledger::load` verifies framed journal, durable head/genesis/spec/vault and replay before supplying frames. `bind_inputs` still checks current epoch read preconditions and every active TaskSpec with `tasks::bind`. Legacy runs retain all tasks as active and original scope preconditions.
- `repair_task_finish` supports the acknowledged-before-TaskFinished crash window; `settle` takes billing from the exact receipt. Historical verification adds no new attempts, refunds, counter resets, restoration or current citation authority.
- `checkpoint::output` remains strict. Replay reusable outputs and spool cleanup call it directly, so historical fallback neither lists an old Source header as reusable nor deletes its spool after refresh. `ChangeEngine::new/inspect/verify_payload` only read; no WriterPermit is acquired beneath the run lock.
- Research rebind remains paused/stopped and requires all attempts Settled/TerminalConfirmed, live trusted roles, current active bindings, and explicit epoch progression. Reservation/dispatch/begin-send still call current input and live-bound gates.

## R14 — P1: stale active dependency closure can evade resume rebind selection

**Found at initial review hash `f5b736...`; source-closed at `d2b1e5...`, targeted runtime closed at the final evidence below.** `runner::resume_with_amendment` substituted the old source snapshot before comparing `relevant` with `old.binding` (1883–1885). That comparison only saw current lexical-selected records/read proofs and service configuration. Captured-source extraction dependencies are separate TaskSpec.source_bindings.

Minimal production-flow reproduction: start with no lexical sources; execute explicit capture and admit/complete Extract; pause before completed Synthesize; refresh that captured source with content/title that does not match the question. New lexical input records/dependencies remain equal to the original empty selection, while active Extract still binds the previous Source header. Resume skips ResearchRebound and `ledger.resume` correctly fails `tasks::bind`. An explicit resume can thus never retire the stale closure despite no remaining uncertain remote work.

Required correction: OR changed active task source bindings into the rebind trigger, using bounded reads returning `Result<bool>`. Propagate I/O/resource failures; do not silently treat them as authorized retirement (the existing retention filter's `is_ok_and` also needs this treatment). Preserve matching tasks and dependency closure, immutable existing TaskSpecs/genesis/history, lifetime counters/origins/holds, and all ledger gates. Regression must establish equal fresh lexical selection, an actual stale Extract, no completed synthesis shortcut, retirement and successful continuation without refetch or stale extraction.

## R15 — P1: Current proof precedes historical-capable packet export

**Found at initial review hash `f5b736...`; source-closed at `d2b1e5...`, targeted runtime closed at the final evidence below.** The initial runner Current citation check occurs before `app.graph_extract_agent` acquires its WriterPermit. The graph packet API intentionally supports explicit historical revision export: `build_packet` and `load_packet` validate immutable revision content/ownership, not Current source head.

Minimal cooperating-writer interleaving: scheduler verifies old revision Current; another writer refreshes the Source before exporter obtains its writer lock; exporter receives the explicit old revision and builds/loads its historical packet against the new Source header. Runner then reloads that packet and plans Extract without a second Current proof. The resulting task bindings match the new header, so ordinary fresh hash checks need not reject paid work on that historical revision.

Required narrow correction: after export, recheck the citation with CitationScope::Current using the **same SourceView** used to obtain the VerifiedPacket for planning. Skip/retain an explicit stale passage gap on mismatch. Subsequent edits then invalidate the admitted task's exact packet dependencies. Preserve historical export API semantics. A deterministic regression should exercise refresh at this boundary or directly prove that a historical packet with otherwise current dependencies is withheld by the exact scheduling helper.

### R14/R15 correction review (05:25 UTC)

`current_reads_match` now uses bounded reads and propagates errors. Resume checks all active task source-binding sets in addition to lexical/service comparison; the same helper replaces `is_ok_and` in task retention. Existing task specifications and history are unchanged, and dependency closure filtering remains in place.

After packet export, `current_extraction_packet` checks Current citation eligibility and loads the packet from the same SourceView. That helper is the scheduler's production path before plan/admit. Historical export API semantics remain unchanged. Its inline regression uses real source capture, refresh and graph packet exports: an explicitly historical packet is loadable and has currently matching dependencies, yet the helper withholds it; original/current cases are accepted. The workflow regression reaches completed Extract with three used requests, proves unchanged empty lexical binding after a nonmatching refresh, and requires epoch retirement/continuation without refetch or further Extract.

The runtime tests were not yet run at this source checkpoint. The R15 test deliberately proves the actual gate against real artifacts; it is not a claim of executing the exact threaded writer-lock race. Existing DurableIo hooks occur after the exporter owns WriterPermit, while WriterPermit acquisition uses NativeIo directly; attempting recursive source_refresh from such a hook would test the wrong interval or deadlock.

```text
d2b1e5d7ad0f67f742c4d8f6f1d1873a30c177fe9d80dbce5c1155d46dae4c22  src/research/runner.rs
e7e4f9d83f7cc413b960895b3c5f0e946bee387270a7d59c7daac3ecfe60257e  tests/research_workflow.rs
```

05:27 UTC initial source handoff (historical checkpoint): worker strengthened the R14 workflow to compare every original AttemptInspection exactly and retain the entire original unknown-attempt set. Runner hash remains `d2b1e5d7ad0f67f742c4d8f6f1d1873a30c177fe9d80dbce5c1155d46dae4c22`; workflow test SHA-256 is now `a550816d33ad2ccfafefe21520662c2bef61cd8c6beb99e850930ae7d91c8617`. Focused whitespace checks pass. New runtime targets await root's Cargo grant; this report lease is returned to root with no unresolved source finding and no claim those new targets passed.

### Final R14/R15 evidence reconciliation (2026-09-29 05:43 UTC)

Read the actual saved logs and final workflow assertions; production runner bytes still match the source-approved `d2b1e5...` hash. R14's final target passes with completed old Extract, equal empty fresh lexical binding, an incremented binding epoch, retired stale Extract, fresh synthesis, five total lifetime requests, no refetch/additional Extract, exact equality of every original AttemptInspection, preserved unknown-attempt membership, and unchanged genesis/origin/task identity.

The R14 fixture correctly expects **Paused with a partial report**, rather than Completed. Its original completed extraction's staged change remains retained with its old source preconditions; report generation explicitly marks that proposal as conflicting with current bytes. The new assertions require the original change references, the conflict warning, and fresh synthesis. This matches `report::build`'s existing conservative proposal handling; production behavior was not relaxed to pass the test. Earlier R14 compile/expectation failures are not passing evidence.

R15's real-artifact production-helper test passes: historical packet export remains valid and its current dependency hashes match, but the exact scheduler helper withholds it; current packets pass. This qualifies the focused gate, not an executed concurrent writer-lock interleaving. No remaining consequential finding was identified in this reconciliation. The complete 23-case workflow and broader integrated P20 gates remain required; neither targeted result constitutes whole-P20 acceptance. Report lease returned to root.

```text
d2b1e5d7ad0f67f742c4d8f6f1d1873a30c177fe9d80dbce5c1155d46dae4c22  src/research/runner.rs
5e88b2679c4c36f0774608901c49f622da624d5919d5ea4ae1f06530a73cfa1e  tests/research_workflow.rs
9cfd1790dbba37191ede95ffe8013394e2ea961970d2390934dcd270d2107c7b  /private/tmp/lwiki-p20-workflow-nonmatching-third.log
02466aa27f522fd7f8ed2e58e9b9e248451734865d0c5afa5678473a6eeb0e1c  /private/tmp/lwiki-p20-post-export-current-first.log
```

## R12 discovery parity

`OfflineApp::prior_accounting_for_new_run` and ledger `check_prior` share the 65,536 scanned-entry bound, fixed `runs/<namespace>/run.md` possible namespace, parsed canonical Run ID discovery (including moved/nested notes), and bounded operational namespace discovery. Fresh embedding, API extraction, Probe and research creation call the helper. It conservatively discloses all found histories and changes no old receipt/budget. The native role-probe test includes namespace/record-ID mismatch, nested moved notes, zero-write dry/offline checks and retained previous unknown attempts.

## Actual inspected evidence

| Artifact | Observed result | Scope limit |
|---|---|---|
| `/private/tmp/lwiki-p20-workflow-nonmatching-third.log` | 1 passed, 22 filtered; build 1.68s, test 16.92s | R14 actual workflow; fresh synthesis with conservative historical-proposal partial report |
| `/private/tmp/lwiki-p20-post-export-current-first.log` | 1 passed, 138 filtered; build 6.58s, test 1.99s | R15 exact scheduling helper with real capture/refresh/export artifacts; not threaded race execution |
| `/private/tmp/lwiki-p20-captured-source-history-first.log` | 6 passed; build 10.08s, tests 10.77s | Actual fixture changesets/ledger; mock response bytes, no provider compatibility |
| `/private/tmp/lwiki-p20-workflow-capture-first.log` | 4 passed, 18 filtered; build 9.76s, tests 46.29s | Includes refreshed captured Source, no refetch/stale Extract; R14/R15 variants absent |
| `/private/tmp/lwiki-p20-schema-real-records-first.log` | 1 passed; build 15.67s, test .55s | Actual durable RunPlan/events against published schemas; predates helper integration |
| `/private/tmp/lwiki-p20-cli-stages-eighth.log` | CLI 11 passed (14.98s), stage 1 passed (.77s); build 1.50s | Restored-history regression included; predates new capture helper |

Reviewer read-only `git diff --check -- src/jobs/ledger.rs src/jobs/checkpoint.rs src/app/embeddings.rs` passed. No independent runtime test or whole-tree formatting/lint claim.

## Initial reviewed SHA-256 scope (2026-09-29 05:20 UTC)

```text
e0941c115fe328186cddf9bfefcef94d995f886a9c1a5ba1ee417abdd00a6fe8  src/jobs/capture_history.rs
f3796ace8ff35fc8c65fba0dd907ee0e0421cb48c6a80e5b7e8d1bdea8732405  src/jobs/ledger.rs
13efb8e30db4b0e8079f29d86045ffc97d06f76f48772552c88f3d91c3fbba57  src/jobs/research.rs
e61b39d043549606c380447f7eabb5060f8dffd747b91fae85ff8d3ea0e8caad  src/jobs/checkpoint.rs
e2a37cc3c71e8cf908b950a8869db794ba37263bb556ab9253c9a8ef8b215ec1  src/jobs/replay.rs
254f3dfe10d81bbd8904c8c838ecc9d42dd91f08910c7b5acd685917720c3a86  src/jobs/events.rs
f5b736faac6541b41fed52e5479aa027bb5d8bf8244e0d994803f226b9429c8d  src/research/runner.rs
728aef0a604c001fc925fda661955d198b5d6225ead2579b34853a3eaf2ecb59  src/research/acquire.rs
b583243665b698ec37819fd50801bbd69ae048ea901494858c2ac04202bbd465  src/app/embeddings.rs
dbd69213f2dffeb8eb6b5fe8e3e36274bd896ade0f4655e2689c2ba40f586540  src/app/probe.rs
50c488babeecd627021601a6e24d3eef52e0b4b305ed124b326f7267d1580e38  src/cli/research.rs
30f3662dea14a051c8399f544a83361437a17799dde197419602d669b4e90783  src/graph/api_extract.rs
d8d02116990d775527d5b0bab2cb4f4010f64b59bef255235cbe70599c92cf2a  src/graph/research_extract.rs
4ca870447647ec9344e846bbf457dbbe5d2135d1b2c94e192513092d4e049b22  src/graph/packet.rs
4d1ce13b2b5efb4665a39b4d2969a2caeb8fd69a6283b280427da0fc06106a07  tests/research_workflow.rs
0fc04a1f84ec11499f80b1444827043e52967a52c932da68a414bf2159dc0cf6  tests/research_cli.rs
a5e8b956ca94be4f6bcef41822fc29a3466d7d19174b85dbc122df6724878c99  /private/tmp/lwiki-p20-captured-source-history-first.log
d13633fcc52f6807c968d67eeab685f39d4cf6ff4403c279bfcc795a16bc422e  /private/tmp/lwiki-p20-workflow-capture-first.log
7e7e30aa180ccb345c1850207e8a48402f5b207ff0fd618dda60efdd621b890e  /private/tmp/lwiki-p20-schema-real-records-first.log
8e2e97d114ecde204023c65f8977372c6f2a054627d1b7928cece3fbfce3c873  /private/tmp/lwiki-p20-cli-stages-eighth.log
```


## Final lint-delta review (2026-09-29 05:51 UTC)

**Source-approved; current-tree public/private runtime gates pending.** Read the exact runner diff `/private/tmp/lwiki-p20-runner-clippy.diff`, verified its preserved preimage is the previously approved `d2b1e5...` source, and inspected the changed shared predicates against prior reviewed source and Clippy's exact old snippets. No consequential finding.

- Runner let-chains preserve left-to-right short circuiting, `?` propagation, receipt settlement/failure handling, first-error selection, completed-extraction reuse and the guarded local-publication resume branch. R14's bounded active read comparison and R15's same-view Current/packet gate are unchanged.
- Replay retains `research && Failed && (inactive || durable_outputs || cache_outputs || any_unsettled_or_nonterminal_or_output_attempt)` with the original parenthesized OR group. Research origin replacement remains guarded by existing origin inequality. Redirect validation still requires the parent to exist, be Completed/Fetch, be an explicit dependency, remain active and share the origin; optional-parent matching precedes those checks.
- ResearchLimits initialization supplies the same three explicit argument values and all remaining defaults. Removing an extra packet reference and replacing map `get(...).is_none()` with `!contains_key(...)` preserve behavior.
- The narrow `expect(clippy::large_enum_variant)` on EventPayload leaves public constructors and serialization unchanged; it does not replace or weaken journal size limits. This is a source-level compatibility decision, not a new memory bound.
- Shared p15 support is included once under `cfg(test)` in jobs and imported by the three unit-test modules. Its mutable clocks/state remain fixture instances, with no shared mutable static discovered. Production capture-history code is unchanged beyond its test import. The duplicated outer fixture lint attribute is removed; workflow `StageHook` merely aliases the existing `(TaskStage, Box<dyn FnOnce() + Send>)` type.

Read actual first/third Clippy logs. First failed on production style/enum-size and duplicate fixture loading; third failed on workflow hook type complexity. The StageHook alias was inspected after that failure. Subsequently inspected the fourth Clippy log: it records successful completion in 0.43s (the actual log duration). Root reports exit 0 and final formatting PASS; the formatting log is empty, consistent with that report. No post-lint public/private runtime result is claimed here. Earlier R14/R15 test logs remain evidence for their exact pre-lint source scope; final public/private integration must qualify the frozen tree. Reviewer ran only read-only inspection and focused whitespace checks, no Cargo. Report lease returned to root.

```text
4e4fa9d91b56fe9b19eef5aada6d816ce9a013ec83a11691ee981a2216dda3ba  src/research/runner.rs
d2b1e5d7ad0f67f742c4d8f6f1d1873a30c177fe9d80dbce5c1155d46dae4c22  /private/tmp/lwiki-p20-runner-before-clippy.rs
fd456fb183d20ebe3216063c8e6ce3d12212996defc2dacfd006089953bcb55d  /private/tmp/lwiki-p20-runner-clippy.diff
d96ec22317f43948b5f2e83db00bcae4a0713eff3af5f593769386a5fa694880  src/jobs/replay.rs
bd5c0d7e36962bbdb2075d9fca81282876f260507678eec890d7e28e379bc4ee  src/jobs/research.rs
ac89343c8c19c5026d810c2d5b1446af068f1f7bc9100420cd14c19dc21d1c33  src/jobs/mod.rs
159777b4483eaef2b9016888c1b2488d8890a74ae0f4a49299a9a8ee4168b0d1  src/jobs/capture_history.rs
a7ab134711c39b09ccca982b59caf1902780f13baa6ef4713047b0982cfab7f6  src/jobs/accounting_tests.rs
7cf2dcb5c3c00410593980cdc8d65d2b23741de1482b65382d04f9c3ae42094c  src/jobs/research_tests.rs
805298282fe760984bddc80025df896edb71d6bbbd5ecb461a59a311d11ede6d  src/jobs/types.rs
3bb4661ef955f4b3eb339532133c82a67d463ab49cf8868cf81d14e7c9b1f0d5  src/cli/research.rs
6bfb2ed0d9387fcf48c37b2fb7067b508d3b4e4e2849e99bb394bc947d9d4af5  src/research/report.rs
c1568186b7330e4ddaf19f97370585a2f7033fe43688d57066408c8bb631a5b3  src/graph/api_extract.rs
ca3f8c307498705a46759bafa3b4414113ba8024a9ccaa263c0b949c85c0452b  tests/research_reports.rs
5d8f3e89392db03b3ecfe81932123e6efa4d6a63339f399448834fc321948d74  tests/fixtures/p15/common.rs
f91ad3cbd787f335e51ced8bb5b6ef14892961c6c074b81192cd136c41baec89  tests/research_workflow.rs
6b7572e74cfa83d548c537b778d3b326c6aeb57febb7f63688589a9cc381dc4b  /private/tmp/lwiki-p20-integration-clippy-third.log
```


Final lint evidence received before lease return: current 278-file source manifest fingerprint `e6befa4647750e301323f39662aa7ad904fe9e8f62a3f3e6f59fbcd641bb445e`. Root's broader public batch is running and remains unqualified in this report.

```text
ffc6066a7f00c8faee0f458f24bd8127071a9e2b432c219b0bad249f29de0248  /private/tmp/lwiki-p20-integration-clippy-fourth.log
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  /private/tmp/lwiki-p20-integration-fmt-final.log
3356aba8d8b45e08531a85b5cb9f975508ec90b064371b2307989cb5af296ddf  /private/tmp/lwiki-p20-integration-current-before-hashes.json
```


## Final P20 integrated runtime closure (2026-09-29 06:06 UTC)

**Approved for root's P20 acceptance.** Independently read `P20-checks.json`, recomputed all 278 current file hashes and `SHA256(sorted(path + NUL + file_SHA256 + LF))`, and compared the complete maps against both pre/post manifests. All are exactly equal at fingerprint `e6befa4647750e301323f39662aa7ad904fe9e8f62a3f3e6f59fbcd641bb445e`. This includes the final source-approved runner `4e4fa9...`, capture-history test-import consolidation and lint predicate changes. Independently hashed every recorded log and the saved native binary; all match the check manifest. Recorded exit codes are 0, and actual test log summaries agree.

| Final frozen-tree check | Observed result |
|---|---|
| Public integration | 17 targets, 181 passing parent tests; zero failures. Workflow 23/23 (274.80s), native research CLI 11/11, reports 13/13 and relevant M3 regressions included. One public accounting subprocess helper is explicitly ignored by its parent harness. |
| Private research ledger | 21/21 (11.15s), including actual durable schema encodings, legacy absence, paused/quiescent rebind and original task retention |
| Private capture history | 6/6 (10.56s), including refreshed/deleted Source history, receipt/revision/payload tampering and OutputsCommitted-before-TaskFinished recovery |
| Private providers | 63/63 (62.77s), including seven historical-codec tests and prior provider regression scope; two helpers explicitly ignored and invoked by their wrapper tests |
| Private post-export gate | 1/1 (1.81s), actual historical/current packet artifacts at the production Current gate |
| Private acknowledged API recovery | 1/1 (3.25s), acknowledged validated plan consumed without rewriting output |
| Skill/export recipes | 4/4 (11.05s); maintained examples contain 32 steps iterated by the passing recipe test |
| Strict all-target Clippy / formatting | PASS; Clippy log 0.43s, empty formatting output with recorded exit 0 |

Parent counts are deliberate: the public vault_fs log also prints one nested child's successful summary, which is not added to the 181-parent total. No filtered-out library tests or ignored helpers are represented as separately executed parent tests.

R12's moved/restored namespace disclosure test, both captured-source refresh workflows (R13/R14), and R15's helper all pass in this final tree. R14 continues to preserve the original paid attempts/unknown holds and marks the retained stale proposal as partial rather than silently replacing its read preconditions. The source/read/receipt ownership and current-authority separation reviewed above remain intact. No remaining consequential finding was identified.

Saved `/private/tmp/lwiki-p20-root-artifact/lwiki` is a **native debug** artifact, independently verified SHA-256 `95d1bc3ad8bfe240809c2efb6f90ff501c8c7a9c870e382c9194190c593de9db`. Its recorded capabilities contain 37 commands and 19 schemas; the saved capability JSON was read and hashed. The recipe test's historical name mentions release, but this P20 artifact/evidence does not replace the actual P21 release build/recipe gate.

This final closure supersedes earlier runtime-pending entries only for P20's recorded integration scope. P21 full local qualification, exhaustive required fault/doctest checks, actual release artifact/recipes, fixed corpus and completion audit remain required. No production resource was accessed; reviewer ran no Cargo or Git mutations. Report lease returned to root.

### Final evidence SHA-256

```text
b7ac1ef9b9191ef58a6d0cb3555dc628a6cac1e34c72c470409dfe0ad743c798  docs/execution/reports/P20-checks.json (observed before root acceptance/commit)
3356aba8d8b45e08531a85b5cb9f975508ec90b064371b2307989cb5af296ddf  /private/tmp/lwiki-p20-integration-current-before-hashes.json
3356aba8d8b45e08531a85b5cb9f975508ec90b064371b2307989cb5af296ddf  /private/tmp/lwiki-p20-integration-current-after-hashes.json
3915d7abe1bb816e7870313be092acdc3eb2fd3bf460b0281b6b399af3ee1ec9  /private/tmp/lwiki-p20-integration-public-first.log
f83fd3d2265c607588f193fb806b1f9650ff398b68c055978530a44c5348b508  /private/tmp/lwiki-p20-integration-private-ledger.log
9f694cd0c50094188ad8e37923e677c6d17f138602d7907c1f4f4e29382cf370  /private/tmp/lwiki-p20-integration-private-capture-history.log
d84ad52850aaebf34141b24f9a335e6141f907b3445f819a37dc158602e986e3  /private/tmp/lwiki-p20-integration-private-providers.log
63d78530249d0fe807e960f3e828bacb9a0db8a332849bcb5f1d31068f87f455  /private/tmp/lwiki-p20-integration-private-post-export.log
228444653b355a76ea4af8fadc310003e25c57d2765524b40d29979e05ac91ce  /private/tmp/lwiki-p20-integration-private-api-ack.log
e57f50e34e8ed8fcda4aea5ab8546301dcc4dddee96c5e9267f2fd30c4a9c045  /private/tmp/lwiki-p20-skill-current-second.log
2bb3445dfcfae9c11e8488a75183fe6a023f1a7c0116116747d275ad5b776ee6  /private/tmp/lwiki-p20-root-artifact/capabilities.json
```

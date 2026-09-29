# Astra P20 publication and resume review

Status: bounded independent source review completed at the 04:58 UTC snapshot below, followed by explicitly requested bounded addenda. The initial reviews were read only with no Cargo, tests, network, credentials, source edits or nested delegation by the reviewer. Root subsequently leased the new capture_history.rs implementation for R13; its authored code/tests are separately labeled below and require independent final review. The earlier authority report's three fixes and 20 passing ledger tests are not blanket approval of later paths.

## Summary

The new partial-report admission exception is appropriately restricted to local StageChanges tasks with current quotation dependencies; it does not itself restore stale remote authority. Historical codecs and containing-ledger extraction retain paid provenance. The consequential gaps are at the boundaries between validated historical output, current publication and restart.

Source fixes now distinguish local source failures from rejected provider output, turn stale captured passages/completed synthesis into partial-report gaps, bind initial page updates to admitted page hashes, make local stage timestamps deterministic, and carry typed stop codes to CLI error envelopes. These fixes need matching current regression evidence; earlier report/workflow logs predate some of them.

Source corrections also fence proposal application against epoch retirement/cancellation, preserve pending/completed synthesis across resume, expose explicit recorded limit amendments, and add native signal plumbing. Later corrections authenticate own-write continuation, recover pending local report tasks, and omit retired page drafts from newly published reports. No unresolved P1/P2 source finding remains in these inspected fixes; current matching workflow regressions are still required. No P20 acceptance or live-provider/native crash qualification is asserted.

## Findings

### R1 — P1: local source freshness errors became rejected provider responses

Original path: `runner::remote` treated every `validate_generation` error as OutputDisposition::Rejected, settled it and failed the task. AssessGaps and Synthesize perform Current SourceView verification, so a source refresh during the paid request could permanently label a valid historical response as bad provider output. SourceView reports a noncurrent quotation as SourceIntegrity, not only FreshnessConflict.

Minimal reproduction: dispatch an assessment or synthesis with a current citation; refresh that source before delivering an otherwise valid response; observe a rejected receipt/task failure. Narrow correction: distinguish local proof/recovery failures from schema/provider failures, retain an Unknown-disposition receipt and protected spool, and require revalidation/rebind before publication. Current `runner.rs:608` contains this distinction, including SourceIntegrity. Source-closed; a targeted assessment/synthesis race regression is still required (the existing workflow source-edit case occurs during PlanFrontier).

### R2 — P1: stale captured/synthesis output could prevent the required partial report

Original paths: `runner::context` propagated Current verification failure for a completed captured source, and `final_report` called `synthesis::validate(...)?` before report::build's stale-claim filtering. A refresh of a previously captured source, or a completed synthesis citation becoming historical before final reporting, could therefore return an error without publishing the durable partial.

Minimal reproduction: retain a completed capture/synthesis, advance the source revision, then enter the final-report path on a later stop. Narrow correction: withhold stale passages/claims and retain explicit gaps while verifying every surviving quote. Current context and final_report code converts these failures to gaps (final_report currently withholds the entire stale synthesis). Source-closed for those paths; current-capture and post-synthesis races still need regression evidence.

### R3 — P1: first page proposal staging could adopt and overwrite an intervening user edit

Original path: `report::stage_proposal` read the current allowed page at first staging and used its newly observed hash as the write expectation. Citation read guards and allowed RecordRef identity did not bind the page bytes the model was originally authorized to update.

Minimal reproduction: complete paid synthesis proposing UpdatePage, edit the page before the first report::build, then build with stored scope.apply=true. The proposal could overwrite the edit. The earlier changed-page test staged the draft before editing and did not exercise this window.

Root added admitted_page_state and carries the paid task's immutable source bindings through proposal verification; first staging now conflicts unless the page still matches its admitted hash. Plan's with_read_preconditions helper supports propagating page proofs into task identity. The runner must actually add the applicable page proofs to Synthesize tasks; safe refusal of every real UpdatePage is not complete integration. Source correction and a new regression are present; runtime gate pending.

### R4 — P1: proposal auto-apply lacks an atomic epoch/membership fence

At the reviewed `report::build`/`stage_proposal` boundary, authentication is based on an earlier LedgerInspection. Page ChangeDrafts contain quotation/page guards but no fence against config-only operational rebind.

Minimal interleaving: process A authenticates an active completed paid synthesis; process B pauses and rebinds to another model/config, retiring that synthesis; A prepares/applies a page proposal with apply=true. Sources/pages are unchanged, so storage guards pass. A later report publication conflict is too late: the page was already changed. Rebind does not necessarily update run.md, so a run-note hash alone is insufficient.

Narrow correction: acquire the writer permit first, then recheck the exact synthesis task/current binding under the run lock and keep the authority fence through guarded apply (never acquire the writer while holding the run lock), or an equivalent atomic publication mechanism. Add a deterministic two-party test proving either publication wins before retirement or the retired proposal refuses without changing the page. Open at the initial report snapshot; root notified.

Follow-up source inspection: root added with_research_publication, entered after the writer is held and spanning engine.apply. It checks exact epoch, active completed Synthesize task, current bindings and cancellation. This closes the retirement/cancellation race at the inspected source level; deterministic runtime regression remains required. R8 describes the separate own-write continuation problem.

### R5 — P1: local stage restart could conflict solely because the clock advanced

`stages::output_write` identifies the output using (run, StageOutput), excluding `now`, while serialized frontmatter includes a timestamp from `now`. `publish_local` reconstructed those bytes and required their fresh hash to equal an existing output. InspectExisting used the current execution clock.

Minimal reproduction: apply the InspectExisting output changeset, crash before finish_local_task, advance the clock and resume. The same output ID acquired a different expected hash, permanently conflicting with retained valid bytes. Narrow correction: deterministic timestamp or authenticate/reuse the retained record before finishing the task. Current publish_local and publish_generation use immutable genesis creation time. Source-closed; a clock-advanced applied-output/unacknowledged-task regression is still required.

### R6 — P1: self-authored Markdown can make final-round resume require an unavailable extra round

`runner::resume` compares the entire BindingEpoch, including a freshly scanned source_snapshot, then chooses rounds_started+1 whenever the current round has been assessed. `Catalog::canonical_snapshot` gets control_manifest from a hash over all Markdown, including newly created run receipts/output notes (`catalog/scan.rs:492`). Normal run progress therefore changes this snapshot even without a source/config edit.

Minimal reproduction: finish assessment of the final allowed round, retain a pending synthesis and pause, then resume with unchanged relevant inputs and sufficient remaining/amended request budget. Resume enters the rebind branch because its own Markdown changed and returns "no remaining research round". Narrow correction: separate relevant proof changes from diagnostic whole-vault snapshot changes, and continue still-valid work in its existing admitted round. An assessed-final-round pending-synthesis regression should prove no extra round/source/request counter is reset or invented. Open at the initial snapshot; root notified.

Follow-up: current code ignores snapshot-only differences and supports final synthesis at the round ceiling. Also test default rounds=3 with round 1 assessed stop=true and a request cap of four: increasing the request cap to five should finish the pending synthesis. Fresh inspection now discovers the run's own captured source, changing input_records/read_preconditions even without external edits; using only round-ceiling equality to select synthesis would instead spend the new slot on round 2's frontier. Pending valid synthesis and the persisted assessment's stop semantics must govern continuation.

Latest source inspection: final_synthesis now also reads persisted assessment.stop, no-progress and existing synthesis. Both final-round and stopped-earlier-round regression cases are present; current runtime evidence remains pending.

### R7 — P2: CLI stop/resume/cancellation contracts need concrete wiring

- Run errors were converted into successful ResearchOutcome values, yielding exit 0 for budget/cancellation stops. Root added typed stop_code and CLI error envelopes retaining durable research_outcome data, partial/warnings and actual network activity. Source-closed; native budget exit 7 and cancellation exit 130 tests remain required.
- ResearchResumeArguments accepts lifetime/deadline flags through RemoteArguments, but the inspected runner calls ledger.resume(None); native.runtime's newly computed limits/deadline are unused. Explicit raises silently do nothing, and expiry remains impossible to amend from this CLI. Preserve limits for ordinary resume; expose explicit recorded raises or reject unsupported overrides. Root is addressing this.
- native_job_options created uncancelled tokens with no process SIGINT/ctrl_c handler. Library mock CancellationToken tests do not establish actual Ctrl-C flushing/partial/130 behavior. Root is adding CLI-only signal-aware tokens and an actual child-process SIGINT test; default library tokens should remain isolated.
- Once a completed synthesis exists, cancellation before the next loop can enter final_report with stored apply=true. report::build must not start new page application after cancellation; retain partial/staged references instead. Combine this check with R4's publication fence. Open at the initial snapshot.

Follow-up source inspection: Resume now has dedicated provider/retry arguments and explicit bounded `--amend-limits` JSON (complete lifetime limits, absolute deadline, reason), routed to a ledger-stamped amendment path; default resume supplies None. Old ineffective limit overrides are no longer accepted. Native main installs Unix SIGINT / Windows console handlers that only set a static atomic flag; native_cli tokens observe it, while default library tokens remain isolated. These source changes and the R4 cancellation gate address the identified wiring gaps; native subprocess/monotone-amendment runtime evidence is still pending.

### R8 — P1: applying an allowed page can invalidate its own remaining publication path

The page-proof and publication-fence corrections expose a necessary integration case: a successful UpdatePage changes a path in the synthesis task's immutable source bindings (and normally the effective epoch read preconditions). A second proposal's publication gate then rejects the first changed page. More importantly, a successful single update does not mark the report partial; report::publish admits its StageChanges task through ordinary frontier admission, which rebinds all active tasks and rejects the now-changed synthesis page. The page has already changed, but the requested durable final report is not published.

Minimal reproduction: a current allowed Page is bound into a paid Synthesize task; output proposes one valid UpdatePage; scope.apply=true; report::build applies successfully; report::publish encounters old page proof freshness. Existing explicit-apply coverage used CreatePage, which does not mutate an already-bound page.

Narrow correction: handle authenticated own committed proposal transitions explicitly while preserving guards against external edits, and exercise both a single update and multiple independent updates through build/publish/completion. A broad stale-proof bypass would reopen R3/R4. Root notified; open at the initial follow-up inspection.

Recommended authority model: one bounded ChangeEngine transaction for accepted proposals (reject duplicate targets), applied under the original epoch fence, makes its existing recovery journal the authority for completing interrupted writes. If separate changesets remain, permit only exact before→after substitutions from already-Committed same-run/same-synthesis manifests verified against original proposal content and original page/citation proofs; require current bytes equal the committed after-state. Neither path changes paid admission or grants an effective epoch. Final local report control can cover complete as well as partial reports when it fully authenticates their projection/current evidence; the partial flag is presentation, not authority. A dedicated terminal completion path may verify current report bytes, historical receipt/output integrity, active completion and settled terminal accounting without demanding pre-apply source freshness from historical synthesis. Generic stale remote admission/resume must remain denied.

Correction after inspecting ledger::load: with(true) validates the canonical run/checkpoint, not bind_inputs. complete_run already checks active task completion and all historical settled/terminal attempts; finish_local_task checks its own bindings. The suggested dedicated terminal completion API is unnecessary and withdrawn.

Latest source uses publication_substitutions to authenticate committed same-synthesis manifests, original before/proposed payloads and exact current after-state. The writer→run guard applies substitutions only to matching before-states in cloned local guard inputs; it does not mutate tasks/epoch/accounting or paid authority. local_report_control now covers complete/partial reports and calls full projection authentication. Exercise actual runner resume after a partial proposal batch too: automatic source rebind must not mistake authenticated own changes for external edits and replace a still-usable completed synthesis with another paid call.

Root subsequently added resume_research_publication for this exact local continuation. Independent inspection found its guard appropriately bounded: same epoch/active completed synthesis, every active remote task completed, all historical attempts settled and terminal, full trusted capability/profile/config/endpoint rechecks, nonempty authenticated own-write substitutions into cloned proofs, retained output validation, and ordinary monotone amendment/cancellation/clock/deadline gates. Paid admission still uses immutable original bindings. Runner wiring and no-new-paid-call regression remain pending at this inspection; no separate completion API was added.

Final source follow-up: the same guarded local resume now permits an empty substitution map when original proofs still bind. This preserves completed synthesis after apply=false cancellation without regeneration caused by self-authored source/snapshot drift. The runner calls it before fresh inspection/rebind; actual changed source/service proofs still deny it. Own-write and completed-synthesis resume tests require current runtime results.

### R9 — P1: interrupted local report publication leaves an unrecoverable pending task

At runner.rs:1491 the ready-local branch accepts only InspectExisting. report::publish admits a StageChanges task before publish_local writes its output and calls finish_local_task. A crash in either gap leaves an active Pending report task. runner::recover only handles paid attempts/Running tasks; the scheduler therefore returns "unexpected local research task", publishes a different error report, and leaves the original pending task preventing completion.

Minimal reproduction: stop after report frontier admission, or after its local output applies but before TaskFinished; advance the clock and restart. Narrow correction: authenticate and finish the exact retained report descriptor/output before ordinary scheduling, without regeneration or another paid request. Include both interruption windows. Root notified; open at this source inspection.

Latest runner::recover now authenticates Pending StageChanges report descriptors and invokes report publication before scheduling, then verifies the original task completed. This closes the unchanged-input recovery path at source level; the clock-advanced pending-report regression is present and awaits its runtime gate.

### R10 — P2: local report admission must retain every recomputed publication dependency

authenticate_local_report currently recomputes report_read_dependencies but discards the returned vector. is_local_report_task checks claim/passage dependencies but not all prepared-change target/read dependencies. A caller constructing a report task directly can omit proposal guards: admission verifies the report once, then finish_local_task only binds the supplied incomplete task dependencies.

Minimal reproduction: construct an otherwise authentic report containing a prepared page change, omit its target dependency from the task, admit it, change that page before local output publication. Narrow correction: require the authenticated report's full recomputed dependencies to equal the task's source_bindings (or a checked required subset) and retain the same checks during recovery. The normal report::publish caller already supplies those dependencies; this is a shared-ledger boundary gap. Root notified.

Latest source closes this gap: authenticate_local_report returns the complete dependency vector and is_local_report_task requires exact equality. No independent runtime test of direct omitted-dependency admission was run by this reviewer.

### R11 — P1: a prior synthesis proposal can prevent publication after explicit rebind

runner::run initializes changes from the latest report's entire proposed_changes vector. report::build carries these references verbatim; report_read_dependencies correctly requires each marked page change to match the new report's projected synthesis. After synthesis retirement, these requirements conflict.

Minimal reproduction: cancel after completed synthesis so the partial report retains a staged page proposal; explicitly resume after a model/source change; produce a new synthesis with a different proposal. Final publication still carries the old marked draft and fails "report prepared page change has no projected proposal". Narrow correction: carry only authenticated extraction references plus proposals of the current synthesis into the new report; leave retired page drafts discoverable in the immutable prior report, or use explicitly historical references. Do not weaken the marker/proposal ownership check or auto-apply the old draft. Root notified; source fix pending at this inspection.

Final source follow-up: runner now removes all historical research_proposal-marked references from the carry-forward list and lets current synthesis rediscover them through strict stage_proposal verification. This also handles a repeated proposal body whose old manifest before-state differs from its new synthesis admission: it becomes a proposal_conflict gap rather than inserting an unverifiable retained draft. Source-closed; model/source rebind regression remains required.

## Checks and evidence boundaries

Read the selected P20 contract, providers/jobs sections 7–10 and CLI error/dry-run/cancellation contract. Inspected current jobs partial-report transition, runner/report/stage/plan paths, existing-ledger extraction and CLI wiring. Static diff checks passed for the selected leaves. No Cargo was run by this reviewer.

Read actual saved report log `/private/tmp/lwiki-p20-reports-third.log`: 8/8 passed, 5.99s compilation and 8.30s tests. This predates later page-proof/epoch/cancellation fixes. Read `/private/tmp/lwiki-p20-workflow-second.log`: 9/10 passed, 8.43s compilation and 114.90s tests; explicit_model_rebind failed at a fixture pause transition. This is a failed target, not a passing workflow gate. Later source changes and native CLI tests require their own evidence.

Initial source/evidence snapshot at 2026-09-29 04:38 UTC, SHA-256:

```text
dec4f080345e9e1a04ce118fc280b52182ae60fc59c4d12bef1f0820cbe53d15  src/jobs/research.rs
aed5cd9cd484fc9801198fc3ff934ded5733fce821624146e6a03284b0cc44fb  src/research/report.rs
95c756a6b46c1317855042e7b4fd265b1f20c06bebf659cfa53a32ba57d660d2  src/research/runner.rs
7335875d4a45902b86162e88d8c9faa469de2ed8ba9d65e94df1bfb7a3e3e084  src/research/stages.rs
944d555974341b89ad80d446c5daa41de30c996aeeaa9a81fbad4f7e8733319d  src/research/plan.rs
d8d02116990d775527d5b0bab2cb4f4010f64b59bef255235cbe70599c92cf2a  src/graph/research_extract.rs
d3e79950226e2d11d657a7c76bd174f220d84c4aaaadb0477d8e3037f4a84052  src/graph/api_extract.rs
728aef0a604c001fc925fda661955d198b5d6225ead2579b34853a3eaf2ecb59  src/research/acquire.rs
06005f72e3ac24df849be13a7aba6833e7225bfd5cb4f989dc345831925e463c  src/cli/research.rs
432f7cff96529be1edb6857ce94fa5b0b03330fe938516923e48c676a66e0a33  src/cli/dispatch.rs
ebfaec4e38eba1eb57782ead4cad54da5212904557e22128101f957a588eb8df  src/research/types.rs
8e08d8b2de6c0e61128e267f91346a0237f0366c570c87ada97813fd336fb881  /private/tmp/lwiki-p20-reports-third.log
ee989b59026f3398c52f19d40e3c8d3e4dd8dac0067d8f331fad2ec94a65733a  /private/tmp/lwiki-p20-workflow-second.log
```

This snapshot does not qualify later shared-tree mutations, external providers or native crash safety.

Follow-up snapshot, 2026-09-29 04:51 UTC: read actual `/private/tmp/lwiki-p20-reports-seventh.log`, **13/13 PASS**, 1.21s compilation/15.09s tests. It includes first-staging edit refusal, epoch retirement race, two authorized updates, interrupted proposal continuation, external edit refusal and stale-epoch partial report/hold cases. `/private/tmp/lwiki-p20-cli-stages-third.log` is **6/9, FAILED**, 11.65s compilation/9.26s tests: doctor JSONL/metadata expectations and completed research spool expectation fail. Later changes must receive separate evidence. The following are observed source hashes, not a claim that the test harness recorded these hashes at execution:

```text
f5b7e3e2395622897c5434211b7af3fc67eb64ef820a78de4b918820788ae35e  src/jobs/research.rs
399e8adb5cf47bb7a12f9f3aeddc71b65be0ee33b135d007f95e08a4ca32e914  src/jobs/ledger.rs
3e82775f577be2323824e9c0e61b07d6b9de79d4688f229e667481ad8e6921bf  src/research/report.rs
dc3f461700a488d6e8ac76dd4ae167f90be2162ae454e738cc47c56622219e4a  src/research/runner.rs
7335875d4a45902b86162e88d8c9faa469de2ed8ba9d65e94df1bfb7a3e3e084  src/research/stages.rs
944d555974341b89ad80d446c5daa41de30c996aeeaa9a81fbad4f7e8733319d  src/research/plan.rs
363cfc173f367921d9541c7615fd1e980b00d22a1ddbb1a64d4a1ff3f46a46fd  src/cli/research.rs
0c1c56ca79ebda306e6c5f22dcdbaec9ad702d451506df35ed91451183f02328  src/cli/interrupt.rs
18074c8e7f888a68c4e80eb6d26db099bb25fc13e3aea3fca84776381d168ead  src/jobs/types.rs
b1c76c6ca259ce081b622302cf0563d4fa9e0cea7ccd17909924ca46a2e08c21  tests/research_reports.rs
e856f4c232c84424a8615130f12dff114af5bae065ea6ce52dbf5888fa651126  tests/research_workflow.rs
ad42861eca240f531ffa489727aa1e8e3d5690dbf975e07c2d04f671db22e07f  tests/research_cli.rs
0d8d336f2c193366187b0bd6fb14b5834654542df6312a69a8190928f1d9a6e0  /private/tmp/lwiki-p20-reports-seventh.log
fb73a83242b1ad38fa16bbf738386d9f20a9340417769f2631cede51bf5359e9  /private/tmp/lwiki-p20-cli-stages-third.log
```

Final reviewed-source snapshot, **2026-09-29 04:58 UTC**. Selected diff whitespace checks pass. Actual CLI fifth log is **10/11, FAILED** (2.03s build, 14.99s tests): native SIGINT entered-request/130/partial, explicit amendment, and native research completion/budget tests pass; doctor multi-role fixture fails on retained unknown-accounting disclosure. Its binary predates the latest local-resume changes. No current workflow pass has been observed. No test result is attributed to source mutations after its compilation.

```text
12b609050bbf1e4c7d30ef305be72fb12ed9fcf300ec509e0d933e6fa65ebc86  src/jobs/research.rs
f436773d257620e22927a967bb9eeb83b910d7adebbf11253c14613b6c08d5a7  src/jobs/ledger.rs
3e82775f577be2323824e9c0e61b07d6b9de79d4688f229e667481ad8e6921bf  src/research/report.rs
c9b6f1fc89ce2d2022085ecbdb368f588a86d2a37a0abda51c6b40883336786c  src/research/runner.rs
7335875d4a45902b86162e88d8c9faa469de2ed8ba9d65e94df1bfb7a3e3e084  src/research/stages.rs
363cfc173f367921d9541c7615fd1e980b00d22a1ddbb1a64d4a1ff3f46a46fd  src/cli/research.rs
0c1c56ca79ebda306e6c5f22dcdbaec9ad702d451506df35ed91451183f02328  src/cli/interrupt.rs
18074c8e7f888a68c4e80eb6d26db099bb25fc13e3aea3fca84776381d168ead  src/jobs/types.rs
5d97fdf2db623f4d65693078a145915af9cabbac69e2a0ac07d42beec9430f19  tests/research_workflow.rs
b1c76c6ca259ce081b622302cf0563d4fa9e0cea7ccd17909924ca46a2e08c21  tests/research_reports.rs
ec2bdced4125b42911bafad51bde3a975498eecfff5489ee52066178a1b091b4  tests/research_cli.rs
7a2fe4a1345d5b9790dbe0b4312ae347a7981d5f3e8b55dd76356e3aa1260066  /private/tmp/lwiki-p20-cli-stages-fifth.log
```

Required evidence still outstanding: latest workflow target including paid assessment/synthesis source races, pending-report admission and applied-output crash windows, own-write and apply=false completed-synthesis resume with no new paid calls, and historical page proposal filtering after rebind. The passing report gate is narrower than these runner paths.

## Bounded follow-up: prior accounting disclosure, 05:06 UTC

Reviewed only the shared prior_accounting_for_new_run helper and its embeddings/probe/research/extraction callers against ledger::check_prior and private namespace discovery, plus the pending-report dependency equality delta. No Cargo/source edits or broader M3 re-review.

The caller change preserves existing histories and holds. It records a conservative Unknown disclosure in new immutable genesis; operational namespaces are included even without canonical notes. Ledger creation independently checks prior histories, so discovery races fail closed. input_fingerprint covers task/source inputs rather than prior_accounting; the complete immutable spec_hash/genesis still authenticates the disclosure. Pending StageChanges recovery now requires exact equality of recomputed complete report dependencies and the admitted task's source_bindings before publication.

### R12 — P2: automatic disclosure discovers fewer restored identities than ledger admission

At app/embeddings.rs:541, the shared helper gathers fixed-layout directory IDs plus operational namespaces. ledger.rs:1868 check_prior additionally gathers canonical Run IDs from runs/**/run.md. Minimal reproduction: restore `runs/imported/run.md` containing canonical `wiki_id: run_old`, with no old operational namespace. The helper discloses `imported` but admission also requires `run_old`; every fresh probe/research/extraction refuses with RecoveryRequired despite automatic disclosure. This fails closed, but blocks the promised fresh-budget path after Markdown-only restore.

Narrow correction: share or mirror the candidate-discovery logic, including both fixed-layout IDs and bounded parsed canonical Run IDs. Match check_prior's 65,536-entry ceiling: the current helper calls unbounded scan_markdown before the bounded final check. Do not infer accounting completion from note prose or erase any old hold. Root notified; source correction pending at this snapshot.

Follow-up source inspection confirms root mirrored both candidate rules, bounded note reads and the same 65,536-entry discovery limit. R12 is source-closed; restored mismatched-layout-ID runtime coverage is still required.

Actual `/private/tmp/lwiki-p20-cli-stages-sixth.log`: **native CLI 11/11 PASS**, 8.71s compilation/15.27s tests; **local stage restart 1/1 PASS**, .74s, covering applied output without TaskFinished after clock advance. This supersedes the earlier failed CLI target for its compiled scope. It does not establish R12 restore-discovery coverage or qualify later mutations. Observed SHA-256 snapshot:

```text
cbf441349052769ef5a9537a09b10b85caa3f1e2531bd89a1fbee4ba6951b985  src/app/embeddings.rs
dbd69213f2dffeb8eb6b5fe8e3e36274bd896ade0f4655e2689c2ba40f586540  src/app/probe.rs
50c488babeecd627021601a6e24d3eef52e0b4b305ed124b326f7267d1580e38  src/cli/research.rs
30f3662dea14a051c8399f544a83361437a17799dde197419602d669b4e90783  src/graph/api_extract.rs
f436773d257620e22927a967bb9eeb83b910d7adebbf11253c14613b6c08d5a7  src/jobs/ledger.rs
81d5f22e75f64a7d8a2dfbfe318744360147f8dd16a5a1a9b55df866ca03a21b  src/vault/operational.rs
384e19d6f22a3c7f33d1b3f35cc426823cdd2fca0b4722a5026efcbcbd34c1c8  src/research/runner.rs
f15fc6f71528c42d414670357b40026266e6705cb1ecb78d47aa7f2dc9b34a05  /private/tmp/lwiki-p20-cli-stages-sixth.log
```

### R13 — P1: refreshing an actually captured source permanently invalidates historical output checks

This follow-up was explicitly requested by root. acquire.rs:256–280 records both immutable Revision and mutable Source header as hash-bound Capture outputs. A legitimate source_refresh updates the Source header's current revision. ledger::bind_inputs:777 requires exact present bytes for every historical task output, including retired tasks, so a rebind cannot make the old Source header hash current again and resume remains blocked.

Minimal reproduction: capture an explicit URL, pause after extraction/assessment, refresh that captured source through SourceStore, explicitly resume with new current source proofs. Old Capture output Source hash causes "durable output changed", even if its task is retired. Existing paid-response source-race tests refresh an initial source and do not establish this case.

Narrow correction: distinguish historical capture materialization from current source authority. For only an acknowledged Capture/Fetch Source output, authenticate original exact bytes using the committed materialization manifest/proposed payload, same receipt/attempt/output ownership and recorded change identity (or equivalent complete proof). Preserve immutable Revision/receipt/output checks, task/read freshness and Current citation validation. Do not globally weaken checkpoint::output or treat a matching mutable record ID as sufficient historical proof. Also skip/withhold a completed capture's historical revision before scheduling graph_extract_agent; revision_content_bounded deliberately accepts retained historical revisions and alone is not a Current proof. Root notified; source correction pending.

R12 correction / R13 finding source SHA-256 scope (05:09 UTC; source review only):

```text
b583243665b698ec37819fd50801bbd69ae048ea901494858c2ac04202bbd465  src/app/embeddings.rs
728aef0a604c001fc925fda661955d198b5d6225ead2579b34853a3eaf2ecb59  src/research/acquire.rs
f436773d257620e22927a967bb9eeb83b910d7adebbf11253c14613b6c08d5a7  src/jobs/ledger.rs
e61b39d043549606c380447f7eabb5060f8dffd747b91fae85ff8d3ea0e8caad  src/jobs/checkpoint.rs
384e19d6f22a3c7f33d1b3f35cc426823cdd2fca0b4722a5026efcbcbd34c1c8  src/research/runner.rs
5fe7c3c25f43fec7d5276fd5c6aad85b01ae440c0eebc82198c7b7afaf65ed82  src/sources/capture.rs
3e47fcba6d36429b175a3f4242643ac425f30855e96f8727d9f1be319590e884  src/sources/revision.rs
```

### R13 implementation handoff (author scope, not independent acceptance)

Root explicitly leased new src/jobs/capture_history.rs plus this report; shared callers remain root-owned. Implemented output(fs, inspection, replayed_frames, task, reference): exact current checks remain the first path; fallback is only Capture/Fetch Source in an acknowledged Running/Completed task. It authenticates same run/task/attempt, current immutable Validated receipt, exact OutputsCommitted event/PreparedChange, committed manifest/vault, and verified original Source plus receipt proposed payloads. Running is accepted only for the authenticated OutputsCommitted-before-TaskFinished crash window. Other kinds retain strict checkpoint::output.

Current Source edits/deletion do not erase historical materialization and confer no new current authority. The helper does not recreate the Source. Active read/task/citation checks remain separate. Six embedded tests use a real fixture ledger, receipt, source changeset and acknowledgment; cover refreshed history/unknown hold preservation, deletion, immutable Revision/receipt tampering, recorded manifest/payload tampering, foreign or absent acknowledgment, and actual AfterOutputsCommitted crash followed by source_refresh/replay/settlement. The fixture uses manually supplied bounded mock response data, not network/provider compatibility.

Root integrated the module into bind_inputs with verified frames, repair_task_finish and settle. Initial acknowledgment remains strict. Replay reusable-output checks should remain strict current-byte checks; historical success must not advertise the old Source header as current. Cleanup's current-byte failure conservatively retains the spool after refresh; explicitly requested cleanup may use the same historical proof for a validated capture without refunding unknown billing, but no broadened cleanup is required to resume/settle. Targeted Cargo is root-owned and pending at this handoff.

Final handoff, **2026-09-29 05:17 UTC**: read actual root-run `/private/tmp/lwiki-p20-captured-source-history-first.log`: `cargo test --lib jobs::capture_history::tests` target **6/6 PASS**, 132 filtered, compilation 10.08s/tests 10.77s, no warnings in the saved log. This includes the actual acknowledgment-before-TaskFinished fault and root's bind/repair/settle glue. No Cargo was run by this agent. Selected whitespace checks pass. New helper/report leases are returned to root; a separate fresh reviewer is required for independent review of this authored invariant. This is not a whole-workflow/P20 acceptance claim.

Observed final handoff SHA-256:

```text
e0941c115fe328186cddf9bfefcef94d995f886a9c1a5ba1ee417abdd00a6fe8  src/jobs/capture_history.rs
4b8c5be64b0734f49aa4d45885ed41611348a551dfcc71064e79e4173ca22bc0  src/jobs/mod.rs
f3796ace8ff35fc8c65fba0dd907ee0e0421cb48c6a80e5b7e8d1bdea8732405  src/jobs/ledger.rs
13efb8e30db4b0e8079f29d86045ffc97d06f76f48772552c88f3d91c3fbba57  src/jobs/research.rs
e61b39d043549606c380447f7eabb5060f8dffd747b91fae85ff8d3ea0e8caad  src/jobs/checkpoint.rs
f5b736faac6541b41fed52e5479aa027bb5d8bf8244e0d994803f226b9429c8d  src/research/runner.rs
b583243665b698ec37819fd50801bbd69ae048ea901494858c2ac04202bbd465  src/app/embeddings.rs
a5e8b956ca94be4f6bcef41822fc29a3466d7d19174b85dbc122df6724878c99  /private/tmp/lwiki-p20-captured-source-history-first.log
```

# Agent research implementation review

Static review, 2026-09-29, limited to `src/research/{types,codec,storage,inspection,engine}.rs` and directly used storage/citation APIs. No builds, tests, live calls, Git, source edits, or delegation. Root is implementing fixes concurrently; observations below identify the inspected candidate, not an accepted final implementation.

The implementation follows the requested host collaboration model: research returns local task packets, accepts inline host-supplied sources and cited claims, and never opens a provider or executes host tools. `AgentReport` origins and `external_tool_usage=unobserved` preserve the observation boundary. The two-stage protocol and separate state avoid reintroducing paid accounting.

## Findings and narrow resolutions

### R1 — High: identical later reports collide with earlier immutable artifacts

Initial `storage.rs:69–75` (`artifact`) hashes only the serialized value. A report contains a run ID but no generation/round. Return identical claims/gaps with a follow-up in two successive rounds: both partial reports have identical values and therefore the same path; the second create uses ExpectedState::Absent and fails ContentConflict. The concern is within one run: current packet/report/receipt values already contain a run ID, preventing the proposed cross-run counterexample.

Root changed identity to hash `(run_id, generation, label, value)`. This was independently observed in formatted `storage.rs:135–145`, hash `d9a76d2ee3f155651b63e6c652033d8e263d3ccbdf4c63d2260cb632edf11e9d`. **Static cause closed; runtime regression pending.** Test two continuing rounds with identical report content and then a successful final completion.

### R2 — High: an acknowledged identical import retry can fail on newer stale evidence

Initial `engine.rs:132–145` authenticates the matching retained receipt and its immutable outputs, then calls `outcome(..., verify=true)`. That verifies the current outstanding packet. After a legitimate source refresh/withdrawal, retrying a previously successful submission therefore returns FreshnessConflict even though its immutable import is intact. Mutable source heads are deliberately excluded from receipt seals, but current-packet verification reintroduces the same failure indirectly.

Return successful historical reuse independently of current evidence readiness. A stale current packet must be omitted or explicitly not ready, with a concrete refresh action; do not return stale execution authority. Root announced `import_already_committed`, `reused=true`, no packet/ready flag, and a refresh instruction. **Fix announced, not yet present at the final source read for this report.** Test acknowledged retry after refresh and withdrawal, including dry-run, and require unchanged IDs/counters/import count.

### R3 — High: ordinary resume returns readiness without recovering pending import publication

Initial `engine.rs:84–87` acquires the writer/recovery path only for `--refresh`. Normal resume reads the current head directly and returns a ready packet. After interrupted apply before the final head operation, the old packet may still be returned as actionable even though a durable in-progress import will consume it on recovery. `SourceView::from_fs_bounded` and passage verification do not themselves recover or reject that pending changeset.

Recover under the normal writer permit before a non-dry resume issues a packet, or explicitly detect pending work and refuse readiness until recovery. Dry-run must remain read-only and return `persisted=false`, `ready_to_import=false`; initially normal dry resume instead returned both true. Root announced the recovery/readiness fix. **Not yet present at the final source read.** Inject interruption after receipt/packet publication but before head replacement; normal resume should recover one authoritative next state, never the consumed packet. Dry resume must neither repair nor authorize it.

### R4 — High: legal acquisition can become impossible when previous passages fill the packet

Initial `engine.rs:166–185` clones all previous passages and appends every new source; `packet` rejects more than 32 passages or 64 KiB. With 32 existing passages, even one small new source is unimportable although `remaining_sources` is positive and the packet requests acquisition. The same issue appears when accumulated quotation bytes fill the cap. The per-submission schema/source bounds do not expose this smaller effective capacity.

Select a bounded next context deterministically: prioritize newly captured sources, fill remaining passage/byte capacity from prior evidence, and expose any omitted context. New sources together already have a 64-KiB aggregate source-content bound, so they can fit if selected first. Alternatively publish the effective capacity and an explicit evidence-selection route. Do not silently advertise acquisition that the only import path cannot accept. **Open at final read.** Test 32 current passages plus one new source and a nearly full byte budget plus a new source; require successful bounded handoff or an actionable selection protocol before external acquisition.

### R5 — Medium: offline library start can publish before rejecting its own policy

Initial `engine.rs:64–82` trusts `scope_value.offline` independently of `app.options().offline`; policy is checked by `outcome` after publication. An offline app with an online scope writes the run/packet then errors, while plan/dry preview can return external-acquisition instructions. CLI normalization alone does not enforce the exported engine function's contract.

Reject inconsistent scope or force offline scope before locking/planning/publication, and include that decision in the scope hash. Test the library entrypoint as well as CLI for plan, normal start and dry start. **Open at final read; root notified.**

### R6 — Medium: packet dependency verification has an unbounded physical reread

Initial `inspection.rs:44–48` uses `fs.read_before` for every stored dependency after bounded `SourceView` proof verification. That API calls `fs::read` without a limit (`vault/fs.rs:268–277`). Generated dependencies currently correspond to source capture writes, and the preceding proof usually limits those same files; a file growing between reads, or an excessive additional retained dependency, defeats the physical bound on this second read.

Use `changes::prepare::read_bounded` or the existing bounded expected-state path and compare hashes. Keep the aggregate packet/context limits as well. **Open at final read; root notified.** Test an oversized stored dependency before accepting an import; a fault hook may exercise growth between proof and recheck if practical.

### R7 — Medium: packet remaining allowances are not bound back to head counters

`storage::load_packet` checks scope hash/generation/round/fingerprint, but does not require `remaining_sources == scope.max_sources - head.captured_sources` and the corresponding byte equality. Editing only the mutable head counters downward can silently increase accepted lifetime imports while the immutable packet still advertises its original remaining allowance. `load_head` also does not reject duplicate receipt packet keys.

Require these equalities and uniqueness when loading retained state. This catches inconsistent state without introducing a paid ledger. **Open at final read; root notified.** Change only one head counter and verify resume/import reject it before capture planning; duplicate receipt keys must also fail.

### R8 — Medium: unresolved gaps cannot be resolved and are absent from handoff context

Initial `engine.rs:184,197–201` only appends to `head.gaps`. A collection-stage gap remains in every later report even when a new answer resolves it; completed reports remain partial and repeated identical gaps exhaust the 64-gap limit. The packet has no current-gaps field, so a fresh host resuming before the first report cannot see what remained unresolved.

Expose current gaps in the packet, deduplicate them, and define answer gaps as the current unresolved set (or provide explicit bounded resolution IDs). Immutable submissions already preserve gap history. **Open at final read; root notified.** Test a collection gap, follow-up evidence, and an answer resolving it; historical receipts retain the original gap while the current report can become complete.

## Boundaries that looked sound in this candidate

- Strict submission decoding rejects duplicate keys before DTO construction; total input, source content, field, count and depth bounds are finite. Packet/artifact bounds remain separate. The decoder builds a bounded-byte JSON value before its lower node/depth pass; this is not an unbounded input allocation.
- Packet scope and identity hashes include concrete run/vault/generation. Answer citations use explicit packet-local `pN` IDs mapped to verified source/assertion citations; quote hashes cannot pass membership checks. Claims remain `unassessed`.
- New source assets, source records, next packet/report, receipt and final head are one guarded changeset. Head replacement has an expected before hash and depends on all output operations. Receipt checks occur before allocating new source IDs. The source capture plan's operation dependencies are retained when merged.
- New capture passage dependencies cover the source head, revision note, original bytes and extracted bytes. `SourceView::verify` verifies those exact underlying source records/content. There is no concrete mismatch merely because they were initially collected from all source writes; the physical reread in R6 is the issue.
- Receipt outputs intentionally omit mutable `source.md` while preserving immutable revision/original/content and generated artifacts. `provenance` remains in the full retained submission receipt, and source origin kind remains AgentReport. This does not claim observed HTTP or host billing.
- Refresh builds a new generation, retains outstanding-packet dependency, drops withdrawn sources from current selection and verifies selected current evidence. It may replace old assertion context with a bounded current source prefix; the newly issued packet makes that change explicit rather than silently changing old citations.
- Catalog canonical inspection uses an in-memory index, so the observed plan path does not itself create a disposable cache. No research path invokes generation, embeddings, network, an MCP client, a shell command or a provider profile.

Usability limit to document: newly captured and explicitly selected sources expose only their first 4,096 UTF-8 bytes as a passage. The stored source can be longer, but the answer cannot cite a later passage without another explicit selection mechanism. Avoid describing this packet as complete document coverage.

## Source evidence

Initial inspection hashes (line references marked “initial” above refer to this unformatted candidate):

```text
c45f61e1d864ab98446922f98dee4e85f090b9423630196b1f10ff8bb19f45dc  src/research/types.rs
62511768096e3da11b88e14771d1f922ce2d949df1a282988256690abcff24b2  src/research/codec.rs
3851a4e441def417f342ba3e1490eb269d04f0292b5da1080fb2521d4e1ea2c8  src/research/storage.rs
204a922558f5ef5f224cba5f6d4482cad0085ad61289376553ec986692d2c96d  src/research/inspection.rs
9c7edc3b45c0a881c88fb06770000b0530f683437b47e9c094bd4a7db41af3e4  src/research/engine.rs
```

Final observed formatted candidate, including R1 identity fix; other announced fixes were not yet observed:

```text
656cecc00b21c4d3998f880c00314485b585834c5b831dc7f979da337399329f  src/research/types.rs
a416b607266aca45d8745ff77129e63e092cf5066b20cd18316f8f2766a8d3eb  src/research/codec.rs
d9a76d2ee3f155651b63e6c652033d8e263d3ccbdf4c63d2260cb632edf11e9d  src/research/storage.rs
21c3dcb58b56067f44a64f94587fae464d5d55f02f9b96a24b728030cfe8f1a0  src/research/inspection.rs
36e2d1bae0c5dfad49a5e037b01f260981bf431b55d6531feb55260fba11e180  src/research/engine.rs
```

Disposition: R1 static resolution observed; R2–R8 need targeted resolution/qualification before final acceptance. No replacement runtime evidence was available to this review. Report lease returned to root.

## Resolution rereview

Root authorized a bounded rereview after all eight fixes landed. **R1–R8 resolution predicates are now present and statically close the reported causes.** No builds or runtime checks were performed by this reviewer.

- R1 retains the run/generation/label/value artifact identity fix.
- R2 validates the historical receipt/output hashes first; a FreshnessConflict from current packet verification yields successful reused import, no packet/readiness, `freshness=stale`, and a concrete refresh command. It does not grant stale task authority. `imported_sources` comes from the original retained receipt.
- R3 normal resume now calls `lock` and recovers before reading/issuing the head. Dry resume obtains no writer and suppresses persisted/readiness/next-command fields.
- R4 new captures precede previous passages; `packet` selects a bounded count/byte context, deduplicates citations, and reports omission warnings. Captured source IDs remain in the import result even if omitted from context.
- R5 offline scope mismatch is rejected before locking/planning or publishing.
- R6 dependency rechecks use `read_bounded(..., 64 MiB)`.
- R7 packet remaining allowances must match head counters; receipt packet keys must be unique.
- R8 packets expose gaps; collection deduplicates them; answers replace the unresolved set, with instructions to submit complete current synthesis/current gaps. Immutable receipts preserve earlier submissions.

### Added-field issue: report-only outcome incorrectly claims currentness

Current `engine.rs:164–175` initializes every outcome's `freshness` to `current`. If a completed report's source is subsequently refreshed or withdrawn, `resume` has no outstanding packet, so no citation verification runs and the historical report is labeled current. This is a new issue introduced by the freshness field, separate from R2's fixed outstanding-packet branch.

Use an explicit retained/unverified-currentness value for report-only outcomes, or verify report citations before choosing current/stale. No task authority should be issued for a completed report. Root was notified; **this delta remains open in the inspected hash below**.

### Generic Run accounting compatibility

No concrete conflict found. `app/embeddings.rs:541–586` (`prior_accounting_for_new_run`) filters `runs/*/run.md` before reading a canonical Run. `jobs/ledger.rs:1712–1757` (`check_prior`) uses the same filename filter. `RunStore::discover_existing` (`vault/operational.rs:216–243`) scans only `.wiki/state/jobs`, which agent research does not create. The new `runs/<id>/research.md` therefore does not become an alleged paid historical run merely because its kind is Run.

Catalog code treats both kinds as operational records (`catalog/eligibility.rs:837` and `catalog/scan.rs:103ff`) without requiring a paid run-plan body; generic ledger decoding continues to use its fixed `runs/<id>/run.md` path. Keeping distinct run IDs avoids the ordinary global canonical-ID collision; this is not a reason to add a compatibility codec or paid ledger to research.

Add a same-vault research → probe/embedding/API run creation regression to exercise discovery end to end: research alone must not create `PriorAccounting::Unknown` or be passed into a paid codec. Then include a real incomplete generic job and prove its unknown-accounting disclosure remains enforced.

### Remaining test gaps observed

The available `tests/research_handoff.rs` contains four authored cases, not runtime evidence. Its identical successive partial-report comparison exercises R1. Its first case refreshes a source only **after completing the run**, then expects `status=import_already_committed`; current code instead takes the report-only success path and returns `completed/current`. That assertion does not exercise R2's outstanding-packet FreshnessConflict. Add or move a refresh before Answer submission to test R2; separately cover report-only freshness.

Still needed for the changed invariants: interrupted apply recovered by ordinary resume; dry resume flags and exact tree purity; full count/byte context accepting a new source with omissions; offline library-scope mismatch rejected before writes; corrupted remaining-counter/duplicate-receipt head; oversized dependency; and a formerly open gap resolved to a complete report. Root owns test selection and execution.

Rereview source hashes:

```text
ae62e2486e75015c6eef385eb050a63b16a7bc7b6bae0efe0b128e9ca760a8b2  src/research/types.rs
a416b607266aca45d8745ff77129e63e092cf5066b20cd18316f8f2766a8d3eb  src/research/codec.rs
67070a02a1885592b7f1d3d4907246a8a84b52cc386f5d72735a50b7db1a5bdf  src/research/storage.rs
42d6b73a3600acee136eff227f115ecc4bd5ddfd7b572f0595ea9c6624987e68  src/research/inspection.rs
932aa9ad2d40e11a66782fd6ca00655080ab925dcd6bb1e0fffcade99ba62137  src/research/engine.rs
8e68268d94e5c9fdcccf38157708bb8ef350c224c849e961560ade1964f36d47  src/app/embeddings.rs
fba100325ce2992dae84c84071f48fd4e03189c8ad74474d16e1f00230737f09  src/jobs/ledger.rs
9571416bb599834e9e4456ba7f15f3d2623ce286f03934f11386cf88a260ad7b  tests/research_handoff.rs
```

Rereview report lease returned to root; no source changes or builds performed.

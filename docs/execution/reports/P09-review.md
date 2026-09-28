# P09 independent review

Reviewer: independent `gpt-6-sol` fallback; requested Astra was unavailable at the runtime thread limit. The implementation and review gates remain required. Review started against dirty P09 work above accepted P08 HEAD `98d59983d0b11d7031f8a77b45267e6ea910744d`. Source access is read-only; this report is the reviewer's only write lease. No Cargo, provider/helper execution, production vault, or source edits were used. Existing-binary probes used disposable copied fixtures, as attributed below.

Status: **independent source review complete; R1 and R2 resolved** against the stable worker handoff below. No open blocking review finding remains. Root still owns the repository-wide acceptance gate, command activation and final integration fingerprints.

## R1 — bridge overlaps must coalesce the entire component

Blocking on the initial tree. Both `bundles::select` and `context::assemble` stop at the first successful passage merge. Same-revision quotes encountered as spans `[0,5)`, `[10,15)`, then `[4,11)` leave overlapping `[0,11)` and `[10,15)` passages. In selection, the one-support-group rule can discard the latter passage and its contributor; in global packing, overlapping text can survive and consume the document passage allowance. This violates retrieval's overlap deduplication and contributor/stance preservation contract. It is a direct source reproduction, not a claim of an executed regression.

Requested fix: merge to the full overlap-component closure before support-group selection, and during global packing remap every existing/new bundle passage index after coalescing. Root and worker were notified. The subsequent source fix restarts component scanning after each union, returns an old-to-new passage mapping, and remaps/deduplicates all prior/new bundle indices before render/admission. `bridge_overlap_closure_preserves_all_stances_and_remaps_prior_bundles` exercises separate prior bundles and then one assertion with a contradictory bridge, retaining all three contributors in `[0,15)`. Disposition: **resolved**. Stable source re-review confirms overlap-component closure and all bundle-index remapping; the named regression passed in the final focused run.

## R2 — historical assertion citations must report citation lifecycle

Blocking on the initially probed tree. `context::render` labelled assertion citations using the evidence row's derived eligibility. Active evidence on an active current source can retain evidence eligibility Current after its assertion becomes rejected/superseded. `SourceView::verify` correctly classifies the corresponding Assertion citation Historical because its assertion is not accepted. The rendered `Citation (Some(Current))` label therefore contradicted actual citation verification, despite the bundle's historical assertion label.

Executed reproduction: copy `tests/fixtures/bootstrap/vault` into a disposable temporary directory, replace `forward.md`'s `wiki_status` with `"rejected"`, and invoke existing `target/debug/lwiki --wiki FIXTURE --offline --json context assertion_00000000-0000-7000-8000-00000000000a --target graph --strategy relationship --scope historical`. Exit 0; bundle status `rejected`/eligibility `historical`; all three citation text labels `Citation (Some(Current))`. The probe binary SHA-256 was `382ef7944b26aed8f8154e4cc2155755427933333b367992931839c5ee7dc94f`; it is attributed as an intermediate binary, not final source validation. Current source independently confirms the same branch. Root and worker notified.

Requested fix: derive each citation's state from canonical source head/withdrawal, evidence authored lifecycle, and assertion authored lifecycle, matching SourceView's state rules, or carry the actual verified state. Keep contributor derived evidence eligibility distinct from citation state. The new `historical_citation_lifecycle_matches_verified_rejected_assertion` regression compares every exact reference with SourceView Historical verification, expects rendered Historical labels, and separately preserves current/active evidence contributor state. Disposition: **resolved**. The pure `citation_state` helper now matches SourceView lifecycle priority (withdrawal/head, then evidence active/assertion accepted), adds no filesystem reads, and the regression passed in the final focused run.

## Boundaries examined

- Initial and final canonical membership/control/dependency reads reconstruct the complete derived projection with `project_closed`, then compare it to the pinned projection. This binds cached prose and eligibility to captured bytes, beyond self-consistent SQL/projection hashes. The forged projection JSON plus ordinary document-row regression specifically exercises that gap. `SourceView::from_closed_input` and `expected_state` refuse missing captured inputs rather than reading disk.
- Current document assembly excludes draft/unadopted/invalid notes and unsupported entity descriptions. Graph assembly rederives canonical propositions and every path step; Current requires accepted/Current steps, directed continuity and the selected terminal assertion. Mutable discovery text, citations and qualifier claims cannot grant authority. ContextResult sealing is private; standalone assembly returns an unverified draft.
- Historical output retains actual contributor eligibility/authored status and renders source citation/path states. Snapshot output labels unverified text and contains no citations. Required contradiction selection reads the complete evidence registry, independent of discovery display caps, and support/contradiction bundles are admitted or omitted together.
- Owner rank fusion uses the best direct/graph source rank with RRF constant 60. Whole rendered passage/bundle headers, references and qualifiers count toward bytes and `ceil(UTF8 bytes / 4)` estimated tokens. Reserves, document2 and Combined graph50% are checked before committing a candidate packet.
- Proof counters/deadline are shared across the initial/final captures and the single refresh/reassembly retry. Closed projection CPU work is checked before/after; chunked reads and logical path/directory inspections are metered. Budget exhaustion cannot seal verification. D30 explicitly excludes SQLite/guard/recovery/sync bytes from proof byte/file counters; this is not a whole-invocation byte or hard syscall-interruption guarantee.
- CLI validation precedes dry-run; dry-run returns unknown cache facts without opening SQLite. Verified execution routes through the coordinator; snapshot avoids writer/recovery maintenance. The M1 fixture uses real source plans, guarded changes and Catalog publication, rename companions, head advance, staged successor revalidation, one/all support withdrawal, actual post-SQL-commit apply interruption/recovery, cache deletion/rebuild and local doctor.

Root subsequently added `search_context`/`context_policy`: context exclusions now apply in every exact ID/title/alias, FTS and literal SQL leg before candidate admission, preserving ordinary search behavior. The context cursor namespace and historical-policy fingerprint reject ordinary-search cursor reuse. Source re-review confirms those predicates match assembly's canonical authority rules; `context_eligibility_filters_precede_literal_and_lexical_candidate_caps` passed for candidates=hits=1 with earlier draft/unsupported matches. Root also added an actual binary CLI vertical workflow beyond the library M1 fixture.

An initial-read retry boundary question was sent to root without claiming a reproduced failure: initial capture/dependency-fill errors originally occurred outside the two-attempt catch. The subsequent refactor moves these reads inside the same catch, and the closure drops its pinned reader before maintenance. Source re-review confirms the boundary is resolved; shared counters and BudgetExceeded propagation remain intact.

## Evidence attribution and limits

Read selected P09 package/storage/retrieval/CLI/handoff contracts, D30, implementation helpers and context/M1/CLI tests. No independent Cargo/tests have been run: the root/worker own the serialized Cargo lease. The R2 existing-binary probe is separately attributed above. Focused command logs and matching handoff fingerprints are recorded below; root owns final integrated acceptance. Existing native durability/power-loss limits still apply; source inspection does not establish another platform or live provider behavior.

Initial observed SHA-256 fingerprints (intermediate working tree, not final acceptance):

| Path | SHA-256 |
| --- | --- |
| `src/retrieval/context_types.rs` | `8a5f3e532dddd18c40637ca3a11124b3da9b83d71a72d64b21f60931a5befdf1` |
| `src/retrieval/context.rs` | `d4d2288e0cd81622d225d61b6ed4d317a12d0629ffd4c67c3d3f3c01e9299089` |
| `src/retrieval/bundles.rs` | `1591ab52e3caf7e61a46ebb02567ddb24f54e877448f1ccfaef64dadb38fc0ee` |
| `src/retrieval/verification.rs` | `41c55d1cd8753abe61513db681bb249e496df5259527a253d6c33fae8fe5df56` |
| `tests/context_freshness.rs` | `fa7a9058f41a285e9cbda94bf12cde7397c817295fa2464c1c6bd8a246eaf01c` |
| `tests/m1_workflow.rs` | `8d55c22ed7865852ab4ad31992e7d48972a1a9391f366ea896341bb7eb382402` |
| `tests/context_cli.rs` | `66bb67aadd1f33528292f349e865222f2f63c802f072fca7e3eb23db1ebbfb64` |


## Stable handoff and runtime evidence

Exact worker command: `cargo test --locked --offline --test context_freshness --test m1_workflow --test context_cli --test graph_cli --test vault_fs > /tmp/lwiki-p09-focused.log 2>&1`; process session96700 was polled to exit0. Worker formatted leased source/tests immediately before the run.

Root/worker focused run `/tmp/lwiki-p09-focused.log` completed with 42 parent tests, zero failures: context CLI5 (6.75s), freshness18 (3.93s), graph CLI4 (0.73s), M1 workflow1 (8.10s), vault14 (0.95s); compilation 4.46s with no warnings. Reviewer read the log, including both R1/R2 regressions, crowding, forged-cache proof, copied-ID/decision races, shared budget, closure withdrawal, actual CLI lifecycle and real post-SQL-commit recovery. Log SHA-256 `832d98cd8d43b9399aef2f764312de741f22d13344f767648cde9f58da1fe4f0`. These are root/worker runtime results, not reviewer Cargo execution.

Stable source fingerprints observed independently and matched to the worker handoff:

| Path | SHA-256 |
| --- | --- |
| `src/retrieval/context_types.rs` | `8a5f3e532dddd18c40637ca3a11124b3da9b83d71a72d64b21f60931a5befdf1` |
| `src/retrieval/context.rs` | `7e0572f38088353720245d4b14b775228941d62b11b22537330e8191b9fd03c4` |
| `src/retrieval/bundles.rs` | `52f582eadaddaab3bb2646525fb1117da593d13de73c2f0c8a12138773d14945` |
| `src/retrieval/verification.rs` | `0fb53c8ff9ba1c1adad7c2940523e5cee7a48aca36a740510944ca05d68508f8` |
| `src/retrieval/lexical.rs` | `3a6cdb9dad2709ffdce6bbad01540591d353151831670f415c5b7bdbb8188a6a` |
| `src/retrieval/filters.rs` | `235d83679d74a54baff59702b9150c52efdb98590f95f8ee5f0bb4d8673e257d` |
| `src/retrieval/cursor.rs` | `b034d95638ee8c7b9dd5fa25ead908864ca9396c7584df05197015492b89b6f1` |
| `src/cli/context.rs` | `d6c274d982bf975b3feb06ffb04e8f9b87e64ba955b714f19f43296b0fc59e64` |
| `src/cli/dispatch.rs` | `7fad0a1b7b44c459e08098af1db3efddffb3c36d4ad8d57f655e4ff23e979b51` |
| `src/catalog/scan.rs` | `994eb5b949778b09f0e5d6e5163940bf2eb005f429e994f424e62f6df0c105ec` |
| `src/catalog/eligibility.rs` | `4747c7dc606a9a809b903968a00c2174a02becb2e7ad2e4d24e990adfcf224fe` |
| `src/sources/revision.rs` | `7e0006b5f899c71135e57bd9a9f312fcf10ce8ae61fe1d74b401cb8ac418e9a6` |
| `src/vault/paths.rs` | `d81944b68caec8b7392c4463a4cd196653b3518adf038d8efd179ab3a39d366a` |
| `tests/context_freshness.rs` | `840249e88b7f0ff699f5dfe9997228ec9eae3ed74d7b8a9a8b511d2c4a8f9fd4` |
| `tests/m1_workflow.rs` | `8d55c22ed7865852ab4ad31992e7d48972a1a9391f366ea896341bb7eb382402` |
| `tests/context_cli.rs` | `72354016d7291054665efbd82a77bb83d4440d24f8d0391d1ab35328fd6c1522` |

Root may subsequently activate the command registry and perform formatting/lint integration. Root's final checks/fingerprints supersede this handoff attribution for package acceptance; a full all-target native fault/SIGKILL regression gate, lint/fmt/build/seed/diff remain root work and are not claimed complete here. This review establishes local source and focused fixture evidence only, with the explicit D30 budget and native power-loss/platform/provider limits above.

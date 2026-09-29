# P21 independent completion review

## Summary

Independent Astra completion review, 2026-09-29, after accepted P20 HEAD `f4dcf084b75e38d54637dc05b347127f9e7603a4`. Reviewer authored no implementation, ran no Cargo commands and edits only this report. Earlier accepted invariant reviews are reused for unchanged scopes.

**P21 local acceptance is supported. R1–R4, including the R3 API-executor follow-up, are closed. No consequential unresolved finding remains in this bounded review.** Root retains final acceptance/commit ownership.

Independently verified all 319 current source hashes against before/after manifests and recomputed fingerprint `325e425accaad00f69d658c68121d46469cac5e69d7fc3c2ef44d6b07912678a`. Actual log summaries reconcile to all 51 Cargo metadata targets: 618 passing enabled parents and seven deliberately invoked helper tests. Required native fault/recovery/concurrency and same-vault M0–M4 parents explicitly passed. Eighty-four named gate-evidence mappings resolve to actual passing log entries.

Qualification is aggregate. Both full invocations remain FAILED. Earlier reused targets have byte-identical product/Cargo/shared-fixture/target inputs; only the corrected extraction test and path-normalization script differ. The successful continuation covers corrected/unexecuted targets, binary/example harnesses, format, strict lint, doctests (zero cases), release build and release-profile skill tests. No single full-script or `--all-targets` success is claimed.

R1 closes with four actual release skill parents/32 recipe steps. Copied release SHA-256 `55fc573b9c7c302b653249f4c5998cec874a3ff08d3dc2db1ed6528acc0bceba` equals `target/release/lwiki`; 37 commands, 19 exact emitted schemas, native system linkage and cleared-environment smoke checks reconcile. R4's corrected default physical-temp setup and copied-artifact smoke passed separately, not as a full script run.

The final 48 unique baseline pairs satisfy equal budgets, zero citation errors and zero provider usage. The 246-package license inventory matches cached manifests and final dependency metadata. Synthetic vectors qualify mechanics, not semantic quality/entailment; process-cold/prebuilt caches and RSS/disk limits remain explicit.

E01–E05 remain unqualified: live providers/models, actual hosts, other native platforms/clean machines, power-loss and publication/legal clearance are not established. Windows directory durability remains Unsupported. No core local gate is deferred to those external checks.

## R1 — P2: qualification script does not execute maintained recipes against release binary

Initial `scripts/qualify-local.sh:13` runs default-profile `cargo test --locked --offline --all-targets`; release build occurs at line 15. `tests/skill_export.rs:22` always selects `env!("CARGO_BIN_EXE_lwiki")`. Thus `skill_examples_execute_against_release_binary` runs the debug binary in that procedure, regardless of its name. The later copied-release smoke only exports a skill; it does not execute its recipe. This misses the explicit CLI skill acceptance requirement that examples execute against the release binary.

Required narrow correction: after the release build, execute and retain `cargo test --locked --offline --release --test skill_export -- --test-threads=1` (or an equivalent reviewed explicit artifact binding), then associate its actual successful output with the final release artifact/source hashes. No whole release fault rerun is requested solely for this correction.

Root accepted R1 and added the command above with `release-skill.log`, immediately after the release build. Initial source closure used script SHA-256 `0fbe438b5ffd169fca88ecf4069583074dc2ab41af800f47fb08eaca9156b8fb`. **Final source/runtime closure:** actual optimized release skill target passed 4/4, including the 32-step maintained recipe, at the final source/artifact identity documented below. R4 changes only the script's physical path normalization.

## Evidence and coverage audit

- Reviewed execution README/STATE, P21 obligations, VALIDATION V01–V17 and linked technical acceptance/release sections. Compared actual CLI parser/name/dispatch registry against the complete initial command contract and coverage ledger. `migrate` is an additional concrete command. Semantic/hybrid, graph seeds, API extraction and doctor roles are implemented option paths rather than separate registry entries.
- Reused `Astra-P20-final-delta-review.md` for independent R12–R15 closure, `P16-integration-review.md` and recorded earlier package reviews for already accepted invariants. This reviewer does not convert their scoped/historical results into current full-tree passes.
- `release_workflows::m0_m4_full_local_acceptance` verifies actual exported manifest/file hashes and maintained recipes, current SourceView citations, unchanged prior accounting/genesis/attempts, no implicit factual acceptance or page apply, exact wire-entry/attempt counts, two-request partial output, a three-request explicit amendment paying only synthesis, and SIGINT at an acknowledged full-request barrier. Unknown dispatch holds survive refusal to resend and offline inspection. No native public acquisition is claimed by this parent; production acquisition's controlled resolver/transport gates remain separate full-suite obligations.
- `retrieval_baseline::heldout_fixed_corpus_equal_budget_baseline` creates 48 separate measurement children, with equal k=10 and 6,000-byte/1,000 estimated-token ceilings. Directly compared bootstrap and P21 vault trees: the only difference is the added `knowledge/pages/rust_identifiers.md`. Measurements verify current source heads, exact hashes/UTF-8 spans, canonical assertions/qualifiers and budgets. `fixed_corpus_current_evidence_withdrawal_homonyms_and_isolated_spaces` additionally asserts separate Alex Kim IDs, same-dimension model isolation, surviving independent support and no withdrawn support/citations across six methods. Graph/document relevance sets differ intentionally; scores are not directly comparable as semantic-model quality.
- Seven ignored children are reachable from enabled parent tests: changes `crash_child`, catalog `catalog_crash_child`, accounting `admission_child`/`ledger_crash_child`, dispatch `native_proxy_child`/`native_interruption_child`, and baseline `p21_measurement_child`. Their ignore annotations do not waive recovery/measurement gates. Full-suite output must still confirm enabled parents actually ran.
- Native runtime source launches only explicitly configured credential helper processes; ordinary local command paths do not require Python, Node or external database/model/server processes. Source inspection is complemented, not replaced, by pending final linkage and copied-artifact smoke evidence.
- README/SKILL accurately distinguish implementation-in-progress, explicit remote authorization, proposal provenance versus entailment, persistent limits/unknown holds, and external qualification. Coverage still has pending final gates, appropriately; final acceptance must replace these with exact tests/artifacts rather than merely mark the table passed.
- Advisory resolved in source: initial manual CI redirected evidence into runner temporary paths without upload. The reviewed correction uses `if: always()` and a pinned upload-artifact action to retain top-level logs/JSON/text and the baseline JSON for 14 days. Fixture vault/provider configuration trees are excluded. Root separately verified the action pin against its official release/commit; this reviewer inspected the local workflow, not the external action. Authored CI is not an executed platform gate.

## Reviewed SHA-256 identity (after script/CI correction)

```text
1ab52c2ca6b4f2218a4cc33eff26a44fa7cf6803f27e13cdaab6fd1cb52c563b  tests/release_workflows.rs
7ac94496a68bf12b655ea6aedb6ae9f9e01c4204930c14a2a5380bce74e7e67c  tests/retrieval_baseline.rs
a9235ef25236aa8b65572faf39ea58255675ecdd37989755bb389f29218e28f8  tests/fixtures/p21/queries.json
0fbe438b5ffd169fca88ecf4069583074dc2ab41af800f47fb08eaca9156b8fb  scripts/qualify-local.sh
044297d74e1d049850016dfccf093b37308527562fd6037ec6e6da1c019a9036  .github/workflows/qualification.yml
b754c5640df9b6cc80d2bd2146d278291d583346201e1720476025c53a87453c  README.md
f4f263b40c735af47a06f49df71de408ef158ac3d22e91995c0a5c76bd5d965a  skills/llm-wiki/SKILL.md
624969e5ec991cfe1db30005888f607c02a953694f982b903200266bdd9610a7  tests/skill_export.rs
7586128baf6498384341f9f5964aed7b78557847039a3539aec0a0791d793889  src/cli/arguments.rs
662db05034f7a179a6fe7347b623f8367ab2a289eaa840442900e313e8f8f27a  src/cli/dispatch.rs
860d62642306a905bfadd71078db710edf436414432431d3b689d2f6f8a59b70  docs/execution/reports/coverage.md
```

The preceding scope and identities are historical checkpoints. Final aggregate release/full-suite reconciliation appears below and supersedes their then-pending status. The targeted baseline remains historical evidence separate from the final baseline artifact.

## R2 — P2: gate map contains nonexistent test selectors

The initially prepared `P21-gate-map.json` maps V03 to `lib::changes` and V14 to `lib::providers::public_fetch`. Neither source module contains tests; these entries cannot identify actual passing tests. The full suite may exercise their production behavior through integration targets, but those are the targets the evidence map must name. Required correction: remove or replace the nonexistent selectors, explicitly map `vault_fs` and `lib::vault::operational_tests` for V03, and `provider_dispatch` for V16's actual `offline_dry_run_before_secret_resolution_zero_dns_http_helpers` parent. This is an evidence-map correction, not an identified missing production capability.

**R2 source-closed:** reviewer reread the corrected map; both nonexistent selectors are removed, all three required additions are present, V14 retains production acquisition/dispatch targets, and gate statuses still accurately say final qualification pending. SHA-256 `0b025237a200645337d9711277a871917b7367cff1efa8b33e01633b78a89281`.

## Targeted baseline, license and procedure reconciliation

Read `/private/tmp/lwiki-p21-baseline-current-first.log`: build 1.70s; two parents passed, zero failed, one explicitly invoked measurement helper ignored in ordinary discovery; tests 14.62s. `P21-baseline-results.json` parses identically to the actual output `/private/tmp/lwiki-p21-retrieval-baseline.json`. It contains exactly 48 distinct method/question pairs, eight per method and six query classes. Independently recomputed all six report rows' cold/warm median milliseconds, peak RSS and maximum logical disk; all agree. Independently recomputed every per-class applicable count and mean; no mismatch. Every measurement has zero citation resolution errors, zero provider request/token/charge counters and usage within both budgets. All applicable citation-validity and supported-bundle-precision values are 1.0. These fixture invariants do not establish semantic entailment or real embedding quality. Debug build, process-cold/prebuilt caches, unflushed OS caches, setup-inclusive RSS and logical disk are correctly disclosed.

Parsed the locked registry package identities and inventory: exactly 246 unique name/version pairs, no missing or extra packages and no missing license expression. Read all 246 available cached `Cargo.toml` files and independently matched SHA-256, license and license-file values to the inventory. This includes build/dev/other-target packages and preserves compound/alternative license expressions. It is an inventory, not the owner's license selection, generated third-party notices or legal release clearance. Those limits are correctly stated.

Read `docs/qualification.md`: explicit later endpoint/spend authorization, disposable private configuration, role-specific bounded probes, usage/receipt evidence, unknown-charge retention, separate native platform/durability and real-model evaluation are appropriate. E02's abbreviated host procedure initially omitted the design's unrelated-task negative-control discovery trial. Reviewer read the correction explicitly requiring that control; advisory source-closed at SHA-256 `0d0e9ef182e8328bef9728664898f5a1dc663ca6d2b576a7cc8623a2373efed3`. No external trial is executed or approved by this review.

Read all 319 files listed in `/private/tmp/lwiki-p21-final-before-hashes.json`; each actual SHA-256 equals its frozen entry. This observation confirms the still-running gate's source consistency at this review time, not its eventual exit or results. The prepared V01–V17 map otherwise names existing integration targets and relevant library test scopes; final per-target names/results and accepted package records remain needed to turn the mapping into evidence.

```text
23cb5c141eb1be269e5812c2101402b1a51b2e8f8a7db737a30bcfad5340e476  docs/execution/reports/P21-baseline-results.json
ea531acf652e3d275ce202515eafbbdcef914606c175d5dd0458e7e400bf0b8f  docs/execution/reports/P21-licenses.json
97d0d1d34e817d43c712f261a0042f175a8b1184556195b2b5abc86f428c3b24  docs/execution/reports/P21-gate-map.json (before R2 correction)
4735a2cb2742822f94077c2967ddcc232106ab2ba86f47eccce0f4a571224372  docs/qualification.md (before E02 advisory correction)
7adf7dfae1e2676231cb6c645394a849bac92940bfc5127be31e644683793f39  /private/tmp/lwiki-p21-baseline-current-first.log
```

## R3 — obsolete M1 CLI expectations: source and targeted runtime closed

The first final full suite failed `context_cli_dry_run_is_pure_and_bad_requests_never_claim_verification`: it expected exit 2 for `context uses --seed semantic`, but received a valid verified lexical document result. This is an obsolete test expectation. The current context adapter builds no graph plan for the default Documents target, so the valid graph seed option does not alter that document query. Semantic seeds are implemented for graph/combined targets. The CLI/retrieval contracts explicitly permit semantic/hybrid modes; absent offline embedding cache is a capability/cache error (exit 6), not an invalid enum/usage error.

The bounded remaining-target scan also found semantic seed/mode entries in the expected-usage arrays of `graph_cli` and `offline_cli`, plus `offline_cli` expecting failure for `--dry-run doctor --probe`. Current P20 dispatch deliberately permits that dry preview, without loading provider runtime or doing remote work. Existing `remote_cli`, `semantic_retrieval` and native research/probe checks cover the actual semantic/cache/probe behaviors; no production rule needed weakening.

Reviewer reread the exact three-file diff. Each usage array now uses the invalid enum `unknown`, preserving clean usage/error-envelope coverage. The doctor case now requires success, `data.probe.dry_run == true`, `meta.network_used == false` and unchanged complete tree bytes/mtime. No production file is changed by this correction.

Read actual `/private/tmp/lwiki-p21-obsolete-m1-usage-second.log`: build 0.83s; **context 5/5 (7.69s), graph 4/4 (.69s), offline 10/10 (3.18s): 19 parents passed**, zero failures/ignored/filtered. The graph target includes `relationship_seed_paths_connect_through_either_endpoint`; the initial handoff's 18 total/3 graph count was corrected to match the log. First targeted attempt `/private/tmp/lwiki-p21-obsolete-m1-usage-current.log` failed the still-stale doctor expectation; the original full log (now preserved as `/private/tmp/lwiki-p21-final-first-failed/tests.log`) failed context. Neither failed batch is represented as passing evidence. Final aggregate qualification below covers all corrected target bytes.

```text
4c992ec06db078aec684648f2d1fcae264c09f5d95924ea2d41c274e7f24a5d2  tests/context_cli.rs
434dd0f2970d11f9fe8ab2c3e31449e45530b3798ab37444dc30f1462b7dbecc  tests/graph_cli.rs
9860f484587edbf9babb87305bb00f9ea9143a56784304788b31d512f9ebd714  tests/offline_cli.rs
9e70fa493c9cc285c5255dacd0f1bc2c97c4d55384643f132683c5926b2b5ac4  /private/tmp/lwiki-p21-obsolete-m1-usage-second.log
54f3f3f2a5e6ab8fbd81bb352a417050a82122f202af7d27ef7a35ba782d12ab  /private/tmp/lwiki-p21-obsolete-m1-usage-current.log
f4d80a90d66159987a5217505839647622caffbb97943ce7f2ad07a4bffad9fa  /private/tmp/lwiki-p21-final-first-failed/tests.log
```

### R3 API-executor follow-up — closed

The second full invocation failed the obsolete `extraction_cli` assertion that API extraction is unavailable (exit 6). The implemented executor correctly returns `CONFIG_INVALID`/exit 2 when no trusted profile is selected. Reviewer traced `RemoteArguments::runtime`: this rejection precedes default provider configuration lookup, credentials and dispatcher construction. The reviewed correction retains the real API invocation, requires exact exit/code and unchanged tree bytes/mtime; the common invocation helper already requires `network_used == false`. No product behavior changes.

Read `/private/tmp/lwiki-p21-extraction-usage-current.log`: 3/3 parents passed, build 4.80s/tests 4.01s. The final continuation repeats all three successfully in 4.11s. API/probe/semantic/hybrid/capability scans of remaining integration targets and fixtures found no further obsolete rejection expectation. Other capability refusals concern real monetary-bound, model-space, schema/host or media constraints and remain intact.

## R4 — logical temporary path conflicts with exporter policy: closed

The original script retained `/var/...` (or `/tmp/...`) through logical `pwd`; native macOS symlink ancestors are correctly refused by the anchored exporter. Reviewer independently observed the ambient TMPDIR alias and differing Bash `pwd`/`pwd -P` output, and traced the exporter's `AT_SYMLINK_NOFOLLOW` directory checks. Root changed only the evidence-directory normalization to `pwd -P`; exporter safety is unchanged.

Read the actual corrected script, default-smoke wrapper/manifest/log and eight output envelopes. Independently proved the wrapper equals the corrected script with Cargo lines removed and repository-root setup made absolute. No argument uses native `/var/folders/.../T/`; the resulting directory is `/private/var/folders/g0/jdy1y8615k30fs6sbf4x1wt40000gn/T/lwiki-qualification.HD9YTu`. All eight capabilities/init/capture/search/context/rebuild/check/export envelopes are successful with `network_used == false`, their recorded hashes match actual bytes, and their copied binary equals the final release hash. This qualifies the affected setup/artifact block, **not another full-script invocation**.

## Final independent evidence reconciliation

Read final `P21-checks.json`, `P21-gate-map.json`, `P21-test-results.md`, `P21.md` and `FINAL.md`, the actual phase logs, both preserved failed logs, final manifests and release artifacts. Independently performed these read-only checks:

- Current bytes of all 319 files equal both final manifests and the check record. Recomputed the sorted-path/NUL/hash/LF fingerprint. Compared the earlier manifest: exactly `tests/extraction_cli.rs` and `scripts/qualify-local.sh` differ. Therefore reused library/native/early-target results have unchanged production, Cargo/toolchain, shared fixture and individual test inputs.
- Parsed each Cargo target's **last** summary from its actual log, excluding nested child summaries. All 51 target identities exactly equal final Cargo metadata; every recorded parent/ignored count and duration agrees. Sum is 618 passing parents, zero failures in the selected passing target results, seven explicit helpers. Required fault/full-workflow parents appear individually as `ok`. Reconciled 84 named gate selector records to actual passing names and log hashes.
- The continuation log covers 35 integration targets plus binary/example harnesses and has no failure. Its driver reached the terminal evidence path. Final format is clean; strict all-target Clippy completed; doctests ran with zero cases; optimized release built in 68s. Release skill built in 39.06s and passed 4/4 in 5.08s. Read the actual exported 32-step recipe and exact fixture-matching manifest, not merely its test name.
- Release artifact bytes equal `target/release/lwiki`, length 25,526,608, SHA-256 below. Main and default-temp smoke envelopes are successful/offline. All 19 saved emitted schema envelopes equal their published JSON; capabilities has 37 command routes. Linkage records only Security/CoreFoundation/libiconv/libSystem. Combined source/artifact evidence supports no required auxiliary Python/Node/model/server/daemon runtime, not clean-machine or other-platform qualification.
- Final baseline bytes exactly match both retained JSON copies/hash. It has 48 unique method/question pairs with bounded usage, zero citation resolution errors and zero provider requests/input/output/charge. The earlier targeted table remains separately labeled. Final metadata has 247 packages including the root; its 246 registry identities exactly match the independently manifest-verified inventory.

**Disposition:** all consequential findings in this bounded review are resolved; current native V01–V17 local acceptance is supported by complete aggregate evidence. Both whole invocations remain FAILED; no successful single full-script/`--all-targets` run is inferred. The two narrow final deltas are covered by targeted/final tests and affected artifact smoke; repeating unchanged native matrices would add no unresolved coverage. Root can finalize acceptance records and commit. External E01–E05 and documented filesystem/model-quality limits remain unchanged.

```text
Final source fingerprint:
325e425accaad00f69d658c68121d46469cac5e69d7fc3c2ef44d6b07912678a

55fc573b9c7c302b653249f4c5998cec874a3ff08d3dc2db1ed6528acc0bceba  /private/tmp/lwiki-p21-final/lwiki
7a52e9e75aedfbe6768472ba8eecae367a4e8b89ae2f684415f333dd9f0181d2  scripts/qualify-local.sh
be3899f06b0ec3cecd5518ded35d76f1a22b50b70ed0e7e27207a3d1d8e8aaae  tests/extraction_cli.rs
6bad232e1d90bcd0d63e410a2f7ad1f1f547a50fccbb31353e101605527a5cf3  /private/tmp/lwiki-p21-final-second-failed/tests.log
e229ccaf0bc0c2b19557fb5f10a5da5d185ac253d99bb86bee56ae8c75c057b7  /private/tmp/lwiki-p21-final/tests.log
cd44776e14608bae70c86b444ca8ac215bb48188a85d6d1c6ef62ae1323869fb  /private/tmp/lwiki-p21-final/release-skill.log
fb3576af36b0a2649d0d6a7e8800cd606955a7cb46369c5085a16e7c60d29602  /private/tmp/lwiki-p21-default-smoke.sh
ced54fa3aa8854822791482f8f5ad06748b302fc15a967697344258218713888  docs/execution/reports/P21-baseline-final-results.json
316ed62d0218a614e230480196297859cf26674820aaa05acc93a9274fc51a24  docs/execution/reports/P21-checks.json (pre-acceptance draft)
7873258d620a0a7942796806a7afc49bce7e761b88ba2c2e488c38dd551514c7  docs/execution/reports/P21-gate-map.json (pre-acceptance draft)
```

Report lease returned. Reviewer made no source/test/Cargo/Git edits and executed no product tests; conclusions independently reconcile root-run evidence with actual source/artifact bytes.

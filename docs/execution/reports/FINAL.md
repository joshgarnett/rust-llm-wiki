# Final M0–M4 local implementation evidence

Status: **local implementation complete; external qualification pending**. All required P00–P21 packages and V01–V17 local gates are accepted. No required implementation is deferred to external checks.

Branch `impl/autonomous-v1`; pre-P21 accepted HEAD `f4dcf084b75e38d54637dc05b347127f9e7603a4`. The acceptance commit will be recorded in P21-checks.json after integration. All 319 product/schema/skill/test-fixture/script/CI/Cargo/toolchain files were byte-identical before/after the final continuation. Earlier passing native/library/early integration targets ran at fingerprint `ea627f8d6e0cad300f8f29398ab771d61ec32397f97544f0171862edc43d24ab`; only tests/extraction_cli.rs and scripts/qualify-local.sh differ from that tree. Product/Cargo/shared fixtures and each reused passing target are byte-equal. The corrected extraction target and all unexecuted targets, binary/example harnesses, format/lint/doc/release/skill/smoke ran on the final frozen tree. The aggregate evidence preserves both failed whole invocations; no single full-script or --all-targets PASS is claimed. Source fingerprint `325e425accaad00f69d658c68121d46469cac5e69d7fc3c2ef44d6b07912678a` (SHA256 of sorted path, NUL, file SHA256, LF); mutable report/prose updates are excluded. Full per-file identities and log hashes are in [P21-checks.json](P21-checks.json). Earlier package identities and results remain in their reports and [PROGRESS](../../../PROGRESS.md).

## What is implemented

The Rust CLI/library preserves canonical Markdown, immutable captured originals/revisions, independent stable IDs, exact citation spans/hashes and rebuildable bundled SQLite/FTS/vector indexes. It provides recoverable guarded changes; literal/lexical, bounded directed graph and verified context retrieval; explicit host packet/import/resolve/decision/review; portable skill export; trusted remote embedding/generation adapters; isolated semantic spaces and hybrid fusion; bounded public acquisition; and research plan/run/resume/status/report with lifetime accounting, partial output and cancellation. Model-derived claims remain unassessed proposals until explicit review.

Every advertised command has a concrete handler; all 19 emitted schemas exactly equal the maintained schema files. The completion audit found no reachable TODO/unimplemented placeholder or fixture-only production branch. [Coverage](coverage.md) retains every planned command and package; [the final V01–V17 map](P21-gate-map.json) records actual named passing targets/scopes and matching log hashes.

The copied artifact advertises these 37 command routes (executor/mode flags select additional supported workflows):

- `capabilities`
- `skill export`
- `schema`
- `init`
- `read`
- `page put`
- `page rename`
- `source add`
- `source refresh`
- `source withdraw`
- `evidence revalidate`
- `index sync`
- `index rebuild`
- `embeddings check`
- `embeddings sync`
- `search`
- `context`
- `graph extract`
- `graph import`
- `graph resolve`
- `graph decide`
- `graph review`
- `graph query`
- `graph neighbors`
- `check`
- `doctor`
- `research plan`
- `research run`
- `research resume`
- `research status`
- `research report`
- `changes show`
- `changes apply`
- `changes abort`
- `changes rollback`
- `recover`
- `migrate`

## Actual local qualification

Native macOS26.5.2 / Darwin25.5.0 arm64, MacBookPro18,2 / Apple M1 Max /32GiB. Rust/Cargo1.98.0 pinned, edition2024. All product tests use disposable fixtures and deterministic providers; real native HTTP/TLS transport and production serializers/state machines execute against loopback mocks.

The final continuation exited0; aggregate evidence covers every Cargo target, with unchanged earlier passing targets separately identified. Format and strict all-target Clippy, complete aggregate all-target tests, doctests, release build and actual release-profile exported recipe ran serially with actual exit codes recorded; the test command below completes every corrected/unexecuted target and reuses explicitly mapped unchanged passing targets:

- `cargo fmt --all -- --check`: exit0; SHA256 `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.
- `cargo clippy --locked --offline --all-targets -- -D warnings`: exit0; SHA256 `fa75da60813845e17230ee1020098f2b112467f351ebb7e94b3d4918f04b9780`.
- `cargo test --locked --offline --bins --examples --test extraction_cli --test extraction_import --test graph_cli --test graph_queries --test graph_resolution --test graph_review --test job_accounting --test m1_workflow --test m2_workflow --test machine_contract --test offline_application --test offline_cli --test provider_dispatch --test provider_trust --test provider_wire --test records_lossless --test release_workflows --test remote_cli --test research_acquisition --test research_cli --test research_dispatch --test research_extraction --test research_planning --test research_reports --test research_stage_contracts --test research_stages --test research_workflow --test resolution_cli --test retrieval_baseline --test retrieval_lexical --test semantic_retrieval --test skill_export --test sources_evidence --test vault_fs --test windows_acl_policy`: exit0; SHA256 `e229ccaf0bc0c2b19557fb5f10a5da5d185ac253d99bb86bee56ae8c75c057b7`.
- `cargo test --locked --offline --doc`: exit0; SHA256 `9fa8955f0ed84c9494acb7c277683468f430cb3fdf603c0555ae882f52dc65a8`.
- `cargo build --locked --offline --release`: exit0; SHA256 `afd43e8ce319b52edfd489fc00f52f91148e1dd17f2e5cdc675aa7dce86d74cd`.
- `cargo test --locked --offline --release --test skill_export -- --test-threads=1`: exit0; SHA256 `cd44776e14608bae70c86b444ca8ac215bb48188a85d6d1c6ef62ae1323869fb`.

The full suite passed **618 enabled parents, zero failures**, with 7 helper children ignored in ordinary discovery and explicitly run by enabled parents. Release-profile skill passed another 4 parents /32 actual recipe steps. [Per-target results](P21-test-results.md) and P21-checks.json preserve counts, timing, commands and identities. Native ledger/bootstrap/change/vault durable-boundary matrices, SQL publication/migration SIGKILL, dispatch intent/body/spool/receipt restart, final-slot processes and freshness races passed. Current matrices exhaust their dynamically counted positions; the historical1148-position observation is not claimed as a freshly printed count.

One same-vault [complete CLI workflow](P21-release-workflow.md) executes M0–M4: actual skill recipe; complete explicit host graph lifecycle, homonyms/counterevidence/withdrawal and cache deletion/rebuild; mocked embeddings/API extraction and retained offline cache; budget-stop partial research, explicit lifetime amendment, synthesis-only resume/apply; entered-request SIGINT130 with one JSONL terminal event, unknown hold retention and conservative subsequent probe disclosure. Counting transport/DNS/helper and byte/mtime/tree tests enforce ordinary/offline/dry-run absence.

The copied release binary `/private/tmp/lwiki-p21-final/lwiki` is byte-identical to target/release/lwiki, 25526608 bytes, SHA256 `55fc573b9c7c302b653249f4c5998cec874a3ff08d3dc2db1ed6528acc0bceba`. Version: `lwiki 0.1.0`. With an empty environment and only `/usr/bin:/bin` on PATH it executed capabilities, offline init/capture/literal search/context/rebuild/check/skill export and all 19 schema routes. Native linkage requires only the recorded system libraries; no required Python, Node, local model, external database server or daemon was observed. This runtime smoke does not claim a clean other-machine installation.

## Retrieval baseline and dependency inventory

The [final measurement artifact](P21-baseline-final-results.json) holds 48 unique method/question cold/warm pairs, six retrieval paths, eight questions and fixed equal budgets on corpusBLAKE3 `76e9b46977321f65a547019d062e864e731dbbe06bc1be6bba6995eb410c9e1b`. Every pair has zero citation resolution errors and zero provider requests/tokens/spend. Exact Current hash/span/proposition/qualifier checks, homonym separation, vector-space isolation and one/all support withdrawal passed. [Baseline methodology](P21-baseline.md) retains the earlier targeted metrics; final runtime measurements are separately identified, without replacing historical data. Synthetic cached vectors qualify retrieval mechanics, not real semantic quality, entailment or ranking gains. Process-cold caches were prebuilt, OS caches unflushed, RSS includes setup/validation and disk values are logical bytes.

The locked dependency metadata has 247 packages including this crate; [license inventory](P21-licenses.json) reconciles all 246 registry identities and cached manifest hashes/expressions. Owner license remains unset. Inventory is not legal clearance or generated distribution notices.

## Review and failed evidence

Periodic independent Astra reviews addressed storage/accounting, immutable lifetime/epochs, publication authority and historical capture recovery. P20's separate authority/publication/final-delta reviews closed R1–R15. The [P21 independent review](Astra-P21-review.md) required an actual release recipe (R1), corrected evidence selectors (R2), and identified obsolete pre-M3/P20 CLI assertions (R3). Physical temporary-directory normalization (R4) was corrected on the final continuation tree and checked with the actual default-directory release smoke; the obsolete API capability assertion was also corrected without changing production behavior. Final independent Astra evidence audit is closed with no consequential unresolved finding; aggregate acceptance is supported.

The second whole invocation exited101 at the obsolete API executor unavailable expectation; missing-profile rejection is now checked as exit2/CONFIG_INVALID with exact tree purity, and its three targeted parents passed. The first final all-target invocation exited101 at the old context semantic-seed rejection after the library136 parents and completed native change/SQL matrices passed. A first narrow retry exited101 at the old dry-run provider probe rejection. Test-only corrections replace valid semantic enum values with unknown values in usage-negative cases and strengthen dry-run probe success/no-network/exact-tree assertions; all19 corrected targeted parents passed before the second whole attempt. The API correction adds three passing targeted parents. Failed logs remain separately recorded in P21-checks.json. No product behavior was weakened.

## External qualification remains pending

- **E01:** live role/provider compatibility and deployment-specific bounds, cancellation/billing/reconciliation; no live paid API executed.
- **E02:** actual Codex/Claude Code/Cursor discovery, unrelated-task negative controls and Obsidian GUI editing/navigation; no real host profile installed.
- **E03:** other native platforms and clean-machine packaging/recovery. Manual CI is prepared, not run. Windows directory durability is explicitly Unsupported before paid work. Cross-builds and process kills do not establish native other-OS or power-loss safety.
- **E04:** authorized representative corpus and real embedding/model quality, latency, memory/disk and billed usage; synthetic results do not qualify it.
- **E05:** owner name/license/notices/signing/distribution/publication decisions. No push, release or publication performed.

[Exact opt-in follow-up procedures](../../qualification.md) retain these boundaries. External-editor post-check races and hostile path replacement remain documented filesystem limits. M5 experiments and local inference are outside scope.

Build with `cargo build --locked --release`; run `./target/release/lwiki --json capabilities` and follow the [README](../../../README.md) / [maintained workflows](../../../skills/llm-wiki/references/workflows.md). Use new disposable directories for qualification; provider operations require explicitly trusted configuration and budgets.

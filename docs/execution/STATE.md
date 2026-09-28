# Execution checkpoint

Updated: 2026-09-28. Phase: **P06/P07 committed; P08 accepted; P09 implementation next**. Root owns this file.

## Objective and authorization

Implement every P00–P21 package and V01–V17 local gate through M4. User authorized Sol implementation/Astra review, local commits and disposable fixtures/mocks. No real-vault writes, live spending, host installation, publishing or push. E01–E05 qualification remains separate. Persistent goal remains active; M1 alone is not completion.

## Accepted baseline

- Branch `impl/autonomous-v1`, HEAD `c90953e3e610e98f2dc2031cfeff72464a25b805` (P00–P08 accepted, M0 complete; P09 required to finish M1).
- Research4652248; planninga97679c; P0077ef9b7; P01/P02f04a9d5; P0374cfb58; P047196eaf; P056158b58; P06/P07c90953e.
- Rust/Cargo1.98.0 pinned, Rust2024, macOS26.5.2 arm64. Exact bundled rusqlite0.40.1/SQLite3.53.2,132 locked packages. No P06/P07 dependencies added.
- Git/dependency network require permitted escalation; prior authorized reviews passed. No bypass.
- Activated commands23 in src/cli/dispatch.rs COMMANDS: capabilities/schema/init/read/page put+rename/source add+refresh+withdraw/evidence revalidate/index sync+rebuild/search/check/doctor/changes show+apply+abort+rollback/recover/migrate. Graph lexical query/neighbors activated; search literal/lexical only; doctor provider probe unavailable pending P16. Later context/skill/provider/research commands remain unadvertised.

## Latest accepted checks

- P05 broad all-target offline suite:127 parent tests,0 failures. Actual250 durable-I/O boundaries × before/after =500 faults and two changes SIGKILL wrappers passed. Two ignored crash helpers explicitly invoked. Prior fault evidence retained; no changes engine/filesystem/catalog invariant edits in P06/P07.
- P06/P07 selected13-target integration:139 parent tests,0 failures; application14(7.56s), CLI10(3.38s), retrieval13(2.00s), machine6 and catalog31. Existing11 SQLite SIGKILL points reran; one ignored child explicitly called by two enabled wrappers. Actual AfterCommit/FilesApplied-to-CLI recovery test passed.
- Required-name-only machine/CLI rename rerun:16 passed (CLI10/machine6). All-target Clippy -D warnings0.43s, fmt, debug build5.63s, diff and unchanged seed34 passed. Logs/exact75 file SHA256s and tree fingerprint bf3a3e2a92286a071cb30af37cbd2737e67d7aef475f5e5ec1f03cb02da1d4df in reports/P06-P07-checks.json.
- Independent Sol P06/P07 cross-review plus root lifetime/source-map/protocol inspection closed all blocking findings: retained prepared IDs lost on apply failure; identity-only entity excerpt leaking unsupported body; human no-sync missing freshness label. Final reports P06/P07-review.md plus root acceptance updates. Runtime rejects fresh/restarted Astra at thread limit; fallback retains gates.
- D27 dry-run bypasses every SQLite open/check/snapshot/writer because normal read-only WAL may create sidecars. Actual recursive membership/bytes/mtime invariance passed; network/helper handles absent in current local layer, counting provider transports still P16/V16 work.
- Native faults/mocks do not prove power-loss safety, native Linux/Windows, hosts or live providers. E01–E05 remain open.

## Accepted interfaces and decisions

Root owns all P00–P07 code now, shared Cargo/modules/types/schemas/CLI/records/catalog/state/reports/commits. Worker historical hashes remain reports; root checks.json final integrated hashes authoritative.

- OfflineApp typed guarded operations/getters; explicit flat/private JSON preferences; bounded read/plan/mutation/change outcomes. Source outcomes expose allocated source/revision IDs even reused head. Read only visible canonical notes/captured content, hash binding before typed authority. No-sync CLI read uses pinned cached raw bytes; default holds writer and verifies independently.
- Init creates at most one missing final component beneath existing checked parent, syncs parent/root, managed dirs/excludes and guarded marker LAST, no SQLite bootstrap. D28 only compatible0->1 mutable note scalar migration inside already valid v1 vault; future/unknown/immutable versions refuse edits.
- D29 page put existing ID path or pages/ID.md with explicit path override. Changes show verifies exact binary before/proposed, default aggregate16MiB with omitted indices; explicit --operation selects one existing64MiB-per-payload bounded operation. Error envelopes preserve retained change IDs/hash and partial flag.
- P07 QueryPlan/SearchFilters/limits/HitSet are frozen. Locator alone supplies path/record identity. Query4096 bytes/64 lexical terms, hits<=50/candidates<=80/excerpt<=2048, normalized filters<=80 items; cursor binds full snapshot/query/mode/filters/limits.
- Actual connection-borrowed RAII FTS5 unicode61 remove_diacritics2 tokenizer FFI accepted independent/root review; synchronous callback copies/catches unwind, finalize/delete verified, no Send/Sync. Raw match spans only after exact mapped-byte equality; decoded/escaped Markdown omits uncertain spans.
- Literal audit raw bytes permitted but notes/transcripts/duplicate quotes never source citations. Verified intact captured payloads alone get direct source-span citations; IndexSnapshot no citations. Identity-only entities return empty excerpts, preserving name/alias seeds without unsupported description.

## Active leases and next action

P08 accepted in current tested tree; local commit next. Root owns all returned P08 paths. No Cargo process/worker source lease active. Included P09 preparatory shared types/closed inputs/budgeted path callbacks; no P09 evidence/CLI implementation accepted.

- Root final graph17/CLI3/machine6 passed26 tests in /tmp/lwiki-p08-final.log; broader pre-review85 parent tests retain unchanged non-graph evidence. Clippy0.16s/fmt/debug build6.04s/diff/seed34 passed. Independent Sol plus root review closed R1–R4, Invalid defensive citation predicate and navigation depth labels. Exact hashes/fingerprint reports/P08-checks.json.
- `/root/p05_sql_publication` reused Sol P08 completed, idle; available independent P09 review later. `/root/p05_scan_eligibility` reused Sol P08 reviewer completed, next P09 implementation after commit/explicit grant.
- Frozen P09 types src/retrieval/context_types.rs sealed ContextResult; proof meter spans both attempts, closed SourceView no fallback, budgeted VaultRoot entry/component callbacks, pub(crate) manifest_hash. D30 maintenance/proof meter boundary explicit. Future lease retrieval/{context,verification,bundles}.rs, tests/context_freshness.rs/m1_workflow.rs, reports/P09.md; root owns CLI/types/modules/Cargo/schema/catalog/source helpers.
- `/root/p02_vault` reused Sol P10 planning complete, exclusive report P10-plan.md returned. Strict deterministic packets/exact wire/retained raw response+span proofs/reserved IDs and complete Pending mentions; P11 explicit binding alone materializes canonical endpoints. No code/Cargo; root freeze/P10 implementation waits P09 acceptance.
- Next: local P08 commit, grant P09 implementation; root wire context CLI from /tmp/lwiki-p09-cli-context.rs draft. Default current excludes drafts/unsupported descriptions; complete canonical evidence groups before caps, support+contradiction atomic fit, RRF owner fusion/doc2/Combined graph50%, full closed control/dependency proof then final reread and one refresh/reassembly retry. Strict dry no SQLite. Continue P10–P14/P15 through M4.

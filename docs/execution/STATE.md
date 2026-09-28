# Execution checkpoint

Updated: 2026-09-28. Phase: **P06/P07 accepted, local commit next, P08 interface planning**. Root owns this file.

## Objective and authorization

Implement every P00–P21 package and V01–V17 local gate through M4. User authorized Sol implementation/Astra review, local commits and disposable fixtures/mocks. No real-vault writes, live spending, host installation, publishing or push. E01–E05 qualification remains separate. Persistent goal remains active.

## Accepted baseline

- Branch `impl/autonomous-v1`, HEAD `6158b5810908dffbea8251ddd386831143bc24ba` (P00–P05 accepted, M0 complete).
- Research4652248; planninga97679c; P0077ef9b7; P01/P02f04a9d5; P0374cfb58; P047196eaf; P056158b58.
- Rust/Cargo1.98.0 pinned, Rust2024, macOS26.5.2 arm64. Exact bundled rusqlite0.40.1/SQLite3.53.2,132 locked packages. No new P06/P07 dependencies.
- Git/dependency network require permitted escalation; prior authorized reviews passed. No bypass.

## Latest accepted checks

- P05 broad all-target offline suite:127 parent tests passed,0 failed; two ignored crash helpers explicitly invoked by enabled wrappers. Native250 durable-I/O boundaries × before/after =500 faults passed; 11 SQLite SIGKILL points passed (normal publication5/migration6).
- All-target Clippy -D warnings, fmt, debug build and diff/seed34-file invariance passed. Logs and exact54 source/test/schema/Cargo SHA256 hashes in reports/P05-checks.json, accepted commit added after commit.
- Independent Sol SQL and eligibility cross-review plus root invariant review closed all blocking findings. Fresh/restarted Astra rejected by runtime thread limit; playbook fallback retained gates.
- P05 canonical registry reserves malformed/future known IDs; graph/evidence eligibility closure and entity identity/description separation; exact dependency/control/parser freshness; pinned ordinary/bothFTS generations; full SQL rowset validation; atomic migration/reset/publication and latched vector-loss notices accepted.
- Normal WAL readers may create sidecars. D27 strict dry-run must bypass every SQLite open/check/snapshot and mark cache facts unknown.
- Native fault/mocks do not prove power-loss safety, native Linux/Windows, hosts or live providers. E01–E05 remain open.

## Ownership and current work

All P00–P05 workers handed ownership back. Root owns shared Cargo, modules/types/schema, CLI, records migration helper, all catalog files, reports integration/state and commits.

- `/root/p05_sql_publication` reused Sol thread: P06-A exclusive src/config/local.rs, src/vault/discovery.rs, src/app/offline.rs, tests/offline_application.rs, reports/P06-A.md. Leaves implemented; library tests being completed. No nested delegation.
- `/root/p05_scan_eligibility` reused Sol thread: P07 exclusive src/retrieval/{literal,lexical,excerpts,filters,cursor}.rs, tests/retrieval_lexical.rs, reports/P07.md. Leaves implemented; currently first Cargo lease, initial errors fixed, tests next.
- Root created src/app/{mod,types}.rs, src/config/{mod,types}.rs, src/retrieval/{mod,types}.rs; lib/vault wiring; guarded records::edit::migrate_schema compatible0->1; src/cli/{mod,arguments,dispatch}.rs and new main adapter; stream-v1 schema. Root CLI tests pending.
- Shared APIs frozen: OfflineApp typed operations; explicit flat/private JSON preferences; bounded Read/Plan/Mutation/ChangeDetails outcomes; QueryPlan/filter/limits and locator-authoritative SearchHit/HitSet. Later additions expose verified changes payloads with bounded default16MiB aggregation/explicit operation selection, allocated IDs in mutation outcomes.
- P06 init supports existing directory or one new final component under existing regular parent; durable marker WIKI.md LAST, cache excluded, no SQLite bootstrap. Future schema refuses edit; explicit compatible0->1 mutable notes inside valid v1 vault stage through change engine.
- P07 lexical plain query quoted/escaped safe FTS; filters before caps; maximum50 hits/80 candidates/2048 excerpt bytes/4096 query bytes/64 lexical terms; cursor binds full snapshot/query/mode/filters/limits. Literal scans visible original audit bytes; audit excerpts never manufacture source citations.
- Actual bundled FTS5 unicode61 tokenizer FFI in P07 excerpts.rs (RAII, bounded offsets, callback panic guard, no Send/Sync). Requires independent review and exact original byte mapping tests; escaped/decoded Markdown matches omit uncertain spans.
- Root CLI default read/search obtains verified reader with held writer, explicitly recovers incomplete apply when required; --no-sync uses labelled pinned cached data. Strict dry-run never opens SQLite. JSON/JSONL stable protocols; only supported command registry activated on acceptance.

## Dirty paths and next action

P05 acceptance metadata STATE/P05-checks.json plus P06/P07 implementation paths and root CLI/types/schema/migration helper are dirty, uncommitted. No user changes overwritten.

1. Serialize Cargo leases: P07 check/target tests, then P06 target tests, then root CLI and integration/Clippy/fmt. Root owns repository-wide checks.
2. Root adds executable subprocess CLI workflow, format/error/JSONL/schema and full-tree dry-run tests; document D28 migration/bootstrap/preferences and D29 bounded changes inspection/default page placement.
3. Independent Sol cross-review P06/P07 (Astra unavailable), root FFI/protocol/source-freshness review; resolve findings and record exact evidence before commit.
4. Continue P08 graph query, then P09 verified context, and complete full DAG through M4 without routine confirmation. No live providers/real vaults/global installs/publishing.

## Latest integration acceptance

P06/P07 accepted:139 selected parent tests,0 failures,14 app/10 CLI/13 retrieval/6 machine; actual AfterCommit CLI recovery and11 existing SQLite kill points passed. Independent Sol P06/P07 reviews closed lost error IDs, unsupported identity-description excerpts and human snapshot labels. All-target Clippy0.43s/fmt/debugbuild5.63s/diff/seed34 pass. Final75-file fingerprint bf3a3e2a92286a071cb30af37cbd2737e67d7aef475f5e5ec1f03cb02da1d4df in P06-P07-checks.json. P05 unchanged500-fault matrix retained prior evidence, not rerun. No source/Cargo workers active; root source tree quiescent. Root commit coherent P06/P07, update HEAD, then lease P08 planning/graph implementation; P09 context follows.

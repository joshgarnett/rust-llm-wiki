# Execution checkpoint

Updated: 2026-09-28. Phase: **P09 accepted; M0/M1 local gates complete; M2 interfaces next**. Root owns this file.

## Objective and authorization

Implement every P00–P21 package and V01–V17 local gate through M4. User authorized Sol implementation/Astra review, local commits and disposable fixtures/mocks. No real-vault writes, live spending, host installation, publishing or push. E01–E05 qualification remains separate. Persistent goal remains active; M1 alone is not completion.

## Accepted baseline

- Branch `impl/autonomous-v1`, HEAD `98d59983d0b11d7031f8a77b45267e6ea910744d` (P00–P09 accepted, M0/M1 local gates complete; P09 acceptance commit pending).
- Research4652248; planninga97679c; P0077ef9b7; P01/P02f04a9d5; P0374cfb58; P047196eaf; P056158b58; P06/P07c90953e; P0898d5998.
- Rust/Cargo1.98.0 pinned, Rust2024, macOS26.5.2 arm64. Exact bundled rusqlite0.40.1/SQLite3.53.2,132 locked packages. No P06/P07 dependencies added.
- Git/dependency network require permitted escalation; prior authorized reviews passed. No bypass.
- Activated commands24 in src/cli/dispatch.rs COMMANDS: capabilities/schema/init/read/page put+rename/source add+refresh+withdraw/evidence revalidate/index sync+rebuild/search/check/doctor/changes show+apply+abort+rollback/recover/migrate. Graph lexical query/neighbors activated; search literal/lexical only; doctor provider probe unavailable pending P16. Context activated; later skill/provider/research commands remain unadvertised.

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

P09 accepted, M1 local offline exit complete; root owns all accepted source/tests. Local P09 acceptance commit pending immediately after this checkpoint. Context is activated (24 commands).

- Root full `cargo test --locked --offline --all-targets` exit0:210 parent tests; native fault matrix25 recovery tests350.07s, both changes SIGKILL wrappers, both SQLite wrappers11 points, catalog31, contextCLI5/freshness18/M1workflow1, graph17/CLI4. Two ignored helper tests explicitly invoked by wrappers.
- Initial fmt/Clippy found whitespace in CLI and two equivalent nested-if/let forms in context/verification; corrected. Final34 parent context/CLI/M1/graphCLI/machine tests passed; Clippy3.61s/fmt/debug build/seed34/diff passed. Full gate precedes these style-only edits; final34 and lint/build test final sources. Exact131 hashes/fingerprint31afa45336a50aa36121fa5307195dbe2fa258071cda071602694ddaae5cfe5a in reports/P09-checks.json. Logs /tmp/lwiki-p09-{all-targets,final,clippy-final,build-final,seed}.log.
- Independent Sol P09 review closed R1 transitive overlap components/remapping and R2 rejected assertion historical citation labels; exact closed re-projection blocks consistently forged SQL prose, initial/final selected source dependencies bind captured bytes, one shared-meter retry includes collection races. Context policy exclusions apply before every candidate cap; public candidate filters/path/state/prose are rebound. Snapshot citations absent, label within budget; strict dry no SQLite.
- D30 proof counts canonical/source/dependency bytes/files and logical path operations only. Operational SQLite/journal/recovery/index maintenance remains separate; deadline surrounds setup/proof and cannot interrupt blocking syscalls. No provider/helper handles exist yet; counted transport proof P16/V16 pending.
- P08 accepted98d5998; P09 root corrected its relationship-seed subject frontier to Incoming while retaining original proposition direction; meaningful continuity regression and full graph17/CLI4 pass. Reports/coverage/PROGRESS updated; M2–M4 still required.
- `/root/p05_scan_eligibility` Sol completed P09; all source/test/report/Cargo leases returned. `/root/p05_sql_publication` completed independent review, now ACTIVE P15 interface proposal only: /tmp/lwiki-p15-types-proposal.rs and reports/P15-plan.md; no repo source/tests/Cargo. Conservative unknown concurrency, explicit unchanged-limit resume, journal64MiB/events65536/tasks4096/event256KiB/meta64KiB/spool1MiB generation+8MiB others, complete genesis/history without pruning. Root operational IO helper/frozen types required before implementation.
- `/root/p02_vault` completed P10 plan and draft review /tmp/lwiki-p10-types-review.md. Root draft /tmp/lwiki-p10-types.rs awaiting final integration after P09 commit. Required additions: sealed VerifiedExtractionArtifact/load_extraction for P11; optional PreparedChange + canonical-restored disposition; Debug PacketPlan; strict nested candidate RecordRef shape; bounded source original/content reads before allocation. Limits/depth/maps and candidate snapshot semantics reviewed. No P10 repo implementation yet.
- Dirty acceptance paths: P09 CLI/retrieval leaves/shared helpers/graph continuity and context/M1 tests, reportsP09/P08-checks metadata, STATE/DECISIONS/PROGRESS/coverage. P15-plan remains planning-only and outside P09 source acceptance.
- Next: commit coherent accepted P09 locally; update accepted commit metadata/HEAD; integrate P10 shared types/schemas and bounded source helper; dispatch P02 P10 leaves/tests. Freeze P15 interfaces/operational IO, then bounded independent jobs implementation/review in parallel. Continue P10–P21 through M4 without routine confirmation.

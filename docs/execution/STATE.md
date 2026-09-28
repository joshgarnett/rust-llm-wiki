# Execution checkpoint

Updated: 2026-09-28. Phase: **P10 locally accepted awaiting commit; M0/M1 complete; P11 interfaces/P15 accounting active**. Root owns this file.

## Objective and authorization

Implement every P00–P21 package and V01–V17 local gate through M4. User authorized Sol implementation/Astra review, local commits and disposable fixtures/mocks. No real-vault writes, live spending, host installation, publishing or push. E01–E05 qualification remains separate. Persistent goal remains active; M1 alone is not completion.

## Accepted baseline

- Branch `impl/autonomous-v1`, HEAD `21a1c2968a52f59a01cda6cbd4cd163b5dd54b51` (P00–P09 accepted, M0/M1 local gates complete).
- Research4652248; planninga97679c; P0077ef9b7; P01/P02f04a9d5; P0374cfb58; P047196eaf; P056158b58; P06/P07c90953e; P0898d5998; P0921a1c29.
- Rust/Cargo1.98.0 pinned, Rust2024, macOS26.5.2 arm64. Exact bundled rusqlite0.40.1/SQLite3.53.2,132 locked packages. No P06/P07 dependencies added.
- Git/dependency network require permitted escalation; prior authorized reviews passed. No bypass.
- Activated commands26 in src/cli/dispatch.rs COMMANDS: capabilities/schema/init/read/page put+rename/source add+refresh+withdraw/evidence revalidate/index sync+rebuild/search/check/doctor/changes show+apply+abort+rollback/recover/migrate. Graph lexical query/neighbors activated; search literal/lexical only; doctor provider probe unavailable pending P16. Context/agent extract/import activated; later skill/provider/research commands remain unadvertised.

## Latest accepted checks

- P05 broad all-target offline suite:127 parent tests,0 failures. Actual250 durable-I/O boundaries × before/after =500 faults and two changes SIGKILL wrappers passed. Two ignored crash helpers explicitly invoked. Prior fault evidence retained; no changes engine/filesystem/catalog invariant edits in P06/P07.
- P06/P07 selected13-target integration:139 parent tests,0 failures; application14(7.56s), CLI10(3.38s), retrieval13(2.00s), machine6 and catalog31. Existing11 SQLite SIGKILL points reran; one ignored child explicitly called by two enabled wrappers. Actual AfterCommit/FilesApplied-to-CLI recovery test passed.
- Required-name-only machine/CLI rename rerun:16 passed (CLI10/machine6). All-target Clippy -D warnings0.43s, fmt, debug build5.63s, diff and unchanged seed34 passed. Logs/exact75 file SHA256s and tree fingerprint bf3a3e2a92286a071cb30af37cbd2737e67d7aef475f5e5ec1f03cb02da1d4df in reports/P06-P07-checks.json.
- Independent Sol P06/P07 cross-review plus root lifetime/source-map/protocol inspection closed all blocking findings: retained prepared IDs lost on apply failure; identity-only entity excerpt leaking unsupported body; human no-sync missing freshness label. Final reports P06/P07-review.md plus root acceptance updates. Runtime rejects fresh/restarted Astra at thread limit; fallback retains gates.
- D27 dry-run bypasses every SQLite open/check/snapshot/writer because normal read-only WAL may create sidecars. Actual recursive membership/bytes/mtime invariance passed; network/helper handles absent in current local layer, counting provider transports still P16/V16 work.
- Native faults/mocks do not prove power-loss safety, native Linux/Windows, hosts or live providers. E01–E05 remain open.

## Accepted interfaces and decisions

Root owns all accepted P00–P10 code now, shared Cargo/modules/types/schemas/CLI/records/catalog/state/reports/commits. Worker historical hashes remain reports; root checks.json final integrated hashes authoritative.

- OfflineApp typed guarded operations/getters; explicit flat/private JSON preferences; bounded read/plan/mutation/change outcomes. Source outcomes expose allocated source/revision IDs even reused head. Read only visible canonical notes/captured content, hash binding before typed authority. No-sync CLI read uses pinned cached raw bytes; default holds writer and verifies independently.
- Init creates at most one missing final component beneath existing checked parent, syncs parent/root, managed dirs/excludes and guarded marker LAST, no SQLite bootstrap. D28 only compatible0->1 mutable note scalar migration inside already valid v1 vault; future/unknown/immutable versions refuse edits.
- D29 page put existing ID path or pages/ID.md with explicit path override. Changes show verifies exact binary before/proposed, default aggregate16MiB with omitted indices; explicit --operation selects one existing64MiB-per-payload bounded operation. Error envelopes preserve retained change IDs/hash and partial flag.
- P07 QueryPlan/SearchFilters/limits/HitSet are frozen. Locator alone supplies path/record identity. Query4096 bytes/64 lexical terms, hits<=50/candidates<=80/excerpt<=2048, normalized filters<=80 items; cursor binds full snapshot/query/mode/filters/limits.
- Actual connection-borrowed RAII FTS5 unicode61 remove_diacritics2 tokenizer FFI accepted independent/root review; synchronous callback copies/catches unwind, finalize/delete verified, no Send/Sync. Raw match spans only after exact mapped-byte equality; decoded/escaped Markdown omits uncertain spans.
- Literal audit raw bytes permitted but notes/transcripts/duplicate quotes never source citations. Verified intact captured payloads alone get direct source-span citations; IndexSnapshot no citations. Identity-only entities return empty excerpts, preserving name/alias seeds without unsupported description.

## Active leases and next action

P10 accepted, local commit pending; M1 offline exit complete; root owns all accepted source/tests. P09 committed21a1c29. Context/agent extract/import activated (26 commands).

- Root full `cargo test --locked --offline --all-targets` exit0:210 parent tests; native fault matrix25 recovery tests350.07s, both changes SIGKILL wrappers, both SQLite wrappers11 points, catalog31, contextCLI5/freshness18/M1workflow1, graph17/CLI4. Two ignored helper tests explicitly invoked by wrappers.
- Initial fmt/Clippy found whitespace in CLI and two equivalent nested-if/let forms in context/verification; corrected. Final34 parent context/CLI/M1/graphCLI/machine tests passed; Clippy3.61s/fmt/debug build/seed34/diff passed. Full gate precedes these style-only edits; final34 and lint/build test final sources. Exact131 hashes/fingerprint31afa45336a50aa36121fa5307195dbe2fa258071cda071602694ddaae5cfe5a in reports/P09-checks.json. Logs /tmp/lwiki-p09-{all-targets,final,clippy-final,build-final,seed}.log.
- Independent Sol P09 review closed R1 transitive overlap components/remapping and R2 rejected assertion historical citation labels; exact closed re-projection blocks consistently forged SQL prose, initial/final selected source dependencies bind captured bytes, one shared-meter retry includes collection races. Context policy exclusions apply before every candidate cap; public candidate filters/path/state/prose are rebound. Snapshot citations absent, label within budget; strict dry no SQLite.
- D30 proof counts canonical/source/dependency bytes/files and logical path operations only. Operational SQLite/journal/recovery/index maintenance remains separate; deadline surrounds setup/proof and cannot interrupt blocking syscalls. No provider/helper handles exist yet; counted transport proof P16/V16 pending.
- P08 accepted98d5998; P09 root corrected its relationship-seed subject frontier to Incoming while retaining original proposition direction; meaningful continuity regression and full graph17/CLI4 pass. Reports/coverage/PROGRESS updated; M2–M4 still required.
- P10 final109 parent tests/11 targets passed in coherent HEAD+P10-only checkout, excluding unfinished P15. Strict all-target Clippy/fmt/debug build8.00s/seed34/diff passed. Earlier49 integration tests passed; final enumeration/test aliases matched109 gate. Exact source/fixtures/schema/Cargo hashes and checkout/log evidence in reports/P10-checks.json. CLI3 verifies persisted export/schemas/staged apply/restore/new conflicts/API unavailable/cache-deleted dry byte+mtime invariance. Independent Sol R1/R2 closed; M2 still requires P11–P14.
- `/root/p05_scan_eligibility` Sol completed P10 independent review, leases returned; root owns final source/reports.
- `/root/p02_vault` Sol completed P10 and P11 planning, leases returned. P11 proposal /tmp/lwiki-p11-types-proposal.rs and reports/P11-plan.md; no implementation/Cargo. Root must freeze resolution/receipt/schema/GraphResolve origin/proposed verifier/mention-specific Catalog classification before leaves.
- `/root/p05_sql_publication` Sol ACTIVE P15 exclusive jobs/{budgets,events,tasks,ledger,checkpoint,replay,accounting_tests}.rs, tests/job_accounting.rs, fixtures/p15/**, reports/P15.md. No shared/Cargo/schema/CLI/Git/nested delegation; no Cargo yet. Root exports jobs::types until all leaves exist.
- Root P15 owns jobs/types+mod/lib and vault/{operational,operational_tests,fs,mod}. RunStore/guard binds vault/run/fullAttemptRef, injected DurableIo/fixed lock, expected length/hash,0600/0700, bounded body/meta/checkpoint/owner cleanup. Actual4 helper tests/128 before-after fault points passed12.59s; read_checkpoint added since test, awaits final P15 gate. Durable complete-history head before authority; coordinated rollback cannot be detected. RunPlan/Event/Receipt shape frozen; checkpoint acknowledgment verifies real committed changeset.
- Dirty disjoint P15 source is unaccepted and excluded from P10 commit. P11/P15 planning reports untracked; P10 source/schema/appCLI/reports/STATE/PROGRESS/coverage accepted for local commit. P09 accepted-commit metadata update retained.
- Next: commit exact P10 slice; freeze P11 types/schema/origin/verifier/mention classifier and delegate leaves. Register ready P15 leaves then serialized accounting tests/review. Continue P11–P21 through M4 without routine confirmation.

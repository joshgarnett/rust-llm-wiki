# Defaults and deferred decisions

This file lets the implementation continue without routine questions. Root owns updates. Defaults are reversible implementation choices under the user's autonomous goal; they do not override technical invariants or grant unrelated permissions.

## Decision procedure

| Situation | Action |
|---|---|
| Contract already decides it | Implement that contract; do not reopen it as a preference question |
| Missing reversible detail | Choose the simplest compatible behavior; record material API/format choices and add relevant tests |
| Two contracts conflict | Read their precedence/context, isolate the invariant, request bounded Astra analysis if useful, record the resolution, and update both documents |
| Missing credentials, remote CI, GUI host, target OS, naming/license preference | Build and validate the local implementation; log the specific external qualification or owner decision |
| Dependency/package unavailable | Use a verified compatible cached version or equivalent implementation preserving requirements; otherwise block that package and continue independent work |
| Failed correctness/security/recovery test | Fix it or mark that core package blocked; never classify it as an external deferral |
| Unsafe, destructive, paid, or publishing step outside scope | Do not take it; preserve a reviewable artifact and continue local work |
| All remaining core work blocked | Checkpoint exact failures and alternatives; stop only when no safe meaningful work remains, following runtime goal rules |

Do not reclassify missing code, TODO branches, failing tests, unresolved review findings, or missing model-budget enforcement as optional work. If the safe choice narrows behavior within the contract (for example, refusing a malformed envelope), expose it clearly. If it removes a promised capability, it requires resolution as core work.

## Chosen defaults

| ID | Choice | Rationale / revisit trigger |
|---|---|---|
| D01 | Binary `lwiki`, Rust package `rust-llm-wiki`, initial version `0.1.0`, `publish = false` | Provisional names permit implementation; owner chooses public branding/publication later |
| D02 | One library plus thin CLI; M0–M4 in scope, M5 excluded | Matches technical handoff; do not stop at offline MVP |
| D03 | Root and routine workers `gpt-6-sol`; Astra for explicit invariant questions/reviews | User choice; record model fallback if runtime cannot supply one |
| D04 | Host-agent M2 before direct APIs M3 and research M4 | Existing dependency order; no clarification required |
| D05 | Current macOS platform is first native qualification; Linux/Windows code and CI prepared | Other native test execution deferred only when unavailable; never advertise untested durability |
| D06 | Prefer the installed compatible stable Rust toolchain, pin its exact verified version; 2024 edition | Planning observed Rust/Cargo 1.98.0; verify at bootstrap, avoid unnecessary global installation |
| D07 | Bundled SQLite/FTS5; bounded Rust exact vector scan | Existing decision; ANN/vector extensions deferred to measured M5 work |
| D08 | Remote embeddings and Chat Completions-compatible generation; provisional Brave search plus explicit-URL research | No local models; endpoint/model remain private user configuration; no production defaults/credentials invented |
| D09 | All automated product-provider tests use loopback mocks and fake static/dynamic credentials | All provider/auth/budget paths can be built without paid requests; live interoperability remains separately unqualified |
| D10 | Synthetic fixture vault and labeled local evaluation queries | No access to real user documents required; mock vectors prove math/state behavior, not semantic model quality |
| D11 | Preserve current formatting and unknown fields; refuse unsupported structured mutation explicitly | Avoid corrupting Markdown while implementing the documented lossless subset |
| D12 | Export skills to temporary/test directories; build a portable maintained skill source | Do not install into user-wide host directories or rewrite existing host instructions during development |
| D13 | Local checkpoint commits by root only when permitted by the invoked goal; no pushes/tags/releases | Durable progress without external publication; Git denial is recorded, not bypassed |
| D14 | Do not select a project license or add third-party implementation code without its license obligations | Ordinary dependencies may be evaluated/used; owner licensing decision deferred before publication |
| D15 | Omitted remote credentials produce a clear capability/configuration error; no silent semantic fallback | Existing CLI contract; explicit lexical fallback remains available where specified |
| D16 | No real-vault migration, deletion, or provider spending during implementation | Disposable fixtures can exercise destructive/recovery paths safely |
| D18 | BLAKE3 fingerprints for parser/extractor identities; canonical UTC timestamps accept Z/z/+00:00, reject unknown -00:00 | Explicit reproducible typed spelling; preserves RFC3339 UTC semantics; P00 contracts/tests |
| D19 | yaml-rust2 event validation + byte-range editor candidate; pulldown-cmark offsets; unicase 2.9.0 portable full folding | P01 spike must prove losslessness. Rejected unicode-casefold 0.2.0 Unicode9 tables after upstream source inspection; modern casing regression in P02 |
| D20 | Retained change manifests hash exact deterministic JSON fence bytes; journal header/length and body separately checksummed; missing apply intent is never inferred from matching target bytes | P03 recovery format, staged plan remains readable without authorizing replay; external editors remain outside filesystem compare-and-swap |
| D21 | Graph validation and catalog publication require explicit trusted application backends; publication permit constructor stays engine-private | No permissive production defaults or CLI graph activation before P05; integration tests may inject bounded mocks without claiming SQLite qualification |
| D22 | Applying journal events may start a recovery epoch from Applying/FilesApplied/Indexed, clearing operation completion counters; Indexed may repeat after verified republication | Required replay of observed old bytes despite Done flags; full payload/state/graph preflight first, terminal Conflict/Committed/Aborted never resume automatically |
| D23 | Retain manifest-bound validation.json before Applying with projected scan, parser/control fingerprints and read-dependency states; recovery reuses it rather than accepting a new baseline | Captured out-of-target reads survive process/journal interruptions; known Prepared proposals may become stale without blocking other publication, while missing-state ambiguity and unresolved apply still block |
| D24 | Incomplete preparation directories lacking a manifest and all journal/outcome/validation proof remain retained diagnostics, excluded from actionable change IDs; any missing manifest with authority/baseline proof fails RecoveryRequired | Crash during prepare cannot poison future origin lookup; no partial plan gains application authority, no silent deletion or salvage of lost binding |
| D25 | Revision tree ownership/member claims persist after graph validation and before Applying; only verified same-manifest interrupted creations may resume; namespace components use portable full folding, IDs remain case-sensitive | Recheck entire immutable member set and competing sealed owners at application/recovery, preserve foreign/extra assets, refuse lost baseline; prevents prepared proposals extending already sealed revisions |
| D26 | Original read preconditions live in the retained change manifest; omit empty lists for legacy v1 encoding. Check base states before preparation and throughout apply/recovery; normalize identical guarded write overlap, retain requested no-op guards, reject duplicate/overlapping/case-colliding union paths | Keeps staged evidence revalidation's predecessor hash authorization. Source planners bind base reads into drafts; projected overlay hashes remain separate graph dependencies. Terminal historical results skip live guards; stale Prepared abort stays safe |
| D27 | Normal catalog readers use SQLite read-only WAL transactions with ordinary locking/sidecar semantics. Strict CLI dry-run uses canonical read-only scans/planners and marks cache-dependent facts unknown without opening SQLite | SQLite read-only WAL can create missing sidecars; immutable mode is unsafe for a concurrently published cache. P06 must test unchanged vault/cache/journal trees for dry-run; no index refresh or sidecar creation is authorized by dry-run |
| D17 | Versioned strict model-output schemas for research frontier, gap assessment, and synthesis; root integrates them in P20 | Makes the existing bounded-research contract executable; validated references establish provenance, not semantic truth |

## Deferred items register

These entries are external qualification or owner preferences, not omitted implementation. Keep them open until evidence exists; marking core work complete does not mark these passed.

| ID | Deferred item | Work that still proceeds | Revisit / status |
|---|---|---|---|
| E01 | Actual embedding/generation endpoints, model choice, and credentials | Complete wire/auth/cache/retry/accounting tests with mocks; ship opt-in live contract test procedure | Owner provides trusted profile and bounded spend authorization; open |
| E02 | Live Codex/Claude Code/Cursor discovery and Obsidian editing | Skill export, syntax/layout checks, executable CLI examples, golden Markdown/link round trips | Run only in available disposable host environments; untested hosts remain unqualified; open |
| E03 | Native Linux/Windows fault-injection and clean-machine artifact checks | Portable implementation, platform adapters, CI matrix/test scripts; run every available target | Native environment/authorized CI available; open |
| E04 | Representative user corpus and semantic quality targets | Synthetic regression corpus, eval harness, lexical baseline, exact-vector correctness | User corpus or authorized dataset later; no model-quality claim now; open |
| E05 | Final name/license/distribution signing/publication | Build package privately and document installation from local binary | Owner decision before public release; open |

For a new deferral record: `ID; date; affected package/gate; missing input/resource; safe default/alternative; user impact; exact revisit condition; status; evidence path`. Never write secrets into this log. New behavioral decisions use `D18...`; new external items use `E06...`; actual core blockers use `B01...` and remain package blockers.

## Blockers

None observed at planning time. Before stopping for a blocker, record the failure, two materially different safe approaches when available, any specialist finding, independent work completed, and the smallest external change needed. Two alternatives is a troubleshooting target, not a reason to repeat an unsafe/clearly impossible action.

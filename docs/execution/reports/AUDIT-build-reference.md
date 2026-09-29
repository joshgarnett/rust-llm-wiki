# Reference, build and qualification audit

Audited 2026-09-29 at `63b2439f5cd57211e2a2bc5587ac5bace3d03ea2`, with release source `d8a82a5a345549fada06987dec986801c6842252` / v0.1.2. `git diff v0.1.2 -- src schemas skills Cargo.toml Cargo.lock` is empty. This audit changes reports and execution state only. Existing release archives, tags and draft releases are unchanged.

## B1 — Medium process gap: normal CI and release builds do not execute the important integration suites

`just ci` runs format/Clippy, Python tooling checks, and `//:smoke` (`justfile:57-58`). The smoke suite in `BUILD.bazel` contains only build paths, schemas/contracts, machine protocol, offline CLI and skill export. `.github/workflows/ci.yml:34-37` uses this gate plus a version/capability candidate build. `.github/workflows/release-build.yml` adds the same skill recipe in release mode, but does not add research recovery, provider accounting, graph review, context freshness, embedding bounds, or change recovery tests. Thus a green six-platform release is primarily a build/short-workflow result.

The complete suite exists under `//:test` and `just qualify`, and `.github/workflows/qualification.yml` makes it manually dispatchable on Linux x64 and macOS ARM64. This is a coverage gap in the default integration/release policy, not an absence of recovery tests or a claim that existing build reports misrepresented their scope. The latest 0.1.2 local evidence had affected continuations rather than a fresh whole-suite pass. Root started a new uncached `//:test` invocation for this audit; the consolidated audit records its final result.

**Work:** add a reasonably fast, explicit integration gate for the graph/source/research/provider changes on pull requests; make an exact-source full native qualification result part of release promotion. Keep the long crash/fault matrix separate if needed, but do not rely on release packaging to cover these behaviors. Record platform and source provenance for every gate.

## B2 — Low process gap: qualification retains the older Windows timeout

`.github/workflows/qualification.yml:52` still allows 30 minutes for setup, all-source/test compilation and an optimized Windows candidate. The normal CI Windows job (`ci.yml:56`) and release job allow 60 minutes. Previous build evidence in `STATE.md` records that Windows optimization exceeded 30 minutes and led to the normal CI increase. No new Windows timeout was observed during this audit.

**Work:** align the qualification job with the measured Windows build allowance or narrow its scope explicitly. This remains a build check; increasing its timeout does not qualify native Windows writes.

## B3 — Medium documentation gap: accepted coverage and current plans describe different products

The old planning/coverage documents remain linked from the current README, and several have authoritative-sounding status text:

- `docs/technical/README.md:3`, `docs/technical/cli-and-skills.md:3`, `docs/implementation-plan.md:3`, and `docs/agent-integration.md:3` still describe an unimplemented CLI/skill.
- `docs/architecture.md`, `docs/implementation-plan.md`, and `docs/technical/implementation-handoff.md` still plan a standalone research executor. `docs/execution/VALIDATION.md` V14/V15 and `reports/coverage.md` still require or claim removed acquisition/planner stages and a search provider probe.
- `docs/qualification.md` tells an operator to probe `--role search` and run provider-backed research. `src/providers/types.rs` / CLI roles only expose embeddings and generation, and the research contract expressly delegates all tool work to the host.
- `docs/providers.md:20` still refers to configuration files containing search services. `docs/builds.md` records the initial v0.1.0 release as its latest concrete build evidence; later version guides carry the current result.

Historical exploratory material is useful and need not be deleted. The missing piece is a single current contract/coverage map with prominent supersession notices and executable current instructions. The user's host-agent-only research decision supersedes the old M4 implementation. Restoring the removed executor is not a corrective action.

**Work:** revise the current architecture, technical router, qualification instructions and gate map; archive or label old milestones; make README links point to current evidence. Check help/examples and current docs together during release preparation. A local link scan found no confirmed missing current documentation target (the apparent `docs/wiki-format.md → index.md` link is inside a sample note, not a repository link).

## Reference-to-product comparison

These are design comparisons, not a demand to copy every feature from another project. Original research snapshots remain useful, but live upstream branches are not immutable requirements. Primary reference pages were reopened for this audit; no reference software was installed or benchmarked.

| Reference idea | Present in lwiki | Remaining work / boundary |
|---|---|---|
| Persistent synthesis between raw sources and user answers | Immutable capture, durable pages/graph/evidence, guarded writes, research reports | The maintained host workflow stops at a report and disconnected page examples. Add ingest/query-to-page promotion and refresh/withdrawal repair, with a repeatable end-to-end example. The original pattern emphasizes accumulated linked synthesis and periodic maintenance. [LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f) |
| Separate evidence, research drafts and canonical mutations | Unassessed research claims; explicit graph resolution/review/apply; recoverable changes | Complete conflict resolution and document multi-page synthesis through the host. The comparison project supplies explicit coupled transaction bundles and separates provenance from claim assessment; its approval ceremony is not a requirement here. [Transactions](https://raw.githubusercontent.com/AgriciDaniel/claude-obsidian/main/skills/wiki/references/operation-transactions.md), [provenance](https://raw.githubusercontent.com/AgriciDaniel/claude-obsidian/main/skills/wiki/references/provenance.md) |
| Bounded layered retrieval for agents | Literal/FTS5, remote vectors, RRF, IDs, excerpts and bounded context | Enforce scope through final evidence packing, fix navigation context and owner diversity, then evaluate representative queries. QMD provides a useful retrieval comparison, but its local-model runtime is intentionally outside this product. [QMD](https://github.com/tobi/qmd) |
| Shared human/agent Markdown with typed relations | Durable identities, editable notes, parsed Markdown/wikilinks, graph rebuild | Validate actual host discovery and Obsidian navigation; complete evidence-link diagnostics. Basic Memory is a format/workflow comparison, not a required runtime or protocol dependency. [Basic Memory](https://github.com/basicmachines-co/basic-memory) |
| Entity/relationship retrieval plus original passages | Host/API extraction, conservative identity decisions, evidence review, lexical/semantic graph queries | Retain qualifier/contradiction tests and measure usefulness independently of citation integrity. LightRAG's modes motivate the comparison; lwiki does not claim algorithm parity. [LightRAG](https://github.com/HKUDS/LightRAG) |
| Predictable Unix CLI and note navigation | Stdin input, stable errors, JSON/JSONL, read/search/links, guarded edits | Human truncation/warnings and navigation context remain incomplete. Editor/LSP, interactive pickers and an MCP server are optional scope additions. [zk](https://github.com/zk-org/zk) |

## Qualification and scope still open

- Windows artifacts build, but native vault writes are deliberately refused. Full native recovery qualification on Linux and other architectures, clean-machine installation, and power-loss behavior are not established by six native builds. Existing process-kill tests establish only their documented model.
- The owner has supplied live gateway compatibility and small real-search observations. They are useful evidence, but this audit uses synthetic loopback providers only and does not establish billing behavior, every gateway model, or general retrieval quality.
- The fixed retrieval corpus contains eight labeled questions (`tests/fixtures/p21/queries.json`); `tests/retrieval_baseline.rs` uses hash-derived synthetic vectors. This verifies mechanics and reproducibility, not semantic quality. Build a larger held-out corpus for multi-section documents, paraphrases, multilingual names, conflicts, stale sources and unanswerable questions, and compare under equal budgets before adding ANN/reranking/community summaries.
- Actual Codex/Claude Code/Cursor discovery and negative controls, Obsidian GUI behavior, and the owner's installed-copy macOS SIGKILL are unqualified here. Packaged macOS strict codesign passed in release verification; it does not diagnose a separate installed copy.
- HTML/binary formats remain original-only capture with warnings; hosts should submit extracted UTF-8 text. No built-in browser/fetch/OCR or local-model runtime is required. Literal matching remains intentionally exact; use lexical matching for apostrophe variants. Ownership/responsibility predicates and finer default segmentation are product decisions, not demonstrated corruption.
- Naming/license/signing/notices and publication remain owner/distribution decisions. This audit does not repeat the earlier dependency-verification project, install hosts, dispatch production requests, or publish anything.

Checks and exact full-suite outcomes are collected in the consolidated `AUDIT-0.1.2.md` and `AUDIT-checks.json`.

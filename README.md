# Rust LLM Wiki

A Rust CLI for a local wiki that people and coding agents can read, maintain, and search.

**Status: required cleanup and local qualification complete, September 29, 2026.** The [current contract and coverage map](docs/current-contracts.md) routes implemented behavior and its validation limits; [the cleanup register](docs/execution/CLEANUP.md) records completed work and external follow-up. Earlier M0–M4 acceptance and 0.1.2 release evidence remain historical. `lwiki` is a provisional name. Live providers, other-host recovery, host discovery, real-model quality, licensing and publication need separate qualification or owner decisions.

Canonical records and captured sources live in Obsidian-compatible Markdown. Bundled SQLite provides rebuildable full-text search, metadata, graphs and vector caches. Extraction uses an existing agent or a configured generation API; semantic search uses a remote embeddings API and stores vectors locally. Short notes stay whole by default; long inputs split only when needed. The core does not require a model runtime, database server, Python, Node, or a background service.

Install the pinned development tools described in [builds and CI](docs/builds.md),
then create a native candidate with Just and Bazel and inspect its command registry:

```sh
just deps-fetch
just candidate .artifacts/candidate-001
./.artifacts/candidate-001/lwiki --json capabilities
./.artifacts/candidate-001/lwiki init /tmp/my-wiki
./.artifacts/candidate-001/lwiki --wiki /tmp/my-wiki source add ./notes.txt
./.artifacts/candidate-001/lwiki --wiki /tmp/my-wiki --offline search 'identifier' --mode literal
./.artifacts/candidate-001/lwiki --wiki /tmp/my-wiki --offline context 'question' --max-bytes 12000 --max-tokens 3000
```

Choose a new directory for `init`; it refuses an existing vault. Local capture/search/context do not need provider credentials. Use [the maintained skill workflows](skills/llm-wiki/references/workflows.md) for guarded edits, packet extraction, explicit resolution and review. `skill export --target codex|claude-code|cursor --output DIR` exports the workflow, executable examples and command reference from the running binary.

Remote embedding, API extraction and `doctor --probe` require explicit trusted provider configuration and caller budgets. Research instead hands bounded packets to a host agent: `research run QUESTION` persists a packet, `research import --file FILE` validates the agent's submitted sources or answer, and `research resume RUN_ID` returns the outstanding packet. The CLI does not search, fetch or generate for research. `--offline` research uses local or already captured content; `--dry-run` and `research plan` only preview. Reports retain gaps and unassessed claims; valid citation bytes do not prove a claim's meaning. Host-agent network and token usage are unobserved by CLI accounting.

Developers use Just with Bazel 9.2.0, rules_rust 0.72.0 and Rust 1.98.0. `just ci` checks formatting, Clippy, build tooling, CLI smoke tests and consequential source/provider/retrieval/research/storage regressions; `just qualify` runs the full native test/doctest/release gates with cached test results disabled and exercises the copied binary with a minimal system PATH in a disposable vault. Normal commands use the native platform wrapper and `--nofetch` after `just deps-fetch`; Cargo remains for metadata, lock maintenance and formatting edits. See [build commands and GitHub workflows](docs/builds.md) for setup, evidence and limits.

The [six native GitHub release jobs](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36564578331) passed for Linux GNU, macOS and Windows on x64 and ARM64. The [draft v0.1.0 release](https://github.com/joshgarnett/rust-llm-wiki/releases/tag/untagged-1fb57f8f77dbb46a0898) contains six verified archives and checksum files; it remains unpublished. [Build evidence](docs/execution/reports/BUILD-RELEASE.md) records the source and validation scope. **Windows vault writes remain unsupported**; Windows jobs check source compilation and copied-binary capabilities only.

## Start here

- [Current contract map](docs/current-contracts.md), [cleanup register](docs/execution/CLEANUP.md), and [test-agent guide](docs/testing-cleanup.md): current capabilities, work and local/external gates.
- [Execution playbook](docs/execution/README.md): scoped delegation, checkpoints and evidence; its original M0–M4 sequence is historical.
- [Opt-in external qualification](docs/qualification.md): exact follow-up procedures for provider, host, platform, corpus and publication checks.
- [Technical design](docs/technical/README.md): storage, retrieval, providers/jobs, CLI contracts, and an ordered implementation handoff.
- [Architecture proposal](docs/architecture.md): storage, retrieval, provenance, human and agent workflows.
- [Markdown format](docs/wiki-format.md): small frontmatter/body contract, editable headings, Obsidian links, and best-effort recovery.
- [Implementation plan](docs/implementation-plan.md): milestones, acceptance criteria, and open decisions.
- [Embedding API contract](docs/embeddings.md): full URL, model, static or dynamic credentials, batching, and error handling.
- [Entity/relationship graph](docs/knowledge-graph.md): extraction, entity resolution, evidence, incremental updates, and graph retrieval.
- [Agent integration and skill plan](docs/agent-integration.md): Codex, Claude Code, and Cursor.
- [Progress](PROGRESS.md): completed work, user decisions, and verification limits.

For the packaged historical 0.1.2 release, use the [download and test-agent guide](docs/testing-0.1.2.md), including the offline regression script and live gateway checks.

## Research

- [Comparable wiki and search tools](docs/research/landscape.md)
- [Retrieval, graphs, and evaluation](docs/research/retrieval.md)
- [Rust and embedded storage feasibility](docs/research/rust-stack.md)
- [Obsidian and Markdown research](docs/research/obsidian-markdown.md)
- [Cost-controlled automated research](docs/research/research-flows.md)
- [Prompts for independent external research](docs/external-research-prompts.md)

The initial research and architecture are committed as `4652248`. The current contract map and technical contracts route implemented behavior; historical planning pages preserve the original proposals. Local validation uses disposable vaults and mock providers. It does not establish live-provider compatibility or real-model retrieval quality. The research distinguishes documented capabilities, authors' experimental claims, and this project's design. The supplied Markdown-versus-RAG report informed the questions; its embedded citation identifiers cannot be resolved here, so linked primary-source research supplies the evidence base.

Provider setup and retesting: [Responses, compatible gateways and probes](docs/providers.md).

Human output renders command summaries, warnings and continuation guidance. `check` and `graph neighbors` currently render their structured result as formatted JSON; use `--json` for the stable output envelope. `storage plan` previews migration and retention; preserve a complete vault backup, including `.wiki`, before `storage cleanup`. Read [the cleanup guide](docs/testing-cleanup.md) for migration, guarded recovery and restore.

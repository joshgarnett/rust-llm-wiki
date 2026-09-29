# Rust LLM Wiki

A Rust CLI for a local wiki that people and coding agents can read, maintain, and search.

**Status: implementation in progress, September 29, 2026.** M0–M4 implementation packages are locally accepted; final integrated qualification is under test. `lwiki` remains a provisional name. See [the execution checkpoint](docs/execution/STATE.md) for current acceptance and [the coverage ledger](docs/execution/reports/coverage.md) for outstanding gates. Live providers, other native platforms, host discovery, naming, licensing and publication remain separate qualification or owner decisions.

Canonical records and captured sources live in Obsidian-compatible Markdown. Bundled SQLite provides rebuildable full-text search, metadata, graphs and vector caches. Extraction uses an existing agent or a configured generation API; semantic search uses a remote embeddings API and stores vectors locally. Short notes stay whole by default; long inputs split only when needed. The core does not require a model runtime, database server, Python, Node, or a background service.

Build with the pinned Rust toolchain and inspect the actual command registry:

```sh
cargo build --locked --release
./target/release/lwiki --json capabilities
./target/release/lwiki init /tmp/my-wiki
./target/release/lwiki --wiki /tmp/my-wiki source add ./notes.txt
./target/release/lwiki --wiki /tmp/my-wiki --offline search 'identifier' --mode literal
./target/release/lwiki --wiki /tmp/my-wiki --offline context 'question' --max-bytes 12000 --max-tokens 3000
```

Choose a new directory for `init`; it refuses an existing vault. Local capture/search/context do not need provider credentials. Use [the maintained skill workflows](skills/llm-wiki/references/workflows.md) for guarded edits, packet extraction, explicit resolution and review. `skill export --target codex|claude-code|cursor --output DIR` exports the workflow, executable examples and command reference from the running binary.

Remote embedding, API extraction, `doctor --probe` and research require explicit trusted provider configuration and caller budgets. `--offline` prevents remote work; `--dry-run` previews without writes or credential resolution. Research reports retain gaps and unassessed model claims; valid citation bytes do not prove a claim's meaning. Budget stops return exit 7 with retained partial output, and orderly interruption returns exit 130. Resume preserves lifetime limits and accounting; `research resume RUN_ID --amend-limits FILE` is the explicit amendment path.

Developers can run [scripts/qualify-local.sh](scripts/qualify-local.sh) after fetching locked dependencies. It runs format/lint/full tests/doctests, builds a release artifact, and exercises that copied binary with a minimal system PATH in a disposable vault. Dependency metadata and native linkage are retained with the logs. The [manual CI workflow](.github/workflows/qualification.yml) is prepared for future opt-in execution; its existence is not a passing hosted or other-platform result. Windows currently refuses unsupported directory durability before paid work; its CI job checks building and capability output only.

## Start here

- [Autonomous implementation plan](docs/execution/README.md) and [new-session goal](docs/execution/START.md): Sol orchestration, scoped workers, checkpoints, and M0–M4 acceptance gates.
- [Opt-in external qualification](docs/qualification.md): exact follow-up procedures for provider, host, platform, corpus and publication checks.
- [Technical design](docs/technical/README.md): storage, retrieval, providers/jobs, CLI contracts, and an ordered implementation handoff.
- [Architecture proposal](docs/architecture.md): storage, retrieval, provenance, human and agent workflows.
- [Markdown format](docs/wiki-format.md): small frontmatter/body contract, editable headings, Obsidian links, and best-effort recovery.
- [Implementation plan](docs/implementation-plan.md): milestones, acceptance criteria, and open decisions.
- [Embedding API contract](docs/embeddings.md): full URL, model, static or dynamic credentials, batching, and error handling.
- [Entity/relationship graph](docs/knowledge-graph.md): extraction, entity resolution, evidence, incremental updates, and graph retrieval.
- [Agent integration and skill plan](docs/agent-integration.md): Codex, Claude Code, and Cursor.
- [Progress](PROGRESS.md): completed work, user decisions, and verification limits.

## Research

- [Comparable wiki and search tools](docs/research/landscape.md)
- [Retrieval, graphs, and evaluation](docs/research/retrieval.md)
- [Rust and embedded storage feasibility](docs/research/rust-stack.md)
- [Obsidian and Markdown research](docs/research/obsidian-markdown.md)
- [Cost-controlled automated research](docs/research/research-flows.md)
- [Prompts for independent external research](docs/external-research-prompts.md)

The initial research and architecture are committed as `4652248`. The technical designs define the implementation contracts; some historical planning pages still describe proposed behavior. Local validation uses disposable vaults and mock providers. It does not establish live-provider compatibility or real-model retrieval quality. The research distinguishes documented capabilities, authors' experimental claims, and this project's design. The supplied Markdown-versus-RAG report informed the questions; its embedded citation identifiers cannot be resolved here, so linked primary-source research supplies the evidence base.

# Rust LLM Wiki

A Rust CLI for a local wiki that people and coding agents can read, maintain, and search.

Canonical records and captured sources live in Obsidian-compatible Markdown. Bundled SQLite provides rebuildable full-text search, metadata, graphs and vector caches. Capture deployment notes, find an exact command, and give an agent cited context without provider credentials. Extraction uses an existing agent or a configured generation API; semantic search uses a remote embeddings API and stores vectors locally. The installed binary needs no model runtime, database server, Python, Node, or background service.

## Try it locally

These commands use a macOS/Linux shell from the repository root. Install the pinned development tools in [builds and CI](docs/builds.md), then build a native candidate in a new output directory:

```sh
just deps-fetch
just candidate .artifacts/candidate-001
```

Windows vault writes remain unsupported. Existing binaries can run `--help` and `--json capabilities` to inspect their own version's commands. Historical packaged releases do not include the current cleanup changes; the [historical 0.1.2 guide](docs/testing-0.1.2.md) is for that release only.

Create a disposable wiki and searchable input:

```sh
LWIKI="$PWD/.artifacts/candidate-001/lwiki"
DEMO="$(mktemp -d)"
WIKI="$DEMO/wiki"
cat > "$DEMO/deployment-notes.md" <<'EOF'
# Atlas deployment notes

The Atlas service uses Postgres for its primary database.
Run atlas_migrate before deploying a new release.
Rollback restores the previous release and keeps the database unchanged.
EOF
"$LWIKI" --offline init "$WIKI"
"$LWIKI" --wiki "$WIKI" --offline --json source add "$DEMO/deployment-notes.md" --title 'Atlas deployment notes'
"$LWIKI" --wiki "$WIKI" --offline search 'atlas_migrate' --mode literal --limit 5
"$LWIKI" --wiki "$WIKI" --offline context 'Atlas deployment' --max-bytes 6000 --max-tokens 1500
"$LWIKI" --wiki "$WIKI" --offline check
```

Search should show the deployment note and its `atlas_migrate` command. Context should return the note with source/revision IDs, byte ranges and citation hashes; it retrieves text rather than generating an answer. `check` should report zero errors. `init` requires a directory that does not exist, so initialize the `wiki` child of the temporary directory. Search automatically syncs the local index.

Continue with [getting started](docs/getting-started.md) to add an authored page, refresh a source, inspect citations and export an agent workflow. The [maintained workflows](skills/llm-wiki/references/workflows.md) cover guarded edits, packet extraction, explicit resolution and review.

## Providers and research

Remote embedding, API extraction and `doctor --probe` require explicit trusted provider configuration and caller budgets. Research instead hands bounded packets to a host agent: `research run QUESTION` persists a packet, `research import --file FILE` validates the agent's submitted sources or answer, and `research resume RUN_ID` returns the outstanding packet. The CLI does not search, fetch or generate for research. `--offline` research uses local or already captured content; `--dry-run` and `research plan` only preview. Reports retain gaps and unassessed claims; valid citation bytes do not prove a claim's meaning. Host-agent network and token usage are unobserved by CLI accounting.

## Development and release evidence

Developers use Just with Bazel 9.2.0, rules_rust 0.72.0 and Rust 1.98.0. `just ci` checks formatting, Clippy, build tooling, CLI smoke tests and consequential source/provider/retrieval/research/storage regressions; `just qualify` runs the full native test/doctest/release gates with cached test results disabled and exercises the copied binary with a minimal system PATH in a disposable vault. Normal commands use the native platform wrapper and `--nofetch` after `just deps-fetch`; Cargo remains for metadata, lock maintenance and formatting edits. See [build commands and GitHub workflows](docs/builds.md) for setup, evidence and limits.

The [six native GitHub release jobs](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36564578331) passed for Linux GNU, macOS and Windows on x64 and ARM64. The [draft v0.1.0 release](https://github.com/joshgarnett/rust-llm-wiki/releases/tag/untagged-1fb57f8f77dbb46a0898) contains six verified archives and checksum files; it remains unpublished. [Build evidence](docs/execution/reports/BUILD-RELEASE.md) records the source and validation scope. **Windows vault writes remain unsupported**; Windows jobs check source compilation and copied-binary capabilities only.

## Documentation

- [End-user quality and critic workflow](docs/testing-usability.md): repeatable CLI, documentation and retrieval evaluation.
- [Indexed context](docs/indexed-context.md): verify selected captured-source bytes using an existing lexical index, with an explicit generation-scoped guarantee.
- [Context evaluation](docs/evaluating-context.md): reproducible public datasets, exact evidence coverage and independent completeness assessment.

- [Getting started](docs/getting-started.md): an offline tutorial and practical capture, page, refresh and agent-export examples.
- [Provider setup](docs/providers.md): Responses, compatible gateways and bounded probes.
- [Current contract map](docs/current-contracts.md), [cleanup register](docs/execution/CLEANUP.md), and [test-agent guide](docs/testing-cleanup.md): current capabilities, work and local/external gates.
- [Execution playbook](docs/execution/README.md): scoped delegation and fresh-checkout guidance; its original M0–M4 sequence is historical. Reusable guidance and curated validation summaries are tracked; checkpoints and detailed run reports remain local.
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

## Project status

**Required cleanup and local qualification complete, September 29, 2026.** `lwiki` is a provisional name. The [current contract and coverage map](docs/current-contracts.md) routes implemented behavior and its validation limits; [the cleanup register](docs/execution/CLEANUP.md) records completed work and external follow-up. Earlier M0–M4 acceptance and 0.1.2 release evidence remain historical. A [subsequent usability and live embedding evaluation](docs/execution/reports/UX-VALIDATION.md) exercises OpenAI embeddings and a small public-source corpus. Other providers, generation, other-host recovery, host discovery, broader real-model quality, licensing and publication still need separate qualification or owner decisions.

## Research

- [Comparable wiki and search tools](docs/research/landscape.md)
- [Retrieval, graphs, and evaluation](docs/research/retrieval.md)
- [Rust and embedded storage feasibility](docs/research/rust-stack.md)
- [Obsidian and Markdown research](docs/research/obsidian-markdown.md)
- [Cost-controlled automated research](docs/research/research-flows.md)
- [Prompts for independent external research](docs/external-research-prompts.md)

The initial research and architecture are committed as `4652248`. The current contract map and technical contracts route implemented behavior; historical planning pages preserve the original proposals. Automated regression validation uses disposable vaults and mock providers. Separately authorized live embedding evidence and its limits are recorded in the usability evaluation. The research distinguishes documented capabilities, authors' experimental claims, and this project's design. The supplied Markdown-versus-RAG report informed the questions; its embedded citation identifiers cannot be resolved here, so linked primary-source research supplies the evidence base.

Human output renders command summaries, warnings and continuation guidance. `check` and `graph neighbors` currently render their structured result as formatted JSON; use `--json` for the stable output envelope. `storage plan` previews migration and retention; preserve a complete vault backup, including `.wiki`, before `storage cleanup`. Read [the cleanup guide](docs/testing-cleanup.md) for migration, guarded recovery and restore.

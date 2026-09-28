# Research and planning progress

Started: 2026-09-28. Status: design revised for extracted graphs, Obsidian-compatible Markdown, editable headings, and document-first retrieval. No CLI implementation yet.

## Scope

Design a self-contained Rust CLI for a local, human-readable LLM wiki, with useful human and agent interfaces, lexical and graph retrieval, optional semantic retrieval, cost-controlled research, and a portable usage skill.

## Work tracking

- [x] Inspect repository and local instructions: empty repository; no existing implementation or applicable AGENTS.md found.
- [x] Read OpenAI documentation and skill-creation guidance for the integration plan.
- [x] Start primary-source research on the two named projects and the LLM Wiki concept.
- [x] Ask optional product questions about research execution and semantic-search dependencies.
- [x] Delegate focused research after explicit user authorization.
- [x] Comparable wiki/search systems — `wiki_landscape` agent; `docs/research/landscape.md`.
- [x] Retrieval, knowledge graphs, and evaluation — `retrieval_research` agent; `docs/research/retrieval.md`.
- [x] Rust, embedded storage, remote embeddings, and packaging — `rust_feasibility` agent; `docs/research/rust-stack.md`.
- [x] Research execution, costs, and agent integration — primary agent.
- [x] Synthesize architecture and command contracts.
- [x] Define phased implementation plan and acceptance gates.
- [x] Write independent research prompts for optional external review.
- [x] Review cross-document consistency, evidence, and local links.
- [x] Promote LightRAG-inspired entity/relationship extraction into the planned release scope and document the graph lifecycle.
- [x] Research official Obsidian behavior and the Agent Skills format; draft the Markdown wiki specification.
- [x] Define heading/metadata repair and document-first embedding/segmentation behavior.
- [x] Check the revised architecture, roadmap, research guidance, and skill plan for consistency.

## Confirmed user decisions

- Semantic search uses a configurable `/v1/embeddings`-compatible HTTP API; local embedding models are out of scope.
- Embedding configuration must support a static key or a script that supplies dynamic credentials, a full endpoint URL, a model, and necessary request/runtime options.
- Include a LightRAG-inspired extracted entity/relationship graph as a planned product capability, alongside page links and provenance.
- Put as much durable state as practical in Markdown, with Obsidian-compatible links and an Agent Skills-like frontmatter/body contract. Recovery is best effort; not every index/runtime value must live in Markdown.
- Account for users changing headings or metadata; avoid unnecessary chunking and evaluate where segmentation is useful.
- Use sub-agents where useful and track progress. Specialists contributed the graph design, Obsidian research, and independent consistency review.

## Working assumptions

These are proposals, pending discussion:

- Markdown and captured source files preserve durable knowledge; SQLite lexical and graph indexes are rebuildable. Missing vectors may require separately requested re-embedding.
- The default binary works without a model, daemon, database server, or API key.
- Existing agents can use the CLI; a standalone research runner is a separately scoped capability.
- The first agent-usable release includes extraction through the host agent and graph storage/querying; a configured generation API adds direct extraction in the CLI.
- Semantic search is an optional remote capability. Lexical lookup and traversal of an existing graph remain offline; semantic graph seeding and automatic extraction can require remote calls.
- Start with whole short-note embeddings and document-level text search. Split long model inputs or introduce smaller retrieval units when limits/quality justify it; no user-managed chunk files.

## Decisions still open

- Priority of agent-driven versus standalone automated research.
- Final binary name, initial target platforms, and representative evaluation corpus.

## Findings incorporated

- Separate durable sources, maintained wiki pages, research records, and disposable search projections.
- Include explicit links, provenance, and extracted entity/relationship graphs. Evaluate extraction quality and query costs as release gates; community summaries remain a later extension.
- Keep embedding HTTP transport separate from local vector storage; use exact vector search as the initial baseline.
- Do not equate wiki compilation, multi-agent research, or prompt caching with automatic savings.
- Enforce resource limits in the CLI-owned dispatcher; label budgets for external-agent work advisory.
- A filesystem changeset and SQLite transaction are not jointly atomic; recovery and external-edit conflicts need explicit contracts.
- Current libraries and projects have version-dependent capabilities and licensing; pin and recheck before implementation reuse.

## Verification boundary

Research uses current project documentation and papers. No project has been benchmarked locally, no provider calls have been purchased, and no cited performance claim should be read as a measurement of this proposed CLI.

## Review and validation completed

- Three sub-agents produced separate research briefs; two then reviewed the combined design independently.
- Incorporated review fixes for source withdrawal, snapshot tampering versus refresh, URL capture prerequisites, endpoint-bound credentials, external-editor races, embedding reuse across parser upgrades, and evidence freshness.
- Checked all 14 Markdown files for balanced code fences, opaque citation IDs, trailing whitespace, and valid local links (34 references outside example fences).
- Parsed both TOML configuration examples, the JSON credential example, and six YAML/frontmatter examples successfully using installed `tomli` and Ruby Psych.
- Reviewed the revised format/graph design independently. Added dependency/status verification, actual pending-change payload preservation, and precise UTF-8 span/hash rules. Obsidian behavior is documented from official sources; live compatibility tests remain an implementation gate.
- All command examples are labeled proposed. No runnable CLI or discoverable usage skill is claimed; the skill ships with tested commands in M2.

## Next implementation slice

Discuss [the Markdown format](docs/wiki-format.md), [graph design](docs/knowledge-graph.md), and [milestones](docs/implementation-plan.md), then build the offline capture → linked pages and fixture graph → search → evidence → heading/source edits → refresh/rebuild scenario. Remote embedding requirements are specified in [the API contract](docs/embeddings.md); independent follow-up research can use [these prompts](docs/external-research-prompts.md).

# Rust LLM Wiki

A proposed Rust CLI for a local wiki that people and coding agents can read, maintain, and search.

**Status: research and design, September 28, 2026. No CLI has been implemented.** `lwiki` is a working command name; every command shown in these documents is proposed.

The recommended foundation is Obsidian-compatible Markdown and captured sources, with bundled SQLite for full-text search, metadata, and graphs. Entities, assertions, evidence, and recorded decisions live in Markdown; indexes are derived. A LightRAG-inspired entity/relationship graph is part of the planned product. Extraction uses an existing agent or a configured generation API; semantic search uses a remote `/v1/embeddings`-compatible API and stores vectors locally. Short notes stay whole by default; long inputs split only when needed. The core does not require a model runtime, database server, Python, Node, or a background service.

## Start here

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

The research distinguishes documented capabilities, authors' experimental claims, and our proposed design. No local benchmark or paid research run has been performed. The supplied Markdown-versus-RAG report informed the questions; its embedded citation identifiers cannot be resolved here, so the linked primary-source research is the evidence base for this proposal.

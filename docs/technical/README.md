# Technical design

Status: proposed implementation baseline, reviewed 2026-09-28. Requirements baseline: commit `4652248`. Three design agents and the primary agent developed and cross-reviewed these contracts; no CLI has been implemented. Implementation is the next step, following the [ordered handoff](implementation-handoff.md).

## Design areas

| Design | Responsibility |
|---|---|
| [Storage and recovery](storage.md) | Markdown records, identities, immutable sources, SQLite projections, transactions, recovery |
| [Canonical record schemas](record-schemas.md) | Flat frontmatter fields, states, predicates, qualifiers, and evidence quoting |
| [Retrieval and extraction](retrieval.md) | Text/vector/graph search, input rendering, extraction packets, entity resolution, evidence |
| [Providers and jobs](providers-jobs.md) | HTTP, trusted credentials, remote extraction, resumable work, budgets |
| [CLI and skills](cli-and-skills.md) | Command contract, structured output, errors, human workflow, agent integration |
| [Implementation handoff](implementation-handoff.md) | Module boundaries, dependency order, acceptance tests, unresolved choices |

## Requirements held fixed

- One Rust executable; no required Python, Node, database server, local model, or resident daemon.
- Durable knowledge in Obsidian-compatible Markdown where practical: sources, pages, entities, assertions, evidence, extraction results, and decisions.
- SQLite is a projection and cache. Markdown recovery cannot recreate lost vectors, deleted metadata, secrets, or unrecorded remote charges by magic.
- A LightRAG-inspired extracted graph is included in the first agent-usable release. Semantic graph search and direct API extraction follow.
- Remote embeddings accept a full URL, model, static key or dynamic credential executable, and bounded request options. Extraction uses a separate generation capability.
- Document IDs and source revisions carry identity; headings remain editable labels. Whole short-note inputs precede optional derived segmentation.
- Ordinary local operations, offline mode, and dry-run do not invoke remote services or credential helpers.

## Shared contracts

Storage owns `RecordRef`, `SourceSpanRef`, `EvidenceRef`, `CitationRef`, `ReadSnapshot`, authoring states, and derived eligibility. Retrieval returns internal hits and evidence references; the CLI owns their public JSON envelope. Providers own authenticated requests and observed usage; jobs own reservations and resumable state. No caller bypasses the provider dispatcher for CLI-owned network operations.

Source evidence uses zero-based, half-open UTF-8 byte spans against immutable normalized bytes, with named BLAKE3 hashes. Path links help humans navigate; IDs plus kind checks identify records. A graph path is not proof of an unstated new assertion.

The technical documents take precedence over exploratory implementation alternatives in the earlier research and architecture. They preserve the confirmed product requirements; remaining naming/provider/platform choices are listed in the handoff. These are design decisions with acceptance gates. Performance defaults and provider compatibility remain unmeasured until implementation tests and a representative corpus exist.

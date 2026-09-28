# Prompts for independent research

These are optional. Current planning does not depend on another agent completing them. Use separate agents for independent conclusions; the most valuable addition is research that challenges our assumptions. Return Markdown with ordinary source URLs so results can be imported and checked later.

Include this scope update with any prompt: the product includes a LightRAG-inspired entity/relationship graph; entities, assertions, evidence, decisions, and recorded research should be recoverable from Obsidian-compatible Markdown with flat frontmatter. SQLite is derived where practical; vectors, private credentials, and transient execution state may remain separate. Headings are editable labels, not identities. Start with whole short-note embeddings, using index-only segments for long inputs or measured retrieval needs; no visible chunk files are required.

## Prompt 1 — research economics and stopping rules

```text
Research a cost-efficient automated research workflow for a proposed Rust CLI that manages a local LLM wiki. Documents and captured sources are human-readable files; SQLite stores rebuildable indexes. Existing coding agents can use the CLI, and a later CLI-owned runner may call search and generation APIs. Semantic embeddings use a configurable remote /v1/embeddings-compatible endpoint; no local inference models and no required database servers.

Use current primary project documentation, source code, and research papers. Inspect representative deep-research systems and incremental knowledge-maintenance systems. Date your findings. Distinguish measurements, author claims, and your recommendations. Do not assume Markdown compilation is cheaper than retrieval.

Design two workflows: an external coding agent using the CLI, and a standalone bounded runner. For each, explain what budgets the CLI can truly enforce. Cover search/fetch/model costs, duplicate sources, context growth, model routing, cache reads/writes, source refreshes, checkpoint/resume, uncertain billing after timeouts, and stopping when evidence stops improving. Propose a durable run/evidence format and an evaluation that measures cost per supported finding plus correction effort.

Deliver: an executive recommendation; comparison of 4–6 systems; a small cost model with explicitly hypothetical or cited inputs; enforceable budget/resume semantics; failure cases; and what you would omit from v1. Cite primary URLs near claims. Identify findings that would overturn agent-driven-first or compile-once assumptions. Do not build or execute the product.
```

## Prompt 2 — adversarial review of file and index consistency

```text
Independently review a proposed local Rust LLM wiki architecture: canonical Markdown pages, immutable captured source revisions, readable provenance/research records, and disposable bundled SQLite FTS5/link/vector indexes. Humans and Codex/Claude Code/Cursor may edit the same files. There is no database server or required daemon. Remote embedding APIs generate vectors, which are stored locally.

Research current primary implementations and documentation for safe note/wiki mutations, SQLite concurrency, cross-platform atomic replacement, stable IDs, citation spans, and incremental index invalidation. The design uses expected-content hashes, an advisory vault lock, same-directory temporary files, and a journal for multi-file changes. Critique this design: files and SQLite cannot share a transaction, editors may bypass locks, and Windows/network filesystems have different behavior.

Deliver a concrete crash/recovery state machine, ownership rules for durable versus rebuildable state, source-update/retraction semantics, and a fault-injection test matrix. Cover rename/copy/duplicate-ID handling, symlinks/path escape, stale query snapshots, withdrawn evidence, changed embedding spaces, and what an index rebuild can and cannot recover. Separate v1 necessities from later features. Give primary URLs, date/version observations, and unresolved assumptions. Avoid broad enterprise requirements unless necessary for a single-user local vault.
```

## Prompt 3 — embedding gateway interoperability

```text
Research and challenge an implementation plan for a Rust CLI that performs semantic search using remote /v1/embeddings-compatible HTTP APIs and stores vectors locally. No local embedding models. Mandatory options: full request URL (not only a base URL), model, either a static key or an executable credential helper for dynamic keys. The offline lexical/graph core must never invoke credentials or make network calls.

Use current official API docs and source from representative providers/gateways. Identify the actual portable request/response subset and where compatibility breaks. Cover custom auth header/prefix, expiry/401 refresh, helper output schema, environment/file secrets, endpoint paths/query strings, dimensions, float/base64 output, array indices, query/document task options, token limits, batching, rate limits, retries, TLS/proxy/CA requirements, and missing usage accounting.

Propose a minimal TOML schema and credential-command protocol. Explain embedding-space identity, model aliases changing silently, query-vector caching, resumable bulk embedding, exact vector search versus ANN, and cost ceilings with unknown usage/prices. Specify mock-server tests and which provider-specific options to defer. Assess static linking and native dependencies only from pinned/current Rust manifests. Cite direct primary URLs and distinguish documented guarantees from assumptions. Produce a design review, not an implementation.
```

## Returning results

Keep the report's date, model/tool name if known, ordinary URLs, and limitations. A Markdown file in this repository or text pasted into the conversation is enough for later review. Include original references rather than opaque citation IDs. We will preserve the report as an input and verify material claims before promoting them into architecture decisions.

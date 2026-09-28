# Local LLM wiki landscape

Research date and source access: **2026-09-28**. This review inspected primary project READMEs, format/operation documentation, license files, and a recent paper. Features below are **documented capabilities**, not results of installing or benchmarking these products. Recommendations are our design judgments. References to `main` describe the inspected state and should be pinned to commits before implementation reuse.

The strongest direction is a small, agent-neutral Rust core that owns durable knowledge and reliable mutations, with an existing coding agent doing synthesis. The projects below demonstrate useful pieces of that design. None of the reviewed implementations combines all the requested constraints: a Rust CLI, ordinary readable documents, lexical/graph/optional vector retrieval, and accountable research workflows without installing a separate application stack.

| Reference | Most useful comparison for this project | Adoption fit |
|---|---|---|
| [Karpathy's LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f) | Persistent synthesis as a knowledge artifact | Conceptual pattern, not an executable product |
| [claude-obsidian](https://github.com/AgriciDaniel/claude-obsidian) | Ingestion, provenance, maintenance, research orchestration | Closest workflow reference; Python core |
| [QMD](https://github.com/tobi/qmd) | Local layered retrieval and bounded agent output | Strong benchmark candidate; Node/Bun runtime |
| [Basic Memory](https://github.com/basicmachines-co/basic-memory) | Shared human/agent Markdown and typed graph | Broad existing product; Python runtime |
| [zk](https://github.com/zk-org/zk) | Unix CLI ergonomics and deterministic note graph | Useful adjacent tool; implemented in Go |

## 1. What the LLM Wiki pattern contributes

Karpathy's gist describes an intermediate artifact between raw sources and the user: an LLM incrementally maintains interlinked Markdown, including entities, topic synthesis, contradictions, and cross-references. It explicitly presents an idea for coding agents to adapt, and describes Obsidian as a browsing environment. Its value proposition is accumulating synthesis at ingestion rather than repeatedly reconstructing it at query time. This is a design proposal, not an empirical proof of quality or cost superiority. [Original gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)

**Our interpretation:** wiki compilation and retrieval are independent choices. A compiled page can itself be retrieved by exact search, BM25, graph traversal, or embeddings. We should keep both raw evidence and authored synthesis searchable, and mark which kind a result represents. A concept page may omit details, become stale, or contain an incorrect interpretation; it must never be the only surviving evidence. Human-authored notes also remain first-class documents rather than being automatically regenerated.

## 2. claude-obsidian: closest workflow reference

The inspected version is considerably more substantial than a collection of prompts. Its README documents a Python 3.11 portable CLI, 15 skills, content-addressed local source capture, deterministic BM25, provenance ledgers, vault maintenance, and recoverable mutations. It documents portable skill setup for Codex and workspace discovery for Cursor, alongside its Claude Code integration. Obsidian is optional for using the Markdown. [Project README](https://github.com/AgriciDaniel/claude-obsidian)

Capability boundaries matter: URL/YouTube and OCR processing require configured external runners; PDF/EPUB support records metadata and hashes without built-in semantic extraction. Native Windows writes require WSL. Consequently, the README's broad product description should not be read as an implemented universal document-ingestion pipeline. [Documented capability and platform boundaries](https://github.com/AgriciDaniel/claude-obsidian#honest-capability-boundaries)

Its transaction contract lets workers produce drafts while an orchestrator performs one recoverable application. Expected content hashes prevent silently replacing changed files; bundles couple related writes, and inspection binds the plan to a selected vault. Querying is read-only; persistence is a separate operation. The contract also documents interrupted-operation recovery and an explicit Git checkpoint command. These are useful concrete answers to concurrent agents changing the same knowledge base. [Operation contract](https://github.com/AgriciDaniel/claude-obsidian/blob/main/skills/wiki/references/operation-transactions.md)

Its provenance contract distinguishes ingestion bookkeeping, source evidence, and claim assessment. Source records include identity, hash, freshness, and authority; claim records carry support, contradiction, and review state. Shared independence keys prevent duplicated sources from counting as independent corroboration. Unsupported and contested claims remain represented rather than being silently resolved. [Provenance contract](https://github.com/AgriciDaniel/claude-obsidian/blob/main/skills/wiki/references/provenance.md)

The research skill specifies bounded rounds, searches/fetches, elapsed time, and drafted pages; it checks existing knowledge before searching gaps and stops when evidence repeats or the budget ends. Research dossiers and changes to canonical pages are separate operations. These are documented workflow instructions, not evidence that every constraint is enforced by software. [Research workflow](https://github.com/AgriciDaniel/claude-obsidian/blob/main/skills/autoresearch/SKILL.md)

**Our recommendation:** borrow the separation of evidence, drafts, and durable pages, plus conflict detection and recoverability. Put critical invariants in Rust rather than relying on an agent remembering skill instructions. Make routine authorized updates simple; do not copy another product's approval ceremony indiscriminately. Our CLI should enforce resource limits where it performs work and distinguish those limits from instructions given to an external agent.

## 3. QMD: retrieval reference and evaluation baseline

QMD documents BM25 through SQLite FTS5, vector retrieval, local query expansion, reciprocal rank fusion, and local reranking via `node-llama-cpp`. Its CLI separates keyword, semantic, and combined querying. Agent-oriented JSON/file output, document identifiers, bounded line reads, collection context, and optional MCP all help an agent retrieve selected evidence rather than consume an entire vault. [QMD README](https://github.com/tobi/qmd)

The local model pipeline has installation and resource costs: the README lists three auto-downloaded GGUF models of approximately 300 MB, 640 MB, and 1.1 GB, plus a Node/Bun environment and a macOS SQLite requirement. Switching embedding models requires re-embedding. The documentation contains differing candidate-count descriptions in its API and architecture sections, so exact defaults require code/release verification. [Requirements and architecture](https://github.com/tobi/qmd#requirements)

**Our recommendation:** use QMD as a retrieval baseline on the same held-out questions, not as a runtime dependency. Preserve its useful distinction between a cheap lexical request and an expensive enriched request. The user has selected a remote `/v1/embeddings`-compatible API for semantic search, so QMD's local inference stack is outside our scope. Lexical and graph operations should remain usable without credentials or network access.

## 4. Basic Memory: shared files and a typed graph

Basic Memory documents Markdown shared by humans and agents, a local SQLite index without a required server, and MCP integration across Claude, Codex, and Cursor. Current features include hybrid full-text/vector search with FastEmbed and optional cross-encoder reranking. Local installation requires Python 3.12+. Its current README also documents automatic updates for supported install methods and promotional telemetry with an opt-out, relevant when defining our own offline behavior. [Basic Memory README](https://github.com/basicmachines-co/basic-memory)

Its format is a useful counterexample to assuming a graph needs a graph database. Notes contain YAML metadata, categorized observation bullets, and typed relations expressed with wikilinks. Observations are individually indexed; a stable permalink can survive a file move. Ambiguous link aliases remain unresolved, and forgiving link lookup is explicitly separate from write identity. Context construction can follow outgoing and incoming relations. [Knowledge format](https://docs.basicmemory.com/concepts/knowledge-format)

**Our recommendation, incorporating the user's graph preference:** use stable document IDs, ordinary Markdown links, and extracted assertions recorded as linked Markdown notes. Avoid requiring every paragraph to become a claim. Preserve unresolved links as useful intent while reporting ambiguity. Extracted edges need provenance and must remain distinguishable from authored navigation links. [The format proposal](../wiki-format.md) specifies how to recover this state without relying on SQLite as its sole store.

## 5. zk: what a good note CLI already solves

`zk` is a Go command-line personal wiki/Zettelkasten tool with prebuilt binaries, templates, editor/LSP integration, Markdown and wikilink support, and frontmatter. Its interactive picker uses `fzf`, while the core remains a command-oriented note assistant. It is not Rust; it is included for behavior and scope, not code reuse. [Project README](https://github.com/zk-org/zk)

Its search documentation separates full-text, literal, and regular-expression matching. Graph filters include backlinks, outgoing links, recursive traversal with a distance bound, orphans, broken links, and related notes sharing neighbors. These capabilities show how much useful graph navigation is available without model-generated entity extraction. [Filtering and graph operations](https://zk-org.github.io/zk/notes/note-filtering.html)

Automation includes initial note content from stdin and composition with other programs. [Automation guide](https://zk-org.github.io/zk/tips/automation.html) **Our recommendation:** prioritize pipes, explicit scopes, predictable exit statuses, and bounded output before building a full-screen interface. A human can use their editor; an agent can use the same operations through structured output.

## 6. Evidence for compiled wikis remains preliminary

A September 2026 preprint compares a compiled course wiki with vector retrieval on 59 questions and reports stronger wiki grounding, especially across pages. Its actual experiment is small: 12 wiki pages and 21 vectors. Critically, the wiki arm receives fixed sections covering the questions, so it does not face a context-selection failure; the retrieval arm selects five chunks. The study uses one answer/judge run per question, lacks a hybrid arm, and did not measure cost or latency. It is evidence worth following, not a fair demonstration that wiki navigation outperforms tuned hybrid retrieval at scale. [Paper, especially limitations and conclusion](https://arxiv.org/html/2609.25303v1)

**Our evaluation requirement:** compare identical source snapshots under equal evidence budgets. Measure retrieval recall separately from answer correctness, citation support, update propagation, and lifecycle cost. Include source deletion, a reversed claim, an ambiguous entity, a renamed page, and a question that the corpus cannot answer. Successful demonstrations alone will not reveal whether compilation quietly preserves obsolete beliefs.

## 7. Build versus adopt

The inspected license files identify [claude-obsidian as MIT](https://github.com/AgriciDaniel/claude-obsidian/blob/main/LICENSE), [QMD as MIT](https://github.com/tobi/qmd/blob/main/LICENSE), [Basic Memory as AGPL-3.0](https://github.com/basicmachines-co/basic-memory/blob/main/LICENSE), and [zk as GPL-3.0](https://github.com/zk-org/zk/blob/main/LICENSE). This records project licenses, not a dependency-level compliance review. Any copied code or bundled model needs its own review against the project's chosen distribution license.

Our proposed boundary is deliberately narrower than these complete products:

1. **Rust owns deterministic operations:** vault discovery, IDs, Markdown parsing, source capture, hashing, index refresh, graph queries, lint, evidence bundles, conflict detection, and recoverable writes.
2. **The agent owns interpretation initially:** reading sources, asking follow-up research questions, drafting synthesis, explaining contradictions, and deciding whether additional evidence is useful. This lets users apply the tool through their existing coding agent.
3. **The skill teaches a compact workflow:** discover capabilities, search before researching, retrieve bounded evidence, capture sources, draft changes, validate, apply, and report changed paths and unresolved gaps. Tool-specific adapters should stay thin.
4. **Markdown records preserve expensive work:** original evidence, normalized text, curated pages, entities/assertions, decisions, source manifests, and research records survive index deletion. SQLite lexical and graph projections can be rebuilt without paying for synthesis again; lost vectors may require separate re-embedding.
5. **Research gains a measurable stopping rule:** save completed questions, consumed budget, already-seen URLs/hashes, contradictions, and remaining gaps. Resume from these records rather than replaying an entire investigation.
6. **Vectors use the selected remote embedding interface:** configure the full endpoint URL, model, and either a static key or a credential script. Cache embeddings by content and model identity, keep the vector index local, and evaluate semantic/hybrid gains on labeled questions. Local model hosting is outside scope.

This gives the project a clear reason to exist: one portable operational core for readable knowledge, with the same dependable commands serving humans and multiple agent hosts. It also leaves useful integration options open without making Obsidian, MCP, a cloud account, or another product installation prerequisites.

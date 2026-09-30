# Implementation plan

Status: historical proposal, 2026-09-28. The [current contract and coverage map](current-contracts.md) and [cleanup register](execution/CLEANUP.md) describe implemented behavior and remaining gates. References below to a future CLI, standalone research executor or unshipped skill are preserved as original planning, not current capability claims. Research now uses host-agent handoffs; the maintained skill ships with the binary.

## Product choices

Confirmed: Rust CLI; human and agent interfaces; a usage skill; cost-efficient research; multiple retrieval modes including an extracted entity/relationship graph; best-effort durable state in Obsidian-compatible Markdown; no separately installed database stack; remote embeddings with full URL, model, and static or dynamic credentials; no local model runtime.

Recommended: bundled SQLite/FTS5; the versioned [Markdown format](wiki-format.md); page links, provenance, and an [extracted entity/relationship graph](knowledge-graph.md); whole short-note embeddings with derived segmentation when needed; agent-driven research before a standalone runner; one CLI/library package with focused modules initially; `lwiki` as a temporary executable name.

Still open: first research-generation/search provider, initial release platforms, representative documents and questions, import formats beyond Markdown/text, and project license/name. These do not prevent building the offline foundation. Assume macOS/Linux development first and verify Windows semantics before claiming Windows write support.

## Milestones and gates

| Milestone | Deliverable | Acceptance gate |
|---|---|---|
| **M0 — contracts and risks** | Markdown envelope, page/entity/assertion/evidence/source identity, predicate/qualifier vocabulary, command/error envelopes, cache ownership, release targets | Fixture knowledge survives index deletion without model calls; heading edits preserve IDs; Obsidian compatibility assumptions listed; FTS5 builds bundled; recovery reviewed |
| **M1 — useful offline core** | `init`, source capture, `read`, `page put`, `index sync`, document-level literal/lexical `search`, links/backlinks, canonical Markdown graph fixtures/queries, `context`, `check`, `doctor`, schemas | Human and JSON workflows work offline; graph rebuild preserves identities/evidence; headings/paths change safely or produce actionable errors; conflict/recovery tests pass |
| **M2 — agent extraction and research** | Bounded extraction packets, graph import/validation, conservative entity resolution, Markdown extraction/decision/run records, changesets, portable usage skill | Hosts extract and apply supported relationships; homonyms remain separate; source withdrawal propagates; Markdown-only knowledge recovery and Obsidian note navigation verified; external costs labeled accurately |
| **M3 — API extraction and semantic graph search** | Direct generation adapter for graph extraction; embedding client with static/command auth; resumable sync; whole-note/derived-unit exact vectors; entity/relationship/chunk fusion | Mock API suites pass; unchanged extraction/embedding inputs reused; no mixed spaces; offline graph remains usable; graph-assisted retrieval and whole-note/section choices evaluated |
| **M4 — standalone bounded research** | Bounded HTTP fetch and HTML/text normalization with original-byte provenance; one search adapter and one generation adapter; run/resume/status/report and budget accounting | Stop/resume reuses completed work; outstanding/retry costs reserved; partial findings saved; citations checked; requested apply works |
| **M5 — evidence-driven extensions** | ANN, reranking, community summaries, advanced entity resolution, additional imports or MCP | Each addition independently improves an agreed quality/latency/cost metric enough to justify its maintenance |

M0–M2 form the first agent-usable release, including the extracted graph populated through host-agent output. M3 adds direct API extraction and semantic graph retrieval; M4 reuses its generation/budget machinery for standalone research. Embeddings are not a prerequisite for graph storage/traversal or agent-driven research; the extraction model is separately configured from the embedding model.

## First coding slice

Create a Cargo package with a reusable library and a thin `clap` binary, following the module ownership in the [implementation handoff](technical/implementation-handoff.md). Add mutation/recovery support with the first write operations; avoid a large crate workspace before module boundaries are proven.

Implement one end-to-end scenario:

```text
initialize a vault
 -> capture two Markdown sources
 -> add two linked, cited pages
 -> add a prepared entity/assertion/evidence fixture in Markdown
 -> index
 -> search and assemble evidence
 -> edit a page externally
 -> rename a heading and verify identity and evidence behavior
 -> change the original input and capture a new source revision
 -> refresh and show the changed evidence
 -> delete the cache and rebuild
```

Keep a small fixture vault with tricky headings, duplicate titles, a broken link, Unicode filenames, a code block containing fake links, and a source revision change. Include two homonymous entities and a disputed assertion. Separately tamper with a captured snapshot and require an integrity error; delete required frontmatter and require a diagnostic without losing readable text. Only add commands needed for this scenario initially. Use fixed extraction fixtures before connecting generation providers.

## Targeted feasibility spikes

1. **Storage/distribution:** bundle SQLite/FTS5; inspect the release binary on clean target environments and measure startup/size. Pin dependency versions after this succeeds.
2. **Recovery:** inject failures before/after each file replacement and database commit; exercise competing CLI writers and external edits. Document races that advisory locks cannot eliminate.
3. **Exact vectors:** implement the selected bounded Rust scan over SQLite-stored vectors, using expected dimensions. Measure cold/warm p95, RAM, filtering, deletes, and binary impact. Compare a pinned SQLite vector extension or ANN only when measurements identify a limitation.
4. **Embedding gateway:** mock full custom URLs, reordered indices, missing usage, dimensions, malformed vectors, timeouts, throttling, retry costs, and credential expiry. Validate no secrets appear in logs or errors.
5. **Markdown/graph recovery:** delete indexes, rebuild from notes, and compare supported assertions and decisions. Test heading/filename changes, quote ambiguity, broken IDs, Obsidian edits, and loss of one versus all supporting sources. Missing vectors may require a separate explicit re-embedding operation.
6. **Extraction and granularity:** test whole short notes and bounded long-document windows with the same extraction schema; compare whole-note versus section-based embeddings under equal context budgets. Measure supported relation precision, entity-resolution errors, retrieval quality, and repeat-work cost.

## Evaluation plan

Start with approximately 30–50 hand-labeled tasks covering literal terms, paraphrases, links, provenance, updates, and unanswerable questions; expand toward 100–300 representative queries. These counts are planning targets. Split tuning from held-out evaluation and label original source spans, not only desired answer text.

Compare literal, document-level FTS5, exact semantic, entity-focused graph, relationship-focused graph, and fused graph/text retrieval under equal context budgets. Test whole-note and optional section representations. Measure Recall@k, nDCG, complete multi-hop evidence, supported assertion precision, entity-resolution errors, citations, unsupported statements, latency, index size, API cost, and repair effort. Report cold/warm performance on named hardware with an explicit corpus revision. Evaluation tunes the included graph capability; its inclusion is already a user decision.

Initial correctness gates are strict within the documented concurrency model: cooperating writers serialize or conflict, detected external edits are preserved, recovery images exist, and the remaining external-editor race is disclosed. Also require no credential leakage, no active result using a withdrawn source as current support, no silently stale vector, and no budget over-dispatch under the declared accounting model. Retrieval targets should be set after baseline measurements; the supplied file/token thresholds are not release requirements.

For Rust changes, run formatting, linting, unit and targeted integration tests. Broaden checks for actual cross-platform and concurrency risk, not merely to increase test counts. Documentation-only research has been checked for links and consistency; no executable tests can run until the implementation exists.

## Concrete decisions for the next discussion

- Adopt the proposed durable-files/rebuildable-SQLite boundary.
- Confirm the first slice and agent-driven research priority, or move standalone research earlier.
- Choose the first actual embedding endpoint/provider to validate against after mocks. Keep the adapter provider-neutral regardless.
- Supply a small representative corpus when available; start with synthetic fixtures until then.

Independent review prompts are in [external-research-prompts.md](external-research-prompts.md). Those reviews can challenge storage safety, research economics, and embedding interoperability while implementation starts on the offline core.

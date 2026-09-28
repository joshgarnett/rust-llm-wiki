# Extracted entity and relationship graph

Status: proposed, 2026-09-28. **An extracted knowledge graph is a first-class planned capability**, alongside page links, provenance, and chunk search. This implements the user's LightRAG preference within a Rust executable and embedded SQLite, without a graph server. Source-access date: 2026-09-28.

## Relationship to LightRAG

LightRAG's paper describes chunk-level LLM extraction of entities and relationships, descriptions for retrieval, deduplication, and incremental insertion. These are the architectural ideas adopted here. [LightRAG paper](https://arxiv.org/html/2410.05779v3)

Current upstream retrieval separately selects entity and relationship candidates; its `mix` mode also retrieves chunks. Its default NetworkX implementation creates an undirected graph. Our directional, qualified assertions and durable resolution decisions are deliberate design differences; this proposal does not promise API compatibility, identical ranking, benchmark parity, or a line-by-line port. [LightRAG retrieval implementation](https://raw.githubusercontent.com/HKUDS/LightRAG/main/lightrag/operate.py), [NetworkX backend](https://raw.githubusercontent.com/HKUDS/LightRAG/main/lightrag/kg/networkx_impl.py)

## Durable records

Canonical Markdown notes, including accepted machine output and editorial decisions, survive index deletion. Follow [the vault format](wiki-format.md): YAML frontmatter for structured fields, readable Markdown for explanation, and real wikilinks for navigation. The vault's `WIKI.md` records its format and conventions.

```text
knowledge/entities/<id>.md         # identity/type/aliases, descriptions, citations
knowledge/assertions/<id>.md       # directional proposition, qualifiers, evidence
knowledge/evidence/<id>.md         # one assertion/source/span association
knowledge/extractions/<job-id>.md  # preserved output and mention-to-entity mapping
knowledge/decisions/<id>.md        # merge/split/reject decisions and rationale
```

Entity notes carry stable ID, type, aliases, and status; descriptions cite assertions or original evidence. A simplified assertion note illustrates the pattern; exact schema fields follow the vault specification:

```markdown
---
wiki_schema: "1"
wiki_id: a_01
wiki_kind: assertion
title: Ada works for Acme
wiki_subject: "[[knowledge/entities/entity_ada]]"
wiki_subject_id: entity_ada
wiki_predicate: works_for
wiki_object: "[[knowledge/entities/entity_acme]]"
wiki_object_id: entity_acme
wiki_valid_from: 2024-01-01
wiki_status: proposed
wiki_evidence:
  - "[[knowledge/evidence/evidence_01]]"
---
# Ada works for Acme
[[knowledge/entities/entity_ada|Ada]] works for
[[knowledge/entities/entity_acme|Acme]] from January 2024.

Evidence: [[knowledge/evidence/evidence_01]].
```

Evidence notes authoritatively associate `wiki_assertion_id` with `wiki_source_id`, `wiki_source_revision`, `wiki_stance`, `wiki_span_start`, `wiki_span_end`, and `wiki_quote_hash`; assertion/source wikilinks and a quoted excerpt make them browsable. Offsets/hash refer to immutable normalized source text. IDs survive heading changes; IDs and links must agree, with paths repairable by ID after renames. Conflicts are diagnosed, not silently resolved.

Objects may instead be typed literals. Preserve negation, modality, units, and time scope; two entities can have several assertions. Evidence supports or contradicts the proposition; qualifier differences need not be contradictions. Model confidence is an uncalibrated extraction signal, never truth probability.

Keep properties flat with native `aliases`/`tags` lists: Obsidian supports quoted wikilinks, but its property editor does not support nested structures. [Obsidian properties](https://obsidian.md/help/properties)

Assertion notes appear as **nodes** linked to entity/source notes in Obsidian; native graph lines represent note links, not our typed predicates. Body links provide readable navigation. The CLI reads assertion metadata to recover direction and qualifiers; it never infers a factual relationship merely from backlinks. [Obsidian graph view](https://obsidian.md/help/plugins/graph)

Extraction notes retain job/input/model fields and raw output in fenced JSON for audit, not canonical active graph state. Decision notes preserve resolution metadata and explanations. Source manifests use `source.md`, revision metadata uses `revisions/<hash>/revision.md`, and run events use `events/<seq>.md`. Original bytes may remain blobs; indexes, vector caches, and locks remain non-Markdown.

SQLite projects `entities`, `entity_aliases`, `assertions`, `assertion_evidence`, and `graph_fts`. Embeddings identify `target_kind` and `target_id` so entity, assertion, and chunk vectors remain distinguishable. SQL adjacency indexes support bounded traversal. Page links remain navigational edges; citations remain dependency edges; neither silently becomes a factual assertion.

Rebuilding from intact canonical notes and journals requires no paid calls. Missing IDs trigger diagnostics and best-effort cache/history repair; deleted identity cannot be recovered from Markdown alone. Deleted embedding caches require separately requested re-embedding, never automatic paid regeneration.

## Extraction and identity resolution

Initially, `graph extract --executor agent` emits a bounded work packet for the active host agent. It does **not** launch Codex, Claude Code, or Cursor. The packet contains source revision/spans, heading context, output schema, candidate limits, and extraction instructions. The host supplies generation; `graph import` stages and validates its output; ordinary changeset apply publishes accepted records. Validation checks IDs, schema, and exact cited spans, while entailment still requires evidence review.

Later, `graph extract --executor api` uses a generation provider under the central dispatcher. Embedding endpoints cannot extract assertions. Short sources can be processed whole; long sources need bounded extraction windows independently of embeddings. Retrieval units are derived index records, not canonical chunk files.

Prefer source-local extraction followed by a separate resolution step. Reuse an existing entity only through an unambiguous identity match or explicit decision. Homonymous names, overlapping aliases, and uncertain pronouns remain separate unresolved mentions. Show candidate matches with type, context, and citations. Never merge solely on spelling or embedding proximity. Preserve merge/split/reject decisions outside SQLite, with before/after IDs and rationale; redirects must not silently retarget contradictory assertions.

An extraction cache key hashes the **actual complete input** and extractor fingerprint: prompt/schema version, normalization/chunker version, provider/model or declared deployment revision, generation settings, and any supplied context. Candidate-registry context participates in the hash when included. Independently resolving preserved source-local output can avoid paying for extraction again when the registry changes. Cached output is a reusable proposal, not authorization to overwrite accepted records or resurrect rejected assertions.

## Retrieval strategies

Proposed commands separate graph strategy from seed selection:

```sh
lwiki graph query 'Ada' --strategy entity --seed lexical --json
lwiki graph query 'funds research' --strategy relationship --seed lexical --json
lwiki graph query 'research partnerships' --strategy combined --seed semantic --json
lwiki search 'research partnerships' --mode hybrid --graph entities --json
```

* **Entity:** seed names, aliases, or descriptions; return matching entities, qualified incident assertions, and supporting source spans.
* **Relationship:** seed predicates, qualifiers, or assertion descriptions; return relevant assertions, endpoints, and evidence.
* **Combined:** fuse entity and relationship candidates and apply bounded expansion. Hybrid search with `--graph entities` additionally combines these graph candidates with chunk retrieval.

Lexical seeding uses FTS5 and exact names/IDs; saved-graph traversal is offline. Semantic seeding obtains uncached query vectors remotely, then compares locally. M3 embeds entity/assertion representations and short notes whole; split long documents only for model limits or measured retrieval quality, not corpus count. Track representation fingerprints and coverage. Model-generated keyword expansion is optional, explicitly enabled and budgeted; lexical graph queries never secretly invoke an LLM.

Each result explains its seed, directed path, predicate and qualifiers, selected evidence spans, contradictory evidence, and ranking contributions. Rank fusion combines candidate rankings rather than raw BM25/cosine values. A path is a discovery route, not proof of a new inferred fact. Deduplicate shared evidence without counting mirrored sources as independent corroboration.

## Budgets, freshness, and withdrawal

Extraction limits bound source bytes/tokens, chunks, calls, output assertions, retries, and concurrency. API execution reserves cost before dispatch; host-agent spend remains reported or unknown, not enforceable by the CLI. Resume reuses completed packets. Retrieval separately caps seeds, depth, edges, returned passages, and total context tokens/bytes. Report skipped work, truncation, unresolved mentions, and incomplete coverage.

A refresh adds a source revision. Exact unchanged evidence can be revalidated; changed spans invalidate dependent assertions/descriptions and vectors until reviewed or recomputed. Accepted curated descriptions remain durable, but stale status blocks their use as current support. Query one published source/graph generation and verify relevant status/evidence/decision files as well as quoted snapshots. Report verification time; non-cooperating external edits after the check are outside that snapshot guarantee. Incomplete changesets must be recovered or explicitly reported before serving their affected current evidence.

Withdrawal removes that source's active support transitively. Assertions retain remaining valid evidence, with disputes visible; when **all supporting sources are withdrawn**, suppress the assertion and unsupported derived descriptions from current evidence, including cached contexts and semantic results. Historical access is explicit and labeled. Entity identities may remain for history without implying any active assertion. Purging content is a separate operation covering stored extraction excerpts and caches too.

## Delivery and acceptance

* **M0:** settle entity/assertion/evidence schemas, predicate direction and qualifier rules, lifecycle states, extraction envelopes, and durable decisions.
* **M1:** import a fixture graph, rebuild it from files, seed lexically, traverse, and explain paths with original source spans.
* **M2:** ship bounded agent extraction packets, import/validation, conservative resolution, and recoverable changeset apply with the usage skill.
* **M3:** add API extraction, remote embeddings, semantic graph seeds, and combined graph/chunk retrieval.

Acceptance fixtures must demonstrate homonyms remain separate; aliases resolve only when unambiguous; opposite directions and dated claims survive distinctly; unchanged extraction is reused; missing support is diagnosed; withdrawal of one versus all supporting sources behaves correctly; index rebuild preserves decisions; interrupted apply recovers; and each returned path resolves to matching source spans. Evaluate graph-assisted evidence recall, supported assertions, and cost against chunk retrieval under equal context budgets. Quality determines tuning and defaults, while the graph capability remains in scope. Predicate vocabulary, multilingual resolution, and extraction model selection remain explicit design gaps.

# Retrieval and extracted graph implementation design

Status: proposed implementation contract, 2026-09-28; no implementation or benchmark results are claimed. This refines [the retrieval research](../research/retrieval.md), [graph requirements](../knowledge-graph.md), and [document-first format](../wiki-format.md). It selects a Rust exact vector scan for M3; vector extensions, ANN, learned reranking, and community summaries require later evidence. The LightRAG influence is separate entity/assertion retrieval followed by original-evidence expansion, not upstream API or ranking parity.

## Boundaries and types

`search` owns candidates, fusion, and excerpts; `graph` owns extraction validation, resolution, and bounded typed traversal. Storage owns canonical identities, eligibility, transactions, and snapshots in [storage](storage.md) and [record schemas](record-schemas.md). [Provider dispatch](providers-jobs.md) owns network requests, credentials, retries, and spending. The [CLI](cli-and-skills.md) owns JSON envelopes, pagination, and error codes.

```rust
enum TargetKind { Document, Entity, Assertion }
enum GraphStrategy { Entity, Relationship, Combined }
struct RetrievalUnit {
    unit_id: DerivedId, owner: DocumentLocator, target: TargetKind,
    source_span: Option<ByteSpan>, input_hash: Blake3Hash,
}
struct QueryPlan {
    mode: SearchMode, graph: Option<GraphStrategy>, filters: Filters,
    limits: RetrievalLimits, offline: bool,
}
fn search(snapshot: &ReadSnapshot, query: &str, plan: &QueryPlan)
    -> Result<HitSet>;
fn assemble(snapshot: &ReadSnapshot, hits: HitSet, budget: ContextBudget)
    -> Result<EvidenceBundle>;
```

Use shared `RecordRef`, `CitationRef`, `Eligibility`, and `DocumentLocator`; paths/headings are presentation. `CitationRef::Source(SourceSpanRef)` cites captured text without invented assertions; `CitationRef::Assertion(EvidenceRef)` additionally binds a proposition. Unadopted/invalid notes retain lexical locators without canonical IDs. Search defaults to lexical. `index sync` never extracts/embeds; semantic/hybrid modes explicitly embed uncached queries. Offline execution permits matching cached vectors and never credential helpers. An unavailable semantic leg requires an error or an explicitly selected, disclosed lexical fallback.

Follow storage's per-kind eligibility: reviewed dependency-free pages enter current context as verified note text; drafts remain discovery-only. Entity identity eligibility permits name/alias seeds independently of description eligibility. An unsupported description cannot rank as current description text or reuse its old vector.

## Lexical pipeline

Index one FTS row per document; index readable entity/assertion representations separately in `graph_fts`. Documents contain `title, aliases, headings, tags, body`. Exclude managed metadata syntax, extraction transcripts, changesets, and duplicate evidence-note quotations by default. Captured `content.md` is payload, never an envelope supplying vault identities. Preserve raw bytes for citations. Use `unicode61 remove_diacritics 2`, no stemming, and pin tokenizer/parser configuration in the catalog fingerprint.

Catalog text and excerpt mapping share one parser-offset stream for Markdown and ordinary HTML. Valid UTF-8 `.html`/`.htm` captures retain identical original and `content.md` bytes. Readable HTML runs omit tags, attributes, comments, script and style; quoted `>` does not end an attribute. Unclosed admitted regions remain omitted, and separators prevent invented words across removed markup. HTML headings contribute body text, without new heading metadata. Runs scanned from HTML events retain raw entity spellings; Markdown Text/Code events retain their existing decoding even beside inline tags. This is lexical text support, without browser rendering, CSS visibility, encoding conversion or malformed-document repair. Citations remain exact raw byte slices, including intervening markup. The parser fingerprint binds this projection; rebuilding a derived catalog does not rewrite immutable revisions or pending operations.

Plain lexical queries split on Unicode whitespace; double embedded `"`, quote each term, and join with `OR`. Bind the expression as an SQL parameter. `OR`, `foo:bar`, and `a*` cannot become operators, but quoted punctuation still follows tokenizer rules. Literal mode uses case-sensitive exact UTF-8 substring matching over allowed original text; no regex in v1. Normalized literal discovery, selected verified search and automatic indexed-document context use the same exact matcher and original byte spans. Filtering precedes candidate truncation; exact totals and capped decoding share one snapshot and cumulative query budget. Cached substring matching scans filtered raw text and may scan twice for exact counts; candidate limits do not bound scanned bytes. Reject blank/overlong queries; punctuation-only lexical input receives a literal-mode diagnostic. The term-count limit applies only to lexical queries. Advanced FTS syntax is deferred. FTS5 documents quoting and ranks better BM25 matches with lower scores. [SQLite FTS5](https://sqlite.org/fts5.html)

Constrain every FTS query to the read generation and apply kind/path/status filters before limits. Rank by `bm25(documents_fts, 8, 6, 3, 2, 1, 0, 0)` ascending, then canonical ID or locator path; final columns are unindexed generation/document keys. Exact ID lookup precedes FTS; exact title/alias lookup supplies a separate ranking without asserting identity. Derive excerpts only for selected documents using tokenizer/parser source maps; FTS snippets are not validated source spans. Publish both FTS rowsets atomically with the generation pointer. Store only the published generation in these virtual tables so retained history cannot distort BM25 statistics; existing readers retain their SQLite snapshot.

## Representations and segmentation

`render-v1` produces UTF-8 with LF separators and fixed field order:

| Target | Embedded fields, in order |
|---|---|
| Document | `Title: …`, `Headings: …` (ancestor labels), blank line, selected body text |
| Entity | `Entity: …`, `Type: …`, sorted declared aliases, eligible readable description |
| Assertion | resolved subject label/type, predicate, object label/type or typed literal, all qualifiers, eligible readable description |

Header scalars use JSON escaping; alias arrays sort bytewise after deduplication, retaining spelling. Assertion labels are `Subject`, `Predicate`, `Object`, `Negated`, `Modality`, `ValidFrom`, `ValidUntil`, `Property`, `Unit`, `Description`, in that order. Endpoints render as JSON `{label,type}`, literals as `{type,value}`, with that key order. Always render negation/modality defaults; omit absent optional fields. Body bytes preserve selected Markdown, removing only managed-note frontmatter; synthetic separators use LF. Exclude bookkeeping IDs and duplicated evidence quotations. Endpoint renames invalidate dependent assertion representations. Descriptions with invalid dependencies cannot seed current evidence.

`auto` keeps a short note whole. The optional quality target is unset initially. Split only when formatted input exceeds a configured quality target or provider input bound, counting all prefixes/labels. Prefer headings, then paragraphs, then deterministic UTF-8-safe bounded slices; use no overlap initially. Preserve a mapping from every body slice to its original bytes; synthetic headers are not evidence. An unsplittable oversized header is an error, never silent truncation. Extraction windows are independently bounded. No chunk files are created.

`unit_id = H(owner identity or locator, source hash, span, segmenter version)` is disposable. Heading labels never establish identity. Cache reuse instead uses `input_hash = H(actual complete request input bytes)` and `SpaceId`. Prepend the configured document prefix verbatim; query rendering is the query prefix followed verbatim by query bytes. `SpaceId` hashes sanitized endpoint/profile identity, requested model and explicit deployment revision, dimension, metric, query/document role options and prefixes, normalization policy, and render version. A renderer change creates a new space; changed text changes only that input. Never join vectors across spaces, even with equal dimensions. Provider aliases cannot prove model stability; deployment revision is operator managed.

## Exact semantic retrieval

Store validated, normalized little-endian float32 blobs in SQLite, keyed by space/input hash, with generation-specific target/unit memberships. Unconfigured dimension hashes as `auto`; the first valid corpus response fixes actual dimension. Validate dimensions, finite elements, and nonzero norm on write/read. Normalize with float64 accumulation, store float32, and calculate cosine with float64 dot/norm accumulation over stored values. A corrupt blob is unavailable, not a zero score.

Stream eligible rows in bounded Rust batches; keep a top-k heap per target kind. Cost is O(Nd), memory O(batch × d + k); filter before scoring, with target ID/unit ID ties. Read only active-space memberships matching current inputs/dependencies. Replacement preparation retains the old active space for unchanged inputs, with partial coverage reported. Its query embeddings must use its retained nonsecret service/model/render configuration and current trust; if unreproducible, return semantic unavailable. Publish replacement only after coverage checks. Never blend spaces or compare a new-model query to old vectors.

## Graph candidates, fusion, and budgets

`graph query --strategy entity|relationship|combined --seed lexical|semantic` selects seed strategy independently of traversal. Indexed `graph_fts` columns are `name, aliases, endpoints, predicate, qualifiers, description`: entity BM25 weights are `8/6/0/0/0/1`; assertion weights are `0/0/4/6/2/1`, each filtered by target kind. Unindexed routing columns receive zero weights. Semantic seeds use corresponding target vectors. Combined strategy fuses both. `search --mode hybrid --graph entities` additionally retrieves documents. Lexical graph queries make zero model calls; optional generated query expansion is deferred.

Entity seeds expand to incident directed assertions; relationship seeds expand to endpoints and evidence. Traverse only explicit typed assertions and preserve their direction, qualifiers, and status. Never infer facts from backlinks, co-occurrence, an inverse edge, or transitive paths. Page-link expansion, when requested, is a separately labeled navigation reason. Cycles use visited record/edge sets.

Normalized catalogs currently implement [exact named Entity neighbors](../named-neighbors.md)
through indexed endpoint-role streams, bounded before retained record hydration.
Default lookup authenticates displayed records and their complete selected
evidence/opposition/policy closure, constructs values from that proof, then
rechecks selected bytes and operation authority. Metadata uses `indexed_evidence`
with domain `selected_graph_neighbors` and global membership unverified.
`--no-sync` returns uncited `index_snapshot` adjacency; explicit `--verify-selected`
authenticates selected dependencies even with that flag. Assertion filters precede
candidate caps; a Source filter requires Current supporting Evidence and scopes
displayed evidence. Display caps never truncate proof membership. Stable traversal
order is hop then Assertion ID; sentinel omission counts are lower bounds, marked
in coverage. Unsupported normalized general-query, semantic, historical/proposed,
navigation and cursor modes refuse explicitly. The independent workflow/access
acceptance gates remain pending.

All values below are **proposed tuning defaults**, not measured quality or provider limits:

| Control | Default |
|---|---:|
| Query UTF-8 bytes / lexical whitespace terms | 16,384 / 256 |
| Candidates per lexical/dense list | 80 |
| RRF constant / list weight | 60 / 1 |
| Graph seeds total / traversal depth | 12 / 1; hard depth cap 2 |
| Incident assertions per seed / total visited assertions | 16 / 128 |

| Search hits / context passages per document | 10 / 4 for source-aware document context; 2 for literal/legacy assembly |
| Context total / hybrid graph share | 3,000 estimated tokens and 12,000 bytes / at most 50% |
| Evidence passages per assertion | 2 support + 1 contradiction |

The final rendered context defaults to 12,000 bytes / 3,000 estimated tokens. Callers can explicitly raise both limits to 16,384 bytes / 4,096 estimated tokens. Instruction and output reservations share these limits; the first exhausted budget wins. Counts use UTF-8 bytes divided by four, rounded up, rather than a model tokenizer. These bounds cover rendered context, not the complete JSON response or an intermediate host-selection task. The historical evaluation profile uses 6,000 bytes / 1,500 estimated tokens; increasing a limit does not establish an improvement in completeness.

Query limits are application bounds, with [comparison and measurement rationale](../query-limits.md). Every accepted lexical whitespace phrase participates in the quoted OR expression; excess input returns `USAGE`, without truncation. The whitespace-term ceiling applies to lexical search (including the lexical branch of hybrid search), not literal or semantic-only input. Embedding spaces independently enforce `max_input_bytes` on the complete query plus prefix (default 12,000 bytes); exceeding that bound returns `BUDGET_EXCEEDED` before dispatch. Passage selection has its own 256 distinct tokenized-term work limit and discloses any omission: punctuation can turn one whitespace phrase into several tokens.


Use `rrf(x) = Σ weight/(60 + one_based_rank(x))`; absent candidates contribute zero. This combines ranks rather than BM25/cosine scales. [Original RRF paper](https://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf) Before fusion, collapse document units by owner using their best rank, keeping the best two passages; multiple units do not manufacture multiple votes. Exact lookup, lexical, and dense lists retain named contributions.

### Source-aware document context

Nonliteral document context uses the query and authenticated, eligible source owners to propose additional passages from their pinned original bytes. Discovery ranks remain unchanged. This separates finding a source from finding all useful passages within that source; it does not assert that query overlap proves a complete answer.

With complete cached-unit coverage, semantic and hybrid context retain exact eligible embedding-unit locations for each discovered owner. Hybrid discovery still combines lexical and dense owner candidates. Evidence selection orders retained units by dense cosine, with conventional unit-body BM25 only for equal-cosine ties, followed by deterministic path/span order. Diagnostic dense/BM25 ranks remain visible separately from the final selection rank; they do not add duplicate selection votes. Missing/corrupt vectors are unavailable rather than zero similarity. Incomplete owner/unit coverage falls back to bounded lexical passage selection for the whole retrieved owner set and emits a warning. No new embedding request is made by passage selection.

Each unit maps to one canonical structural parent: bounded paragraphs, lists with introductions, and code/explanation groups. Identical parents retain their strongest child without extra votes, before the global candidate cap. Packing attempts the parent first and its associated exact child if the parent does not fit. A unit larger than the requested excerpt uses a bounded query-focused child and discloses clipping. Managed-note metadata is excluded; captured content remains payload. Structural, lexical-unit and coarse-focus scans are separately bounded to1MiB per owner/4MiB total each; structural starts and units are capped at4096, distinct tokenized query terms at256. Cached-vector reads reserve at most64MiB. Limits and omitted regions are disclosed.

Lexical-only selection uses SQLite tokenization, conservative English suffix normalization and grammatical-word exclusion while preserving negation, restrictions and marked identifiers. It proposes at most32 windows per owner under bounded source scans, including two discovery anchors, and packs by owner rank, local relevance, query-term coverage and actual rendered cost. These heuristics affect neither FTS indexing nor embedding inputs.

Every admission checks the actual rendered byte/token budget, including metadata and reservations. Source-aware context allows at most four passages per owner, each within the requested excerpt bound; overlapping merges must respect that bound too. Literal mode and graph evidence selection retain their existing contracts. Retrieval order and a fitting structural parent do not establish answer completeness.

Overlapping direct-source quotes consolidate citations to one exact merged span/hash per source revision. Mirrored revisions retain separate references. Assertion-bound evidence and contributing stances are preserved. The final coordinator independently verifies exact bytes, current/historical eligibility and citation dependencies before sealing output. Snapshot-only context remains explicitly unverified and citation-free. The [evaluation protocol](../evaluating-context.md) measures source hits and evidence coverage separately from independently assessed answer completeness.

Rank incident assertions by direct assertion-seed rank first, then parent-seed rank, then assertion ID; degree is never a relevance boost. Round-robin expansion across seeds prevents hubs consuming the cap. Expanded-only assertions inherit the best parent rank, with hop count and ID as ties; keep them distinct from direct semantic/lexical scores. Form graph evidence bundles after graph candidate fusion, then fuse their source-document ranks with direct document ranks. Deduplicate identical evidence by revision/span/hash and overlapping spans within a source revision, preserving every contributing assertion and stance. Mirrored content has one support group and is never counted as independent corroboration merely because it has two paths.

Pack verified context greedily in ranked order within both byte and token budgets. Graph-only queries can use the full context budget. Count exact tokens only with the configured tokenizer; otherwise label estimates and enforce bytes. Reserve caller-specified instruction/output space before retrieval. Graph bundles must include original support; if the required support/contradiction cannot fit, omit that bundle and record why. Evidence caps expose omitted support/contradiction counts; they never imply exhaustiveness. Navigation expansion consumes the same budgets.

`SearchHit`/`EvidenceBundle` carry locators, eligibility, rank contributions, seed/directed paths, predicates/qualifiers, `CitationRef`s, excerpts, generation, dependency fingerprint, and verification time. Final ties use kind/record ID/unit ID; cursors bind query/filters/generation. Plain notes are labeled note text. Return omissions/coverage/truncation; unit IDs alone are never factual citations. Before emission, storage checks the control manifest and dependency bytes, detecting new decisions/duplicate IDs and changed files. Mismatch requires refresh/retry or explicit unverified output. Edits after verification remain outside the snapshot guarantee.

## Bounded extraction and import

`graph extract --executor agent` emits a packet for the current host; it launches no agent. API execution uses `Dispatcher::execute(DispatchContext, RemoteRequest::Generate)` with separated instructions/data and `SchemaRef("lwiki.extraction.v1")`. Embeddings use `RemoteRequest::Embed` with rendered `EmbeddingInput`s; no caller bypasses the dispatcher. Packet schema `lwiki.extraction-packet.v1` contains packet ID/fingerprint, source/revision/hash, window IDs/ranges/text, heading context, registry, output schema, limits, and instructions treating sources as untrusted data. Candidate identities are context, not authority. Hash canonical task fields excluding self-identifying fields to obtain the packet fingerprint; derive its packet ID deterministically so repeated tasks remain cacheable.

Wire field names are `schema`, `packet_id`, `packet_fingerprint`, `source_id`, `source_revision`, `snapshot_hash`, `windows`, `registry_version`, `output_schema`, `limits`, `instructions`, and optional `candidate_context`. A window contains `id`, `span: {start,end}`, `text`, and `headings: string[]`; the text must equal its snapshot slice. Limits contain `max_mentions`, `max_assertions`, and `max_output_bytes`. Fingerprints use canonical JSON: sorted object keys, UTF-8, no insignificant whitespace, preserved array order, integers only where specified.

Before stdout/dispatch, persist the immutable packet at `knowledge/extractions/packets/<packet-id>.md`, kind `extraction_packet`, with full fenced JSON. `packet_id = packet_<64-hex-fingerprint>` is the deterministic-ID exception. Completed extraction notes retain packet ID, response, and durable ID mappings. Re-export/re-import checks canonical extractions and prepared changeset `origin` keys, reusing matching payloads/IDs. Conflicting output fails by default; `graph import --new-extraction` uses packet-plus-response hash identity and preserves prior decisions. Pending packets survive cache deletion; unknown packet import fails.

Proposed packet limits: 16 windows, 12,000 source bytes per window, 64 mentions and 128 assertions total, 256 KiB response. Provider token/context limits can lower these ceilings. Unknown tokenizer counts remain estimates; byte/item caps do not imply hard token/dollar guarantees. Provider input-limit errors return for deterministic resegmentation; never truncate. Window splitting follows source boundaries; relationships crossing windows may remain unresolved. Report coverage gaps. Do not invent missing subjects or relations to fill the schema.

Import accepts one JSON object without prose/fences, bounded before parsing. Required shape:

```json
{
  "schema": "lwiki.extraction.v1",
  "packet_id": "packet-1",
  "packet_fingerprint": "blake3:…",
  "mentions": [
    {"id":"m1","window_id":"w1","label":"Ada","type":"person","quote":"Ada"},
    {"id":"m2","window_id":"w1","label":"Acme","type":"organization","quote":"Acme"}
  ],
  "assertions": [{
    "id":"a1","subject":"m1","predicate":"works_for",
    "object":{"kind":"mention","mention_id":"m2"},
    "negated":false,"modality":"asserted",
    "evidence":[{"window_id":"w1","stance":"supports","quote":"Ada works for Acme."}]
  }],
  "unresolved": []
}
```

V1 rejects unknown fields, duplicate keys/IDs, unsupported versions, missing references, and fields outside the registry. IDs are packet-local; the importer allocates durable IDs. Mention `description` is optional; asserted aliases cannot automatically update a global entity. Assertion optional fields are `valid_from`, `valid_until`, `unit`, and `property`, with the exact types/constraints in [record schemas](record-schemas.md). Literal objects replace the mention object with `{ "kind":"literal", "type":"decimal", "value":"2.5" }`. Unresolved entries contain `window_id`, `quote`, and `reason` strings.

Each mention/evidence quote must match uniquely inside its declared window. Optional `span: {start,end}` uses absolute source UTF-8 offsets and must exactly match the quote. Ambiguous matches require explicit validated spans or a revised quotation; the CLI derives offsets/hash otherwise. Validate packet/source fingerprints, UTF-8 boundaries, predicate/object compatibility, qualifier intervals, and evidence stance. Structural validation proves traceability, not entailment.

Extraction caching covers complete model input, schema/prompt/segmenter versions, model/deployment, generation parameters, and context. Keep source-local output separate from resolution. Import stages readable notes with raw JSON; unresolved endpoints remain source-local proposals until bound/created. Markdown is canonical; assertions start proposed. The executable workflow is `import → changes apply → resolve → changes apply → review → changes apply`, using returned IDs/hashes at each step. It needs no repeated permission prompt. M1 fixtures are accepted canonical Markdown; packet import starts in M2.

`graph review --file` consumes `lwiki.graph-review.v1`: decisions contain `assertion_id`, `expected_hash`, `decision: accept|reject`, `reason`, and `evidence_checks` (`evidence_id`, `expected_hash`, `assessment: supports|contradicts|insufficient`). Cover every active evidence record and recheck membership at apply. Insufficient retracts evidence; changed stance creates successor evidence for the same verified span and retracts its predecessor. Accept only with post-review current support. Stage all edits in one recoverable changeset. An authorized human, host agent, or configured bounded reviewer supplies the assessment; confidence cannot substitute. Host spending is reported or unknown.

## Identity resolution and invalidation

Mention states are `unresolved`, `new_entity`, `resolved(existing ID)`, or `rejected`. Names/types/aliases and vectors suggest candidates only. Reuse requires verified identity or a persisted decision; ambiguous aliases/pronouns remain unresolved. `graph resolve --file` accepts `lwiki.graph-resolution.v1` with `extraction_id`, `expected_hash`, and `mappings`. Each mapping has `mention_id`, `operation`, and `reason`: `BindMention` adds `entity_id`/`expected_entity_hash`; `CreateEntity` adds `title`/`entity_type`; `RejectMention` adds no target. The importer allocates new IDs and persists the complete map. Accepted aliases require a separate `AddAlias` decision, never name guessing.

`MergeEntities` requires explicit source IDs, retained target ID, expected hashes, rationale, and a complete mention/assertion remap. Reject unresolved conflicting identity assignments. `SplitEntity` allocates new IDs and requires an exhaustive partition of references; ambiguous references block publication. Preserve before/after IDs and supersession history. Never silently retarget assertions through redirects. These are ordinary reviewed changesets, replayable during index rebuild; extraction responses cannot perform them.

`graph decide --file` accepts `lwiki.entity-decisions.v1`, at most 16 `decisions`/256 KiB. Each has `operation`, `reason`, `expected_records: [{record_id,hash}]`, and `remaps`. Merge adds `source_ids`/`target_id`; split adds `source_id`/`new_entities: [{key,title,entity_type}]`; alias adds `entity_id`/`alias`. Remaps name each assertion field (`subject_id|object_id`) or extraction/mention reference, old entity, and target ID/new-entity key. Require hashes for all affected records and exhaustive remaps; alias uses an empty remap. Allocate IDs, validate the complete result, then stage canonical decisions/edits in one changeset. Conflicting or incomplete partitions fail.

Current assertions require accepted status and at least one intact active supporting evidence record against an active source's current revision. Contradicting evidence remains visible. Current accepted assertions with identical canonical subject, predicate, object/literal, property, unit, modality and date interval but opposite negation are also marked `disputed`. Graph results expose bounded `opposing_assertions` references and `omitted_opposing_assertions`; the `contradictions` list remains evidence records. Property names are in `qualifiers.property`, with no separate top-level property field. A source-head advance makes old evidence historical; revalidation creates successor evidence, never silently changes its revision. On withdrawal, recompute the complete dependent closure. One remaining current support preserves an assertion; loss of all support makes it unavailable as current evidence. Invalidate dependent entity/page descriptions, assertion/vector memberships, graph projections, and cached context bundles using their dependency fingerprints. Identities may remain navigable historically. Purge separately removes content-bearing extraction/context/vector caches within the requested scope.

## Acceptance and evaluation

Use deterministic fixtures first; these are acceptance paths, not measured benchmarks. Compare literal, FTS, exact dense, entity, relationship, and fused retrieval under equal context budgets on a fixed corpus revision. Track Recall@k/nDCG, complete evidence-set recall, citation validity, resolution errors, supported-assertion precision, cold/warm latency, memory, and provider usage. Tune on separate questions from held-out evaluation.

| Fixture and command path | Required result |
|---|---|
| `Vec<T>`, `E0308`, quoted `OR`; literal/lexical search | Literal preserves symbols; lexical never executes query operators or malformed SQL |
| Two Ada people at different Acmes; extract → import → resolve | Homonyms stay separate; ambiguous alias/quote blocks automatic binding |
| Short notes plus long Unicode report; sync → embed → search | Whole short inputs; deterministic bounded splits; exact original byte citations |
| A depends on B; B maintained by C; combined query at depth 2 | Both source-backed edges returned within budget; no invented A–C assertion |
| Opposite directions, negation, dated employment, contradiction | Distinct propositions and qualifiers survive import/rebuild and retrieval |
| Rename heading; switch equal-dimension model; stale note edit | Correct input reuse/invalidation; no mixed space or stale current evidence |
| Withdraw one then all supports; cached semantic/context query | Remaining support survives; zero withdrawn/unsupported current-evidence leakage |
| Delete SQLite; rebuild → lexical graph query | Same accepted graph/decisions without model calls; vector coverage honestly missing |
| Malformed packet, interrupted apply, offline cache miss, hub graph | No partial activation; recovery respected; no remote calls offline; bounds/omissions visible |

Quality thresholds follow baseline measurements; citation hashes, forbidden identity merges, space isolation, offline behavior, and withdrawn-evidence leakage have strict fixture assertions from M1 onward.

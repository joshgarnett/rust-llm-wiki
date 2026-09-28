# Retrieval, graphs, and compiled knowledge for a local Rust wiki

Research date and source-access date: **2026-09-28**. This is a design assessment, not an implementation or a reproduced benchmark. Documentation establishes available behavior; papers and vendor experiments establish results only for their tested configurations. Recommendations below are engineering judgments for one executable and readable files. The user's chosen semantic-search boundary is a configurable embedding API; local model downloads are excluded.

The selected foundation is **Markdown plus an embedded SQLite catalog, full-text search, page links, provenance, and an extracted entity/relationship graph**. The user explicitly included LightRAG-inspired graph extraction and best-effort Markdown recovery. Evaluation governs quality, defaults, and cost rather than whether the graph belongs in scope. Semantic retrieval remains an optional remote capability. A compiled wiki and retrieval complement each other: curated pages supply reusable synthesis, while source retrieval recovers details and checks claims. See [the graph design](../knowledge-graph.md) and [Markdown format](../wiki-format.md).

## What current systems actually provide

**LightRAG.** Its current documented modes are `local` for entity-focused retrieval, `global` for relationships and broader topics, `hybrid` for their combination, `naive` for vector retrieval of chunks, and default `mix` combining graph and chunk retrieval. Its `hybrid` label therefore does **not** mean lexical BM25 plus vectors. Current features include citations, several chunking strategies, reranking, and document deletion with reconstruction of affected graph content from extraction caches. The README explicitly describes its default file-backed, in-memory stores as suitable for small-scale testing/evaluation, recommending other backends for production. This is useful architectural inspiration, but its Python runtime and growing service/parser ecosystem do not satisfy this project's distribution requirement directly. [LightRAG README](https://github.com/HKUDS/LightRAG)

The implementation confirms four distinct defaults: `JsonKVStorage`, `NanoVectorDBStorage`, `NetworkXStorage`, and `JsonDocStatusStorage`. It also exposes separate entity, relationship, and total-context token limits. These distinctions matter: a graph, a vector index, an ingestion ledger, and a context assembler are separate responsibilities even when stored in one SQLite file. [LightRAG implementation](https://raw.githubusercontent.com/HKUDS/LightRAG/main/lightrag/lightrag.py)

The LightRAG paper extracts entities and relationships with an LLM, retrieves relevant graph elements using query keywords and vector similarity, expands neighboring graph context, and includes supporting original text. Its evaluation uses four textbook-derived corpora, 125 generated questions per corpus, and LLM pairwise judgments of comprehensiveness, diversity, and usefulness. Reported wins are evidence for high-level synthesis in that setting; they are not measurements of exact-identifier retrieval, citation correctness, deletion safety, or our local CLI's latency. In particular, modest wins against GraphRAG on several datasets should not become a universal superiority claim. [LightRAG paper, v3](https://arxiv.org/html/2410.05779v3)

**Microsoft GraphRAG.** Standard indexing performs LLM entity/relationship extraction and summarization, community detection, and community reporting. Its current `fast` method replaces extraction with noun phrases and co-occurrence relationships, but **still generates community reports with an LLM**. FastGraphRAG and LazyGraphRAG are different cost strategies. [GraphRAG indexing methods](https://microsoft.github.io/graphrag/index/methods/)

GraphRAG's default indexing outputs are Parquet tables, with embeddings written to the configured vector store; a graph database server is not intrinsic to the method. The original research targets corpus-wide questions through hierarchical community summaries. This is relevant for questions such as “What are the recurring tradeoffs across all research?”, rather than proof that a graph must mediate every lookup. [GraphRAG outputs](https://microsoft.github.io/graphrag/index/overview/), [original GraphRAG paper](https://arxiv.org/abs/2404.16130)

**LazyGraphRAG.** Microsoft's published method creates a noun-phrase/co-occurrence graph without LLM summarization at ingest, then spends a configurable relevance-test budget during queries. It combines embedding search, community exploration, relevance assessment, and query-specific claim extraction. The headline indexing cost of 0.1% of full GraphRAG came from a particular comparison; it is not a total-cost guarantee. The reported experiment used 5,590 AP articles, 100 synthetic local/global questions, and LLM answer comparisons. Query cost varies with the relevance budget. The updated article describes availability through Microsoft Discovery and Azure Local; this evidence does not establish a ready-made, independent Rust library. The transferable idea is **defer expensive processing until a query or repeated demand justifies it**. [Microsoft LazyGraphRAG research report](https://www.microsoft.com/en-us/research/blog/lazygraphrag-setting-a-new-standard-for-quality-and-cost/)

Two further research directions deserve experiments later. HippoRAG 2 integrates passages with a graph and Personalized PageRank, using an LLM during retrieval; KET-RAG extracts a detailed graph from selected chunks and supplements it with a cheaper keyword–chunk graph. Both suggest alternatives to exhaustive extraction. Their published gains remain benchmark-specific, and neither establishes that a maintained wiki requires their full pipeline. [HippoRAG 2](https://arxiv.org/abs/2502.14802), [KET-RAG](https://arxiv.org/abs/2502.09304)

## Three graphs, with different meanings

These are proposed data-model distinctions, not interchangeable marketing names:

| Graph | Edge meaning | How obtained | Recommended role |
|---|---|---|---|
| Page-link graph | Page A links to page B | Parse Markdown links, headings, aliases | Ship initially: navigation, backlinks, bounded expansion, broken-link checks |
| Provenance graph | Claim/page was derived from source revision and span | Record explicit evidence references during authoring/compilation | Ship initially: citations, impact analysis, retraction, stale-page detection |
| Entity/claim graph | A named entity has a typed relationship or assertion about another | Agent/API extraction into Markdown assertions with explicit evidence | Included in first agent release: relation-heavy questions and multi-document exploration |

A backlink proves that somebody linked two pages; it does not prove an asserted relationship. Co-occurrence is similarly weaker than a sourced factual claim. Model-inferred edges should retain extraction method, source support, status, and time scope. Entity aliases require explicit identity resolution: identical names need not designate the same entity.

All three can use indexed adjacency tables and bounded traversal in SQLite. A graph view does not require Neo4j. Keep authored links, entities, assertions, evidence, and resolution decisions in Markdown so graph projections remain rebuildable. Generated summaries that humans have accepted are durable knowledge artifacts; recreating them nondeterministically from raw sources is not an equivalent backup. Vector regeneration is separate and may need retained cached vectors or paid embedding calls.

## What the new wiki research does—and does not—show

The evidence is emerging and mixed:

* **Retrieval as Reasoning / LLM-Wiki** reports gains from compiling linked pages and composing search/read operations, with an “Error Book” recording recurring compilation failures. Its reported human audit agrees substantially with model judgments. The authors explicitly leave large-scale dynamic maintenance, stale facts, and unwieldy directories as limitations or future work. Useful implications are typed search/read tools and structural validation, not an assumption that self-editing wikis are already reliable. [LLM-Wiki preprint](https://arxiv.org/html/2605.25480v1)
* **A preregistered wiki-versus-vector-RAG comparison** used 24 papers and 13 questions. Wiki answers connected papers better, but consumed about 21 times the query tokens in the tested setup. An exploratory decomposition-RAG variant recovered much of the synthesis advantage at lower token cost. Citation checks assessed support in retrieved artifacts, not complete fidelity back to original PDFs. The tiny sample and restricted baselines prevent a broad ranking, but directly challenge the assumption that upfront compilation automatically creates cheaper queries. [Preregistered comparison](https://arxiv.org/html/2605.18490v1)
* **WiCER** investigates facts lost during compilation and iterative repair using diagnostic questions. It reports improvements but also describes fact displacement: preserving targeted facts can lose other information. Hardware/model specificity and a fixed-chunk, non-reranked RAG baseline constrain generalization. Its cached full-context wiki regime also differs from a general agent browsing many Markdown pages. The practical lesson is to retain sources and test preservation, not treat summaries as lossless compression. [WiCER preprint](https://arxiv.org/html/2605.07068v1)

Our inference: compile valuable, repeatedly used synthesis selectively; retain source evidence and a direct retrieval path. Benchmark both “search raw sources” and “search compiled pages,” including their combination.

## Retrieval design that fits one executable

SQLite FTS5 supplies phrase/prefix matching, configurable tokenization, column-weighted BM25, snippets, and index maintenance. Its BM25 ordering uses **lower scores for better matches**. Title, alias, heading, tag, and body fields can therefore support useful ranked lookup without an embedding runtime. Tokenizer behavior deserves explicit tests for Rust symbols, punctuation, error codes, and non-English material. An external-content FTS table refers to SQLite content tables; it does not automatically monitor Markdown files. [SQLite FTS5 documentation](https://sqlite.org/fts5.html)

Recommended progression:

1. Implement literal search, ranked FTS, metadata filtering, explicit graph traversal, and source-aware reads without any model requirement.
2. Index documents first, preserving stable IDs, source hashes, display headings, and exact evidence spans. Use whole short-note embedding inputs; split long inputs for model limits or a measured retrieval-quality need. Derived units stay in the index, with no user-managed chunk files. Heading text never serves as permanent identity. Long entity-extraction windows may be needed even without embeddings.
3. Offer optional embeddings through a configurable `/v1/embeddings` API: full endpoint URL, model, and either a static key or credential script. Store vectors locally and benchmark exact vector scan first; adopt ANN only when measured memory or latency requires it. Never mix embedding model revisions or dimensions in one search space.
4. Fuse lexical and dense candidate lists using reciprocal rank fusion, then deduplicate and optionally expand selected links. RRF combines ranks rather than incomparable score scales; its historical constant of 60 is a starting point, not a law. [Original RRF paper](https://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf)
5. Include entity- and relationship-focused retrieval with bounded evidence expansion. Evaluate query decomposition, learned reranking, and community summaries as later additions. Preserve offline lexical seeds and make semantic/LLM query processing explicit.

Give agents small search responses containing IDs, titles, concise excerpts, source status, and reasons for retrieval. A separate read/context command should assemble selected passages under an explicit token budget. Reserve room for instructions and output; cap candidates, traversal depth, total reads, and model calls independently. Return truncation and omitted-result metadata. Do not silently equate “top ten matches” with complete corpus coverage.

Lexical and explicit graph retrieval remain offline. Semantic ingestion sends selected document content to the configured endpoint; an uncached semantic query needs a remote query embedding even though vector matching is local. Measure endpoint latency, retries, token charges, and embedding backlog separately from local search. Cache unchanged chunk and query embeddings under a model/configuration fingerprint, and show lexical freshness separately from semantic freshness. Provider failure should produce an explicit semantic-unavailable status or a disclosed lexical fallback, never a silent claim that hybrid search completed.

Older long-context research found position-sensitive performance; it motivates testing evidence ordering on the actual chosen models, not assuming the measured degradation transfers unchanged to every 2026 model. [Lost in the Middle](https://arxiv.org/abs/2307.03172)

## Updates, contradictions, and retraction

The following are recommended correctness requirements:

* Evidence references identify a source revision and span, not just a URL. A renamed page should preserve its identity; a changed source should invalidate affected evidence.
* Persist claim support and identity decisions in Markdown records with flat frontmatter and links. Rebuild links, retrieval units, and FTS from canonical material; restore vectors from cache or regenerate explicitly through the embedding API. Invalidate derived caches by dependency hash.
* Separate `observed_at`, publication date, and the period when a claim applies. Represent conflicting claims with their respective evidence and `disputed`, `superseded`, or `withdrawn` status. Newer does not automatically mean truer.
* Source deletion/retraction must traverse dependencies. Claims with remaining independent support may survive; unsupported summaries and entity edges must become unavailable or explicitly withdrawn before normal retrieval serves them again. Recompute affected aggregates instead of merely deleting a source row.
* Distinguish withdrawing evidence from permanently purging stored material. A purge must include chunks, embeddings, caches, generated excerpts, and relevant local history according to the requested scope. Historical tombstones must not retain content that was meant to be erased.
* Detect external file edits and index-generation mismatch. If a current source differs from the indexed revision, refresh or return an explicit stale result status rather than presenting old spans as current citations.

## Evaluation and economics before architectural escalation

BEIR's heterogeneous evaluation supports retaining strong lexical baselines rather than presuming dense retrieval wins everywhere. GraphRAG-Bench likewise evaluates graph construction, retrieval, and generation separately across task types. Neither supplies a universal file-count threshold for this product. [BEIR](https://arxiv.org/abs/2104.08663), [GraphRAG-Bench](https://arxiv.org/abs/2506.05690)

Build a small labeled seed set first, then expand toward roughly 100–300 representative questions as coverage improves; this is a planning heuristic. Include literal identifiers, paraphrases, multi-hop questions, exhaustive lists, historical questions, contradictions, source retractions, and unanswerable questions. Split tuning and held-out cases. Label necessary source spans and, for multi-hop questions, all required evidence.

Compare literal search, FTS, dense, hybrid, hybrid-plus-links, and compiled-page retrieval under equal context/model budgets. Measure Recall@k and nDCG, complete evidence-set recall, citation correctness against original sources, unsupported claims, abstention, and stale/deleted-evidence leakage. Track indexing time, warm/cold latency, peak RAM, disk, and input/output tokens. Human-check a stratified sample of model judgments. Test mutations and interrupted indexing in addition to static QA.

Use `total cost = initial compilation + updates/repairs + query count × mean query cost`, separately reporting cash expense, compute time, and storage. Cache hits need revision-aware keys. An expensive compilation amortizes only when later savings actually exceed it. Escalation gates should require measured quality gains on important questions while respecting latency, context, and spending budgets. The user's supplied size bands and prices are useful hypotheses, not implementation triggers or validated economics for this CLI.

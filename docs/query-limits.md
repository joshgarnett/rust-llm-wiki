# Query limits and comparison

lwiki accepts up to **16,384 UTF-8 bytes** per query and **256 whitespace-separated phrases** in lexical search, including hybrid's lexical branch. Input above either applicable ceiling fails explicitly; accepted lexical input is escaped and retained in full. Literal and semantic-only queries do not have the lexical phrase ceiling. These are application choices, not SQLite engine limits.

## What other tools do

Primary documentation checked 2026-10-03. Bytes, words, analyzed clauses, syntax-tree nodes and transport limits are different units; none is a universal safe query length.

| Tool | Documented behavior | Relevance to lwiki |
| --- | --- | --- |
| [Elasticsearch](https://www.elastic.co/docs/reference/elasticsearch/configuration-reference/search-settings) | Dynamically determines maximum Boolean clauses from heap/thread-pool resources, with a minimum 1,024. The old `indices.query.bool.max_clause_count` setting has no effect. | Resource-based clause limits allow much longer queries than short search boxes. Analyzed clauses are not whitespace words. |
| [PostgreSQL18](https://www.postgresql.org/docs/current/textsearch-limitations.html) | A `tsquery` must contain fewer than 32,768 nodes, including lexemes and operators. Individual lexemes are less than 2 KiB. | Representation ceilings do not promise useful relevance or interactive latency at the maximum. |
| [Algolia](https://www.algolia.com/doc/api-reference/api-parameters/query) | Query strings may be at most 512 bytes. | A short-query API policy, too restrictive for some multi-part wiki questions. |
| [Meilisearch](https://www.meilisearch.com/docs/reference/api/search/search-with-post) | Considers only the first ten words. Its default matching strategy also relaxes matches by dropping trailing words. | Truncation could discard a scenario's decisive condition; lwiki must retain full accepted input. |
| [Typesense 30.1](https://typesense.org/docs/30.1/api/search.html) | Can drop query tokens when too few results match; `drop_tokens_threshold: 0` disables this. Optional `search_cutoff_ms` bounds search effort imperfectly. | Query relaxation and time budgets are separate design decisions from input size. |
| [SQLite FTS5](https://www.sqlite.org/fts5.html#full_text_query_syntax) | Supports quoted phrases combined with Boolean operators. Bundled 3.53.2 source defaults expression-tree depth to 256 and flattens homogeneous OR nodes. | Depth 256 does not impose 256 terms on our flat quoted OR expression. |

[Typesense's historically discussed 4,000-character GET URL limit](https://threads.typesense.org/t/29448287/hi-i-ve-been-working-on-creating-a-repair-mechanism-for-my-e) concerns transport parameters, not a general semantic `q` ceiling; [POST multi-search](https://typesense.org/docs/30.1/api/federated-multi-search.html) supplies queries in a JSON body. Do not transplant URL limits to a local CLI.

## Local measurement and decision

Measured the actual bundled SQLite 3.53.2 optimized static library on Apple M1 Max/macOS 26.5.2. An in-memory FTS5 table held 10,000 documents, each 5,183 bytes and containing 512 shared tokens plus one 63-byte token. Each query quoted whitespace phrases joined by `OR`, executed `SELECT rowid,bm25(ft) FROM ft WHERE ft MATCH ? ORDER BY bm25(ft),rowid LIMIT 80`, and returned 80 matches or zero for absent terms. Reported values are medians of five executions, including statement preparation and result consumption. Other development work was active; these are diagnostic measurements, not isolated performance guarantees.

| Phrases | All phrases match every document | All phrases absent | Repeated 63-byte token |
| ---: | ---: | ---: | ---: |
|64|0.072s|0.00036s|0.078s|
|128|0.202s|0.00073s|0.211s|
|256|0.570s|0.00146s|0.577s|
|512|1.693s|0.00290s|1.807s|
|1,024|6.118s|0.00601s|5.828s|

The 256-phrase repeated-token input uses 16,383 raw bytes, exercising both proposed limits together. The 1,024-phrase shared case repeats the 512 vocabulary items. No query failed. The measurement excludes disk I/O, the CLI's remaining retrieval stages, source verification and model calls. It does not measure answer quality or every pathological input.

Sixteen KiB leaves space for Unicode, identifiers and detailed scenarios. The 256-phrase ceiling accommodates much longer questions than the previous 64-phrase bound without assuming that 1,024 clauses will be fast on a local wiki. Revisit the bound using representative rejected questions and measured latency, rather than automatically inheriting another engine's maximum. Detailed source, timings and compile artifacts are retained locally under `.artifacts/query-limits/`.

## Independent limits remain visible

An embedding space separately bounds **query prefix plus full query** using its configured `max_input_bytes` (default 12,000). A query can satisfy the CLI input ceiling and exceed that provider-input budget. It then fails with `BUDGET_EXCEEDED`, stating the actual bytes and configured bound, before a provider request. This change neither truncates provider input nor changes existing space identities.

Context passage selection separately bounds distinct tokenized terms at 256 and reports omissions. Punctuation may yield several tokens from one accepted whitespace phrase. Output bytes, candidate counts, source scans and verification deadlines remain independent; a larger input ceiling does not expand the final answer context or prove completeness.

The independent code review also found an older 8,192-byte internal graph-key ceiling. It now permits the shared query ceiling plus 128 bytes for its fixed space prefix. The complete key still participates in cursor identity. Integration tests exercise 16 KiB input through semantic graph, hybrid graph search and graph context, and reject a cursor reused with only the query's final byte changed.

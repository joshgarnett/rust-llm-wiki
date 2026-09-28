# Rust implementation and distribution feasibility

Research date: **2026-09-28**. Primary documentation, upstream manifests, and release notes were inspected on this date. Recommendations below are engineering judgments; no prototype or benchmark was run. Versions are observations, not dependency pins.

## Recommendation

Build a small Rust executable around **Markdown plus bundled SQLite**, with FTS5 for ranked text retrieval and ordinary relational tables for links, metadata, and provenance. Provide literal search without a separately installed `rg`. Per the user's decision, semantic search uses a remote **`/v1/embeddings`-compatible API**, with vectors stored locally. No local models or inference runtime belong in the implementation. “No services to install” remains realistic: lexical and graph operations work offline, while embedding generation requires the configured endpoint.

Keep canonical knowledge and research records outside the search cache. Deleting the index must never delete source attribution, manually assigned IDs, accepted relationships, recorded budgets, or completed-job records. Per the user's refinement, use Markdown with flat frontmatter for durable records; reserve other formats for original attachments, vectors, private settings, and operational journals. Rebuildable SQLite tables project the notes for querying.

## Minimal core

| Concern | Candidate | Implementation judgment |
| --- | --- | --- |
| Commands and output | `clap`, `serde`, `serde_json` | Human help plus versioned JSON envelopes, structured errors, stable exit codes, stderr progress, and no prompts in machine mode. [clap docs](https://docs.rs/clap/latest/clap/) |
| Markdown | `pulldown-cmark` | Use events and source offsets for headings, links, citations, and structure-aware chunks. Define explicit support for wiki links/frontmatter; do not mistake CommonMark parsing for complete Obsidian compatibility. [Parser docs](https://docs.rs/pulldown-cmark/latest/pulldown_cmark/) |
| File enumeration/search | `ignore`, `regex` or ripgrep's `grep-*` libraries | Respect configured excludes and provide literal/regex escape hatches internally. Hidden-file and ignore behavior must be deliberate. [ignore docs](https://docs.rs/ignore/latest/ignore/) |
| Fingerprints | `blake3` | Separate source-byte hashes, normalized-text hashes, and embedding-input hashes. [BLAKE3 crate](https://docs.rs/blake3/latest/blake3/) |
| Persistence | `rusqlite` with `bundled` | Compile SQLite into release artifacts; no system SQLite installation. Keep optional features narrow. [rusqlite upstream](https://github.com/rusqlite/rusqlite) |
| Embedding HTTP | `reqwest`, Rustls, `serde` | Typed requests/responses, reusable connections, explicit timeouts and bounded concurrency. [reqwest docs](https://docs.rs/reqwest/latest/reqwest/) |
| File mutations | Same-directory temporary files and platform-aware replacement | Use expected-version checks and a recovery protocol; do not assume a database transaction includes filesystem writes. |

`rusqlite` is MIT-licensed; bundled SQLite is public domain. Its bundled build uses a C compiler during development, ships pregenerated bindings, and explicitly enables FTS5. This adds a build dependency, not a requirement that users install SQLite or a C compiler. [Build instructions](https://github.com/rusqlite/rusqlite#notes-on-building-rusqlite-and-libsqlite3-sys), [build flags](https://github.com/rusqlite/rusqlite/blob/master/libsqlite3-sys/build.rs)

FTS5 supplies BM25, snippets, field weighting, and tokenizer choices. Index title, headings, aliases, and document text separately. Start with document tables plus an FTS projection updated in the same SQLite transaction; add derived passage records when retrieval/model limits justify them. Test consistency during deletes/rebuilds. SQL-bound parameters prevent SQL injection but do not make arbitrary input valid FTS syntax: distinguish plain-text queries from an explicit advanced query mode. [FTS5 documentation](https://sqlite.org/fts5.html)

**Tantivy is the alternative if lexical requirements outgrow FTS5.** It is an MIT-licensed embedded Rust search library with incremental indexing, but edits involve delete/reinsert, commits, and reader reloads. It also permits only one `IndexWriter` at a time. Using it alongside SQLite creates another independently published index generation. That cost is justified by measured ranking, tokenizer, or scale needs, not by assuming every wiki needs a Lucene-style engine. [Tantivy upstream](https://github.com/quickwit-oss/tantivy), [writer example](https://docs.rs/crate/tantivy/latest/source/examples/index_from_multiple_threads.rs)

Store explicit links as indexed edges with relation type, origin, and source spans. SQL adjacency queries are sufficient for backlinks and bounded traversal; graph search does not require Neo4j. Model-extracted claims should remain distinguishable from author-written links, with evidence and extraction version attached. This is a proposed data design, not an assertion that graph algorithms guarantee better retrieval.

## Embedded vector choices

| Option | Verified position | Recommendation |
| --- | --- | --- |
| **sqlite-vec** | MIT/Apache-2.0; dependency-free C; official Rust crate compiles and statically registers the extension. Stable `0.1.9` provides the flat baseline. `0.1.10-alpha.1` introduced rescore/DiskANN and experimental, disabled IVF; alpha.4 fixes related bugs. | First semantic-storage spike. Pin a stable implementation and benchmark exact search before accepting an ANN prerelease. [Rust integration](https://alexgarcia.xyz/sqlite-vec/rust.html), [releases](https://github.com/asg017/sqlite-vec/releases), [stable source](https://github.com/asg017/sqlite-vec/blob/v0.1.9/sqlite-vec.c) |
| **sqlite-vector** | Current license is Apache-2.0. Exposes full scans and quantized approximate scans; docs require rebuilding quantization after relevant data changes. | Credible second SQLite candidate. Quantization-based approximation should not be described as HNSW or DiskANN. Verify filter/update semantics. [License](https://github.com/sqliteai/sqlite-vector/blob/main/LICENSE.md), [API](https://github.com/sqliteai/sqlite-vector/blob/main/API.md) |
| **libSQL / Turso** | libSQL implements DiskANN and is a SQLite fork. Turso's Rust database is a separate implementation; upstream explicitly distinguishes them. Both repositories identify MIT licensing. | Possible consolidated ANN path, but evaluate exact engine/version compatibility. Do not transfer libSQL capabilities to the Rust rewrite by name association. [Vector docs](https://docs.turso.tech/features/ai-and-embeddings), [libSQL](https://github.com/tursodatabase/libsql), [Turso](https://github.com/tursodatabase/turso) |
| **USearch** | Apache-2.0; native C++ core with Rust `cxx` bindings; supports approximate search and persisted/mapped indexes. Current Cargo defaults include NumKong. | Good ANN candidate if exact search becomes inadequate; adds native compilation and a second consistency boundary with SQLite. [Repository](https://github.com/unum-cloud/USearch), [manifest](https://github.com/unum-cloud/USearch/blob/main/Cargo.toml) |
| **hnsw_rs** | Rust HNSW implementation, MIT/Apache-2.0; optional SIMD features. HDF5 is a development dependency in its manifest, not a runtime requirement of the library. | Smaller conceptual fit than a database, but application owns persistence, filtering, update/delete policy, and crash recovery. [Repository](https://github.com/jean-pierreBoth/hnswlib-rs), [manifest](https://github.com/jean-pierreBoth/hnswlib-rs/blob/master/Cargo.toml) |
| **LanceDB** | Apache-2.0 embedded retrieval library with Rust SDK. Current crate has empty default features, but retains substantial Arrow/Lance/async infrastructure. | Serverless is compatible with the goal; added dependency and storage complexity makes it a later candidate for larger or multimodal workloads. [Repository](https://github.com/lancedb/lancedb), [manifest](https://github.com/lancedb/lancedb/blob/main/rust/lancedb/Cargo.toml) |

Do not claim sqlite-vec is universally “flat-only”: that is now version-dependent. All exact/ANN choices still require embedding generation. Isolate embedding spaces by configured model revision, dimensions, normalization, and query/passage conventions; record chunker versions separately. Remote tokenizer/pooling changes may be hidden behind provider aliases, which makes explicit revisions necessary. Fusion operates on candidate rankings; BM25 and cosine scores are not directly interchangeable.

## Remote embeddings and distribution

Use a direct, narrowly scoped HTTP adapter rather than coupling storage to a provider SDK. `reqwest` offers JSON, Rustls, connection pooling, proxy configuration, and redirect policy controls. Its async client needs Tokio; a blocking client is also available. A bounded async worker queue is a reasonable fit for research and embedding batches, but database transactions must not remain open while awaiting HTTP. [reqwest](https://docs.rs/reqwest/latest/reqwest/), [client configuration](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html)

Required configuration is the **full endpoint URL**, model, and either a static API key or a command that retrieves dynamic credentials. Preserve the supplied endpoint path and query exactly rather than appending `/v1/embeddings`. Support optional dimensions, provider-specific headers/options, batch item/token limits, request timeout, retry limit, and concurrency. Keep credential material in user-controlled secret configuration or environment references, never research outputs or committed index metadata. The main design document owns the final configuration schema.

Invoke credential helpers using an executable plus argument list, with bounded output, timeout, nonzero-exit handling, and an explicit refresh policy. Do not interpret retrieved wiki text as a command or invoke a shell implicitly. A user may explicitly configure an interpreter for their script. Rust's `Command` normally passes arguments literally, although Windows batch files have special shell behavior; test and document supported helper types. Never log helper stdout or authorization headers. [Rust process API](https://doc.rust-lang.org/std/process/struct.Command.html)

Own retry policy at the application layer: cap total attempts, respect rate-limit delays, refresh expired credentials in a controlled way, and retain completed batches. A timed-out request may have been billed even when no response arrived; distinguish confirmed usage from estimates. `reqwest` has its own retry behavior, so avoid accidentally multiplying retries across layers. [Retry documentation](https://docs.rs/reqwest/latest/reqwest/retry/index.html)

Validate response cardinality, indices, dimensions, finite numeric values, and error bodies before persistence. Key embedding caches by actual input plus a configured embedding-space identity, including endpoint identity without credentials, model/revision, dimensions, normalization, and query/passage conventions. Credential rotation must not invalidate vectors. A provider changing the model behind an alias requires a new user-controlled revision; equal dimensionality does not imply compatible embeddings.

Ship platform binaries with bundled SQLite and explicitly configured TLS dependencies; verify linked libraries on clean machines. No model downloads are needed. Disable redirects by default for credentialed embedding requests unless an explicitly supported policy permits them. `--offline` must prevent network and credential-helper execution; uncached query embeddings then produce a clear capability error or an explicitly requested lexical fallback.

Rustls removes the need for a system OpenSSL choice, but its default cryptography provider is `aws-lc-rs`, with native build dependencies such as CMake. Distinguish build tooling from user installation requirements and audit licenses/linkage for the selected dependency tree, not only top-level crate licenses. [Rustls platform/features documentation](https://docs.rs/rustls/latest/rustls/)

## Filesystem and concurrency contract

These are proposed correctness requirements:

1. Give adopted documents immutable IDs in flat frontmatter. Paths, titles, and headings are attributes. Flag copied duplicate IDs rather than silently merging documents. External Markdown lacking an ID remains readable; adoption is explicit. Deleted metadata gets a diagnostic and best-effort repair, not an invented old identity.
2. Start with whole short-note inputs. When segmentation is needed, treat unit IDs as derived retrieval addresses, not permanent semantic identities. Store document ID, source revision, parser/segmentation version, display heading, and source span. Reuse embeddings by actual formatted-input hash; unchanged inputs should not require embedding again.
3. Serialize CLI mutations with a vault lock. Read and compare expected content hashes immediately before replacement; return a conflict when another writer changed the page. Advisory locks cannot make external editors cooperate, so preserve originals and document the remaining race.
4. Stage and sync replacement files in the destination directory, then replace using tested platform semantics. Persist an operation manifest for multi-file changes and recover interrupted operations. SQLite cannot atomically commit unrelated Markdown writes. [Rust rename behavior](https://doc.rust-lang.org/std/fs/fn.rename.html)
5. Keep database write transactions short; use a bounded busy timeout. WAL supports concurrent readers with one writer and is unsuitable for multi-host network-filesystem access. Store caches locally; synchronize canonical files rather than live database/WAL files. [SQLite WAL](https://sqlite.org/wal.html)
6. Reconcile external edits at command boundaries; watchers are accelerators, not the correctness mechanism. Publish rebuilt generations without replacing open database files blindly, especially on Windows. Expose freshness/revision information and refuse stale citations when source hashes differ.

## Feasibility gates before committing architecture

| Spike | Acceptance evidence |
| --- | --- |
| Release packaging | Core runs on clean macOS ARM64, Linux x86-64, and Windows x86-64 without Python, Node, database packages, or network. Inspect linked libraries and record supported OS/CPU baselines. |
| Index lifecycle | Create/edit/rename/delete, duplicate IDs, malformed Markdown, Unicode/case-sensitive paths, and full rebuild produce consistent lexical and graph results. |
| Crash/concurrency | Terminate between staging, rename, and DB commit; recover without lost canonical content. Two CLI writers conflict or serialize; an external edit is detected in the supported cases. |
| Embedding adapter | Mock full URLs, reordered response indices, malformed/nonfinite vectors, rate limits, timeouts, static keys, credential expiry/helper failures, and retries. Changing embedding-space configuration never mixes vectors; changing credentials preserves the cache. |
| Retrieval economics | Compare FTS5, exact vectors, hybrid fusion, then ANN on representative labeled queries. Report Recall@k/nDCG, cold/warm p95, RSS, disk size, and incremental update cost on named hardware. |

Do not set a corpus-size cutoff for ANN or adopt an engine from an upstream speed claim. The initial deliverable should make these choices measurable while keeping the core useful without embeddings.

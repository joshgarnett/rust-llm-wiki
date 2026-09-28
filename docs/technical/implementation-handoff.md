# Implementation handoff

Proposed technical baseline, 2026-09-28. This is an ordered work specification, not implemented behavior. Research and product requirements were committed as `4652248`; the companion technical designs refine them. No Rust package, binary, or installed agent skill has been created in this step.

The [autonomous execution plan](../execution/README.md) turns this handoff into bounded work packages, model assignments, decision defaults, validation evidence, and a restartable new-session goal.

## Initial package and module ownership

Start with one Cargo package exposing a library and the working `lwiki` binary. A workspace of separate crates is unnecessary until dependency isolation or build times justify it. Keep the binary thin: argument parsing, configuration selection, presentation, and exit mapping. The library must be usable by integration tests without subprocesses and must return typed outcomes rather than printing.

| Proposed module | Owns | May depend on |
|---|---|---|
| `domain` | Validated IDs, hashes, paths, record references, evidence spans, status/eligibility, shared errors | Standard library and serialization |
| `config` | Portable preferences, private profiles, local trust, effective options | `domain` |
| `records` | Lossless frontmatter parsing/editing, Markdown offsets, record schemas | `domain` |
| `vault` | Discovery, enumeration, lock and filesystem adapters | `domain`, `config`, `records` |
| `sources` | Immutable captures, revisions, exact evidence verification | `domain`, `records`, vault reads |
| `catalog` | SQLite generations, reference resolution, eligibility/dependency projections | Records and source validators |
| `changes` | Prepared payloads, expected hashes, journal, apply/recovery coordination | Vault filesystem and catalog publication interfaces |
| `providers` | Typed embedding/generation/search/fetch adapters, private authentication, bounded HTTP | `domain`, `config`; dispatcher authority required |
| `jobs` | Durable events, dispatch reservations, checkpoints, cancellation, retry decisions | Change/event storage and provider adapters |
| `retrieval` | FTS, exact vectors, RRF, bounded excerpts/context, provenance | Catalog snapshots, dispatcher for explicit semantic requests |
| `graph` | Extraction packets/import, resolution/review, typed traversal | Shared records, retrieval primitives, changesets, dispatcher |
| `research` | Bounded plan → retrieve → acquire → synthesize workflow | Jobs, sources, graph, retrieval, changesets |
| `cli` / `output` | Implemented command registry, JSON schemas/envelopes, human display, skill export | Public application operations |

Avoid an implementation cycle between `catalog` and `changes`: place the publication coordinator in `changes`/application code; the catalog receives a verified scan and a held publication permit. Likewise, provider transports cannot schedule themselves or retry invisibly. Jobs grants a reservation for every dispatch, then invokes the private transport. Retrieval/extraction receive a dispatcher interface, never an unrestricted HTTP client.

The storage document owns shared record types and the read/publication contract. Retrieval owns candidate and evidence-bundle types. Providers/jobs owns request, receipt, reservation, and checkpoint types. CLI owns the external result schema and exit codes. Convert internal errors once at the application boundary. Do not create competing source-revision or eligibility enums in each module.

## Dependency choices and short spikes

Use the [researched Rust candidates](../research/rust-stack.md) as the starting dependency list: `clap`, `serde`/`serde_json`, `pulldown-cmark`, `blake3`, `rusqlite` with bundled SQLite, and a narrow HTTP stack using `reqwest`, Rustls, and Tokio. The first vector implementation is a bounded Rust exact cosine scan over SQLite blobs. `sqlite-vec`, ANN libraries, local inference, and a second search engine are absent from the initial dependency set.

Before pinning Cargo versions, verify current upstream manifests, supported Rust versions, license trees, SQLite FTS5 build flags, TLS crypto provider, and target linkage. Record the chosen toolchain and commit `Cargo.lock`; do not invent a minimum Rust version from this design. Prefer Rust 2024 edition if the verified dependency/platform baseline supports it. Binary consumers should not need a C compiler; building bundled SQLite may need one.

Three focused M0 spikes resolve implementation uncertainty:

1. **Lossless YAML adapter:** demonstrate exact preservation of comments, unknown nested properties, quoting, BOM, and CRLF while rejecting duplicate keys/unsupported syntax. Select a parser/range adapter only after these fixtures pass. If none is adequate, implement a narrow validated range editor; never silently rewrite entire frontmatter.
2. **Filesystem replacement and lock:** demonstrate same-directory staging, expected-hash checks, advisory lock ownership, replacement, and recovery on the developer platform. Expand to the other release targets before claiming their durability. Keep platform differences behind one interface.
3. **SQLite projection publication:** demonstrate bundled FTS5, one reader snapshot across a generation switch, atomic replacement of published FTS rows, and preservation of the previous index on failed publication.

These spikes belong to implementation. Design-stage SQL parsing does not satisfy them.

## Ordered implementation slices

Each slice ends in a reviewable command workflow and its relevant tests. Implementation should stop at a failed prerequisite rather than layering remote features on an unreliable record store.

| Slice | Deliverable and dependencies | Acceptance gate |
|---|---|---|
| M0.1 contract fixtures | Minimal crate/toolchain; common types; machine-readable schemas derived from these documents | Example records and packets validate; malformed counterparts fail with stable diagnostics |
| M0.2 adapters | Lossless parser, filesystem/lock and FTS publication spikes | Documented decisions, passing preservation/recovery fixtures, verified build dependencies |
| M1.1 offline records | Vault initialization, discovery, `read`, expected-hash page writes and changesets | Round-trip unknown metadata; refuse copied IDs, path escapes, unexpected overwrite |
| M1.2 sources | Local UTF-8 capture, revision manifests, exact evidence verification, refresh/withdraw | Original bytes preserved; heading edits do not change IDs; altered snapshots cannot supply evidence |
| M1.3 indexes | Document FTS, page links, entity/assertion/evidence tables, diagnostics, sync/rebuild | Delete the cache and recover equivalent canonical graph/search behavior without remote work |
| M1.4 discovery | Literal/lexical search, graph fixtures/traversal, verified context, output envelopes | Stable ranking ties; bounded output; no invented transitive facts or stale current evidence |
| M1.5 resilience | Recovery/doctor, failure injection, concurrent writers, control-manifest verification | Crashes preserve canonical payloads; new decisions/duplicate IDs detected before verified output |
| M2.1 host extraction | Packet export, strict import, mention resolution, assertion review, changes apply | Host JSON becomes readable proposed records; explicit authorized review activates supported facts |
| M2.2 host skill | Release-matched skill source/export and command examples | Examples run against capabilities; discovery checked in Codex, Claude Code, and Cursor |
| M3.1 dispatcher | Private profiles, credential helpers, adapters, job receipts/reservations, mock transport | Every attempt accounted for; no calls/helpers offline or dry-run; uncertain billing retained |
| M3.2 semantics | Embedding generation/cache, document/entity/assertion rendering, exact vectors, hybrid fusion | Full URL preserved; reordered indices validated; no mixed spaces; whole short notes remain whole |
| M3.3 API extraction | Generation through the same packet/import/review contract | Cached/resumed packets preserve decisions; malformed responses cannot partially activate a graph |
| M4.1 research | Search/fetch acquisition, planner, budgets, checkpoints, evidence-backed reports | Bounded resumable run preserves completed work; unverifiable claims/coverage gaps remain explicit |
| M5 measured extensions | ANN, reranking, richer formats, community summaries | Representative evaluation establishes benefit over the simpler released path |

M1 graph fixtures are prepared accepted canonical Markdown records in a fixture vault. Extraction-wire import arrives in M2 and never accepts a model's `accepted` flag. M2 needs a runnable host agent, but the CLI neither installs nor launches one. M3's minimal job/dispatch accounting is a prerequisite to paid embedding and extraction work, even though the complete research runner arrives in M4.

## First end-to-end fixture

Use a small checked-in vault containing two different people sharing a name, two organizations, a short architecture note, a long Unicode report, a superseded source revision, and opposite/negated/dated relationships. Include real captured text, expected IDs/hashes, extraction packets, review decisions, and expected semantic outcomes. Use synthetic vectors with known ordering for correctness; use a separately configured real endpoint only for explicit compatibility/evaluation runs.

The first vertical workflow is:

```text
init → capture local source → create linked notes and graph fixture
     → sync → lexical and entity/relationship query → bounded context
     → rename heading/path → edit/refresh source → sync → revalidate
     → withdraw one support, then all support → verify invalidation
     → interrupt changeset → recover → delete cache → rebuild
```

Assert that every current graph evidence span matches immutable bytes and its hash; paths/headings remain navigation; no unintended entity merge occurs; canonical knowledge survives cache deletion; and rebuild has zero provider calls. Source withdrawal must affect dependent descriptions and cached context, not only hide one search row.

M2 extends this with packet → host response → import → apply → resolve → apply → review → apply, performed under one authorized workflow without obligatory human prompts per relationship. Each intermediate apply materializes records for the next command; no staged overlay is assumed. M3 adds failed/reordered embedding responses, rotating credentials, a timed-out billed-unknown request, source edits during in-flight embedding, and a changed model revision. M4 adds acquisition deduplication, budget exhaustion, cancellation, restart, and partial report preservation.

## Verification and release gates

Unit tests should target invariants: exact bytes, state transitions, reference ambiguity, budget arithmetic, and wire validation. Integration tests cover the fixture workflows. Fault injection belongs at journal/replace/sync/SQLite publication boundaries; a happy-path subprocess test cannot demonstrate recovery. Use mock providers and a counting credential helper to prove absence of external work in offline/dry-run operations.

Run formatting, linting, and the relevant test suite as slices land; broaden checks when scope or failures justify it. CI later builds macOS ARM64, Linux x86-64, and Windows x86-64 as proposed initial targets, with artifact smoke tests on clean machines. A target is supported only after its filesystem and packaging gates pass. Inspect binary linkage and transitive licenses for release artifacts.

Retrieval evaluation compares literal, FTS, exact dense, entity/relationship, and hybrid paths on a fixed corpus revision under equal context budgets. Start with representative labeled questions and expand before claiming ranking gains; include exact identifiers, paraphrases, homonyms, multi-document evidence, stale knowledge, and unanswerable questions. Report quality by query class, cold/warm latency, memory, disk, and provider usage on named hardware. Set performance targets after the first measured baseline. Correct citations, no unintended merges, isolated vector spaces, and no withdrawn-evidence leakage are required invariants, not average metrics.

## Deferred choices and constraints

The final binary name, priority between host-driven and standalone research, target-platform order, and the user's evaluation corpus remain open product preferences. They do not block the offline foundation. Default to host-driven M2 before standalone M4, use `lwiki` in provisional examples, and begin on the current macOS workspace while preserving portable interfaces.

An endpoint implementing embeddings does not imply generation or search support. Configure capabilities separately, and make exact provider compatibility a test result. No universal tokenizer, truthful dollar ceiling, internet search API, PDF extractor, or model-quality guarantee is implied. Dynamic credentials remain private and endpoint-bound. Additional external research prompts are available in [the research handoff](../external-research-prompts.md), but implementation need not wait for them.

The main remaining engineering risks are lossless frontmatter editing, cross-platform recovery, conservative remote-cost accounting, and extraction/retrieval quality. Their tests and milestones are explicit above. Avoid expanding scope into a graph server, local model runtime, GUI, or general agent framework before those gates are met.

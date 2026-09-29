# Agent research handoff contract

Research is local coordination with a host agent. There is no built-in research executor or legacy-run compatibility layer. The host owns its authorized tools, acquisition, reasoning and external spending. Embeddings, direct API graph extraction and probes keep their separate provider contracts.

## Commands and state

`research plan QUESTION` previews scope and current lexical/source passages without writes. `research run QUESTION` publishes a run head and immutable `collect_sources` packet. `research import --file FILE` accepts a bounded submission for the outstanding packet and publishes its result atomically. `research resume RUN` recovers interrupted local application and returns the outstanding packet or terminal result. `--refresh` explicitly replaces a stale packet against current sources. `status` and `report` inspect retained local state without providers.

A collection response submits inline text with a source-local key, title, claimed origin, optional provenance and gaps. Captures use `SourceOrigin::AgentReport`, preserving exactly the submitted bytes. A host-reported URL does not establish an observed HTTP fetch. The subsequent answer packet assigns short `passage_id` values to exact `CitationRef`/quotation pairs. An answer references those IDs; record IDs and quote hashes are not selectors. Valid citation bytes do not establish entailment. Claims stay `unassessed`, and research never accepts graph facts or applies authored pages automatically.

An answer may request a follow-up collection round. Its gaps replace the currently unresolved set; collection gaps are deduplicated and added to it. Immutable submission receipts preserve earlier reports and gap history. A requested continuation at the round ceiling yields a partial terminal report. The host should return a complete current answer, not an incremental patch to an earlier answer.

## Storage and guarded publication

The mutable typed Run record is `runs/RUN/research.md`; it intentionally does not use the paid-job `run.md` path or `.wiki/state/jobs`. Typed immutable RunEvent records under `runs/RUN/outputs/` hold packets, submissions/receipts and reports in one bounded `lwiki-agent-research-v1` fence. Output identity hashes run, generation, artifact role and canonical JSON; repeated report content in different generations cannot collide.

The run head binds vault/run identity, immutable scope hash, generation, round, lifetime capture counters, outstanding packet hash, latest report hash and consumed-packet receipt references. Packet fingerprints cover canonical packet JSON excluding the fingerprint itself. Loading validates scope/counters/identity/hash relationships. Neither a client-supplied packet nor an echoed fingerprint grants authority without the retained head and packet.

An import first normalizes and bounds its strict JSON. Under the normal writer permit, it recovers incomplete changes, then loads current head and packet. It checks a matching retained submission receipt **before** planning captures, because source planning allocates random IDs. An identical consumed submission returns its original capture IDs and current retained run status; a different submission for that packet conflicts. Retained immutable outputs are checked before reuse.

For new work, source/citation proofs and packet bytes become read preconditions; the run head uses a compare-and-swap expected hash. Captures, successor packet or report, receipt, counters and head are published in one guarded changeset. The head is ordered after the other writes. A failed response after durable application can therefore recover/reuse the same import. Concurrent different submissions cannot both advance the same head. Prepared changes without applying intent are not silently promoted by recovery.

Current source changes invalidate an outstanding packet. Refresh explicitly creates a new generation and fingerprint; withdrawn sources are omitted from its current passages. Retrying an older committed submission may succeed with `import_already_committed` / `freshness: stale` and no ready packet when later evidence changes invalidate current work. Report-only results use `freshness: retained`, not a claim that their sources remain current.

## Bounds and evidence selection

| Bound | Default / hard ceiling |
| --- | --- |
| Rounds | 3 / 8 |
| Imported sources over a run | 15 / 64 |
| Source bytes over a run | 524288 / 4 MiB |
| One submission JSON | 256 KiB |
| Inline sources per submission | 32 |
| Aggregate inline content per submission | 64 KiB |
| Packet passages / quotation bytes | 32 / 64 KiB |
| Newly imported or explicitly selected source passage | First 4096 UTF-8 bytes, ending at a character boundary |
| Claims / citations per claim | 32 / 16 distinct packet IDs |
| JSON nesting / values | 24 / 16384 |
| Packet/head/receipt/report JSON | 1 MiB |
| Packet generation / consumed imports | 64 / 16 |

Per-field byte checks, strict duplicate-key rejection and checked aggregate source arithmetic apply in addition to JSON schemas. Newly captured passages take priority over earlier passages. Omitted bounded-context candidates produce an explicit warning; complete captures remain available separately, and imports return all captured source IDs. A host needing a later portion of a document can supply a relevant excerpt as a separately identified agent-report source with honest excerpt provenance. A quotation hash checks integrity; it cannot extend the text an answer is authorized to cite.

## Offline, previews and accounting

All research invocations report `network_used: false` for lwiki. Host tool usage is `unobserved`, never zero or a guaranteed monetary bound. No research path opens provider configuration, resolves authentication or implicitly embeds/extracts imported content.

An offline run requests only local/already acquired material. An online collection packet is not returned for external acquisition under `--offline`; start an offline run to change that scope. An offline answer import cannot publish a new online collection task. Normal local writes are allowed in offline mode.

Planning and dry-run never create caches, packets, receipts, prepared changes or run state, and return no import readiness. Normal non-dry operations acquire the writer lock; a rejected operation may update that lock's operational metadata without changing canonical data. Resume recovers pending local application before returning import authority. Dry resume remains read-only and withholds readiness.

Operational research records are excluded from ordinary source retrieval. Captured sources participate in normal current/historical/withdrawn lifecycle handling. Proposing pages or structured graph facts is a separate explicit workflow.

Public schemas: `research-packet` and `research-submission`. See the [0.1.1 test-agent guide](../testing-0.1.1.md) and handoff/recovery integration tests for executable examples and failure cases.

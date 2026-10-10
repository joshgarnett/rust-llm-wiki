# CLI and skill technical contract

Status: proposed, 2026-09-28. Design only; examples are not runnable yet. `lwiki` remains the working binary name. The library owns behavior; terminal formatting and agent protocols adapt the same operations.

## Invocation, scope, and configuration

Global shape:

```text
lwiki [--wiki PATH] [--format human|json|jsonl] [--offline] COMMAND ...
```

`--json` aliases `--format json`; `--jsonl` aliases `--format jsonl`. Conflicting format flags are usage errors. Human output is the default on both terminals and redirected output, so scripts never depend on terminal detection for data shape. Color/progress adapt to terminal presence; explicit machine output never contains ANSI codes or progress text.

With `--wiki`, use exactly that vault directory and require `WIKI.md` except for initialization. Otherwise discover the nearest ancestor containing `WIKI.md`, stopping at the filesystem root. Never combine nested vaults silently. Resolve the vault root once, reject path escapes, and apply the storage design's symlink policy. Error if discovery fails; do not silently create a vault.

Precedence is command options, trusted user-local vault overrides, portable `WIKI.md` preferences, then built-in defaults. Portable preferences may name a locally permitted provider profile but cannot override credentials, endpoints, executable commands, TLS trust, or permission boundaries. No general environment-to-configuration mapping: document specific environment variables and explicit `key_env` references. `--offline` is final even if a profile or task requests network access.

IDs are case-sensitive opaque strings validated by storage. Read commands accept either an explicit `--id ID` or `--path PATH`, never a guessed title. Commands whose documented positional argument is an ID interpret it only as an ID. Names/aliases belong in search. Display paths relative to the vault; preserve exact UTF-8 spelling and flag unsupported/non-UTF-8 paths in v1 rather than silently lossily converting them.

## Initial command surface

Commands appear in `capabilities` only when implemented. The following is the implementation target, grouped by milestone rather than a claim of present support.

| Command | Input and outcome | Milestone |
|---|---|---|
| `init PATH` | Create `WIKI.md`, managed directories, cache excludes; refuse to overwrite an existing vault | M1 |
| `capabilities`, `schema NAME` | Versioned capability manifest or named JSON schema; no network | M1 |
| `read --id ID` or `read --path PATH` | Record metadata and bounded body; optional byte range on verified UTF-8 boundaries. Dry-run returns only a request plan. | M1 |
| `page put --file FILE` | Create a page; `--if-match HASH` required to replace an existing page | M1 |
| `page rename ID --to PATH --if-match HASH` | Changeset updating the path and known incoming links; ID preserved | M1 |
| `source add FILE` | Capture immutable bytes and normalized text for supported local formats | M1 |
| `source import prepare/run/resume/status` | Freeze a local JSONL list, publish bounded shared-change groups and retain progress/mappings | Subsequent workflow |
| `source refresh ID --file FILE` | Add a revision; unchanged capture is idempotent | M1 |
| `source refresh-batch --file REQUEST.json` | [Guarded 1–16 existing Source refreshes](../source-refresh-batch.md), one combined publication on a normalized catalog | Subsequent workflow |
| `source withdraw ID --reason TEXT` | Persist withdrawal and invalidate dependent current evidence | M1 |
| `evidence revalidate ID --to-revision REV --if-match HASH` | Stage successor evidence only when the original quote matches uniquely in the target source revision | M1 |
| `index sync`, `index rebuild` | Refresh/recreate local projections; never embed or extract | M1 |
| `search QUERY` | Ranked discovery, document-first lexical default | M1 |
| `graph neighbors ID`, `graph query QUERY` | Page/provenance navigation or typed graph search | M1 |
| `context QUERY` | Select verified evidence under byte/token budgets | M1 |
| `check`, `doctor` | Knowledge/format diagnostics; installation/index/profile diagnostics | M1 |
| `changes show ID`, `changes apply ID` | Inspect exact payloads, then apply with expected hashes and recovery journal | M1–M2 |
| `changes abort ID`, `changes rollback ID` | Abort an unapplied preparation; or stage a journaled inverse guarded by the original proposed hashes | M1 |
| `recover` | Reconcile incomplete local operations; preserve unfamiliar edits | M1 |
| `graph extract --executor agent` | Persist a bounded extraction packet and return it to the caller; does not launch a host | M2 |
| `graph import --file FILE` | Validate packet-bound extraction output and stage proposed records | M2 |
| `graph resolve --file RESOLUTION.json` | Stage explicit bindings from source-local mentions to existing or new entities | M2 |
| `graph review --file REVIEW.json` | Stage recorded acceptance/rejection and evidence assessments by an authorized human or agent | M2 |
| `graph decide --file DECISION.json` | Stage explicit entity merges, splits, or alias additions with expected hashes and complete reference remaps | M2 |
| `skill export --target HOST --output DIR` | Export the maintained portable usage skill for explicit installation | M2 |
| `embeddings check`, `embeddings sync` | Validate/probe the configured profile; explicitly generate missing vectors | M3 |
| `graph extract --executor api` | Execute the same extraction contract through the generation adapter | M3 |
| `research plan`, `run`, `resume`, `import`, `status`, `report` | Local [agent handoffs](research-handoffs.md), guarded imports and cited reports | M4 |

`--file -` reads bounded stdin. Body text, bulk payloads, and secrets are not interpolated into shell strings. No command depends on an editor, pager, `jq`, or `rg`. Editor launching can follow after the deterministic file/stdin write path exists. No default destructive purge command or automatic Git commit is needed for v1.

## Source citations on draft Pages

This interface passed [scoped functional acceptance](../validation-cited-page-workflow.md)
on both storage layouts. Earlier trial archives do not advertise it.

`page init` and `page put` accept `--source-refs FILE`, or bounded stdin when the
Page body is not also stdin. `schema page-source-refs` describes the strict
version-1 request: `schema_version: "1"` and a `citations` array of exact
`kind: "source"` references returned by verified context/read. The input ceiling
is 64 KiB and 16 submitted references before stable exact deduplication. Paths,
quote text, eligibility and assertion references are not accepted authority.

Normal staging/application authenticates source ownership, immutable revision
payloads, nonempty UTF-8 spans and quote hashes. It renders actual Source,
Revision and captured-content links relative to the final Page destination,
with exact JSON references and a state explicitly observed during that guarded
write. Those observations participate in publication read guards; they do not
prove prose support or continuing freshness. Normalized verification touches
selected dependencies. Legacy verification retains its bounded SourceView scan;
its initial scan and selected-payload allowance are separate 64 MiB limits.

Only the standalone comment-delimited `lwiki:source-citations:v1` block is owned.
Explicit references replace it without changing other proposed bytes; an empty
array removes it. Duplicate, malformed, fenced or embedded HTML markers refuse,
as does a rendered block swallowed by an unclosed authored fence/comment.
Omitting the flag retains ordinary Page authoring. Existing whole-file
`--if-match` guards and the 16 MiB final Page limit remain. Ordinary Page rename
rebases generated hrefs while preserving exact SourceRef JSON and immutable
targets; arbitrary authored citation links are not automatically repaired.

Dry-run parses/bounds references and previews the ordinary Page request without
source proof or generated links. Its `source_citations.verification_performed`
and `links_rendered` fields are false, and source-dependent paths/spans/states
remain unchecked. Normal metadata explicitly declines prose-support verification.
The [cited-draft recipe](../../skills/llm-wiki/references/cited-page.md) describes
retrieval, saving and guarded reconciliation together.

Verified captured-content reads on both layouts return exact reusable SourceRefs
in `data.source_citation.citation`, with observed eligibility alongside. Cached,
dry-preview, empty and authored-note reads remain uncited.

## Search and evidence controls

`search --mode literal|lexical|semantic|hybrid` uses lexical by default. Literal v1 is a case-sensitive exact UTF-8 substring scan, including straight versus typographic quotes; use lexical search for names with punctuation variants. Regex is deferred. `graph query --strategy entity|relationship|combined --seed lexical|semantic` defaults to combined plus lexical. `search --mode hybrid --graph entities` enables the included entity/assertion candidate path. Entity extraction and LLM query expansion never run implicitly during these commands.

Common filters include record kind, tag, source ID, and path prefix. Initial defaults are proposed product settings: ten displayed hits, maximum fifty per page; graph depth one, maximum two; context target 3,000 tokens with a 12,000-byte ceiling. Retrieval defines the larger candidate budgets behind these output limits. User limits can lower defaults; higher ceilings require a later explicit contract change. Results show truncation; the system never implies that a bounded graph search exhaustively found all relevant facts.

CLI excerpts default to 240 UTF-8 bytes per search excerpt and 1,024 per context excerpt. An explicit `--excerpt-bytes` overrides either default; all context text remains inside the requested total byte/token budgets. Query-aware excerpt selection preserves original byte spans and exact quote hashes; semantic focusing stays inside the selected embedding unit.

`--include-proposed` and `--include-historical` broaden discovery, with authored state and derived eligibility on each hit. They do not turn a proposed, stale, withdrawn, or unsupported assertion into current evidence. `context --scope current|historical|snapshot|indexed-evidence|indexed-documents` defaults to `current` on legacy indexes and `indexed-documents` on selected normalized indexes. The latter verifies selected authored/captured dependencies while discovery remains tied to the published generation. Explicit `current` keeps its strict guarantee and is currently unavailable on normalized indexes. Historical scope still verifies exact recorded bytes and references but permits historical revisions, labeled with their status; snapshot scope permits unverified index data and cannot claim present validity. The opt-in [indexed-evidence scope](../indexed-context.md) searches existing indexed captured sources, verifies selected canonical bytes and explicitly declines global membership/completeness verification. It is lexical, read-only and source/path-filtered; authored pages, graph and host selection are unsupported. No implicit model answer generation occurs in `context`.

Read-side verification follows storage's manifest/dependency rules. On legacy indexes, default mode synchronizes locally and returns a verified snapshot/time. On normalized indexes, plain lexical search returns generation-scoped discovery, plain read verifies only selected dependencies, and `--no-sync` read explicitly returns cached bytes. Cached results are labeled `index_snapshot`; selected proofs are labeled `indexed_evidence`. A budgeted or stale read cannot claim the current-evidence guarantee. For `context`, `--no-sync` requires `snapshot`, `indexed-evidence` or `indexed-documents` scope; the indexed scopes verify selected dependencies while retaining generation-scoped discovery. Historical scope still requires verification of the referenced historical bytes. Literal scans bypass ranking indexes but still label whether they are discovery text or verified evidence.

Normalized `graph neighbors ID` implements [selected named Entity lookup](../named-neighbors.md)
with Current assertions and complete selected dependency verification by default.
`--no-sync` is cached and uncited; `--verify-selected` explicitly verifies selected
dependencies even with `--no-sync`. General graph queries, semantic seeds,
historical/proposed views, navigation and cursors remain unavailable on normalized
catalogs. Graph dry-run on either layout now validates the request before catalog
or target lookup, returning null results and explicit unknown layout/freshness;
normal vault/configuration binding still occurs. Regular legacy graph commands
retain their established snapshot semantics.

Pagination cursors encode schema version, index generation, query/filter fingerprint, and deterministic continuation position. A changed generation or query invalidates the cursor; return `CURSOR_STALE` instead of mixing snapshots. Deterministic rank ties use stable record IDs and unit IDs. Do not persist an unbounded cursor cache.

## Structured output

One JSON result envelope per nonstreaming invocation:

```json
{
  "schema_version": "1",
  "command": "search",
  "ok": true,
  "data": {"hits": [], "next_cursor": null},
  "meta": {
    "wiki_id": "vault-example",
    "index_generation": 7,
    "freshness": "verified_snapshot",
    "verified_at": "2026-09-28T16:00:00Z",
    "partial": false,
    "network_used": false
  },
  "warnings": [],
  "error": null
}
```

Commands that submit joined maintenance jobs also return optional `meta.maintenance`: `submitted`, `completed`, `failed`, `panicked`, `max_active`, `max_in_flight_bytes`, `worker_service_ns`, `joined_batch_wall_ns`, `owner_activation_validations`, `owner_activation_reads` and `owner_activation_read_bytes`. This counts admitted work, including failures, with at most16 workers and1GiB of admitted logical worker workspace. Worker service durations overlap; neither their sum nor joined-batch time is total command elapsed time. Owner activation counters separately report complete validation attempts, read attempts and actual bytes observed while building the shared proof, including partial failed reads. Metadata is omitted when neither jobs nor owner activation validation were attempted. The workspace high water counts owned buffers, results, descriptors and reserved scratch; it is not process RSS. Native qualification separately measures the full process against its memory gate. These counters do not imply workflow or durability qualification.

Commands without a vault/index use null for those metadata fields. An internal search hit serializes `record_ref`, path, title, kind, authored status, eligibility, excerpt, source/evidence references, and retrieval reasons. Scores are labeled by channel; no unexplained cross-channel numeric score is exposed as confidence. Graph hits add seed, directed path, predicate/qualifiers, support, contradictions, and coverage. Citation references are tagged `source` for a direct `SourceSpanRef`, or `assertion` for an `EvidenceRef`. Both preserve source/revision, byte span, and quote hash; assertion citations additionally carry the evidence/assertion IDs. Ordinary note excerpts remain labeled note text, not fabricated source evidence.

Failures use the same envelope with `ok: false` and `error: {code, message, retryable, hint, details}`. `data` is normally null, but may contain a preserved run/change ID and partial outcome. Details are typed per code and redact secrets, helper output, provider bodies, and sensitive URLs. User paths may be displayed; credential file contents may not.

JSONL supports long-running commands only. Each line has `schema_version`, invocation ID, monotonically increasing sequence, event type, and typed data. Events include `started`, `progress`, `checkpoint`, `warning`, and `completed`; `completed` embeds the normal envelope and is the terminal event when orderly shutdown is possible. SIGKILL or an I/O failure can prevent it, so absence means unknown/incomplete, not success. Streaming events are presentation; durable job events remain the recovery authority.

Stderr is for concise diagnostics/progress. With JSON/JSONL, it never carries a second machine protocol. Human output prioritizes matched paths, readable excerpts, provenance, changes made, unresolved issues, and a useful next command. `read` human mode can emit Markdown; `--json` wraps the record and requested body during an actual read. Dry-run instead returns the supplied selector, requested range, effective byte bound and requested mode, with null body/citation and explicit unperformed target/range/verification checks. It does not resolve the target or open the catalog; human output describes the plan. Missing targets remain unverified rather than failing a request preview.

## Error and exit contract

| Exit | Error family/examples | Caller behavior |
|---|---|---|
| `0` | Success, including zero search hits | Consume data; inspect warnings/partial flags |
| `1` | `INTERNAL`, unexpected I/O failure | Preserve run/change IDs and inspect diagnostics |
| `2` | `USAGE`, `CONFIG_INVALID` | Correct arguments/local profile |
| `3` | `VAULT_NOT_FOUND`, `RECORD_NOT_FOUND` | Correct root or reference |
| `4` | `CONTENT_CONFLICT`, `CURSOR_STALE`, `FRESHNESS_CONFLICT`; `LOCK_TIMEOUT` | Reread/sync for state conflicts; retry a lock timeout after the active writer finishes |
| `5` | `RECOVERY_REQUIRED`, `INDEX_CORRUPT`, `SOURCE_INTEGRITY` | Recover/rebuild projections or repair source evidence |
| `6` | `CAPABILITY_UNAVAILABLE`, `OFFLINE_UNAVAILABLE`, `PROFILE_UNTRUSTED` | Configure explicitly or choose supported offline behavior |
| `7` | `BUDGET_EXCEEDED` | Read preserved partial result; resume only under a new applicable budget |
| `8` | `PROVIDER_AUTH`, `PROVIDER_RATE_LIMIT`, `PROVIDER_RESPONSE`, `PROVIDER_UNAVAILABLE` | Follow retryability and provider-specific hint |
| `9` | `RECORD_INVALID`, `REFERENCE_AMBIGUOUS`, `EXTRACTION_INVALID` | Repair source/record/packet, not blind retry |
| `130` | `CANCELLED` | Inspect durable job state; in-flight billing may remain unknown |

`LOCK_TIMEOUT` is retryable and distinct from a stale content hash. Writer-lock waiting defaults to 5000 ms, configurable with local preferences or `--lock-timeout-ms`.

Limit truncation during ordinary discovery is a successful bounded query, with `partial: true`. A requested complete operation that stops at its budget returns exit 7 with its durable partial result. `check` reports diagnostics as data and returns 9 for error-level invalid records; warnings alone do not fail it. `doctor` does not probe providers unless `--probe` is explicit.

With a normalized catalog, explicit `check` holds the writer permit and compares
current canonical input with every stored record family, retained revision-owner
authority and the complete document/graph search indexes. It reads the selected
database without repairing, synchronizing or publishing it. A private temporary
database stores search postings and consumed row identities, without another copy
of document bodies. Source input and publication authority are rechecked before
success. Managed writes wait while this maintenance command holds the permit;
ordinary reads can continue.

Native integrity checking admits the exact optional compact retrieval-inventory
schema as well as older catalogs without it. Canonical agreement covers the
record relations and search indexes described above; it does not certify the
semantic completeness of prepared unit descriptors, owner acknowledgments or
the retained vector cache. Preparation and selected query verification check
those derived inputs separately.

The result separates `canonical_check_performed`,
`cache_integrity_check_performed`, `cache_matches_canonical`, `complete` and
`checked_snapshot`. A faithful cache can agree with invalid canonical documents;
those diagnostics still produce exit 9. A partial, corrupt or resource-limited
audit cannot report agreement. Error details identify the failed phase, known
snapshot and scratch cleanup result; cleanup failures preserve their owned path.
Legacy `check` retains canonical diagnostics with cache checking explicitly
unperformed. Dry-run skips both scans and returns `complete: false`.

Normalized checks have a cooperative 30-minute deadline, finite input/work limits,
32 MiB SQLite caches per connection and a 32 GiB temporary-database growth cap.
The cap does not require 32 GiB free for a small vault. Fixed comparison query
plans refuse temporary sorting. These are admission limits, not measured peak
RSS or large-vault throughput guarantees. Retained owner checking does not audit
every unused historical payload. Use `index sync` for external edits or
`index rebuild` to replace a corrupt cache.

`doctor` is a bounded status observation. Its `check` is null; canonical, history
and cache-integrity checks are marked unperformed, with canonical freshness
unknown. `cache_state: header_available` means the selected header was read,
not that the vault or index passed a full audit. A legacy WAL database without
its ordinary sidecars is `present_uninspected`: this can follow a clean close,
and doctor leaves it unopened to avoid creating sidecars. An active normalized
operation is reported separately in `active_change`; empty history arrays do
not establish that no unresolved changes exist. Use explicit `check` for
canonical diagnostics and `recover` for recovery. Dry-run skips the cache
observation and any requested provider probe.

Legacy header inspection also requires a nonblocking shared lock on the existing
writer lock. A missing or busy lock yields `present_uninspected` without opening
SQLite or creating a lock file; retry after an active writer finishes.

## Mutation and dry-run behavior

All writes route through storage's expected-version changeset API. A caller can stage and later apply, or request a direct authorized single operation whose implementation uses the same journal. Normal complete invocations do not prompt for repeated approval. Conflicts never silently downgrade to unconditional overwrite. Applying model-produced changes is explicit, and import schema validity is distinct from accepting a factual assertion.

M2's complete path is packet export → import → apply → resolve → apply → review → apply. Each apply materializes the IDs/hashes required by the following step. This may run as one authorized agent workflow; it requires no per-step human prompt. V1 has no implicit staged overlay. M1 tests use a prepared canonical Markdown fixture vault, not the later extraction-wire importer.

Repeated identical imports reuse the staged/completed change and allocated IDs. A different response for the same packet returns a conflict unless `graph import --new-extraction` explicitly requests a separate extraction, keyed by packet and response hash. This preserves earlier extraction records and acceptance/rejection decisions.

`evidence revalidate` checks exact quotation bytes against the specified new revision and stages a successor; it never retargets the old evidence or silently changes assertion acceptance. Zero/multiple matches require correction. A rollback stages inverse operations and never replaces unfamiliar bytes; applying it uses the same journal and validation as any other changeset. Immutable captured bytes remain retained for history.

`--dry-run` guarantees no mutation, provider request, credential helper, directory creation, or index refresh. It may read existing files/caches and return an estimate/plan; if fresh information would require prohibited work, mark it unknown. A normal read may update local projections as documented; a dry-run cannot. `--offline` forbids network/helper execution but still permits explicit local writes.

Cancellation stops scheduling new work and asks active workers to stop. It does not promise that a dispatched provider call is cancelled or unbilled. The CLI flushes completed receipts/checkpoints when it can and reports the job's terminal/paused state from the jobs layer.

## Skill packaging and compatibility

Maintain one release-matched skill source, with a concise `SKILL.md` and on-demand command/research references. M2 generates examples from the implemented command/schema registry; no aspirational command goes into the installed skill. Export explicit target layouts described in [agent integration](../agent-integration.md), avoiding duplicate discovery roots. Do not overwrite existing host instruction files.

The skill checks capabilities, locates `WIKI.md`, retrieves bounded evidence, requests extraction packets, imports results, preserves unresolved identities/contradictions, validates expected hashes, and applies authorized changes. It distinguishes local embedding storage from remote embedding generation, and advisory host-agent spend from CLI-enforced budgets. It does not instruct agents to install Python/Node, host a model, or reread the whole vault.

Acceptance cases cover each host's discovery, a negative-control unrelated task, literal and relationship queries, a heading rename, a homonym, source withdrawal, repeated-source deduplication, conflicting edits, unavailable embeddings, and a budget stop. Skill examples must execute against the release binary in implementation CI. The design step only validates document/schema examples; it cannot establish live host compatibility.

# CLI and skill technical contract

The library owns command behavior. Terminal output and agent instructions use the same operations.

## Invocation, scope, and configuration

Global shape:

```text
lwiki [--wiki PATH] [--format human|json|jsonl] [--offline] COMMAND ...
```

`--json` aliases `--format json`; `--jsonl` aliases `--format jsonl`. Conflicting format flags are usage errors. Human output is the default on both terminals and redirected output, so scripts never depend on terminal detection for data shape. Color/progress adapt to terminal presence; explicit machine output never contains ANSI codes or progress text.

With `--wiki`, select exactly that regular wiki root. Otherwise select the nearest
ancestor marker, then an identity-pinned user default only when no ancestor marker
exists. Invalid explicit/nearer/default targets fail rather than falling through.
Resolve one immutable selection context per command; mutation guards verify
`--expect-wiki-id` independently of whole-file author hashes.

Load one bounded user configuration snapshot. Ambient discovery checks
`$XDG_CONFIG_HOME/lwiki/config.toml`, otherwise `$HOME/.config/lwiki/config.toml`.
Relative XDG is rejected. If TOML is absent, read the sibling legacy `config.json`.
When both exist, TOML wins and inspection reports the preserved legacy path.
Invalid winning TOML never falls back. Explicit `--preferences FILE` (`--config`)
replaces ambient discovery, including with `--no-user-config`. A `.toml` suffix
selects TOML; historical other filenames select JSON, without parse fallback.

TOML uses schema3; JSON schemas1,2,3 remain readable. Legacy versions reject new
provider/default tables. Reads never migrate. Setup can explicitly migrate JSON
to TOML, preserving the exact original and a versioned backup. Conflicting
backup/destination bytes refuse untouched. Publication and durable sync are
separate outcomes; retries verify unchanged files and confirm durability.
Schema3 permits an optional absolute `providers_config` in each `vaults.ID`
entry; `[defaults].providers_config` and portable authority paths remain invalid.
Schema1/2 JSON rejects scoped field presence, including null. Migration carries
scoped paths and other wiki preferences together. Older strict binaries reject
schema3 documents using this extension; downgrade requires compatible settings.

Precedence is explicit CLI options, ID-keyed user vault overrides, user defaults,
portable WIKI.md preferences, then built-ins. Explicit offline is final. Global
user config can select a private provider file and embedding/new-job defaults;
the selected authenticated wiki ID can override only that private file selection.
Setup and provider edits persist the selected wiki's file/profile pair without
changing the global provider fallback. CLI provider paths take precedence over
wiki paths, then the global path, then the documented platform fallback.
Portable wiki preferences cannot provide endpoints, credentials, helpers or grants.
Every remote consumer uses the resolved profile. `--no-user-config` also disables
ambient provider authority lookup unless an explicit preferences file was selected.
Retained jobs keep their limits/accounting; configured defaults are not CLI budget
amendments. Explicit cost overrides use the effective currency; amendments default
to a retained cost currency before the user-configured currency.

The default wiki contains its canonical path and expected ID. Help/version/schema
lookup does not require user configuration. An invalid config reports its path and
a bypass. Unused defaults are not opened for an explicit/ancestor selection.

Init/default registration are separate durable outcomes. Init requires explicit
existing-folder intent and all reserved top-level entries absent before writer state
or directory mutation, including preview. Physical inspect/edit and path-selected
Source refresh follow the [current adapter contracts](../current-contracts.md).

## Command surface

`capabilities` is the command registry. This table describes common operations;
use each command's `--help` for its arguments.

| Command | Input and outcome |
|---|---|
| `setup [PATH]`, `config show` | Guided persistent wiki/provider/agent setup and safe configuration inspection |
| `init PATH [--existing] [--set-default]` | Preflight reserved names, preserve author/editor files; publish a complete new root or a complete marker before managed state in an existing folder; register fallback separately |
| `capabilities`, `schema NAME` | Versioned capability manifest or named JSON schema; no network |
| `read --id ID` or `read --path PATH` | Record metadata and bounded body; optional byte range on verified UTF-8 boundaries. Dry-run returns only a request plan. |
| `page put --file FILE` | Create a page; `--if-match HASH` required to replace an existing page |
| `page rename ID --to PATH --if-match HASH` | Changeset updating the path and known incoming links; ID preserved |
| `source capture PATH... [--files-from FILE]` | Capture or refresh local notes in grouped publications; return Source IDs and exact citations |
| `source read ID [--revision REV]` | Read current or historical captured text directly by Source ID, with exact citation and continuation |
| `source add FILE` | Capture immutable bytes and normalized text for supported local formats |
| `source import prepare/run/resume/status` | Freeze a local JSONL list, publish bounded shared-change groups and retain progress/mappings |
| `source refresh ID --file FILE` | Add a revision; unchanged capture is idempotent |
| `source refresh-batch --file REQUEST.json` | [Guarded 1–16 existing Source refreshes](../../user/sources-and-providers.md#refresh-a-guarded-batch), one combined publication on a normalized catalog |
| `source withdraw ID --reason TEXT` | Persist withdrawal and invalidate dependent current evidence |
| `evidence revalidate ID --to-revision REV --if-match HASH` | Stage successor evidence only when the original quote matches uniquely in the target source revision |
| `index sync`, `index rebuild` | Refresh/recreate local projections; never embed or extract |
| `search QUERY` | Hybrid discovery with local fallback when mode is omitted; explicit modes retain their contracts |
| `graph neighbors ID`, `graph query QUERY` | Page/provenance navigation or typed graph search |
| `context QUERY` | Select verified evidence under byte/token budgets |
| `check`, `doctor` | Knowledge/format diagnostics; installation/index/profile diagnostics |
| `changes show ID`, `changes apply ID` | Inspect exact payloads, then apply with expected hashes and recovery journal |
| `changes abort ID`, `changes rollback ID` | Abort an unapplied preparation; or stage a journaled inverse guarded by the original proposed hashes |
| `recover` | Reconcile incomplete local operations; preserve unfamiliar edits |
| `graph extract --executor agent` | Persist a bounded extraction packet and return it to the caller; does not launch a host |
| `graph import --file FILE` | Validate packet-bound extraction output and stage proposed records |
| `graph resolve --file RESOLUTION.json` | Stage explicit bindings from source-local mentions to existing or new entities |
| `graph review --file REVIEW.json` | Stage recorded acceptance/rejection and evidence assessments by an authorized human or agent |
| `graph decide --file DECISION.json` | Stage explicit entity merges, splits, or alias additions with expected hashes and complete reference remaps |
| `skill export --target HOST --output DIR` | Export the maintained portable usage skill for explicit installation |
| `embeddings check`, `embeddings sync` | Validate/probe the configured profile; explicitly generate missing vectors |
| `graph extract --executor api` | Execute the same extraction contract through the generation adapter |
| `research plan`, `run`, `resume`, `import`, `status`, `report` | Local [agent handoffs](research-handoffs.md), guarded imports and cited reports |

`--file -` reads bounded stdin. Body text, bulk payloads, and secrets are not interpolated into shell strings. No command depends on an editor, pager, `jq`, or `rg`. Editor launching can follow after the deterministic file/stdin write path exists. No default destructive purge command or automatic Git commit is needed for v1.

## Grouped local capture and direct source reads

`source capture` accepts files/folders and an optional UTF-8 newline-delimited
`--files-from` list whose relative entries resolve from the list's folder. Folder
selection includes visible regular `.md`, `.markdown` and `.txt` files; links,
hidden directories, managed wiki trees and managed canonical notes are excluded.
Explicit files retain the existing supported/unsupported extraction behavior.
Selection freezes deterministic original bytes/hashes before publication, deduplicates
overlapping paths and reports excluded inputs and errors.

Invocation ceilings are 10,000 files, 100,000 traversal entries, depth 32, 16 MiB per
file, 256 MiB total input and 16 MiB result metadata. Origin lookup has a separate
64 MiB/4096-file SourceView allowance. Those are admission bounds, not qualified
capacity or a whole-command latency guarantee.

The canonical Source envelope binds the absolute canonical original path. Repeating
unchanged capture reuses its Source/revision; changed bytes add a revision while
preserving identity/title and immutable history. Different paths with identical
bytes remain distinct Sources. Missing remembered originals are reported without
removing their capture. Duplicate absolute bindings refuse. Legacy relative origins
are not guessed from today's working directory; explicit `source refresh` remains
the update route for those bindings.

One writer permit covers lookup/allocation and true grouped publication on both
layouts. New groups hold up to eight files, refresh groups sixteen, with a 4 MiB
original-input target; a larger otherwise supported file forms a group alone.
Ordinary engine validation/read guards and normalized sealed projections remain
required. A failed group is not retried as scalar writes. Earlier committed groups
remain saved, later inputs report not-attempted and retry reuses existing bindings.
Per-item identity, action, capture state, citation and error plus aggregate partial
status describe actual results. Partial capture returns `ok:false`, `meta.partial:true`
and a nonzero status while retaining successful groups. A committed item with an acknowledgment error can
contribute to both committed-action and error counts.

Supported nonempty captures return exact whole-document SourceRefs after final
source/dependency verification and membership recheck. Unsupported originals remain
retained without invented readable text or citations. One optional automatic hook
receives the union of actual changed owners after capture, including earlier
committed groups on partial failure; the capture engine itself calls no provider.

`source read ID` resolves the current captured head; `--revision` must belong to
that Source's retained history. `--start`, optional `--end` and `--max-bytes` select
UTF-8-safe text and return exact reusable citation bytes/hash with continuation and
observed eligibility. Empty text has no fabricated citation; unsupported text gives
a useful absence error. Reads are writer-free, preserve current/selected proof and
final rechecks, and distinguish current observations from real persistent generations
without synthetic publication values. Generic `read --id` keeps its existing meaning.
Dry-run returns only its validated request plan, with no text/citation verification.

## Source citations on draft Pages

`page init` and `page put` accept `--source-refs FILE`, or bounded stdin when the
Page body is not also stdin. `schema page-source-refs` describes the strict
version-1 request: `schema_version: "1"` and a `citations` array of exact
`kind: "source"` references returned by verified context/read. The input ceiling
is 64 KiB and 16 submitted references before stable exact deduplication. Paths,
quote text, eligibility and assertion references are not accepted authority.

Normal staging/application authenticates source ownership, immutable revision
payloads, nonempty UTF-8 spans and quote hashes. It renders a compact Sources list
with authenticated titles linked to the exact cited captured content, relative to
the final Page destination. Older saved copies
and withdrawn sources carry plain status labels; a brief qualification states that
status was checked when the Page was saved. Exact JSON references remain losslessly
encoded in a hidden HTML comment inside the existing owned block. Those
observations participate in publication read guards; they do not prove prose
support or continuing freshness. Normalized verification touches
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
The [cited-draft recipe](../../../skills/llm-wiki/references/cited-page.md) describes
retrieval, saving and guarded reconciliation together.

Verified captured-content reads on both layouts return exact reusable SourceRefs
in `data.source_citation.citation`, with observed eligibility alongside. Cached,
dry-preview, empty and authored-note reads remain uncited.

## Search and evidence controls

`search --mode literal|lexical|semantic|hybrid` uses lexical by default. Literal v1 is a case-sensitive exact UTF-8 substring scan, including straight versus typographic quotes; use lexical search for names with punctuation variants. Regex is deferred. `graph query --strategy entity|relationship|combined --seed lexical|semantic` defaults to combined plus lexical. `search --mode hybrid --graph entities` enables the included entity/assertion candidate path. Entity extraction and LLM query expansion never run implicitly during these commands.

Common filters include record kind, tag, source ID, and path prefix. Built-in output defaults are: ten displayed hits, maximum fifty per page; graph depth one, maximum two; context target 3,000 tokens with a 12,000-byte ceiling. Retrieval defines the larger candidate budgets behind these output limits. User limits can lower defaults; higher ceilings require a later explicit contract change. Results show truncation; the system never implies that a bounded graph search exhaustively found all relevant facts.

CLI excerpts default to 240 UTF-8 bytes per search excerpt and 1,024 per context excerpt. An explicit `--excerpt-bytes` overrides either default; all context text remains inside the requested total byte/token budgets. Query-aware excerpt selection preserves original byte spans and exact quote hashes; semantic focusing stays inside the selected embedding unit.

`--include-proposed` and `--include-historical` broaden discovery, with authored state and derived eligibility on each hit. They do not turn a proposed, stale, withdrawn, or unsupported assertion into current evidence. `context --scope current|historical|snapshot|indexed-evidence|indexed-documents` defaults to `current` on legacy indexes and `indexed-documents` on selected normalized indexes. The latter verifies selected authored/captured dependencies while discovery remains tied to the published generation. Explicit `current` keeps its strict guarantee and is currently unavailable on normalized indexes. Historical scope still verifies exact recorded bytes and references but permits historical revisions, labeled with their status; snapshot scope permits unverified index data and cannot claim present validity. The opt-in [indexed-evidence scope](../../user/retrieval.md#search-and-inspect-cited-context) searches existing indexed captured sources, verifies selected canonical bytes and explicitly declines global membership/completeness verification. It is lexical, read-only and source/path-filtered; authored pages, graph and host selection are unsupported. No implicit model answer generation occurs in `context`.

Read-side verification follows storage's manifest/dependency rules. On legacy indexes, ordinary local read and literal/lexical search, context and graph lookup verify readable canonical files using a bounded in-memory view, without acquiring a writer permit or opening the saved SQLite index. This does not publish an index generation. Explicit semantic/hybrid operations retain their provider and maintenance requirements. Explicit `--no-sync` remains a cached, unverified view; its existing SQLite path can require coordination-file writes and is not a workaround for a read-only sandbox. On normalized indexes, plain lexical search returns generation-scoped discovery, plain read verifies only selected dependencies, and `--no-sync` read explicitly returns cached bytes. Cached results are labeled `index_snapshot`; selected proofs are labeled `indexed_evidence`. A budgeted or stale read cannot claim the current-evidence guarantee. For `context`, `--no-sync` requires `snapshot`, `indexed-evidence` or `indexed-documents` scope; the indexed scopes verify selected dependencies while retaining generation-scoped discovery. Historical scope still requires verification of the referenced historical bytes. Literal scans bypass ranking indexes but still label whether they are discovery text or verified evidence.

Normalized `graph neighbors ID` implements [selected named Entity lookup](../../user/retrieval.md#inspect-named-neighbors)
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

One output-v2 JSON result envelope per nonstreaming invocation. The closed output-v1 schema remains published for older output; adding selection metadata is not a backward-compatible v1 extension. `meta.selection` reports the canonical root, actual wiki ID, origin, requested path and non-secret configuration/preference origins. Physical inspection, structured freshness, health and partial outcomes have distinct semantics.

Example (selection omitted here for brevity):

```json
{
  "schema_version": "2",
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

Maintain one release-matched skill source, with a concise `SKILL.md` and on-demand command/research references. Generate examples from the implemented command/schema registry; describe only commands the installed binary provides. Export explicit target layouts described in [agent integration](../../user/integration.md#connect-your-agent), avoiding duplicate discovery roots. Do not overwrite existing host instruction files.

`skill install`, `status`, `update` and `remove` select an explicit host and
`--scope user|project`. User scope defaults to HOME; project scope defaults to
the current directory. `--directory` selects the scope root, not a final skill
folder. Setup uses the same operation with `--agent-scope` and `--agent-directory`.
Neither route edits existing root instruction files such as AGENTS.md or CLAUDE.md.

One immutable package is shared by installed host links. Codex and Cursor share
`.agents/skills/llm-wiki`; Claude Code uses `.claude/skills/llm-wiki`. User packages
live under `.local/share/lwiki/skills`; project packages remain inside the hidden
`.agents/skills/.lwiki` subtree so their agents link resolves within its discovery
root. Keep host selections separate from physical links: removing Codex must
preserve the shared link while Cursor is selected, and vice versa.

Ownership records contain only current state and at most one pending operation.
Before changing a public link, save its staged identity durably. Interrupted
operations verify recorded file identities and exact bytes or link targets before
continuing; matching a target string alone does not establish ownership. Cleanup
uses the exact unchanged files named by that operation. Unknown or edited files
remain untouched. Successful changes do not accumulate a history that eventually
blocks updates. Status and preview do not repair or create files.

Installation reports distinguish applied changes, confirmed durability and failed
or pending work. A successful file installation does not prove that a host has
loaded or used the instructions. Validate discovery and invocation separately.

The skill checks capabilities, locates `WIKI.md`, retrieves bounded evidence, requests extraction packets, imports results, preserves unresolved identities/contradictions, validates expected hashes, and applies authorized changes. It distinguishes local embedding storage from remote embedding generation, and advisory host-agent spend from CLI-enforced budgets. It does not instruct agents to install Python/Node, host a model, or reread the whole vault.

Acceptance cases cover each host's discovery, a negative-control unrelated task, literal and relationship queries, a heading rename, a homonym, source withdrawal, repeated-source deduplication, conflicting edits, unavailable embeddings, and a budget stop. Skill examples must execute against the release binary in implementation CI. The design step only validates document/schema examples; it cannot establish live host compatibility.

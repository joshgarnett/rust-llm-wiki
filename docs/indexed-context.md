# Context from an existing index

`context --scope indexed-evidence` searches the published index for captured-source
text and verifies the selected sources against their canonical files. It is an
opt-in, offline lexical workflow. On legacy indexes, the default `current` scope retains whole-vault
verification and local synchronization. On explicitly activated normalized indexes,
the default is `indexed-documents`, described below.

```sh
lwiki --wiki /path/to/wiki --offline index sync
lwiki --wiki /path/to/wiki --offline context 'release checklist' --scope indexed-evidence
```

The index must already exist. A missing index is an error; this context command
never acquires a writer permit, synchronizes, repairs or rebuilds it. A cache made
before the required source indexes were introduced returns a capability error
with an explicit `index rebuild` instruction. Rebuilding an index is a separate
operation; inspect its vector-cache implications before choosing it.

The connection opens read-only and does not change canonical files, operational
journals or catalog rows. SQLite may create an empty `index.sqlite-wal` and
maintain `index.sqlite-shm` for WAL reader coordination; this is not a promise of
zero filesystem writes. Existing committed WAL data remains part of the pinned
snapshot. Dry-run and rejection before opening the cache create neither file.
[SQLite read-only WAL databases](https://www.sqlite.org/wal.html#read_only_databases).

## What the result establishes

Both JSON metadata and rendered context identify `indexed_evidence`. The result
records its discovery generation, `captured_sources` evidence domain, verification
time and decoded catalog row/byte counters. `global_membership_verified` is false.
The scope warning consumes the same rendered-text budget as the passages.

The command checks the vault marker, selected source and revision records,
current source heads, retained revision membership, provenance relationships,
and complete original/content hashes. It authenticates cached text and metadata
against those captured files, checks final merged spans and quote hashes, then
rereads the selected dependencies before emission. Changed selected data causes
an error with no verified context and no automatic refresh retry. Present
source/revision companion links must resolve to their selected captured targets;
stale companion navigation is refused in this scope.

Discovery and eligibility originate in the pinned generation. The checks do not
prove that all current sources are indexed, that IDs are globally unique, that
unselected files remain unchanged, or that the result contains every fact needed
to answer a question. An external addition may remain undiscovered until an
explicit sync. Empty output does not prove that the vault lacks the answer.
Verification is an observation over sequential reads, not a filesystem-wide
transaction or a promise about edits after the final read.

## Supported requests and limits

This scope supports lexical document queries and source-ID/path-prefix filters.
It excludes authored pages, entities, graph context, semantic/hybrid/literal
modes, kind/tag/status filters and host selection. Unsupported requests fail;
they do not silently switch modes or weaken filters. `--no-sync` is accepted,
although this scope already performs no synchronization. Dry-run validates and
shows the request without executing retrieval.

Existing query, candidate, excerpt, rendered-context and canonical verification
budgets still apply. Catalog reads additionally cap decoded rows at 4,096,
individual JSON rows at 8 MiB and decoded JSON at 256 MiB. SQLite has a separate
32 MiB value/row ceiling and a cumulative 10-million VM-operation allowance.
Its deadline uses the remaining canonical verification time. These are local
safeguards, not an established supported document-size or vault-capacity limit.

SQLite progress callbacks cancel cooperatively. Native FTS operations and
blocking filesystem calls can run between checks; neither those callbacks nor
pager-cache settings provide a hard wall-clock or process-memory limit.
[SQLite progress callbacks](https://www.sqlite.org/c3ref/progress_handler.html),
[FTS5 ranking](https://www.sqlite.org/fts5.html#sorting_by_auxiliary_function_results).
Operational safety guards still inspect retained change history outside canonical
proof byte/file counters. Import, publication, rebuild and strict verification
also retain whole-vault work. This milestone does **not** establish the
[100,000-document / 10 GB target](testing-large-vaults.md).

## Mixed document context on normalized indexes

For an already normalized vault, use the ordinary workflow:

```sh
lwiki --wiki /path/to/wiki --offline search 'release checklist'
lwiki --wiki /path/to/wiki --offline read --path knowledge/pages/checklist.md
lwiki --wiki /path/to/wiki --offline context 'release checklist'
```

Search uses the published index. Read authenticates the selected file and its
supporting dependencies. Context defaults to `--scope indexed-documents`: current
eligible authored pages/entity descriptions and captured sources share passage
selection and the output budget. Authored passages are `note_text` with exact
record/path/hash locators and no fabricated source citations; captured passages
retain exact source revision/span/hash citations. Kind, tag, status, source and
path filters use the existing document filtering rules.

### Selected search citations

An authenticated Current Entity identity can remain an empty, uncited navigation
hit when its description is unsupported or invalid. Verification authenticates
the identity and selected dependencies; it does not promote that description to
evidence. Invalid or unsupported body excerpts still refuse. Discovery ordering,
limits and cursor bindings are unchanged.

The 0.2.0 candidate adds explicit verification to normalized lexical search:

```sh
lwiki --wiki /path/to/wiki --offline search 'release checklist' --verify-selected --no-sync
lwiki --wiki /path/to/wiki --offline --json search 'release checklist' --verify-selected
lwiki --wiki /path/to/wiki --offline --dry-run search 'release checklist' --verify-selected
```

`--verify-selected` preserves the published result order, excerpts, filters and
cursors. It authenticates displayed documents and their supporting dependencies,
then rechecks them before returning. Nonempty captured excerpts receive exact
Source/Revision/span/hash citations. Human output shows the payload path, source
revision and byte range; use that path with `read --path PATH --start START
--end END` to inspect the same bytes. A Revision record ID identifies metadata,
so reading that ID does not navigate to the captured payload. Authored and empty
excerpts remain uncited; authored excerpt offsets address the complete file,
while ordinary authored `read` offsets address its body.

Verification has one 64 MiB, 4,096-file, 16,384-entry and 2-second bound. Selected
edits or exhausted limits refuse the complete verified page; reduce the page or
inspect changes before an explicit `index sync`. Plain search remains cached
discovery. `--no-sync` can accompany verification because it selects published
discovery without refreshing the index. This flag belongs only to search, and
initially requires normalized lexical mode without graph expansion.

Results use `indexed_evidence`, a selected dependency fingerprint and an explicit
warning that global membership, uniqueness, completeness and unselected
freshness are unverified. Citation integrity does not establish truth or answer
completeness. Dry-run returns a validated request with no hits or citations;
index opening, admission and evidence verification are unperformed, including
when the cache is absent or corrupt. The [scoped public assessment](validation-verified-search.md) passed at 9.5/10;
the original uncited-search assessment remains failed.

For authored Markdown, context spans address the complete canonical file,
including front matter, and the locator hash covers those full bytes. `read`
returns front matter separately as metadata; its `--start`, `--end`, range and
continuation offsets address the returned document body. Do not apply an authored
context span directly to `read.body` or pass it unchanged to `read --start/--end`.
Captured source spans address the complete captured content, whose read body has
no stripped record front matter.

JSON reports `indexed_evidence` with `selected_documents` domain and a discovery
generation; rendered context names `indexed_documents`. Selected canonical files,
expected absences and supporting dependencies are verified and rechecked within
finite budgets. Unrelated external edits need not invalidate that bounded proof.
Global membership, identity uniqueness, unselected freshness and answer
completeness remain unverified. Run `index sync` to discover external edits and
`check` when an explicit complete audit is needed.

Explicit `--scope current` keeps its stronger meaning and remains unsupported on
normalized indexes. `read --no-sync` and `context --scope snapshot` retain cached
behavior. Indexed document context supports lexical document queries only;
semantic, hybrid, literal and graph queries remain separate migration
work. Dry-run previews the resolved request without running its proof. Operational
generation-output records currently require explicit cached read because their
selected verification is not implemented.

Lexical indexed-document context also supports the existing host-selection
workflow. Prepare with the same query, filters and budgets you will apply:

```sh
lwiki --json --wiki /path/to/wiki --offline context 'release checklist' --prepare-selection
lwiki --json --wiki /path/to/wiki --offline context 'release checklist' --selection reply.json
```

Preparation returns a candidate packet, not final answer context. Pass only its
`selector_input` to the host agent and save the fingerprint-bound ID-only reply
as `reply.json`. Applying reconstructs the exact packet and rechecks selected
canonical dependencies; changed pages, source refreshes or withdrawals invalidate
the old reply. The host cannot supply replacement quotation text. Candidate order
retains the passage selector's complementary-evidence priority before the card
cap; automatic context keeps its existing selection policy. Neither route proves
answer completeness. The CLI makes no model call and cannot observe host usage.

## Editing Pages on normalized indexes

`page init`, `page put` and `page batch` update a selected normalized publication
without a full-vault scan or a separate sync. `page init` creates a draft; use a
reviewed Page envelope when the text should enter default context. Existing
Pages require their current author hash. Omitted `--path` on `page put` resolves
an existing identity through the index and preserves its custom path.

Admission checks the changed Pages and their actual structural, policy, support
and navigation dependencies. It publishes the resulting rows and search postings
together. Receipt fences are policy inputs under the existing graph rules, even
when they appear in authored Pages; adding or removing one recomputes that policy.
An update that would newly invalidate a dependent record refuses before mutation.
Broad genuine dependency fanout can exceed the finite admission budget; the
command does not silently publish a partial closure.

Unchanged Page proposals report reuse without a new publication, while retaining
and checking supplied read guards. Dry-run previews the request without opening
SQLite or changing bytes/mtimes. It checks explicit target files and their author
guards, but leaves indexed admission, portable path collisions and read
dependencies unchecked. A `page put` preview without `--path` reports the intended
record ID and an unresolved destination/supplied guard. JSON marks
`plan_complete: false` and exposes checked/unperformed validation fields; no
reuse or stageability claim follows from a preview. `--stage` retains the plan; `changes apply ID`
rechecks its author/dependency hashes before applying it. An intervening edit is
preserved. A batch is recoverable across sequential writes, not a multi-file
filesystem transaction. Version-3 receipts identify the admitted write kind;
existing version-2 source-refresh receipts keep their original replay format.

These paths do not yet migrate source addition/withdrawal or page rename.
The normalized layout remains opt-in, and Page fixture checks do not establish
large-vault update throughput or whole-task retrieval completeness.


## Capturing and maintaining sources after activation

On an activated normalized vault, `source add FILE` immediately publishes one
new Source and its immutable Revision. Ordinary lexical search, selected read
and context can use it without a rebuild. UTF-8 text retains exact original and
captured bytes. Empty text and unsupported formats preserve originals but return
`citable: false`; unsupported original bytes are not searchable captured text.
Use `-` for bounded standard input. That input retains the existing `agent-report`
origin classification, without claiming an observed fetch.

```sh
lwiki --wiki /path/to/wiki --offline index rebuild --normalized
lwiki --wiki /path/to/wiki --offline --json source add '/path/to/release notes.md' --title 'Release checklist'
# Set SOURCE_ID to the returned allocated_ids.source string.
lwiki --wiki /path/to/wiki --offline context 'release checklist' --source-id "$SOURCE_ID"
lwiki --wiki /path/to/wiki --offline source refresh "$SOURCE_ID" --file '/path/to/release notes.md'
lwiki --wiki /path/to/wiki --offline source withdraw "$SOURCE_ID" --reason 'Superseded by the author'
lwiki --wiki /path/to/wiki --offline search 'release checklist' --source-id "$SOURCE_ID" --include-historical
```

New Source IDs are 39-digit strings encoding the full UUIDv7 value; preserve them
as opaque strings, including leading zeros. Existing tagged IDs and paths remain
unchanged. Allocation reserves both Source and Revision identities against
published claims, typed references, exact companion paths and policy inputs.
It refuses an occupied generated Source directory. A staged capture also refuses
an independently created directory before applying; it never reallocates a staged
identity to hide that conflict. Numeric naming avoids source-root alias census
only on this admitted capture route. Generic path, containment, symlink, nested
vault, immutable ownership and exact member checks still apply. Unrelated sibling
names, including non-UTF-8 names, are outside that selected numeric probe; explicit
full maintenance remains the global namespace boundary.

Refresh preserves source origin and immutable history. Unchanged input reuses
its authenticated revision; explicit title changes can update discovery without
creating another revision. Withdrawal changes only status, time and reason in the
Source note, then updates affected source/revision/evidence/support and captured
text metadata together. It does not rewrite authored prose or immutable payloads.
The reason must be nonblank and at most 4096 UTF-8 bytes. Repeating withdrawal
preserves its first reason/time and returns reuse. Refresh does not reactivate a
withdrawn source. Historical discovery and selected read retain access to bytes;
strict historical context remains a separate unsupported normalized mode.

`--stage` prepares an inspectable capture, refresh or withdrawal with unchanged
canonical visibility. Apply and recovery use the exact retained operation and
publication; retries do not create another revision or generation. Selected
external edits cause refusal and preserve authored bytes. Abort an unapplied
staged change with `changes abort CHANGE_ID`, then sync external edits separately.

Normalized source dry-run validates the supplied request before any SQLite open.
It preserves all vault bytes and modification times, including shared memory.
Its JSON marks target resolution, guards, admission, dependencies, collisions and
identity reservations unchecked, and reuse unknown. An add preview contains
proposed unreserved IDs/paths; refresh and withdrawal leave existing targets
unresolved. Staging/applying performs actual admission. A successful preview does
not establish that a damaged or missing cache can apply the request.

Selected admission has finite row, byte and time limits; excessive actual fanout
refuses before canonical writes. Public collection import and remaining retrieval
modes are still separate work. Source-root probe behavior alone is not import
throughput evidence: retained object/change namespaces and durable publication
also contribute work. This workflow does not qualify 100k capacity or semantic
answer completeness.

## Continuing a captured-source read

After verified search, use the captured payload's returned path with ordinary
`read --path PATH --max-bytes 8192`. On a normalized catalog this authenticates
the selected dependencies and returns an exact citation for the new source
range. JSON `data.source_citation.citation` uses the existing source-citation
format: Source ID, immutable Revision ID, returned span and quote hash.
`data.source_citation.eligibility` distinguishes Current, historical and withdrawn
evidence. Reading an older revision does not make it Current again.

If the read is truncated, pass its returned `continuation.start` and
`continuation.end` as `--start` and `--end` on the next read. These are UTF-8 byte
coordinates in the captured payload, and the next range has its own citation.
Do not reuse a search excerpt's quote hash for different text. The human
continuation command retains the selected vault and cached-read mode; citation
metadata is printed on stderr while stdout contains the exact requested text.

Cached `--no-sync` reads, dry-run reads, legacy-layout reads, empty ranges and
authored notes return no source citation. Authored read coordinates address the
note body and are not captured-source evidence offsets. An external edit to a
selected dependency causes ordinary verification to refuse instead of emitting
a stale citation. As with context, verification observes sequential reads and
does not guarantee against an edit after the final recheck.

## Rebuilding after complete derived-cache loss

For a previously activated normalized vault whose entire `.wiki/cache` is absent,
run `index rebuild --normalized`. Keep `.wiki/state` and retained history intact:
they are operation authority, not derived indexes. Reconstruction requires idle,
valid authority and reserves one fresh catalog identity in the bounded
`.wiki/state/catalog-rebuild.json` record before creating cache files. A retry
resumes an acknowledged publication or builds under a new reserved identity;
it never reuses an unacknowledged SQLite file.

Authenticated unpublished candidates can be retired through their existing
leases. Unclassified small files are preserved and listed in
`abandoned_rebuild_candidates`; admission allows at most eight such candidates
and 1 MiB of actual database/sidecar bytes. Excess, unfamiliar files, unsafe paths,
partial cache loss or missing authority cause refusal. This explicit recovery
route does not repair arbitrary cache contents. Readers already holding the old
catalog retain its transaction and must still recheck selected canonical bytes.
Dry-run only previews the request and creates no reservation or cache files.

# Large-vault architecture and validation

Status: incremental implementation with an [indexed-context path](indexed-context.md), not a supported-capacity claim. The initial target is
100,000 distinct current documents containing approximately 10 GB of UTF-8 text.
Count original copies, retained revisions, operational records, retrieval units,
vectors and index files separately. A 10,000-document / 1 GB tier supplies the
scaling control. Million-document capacity is outside this initial gate.

The existing [storage](technical/storage.md) and
[retrieval](technical/retrieval.md) contracts remain authoritative until a tested
replacement explicitly changes them. This work must preserve canonical Markdown,
immutable source revisions, rebuildable local indexes, exact citations, offline
operation, bounded remote embedding calls and recovery/accounting invariants.

## Current implementation boundary

An explicitly selected normalized catalog supports plain lexical search,
selected-dependency verified reads, mixed authored/captured document context,
cached `read --no-sync`, document snapshot context, and indexed source capture, refresh and withdrawal
with staged apply and recovery. Page initialization, guarded replacement and
bounded batches also use selected admission and shared publication/replay.
Publication stores per-document rows and updates affected rows in one
transaction. Ordinary normalized queries consult a small publication record;
they do not audit completed operation history. Selected evidence still requires
verification before it becomes a citation.

Literal, semantic, hybrid and graph queries, host context selection,
page rename and public default activation are not fully migrated. These
are required integration work, not optional omissions. Large-vault capacity and
unseen context completeness remain unqualified. See the
[validation protocol and measured results](testing-large-vaults.md) for actual
command timings and their limits.

## Implementation checkpoints

The following records describe successive checkpoints. Statements about pending
work within an earlier checkpoint apply to that checkpoint; the current boundary
above and later connected results take precedence.

The current storage checkpoint includes a streaming normalized builder, leased
catalog selection and direct bounded captured-source queries. The production
publisher still uses the previous representation; ordinary incremental updates
and the default-query transition are unfinished. The foundation passed 289 unit
tests (five ignored), with a subsequent focused catalog check after narrow
hardening. Formatting passed. The lint gate still reports unused entry points
because publication and explicit auditing are not connected yet. These are
component results, not CLI workflow, update-latency or large-vault qualification.

The next checkpoint adds a small durable operation record and an acknowledged
publication floor. Normalized readers consult that record instead of enumerating
completed change history. A query keeps the floor observed when it starts:
unrelated updates may proceed while its selected evidence is checked. New queries
must see at least the acknowledged epoch; missing or corrupt required operation
state is an error. Legacy commands cannot publish behind a selected normalized
catalog. Public activation remains deferred until those commands are migrated.

Snapshot metadata distinguishes a published epoch from a canonical manifest;
the former does not claim a whole-vault audit. Existing canonical receipt encoding
is preserved. This establishes the recovery and read boundaries for incremental
updates; it does not yet implement the fast refresh workflow or establish its
latency.

The operation/read checkpoint passed 272 non-job unit tests (four ignored),
four persisted-journal tests and the produced-run/event/receipt schema test.
The repaired selector fault adapter also verifies that each intended injected
failure is reached. The independent architecture review found no checkpoint
blocker; it did not approve activation or large-vault capacity.

The mutable catalog candidate uses schema 3 and WAL. Its publication hash is
separate from optional, explicitly epoch-bound full-build observations. Closed
builders retain the WAL files needed for subsequent read-only opens. Ordinary
delta writers share the lifetime lease with readers; migration from rollback
mode requires exclusive access before canonical changes begin. A missing WAL
is an error because it may contain committed data. SQLite can reconstruct a
missing shared-memory file from a retained WAL under exclusive writer recovery.
The catalog/retrieval gate passed 148 tests (two ignored), including old-reader
visibility, missing-WAL refusal without mutation and committed-WAL recovery after
shared-memory loss. The public refresh workflow and its latency remain unfinished.

The bounded refresh planner now uses indexed identity claims, revision signatures
and evidence associations. It authenticates the selected source, current revision
and any historical revision actually reused, including that revision's retained
position. Unselected historical payloads are outside this planning operation.
The catalog/source/authority gate passed 122 tests (two ignored), including
no-op, title-only, historical reuse, malformed identities, changed selected bytes
and invalid retained positions. These tests cover planning and lookup behavior;
the public command still needs bounded apply, dependency updates and publication.
The shared file-application loop preserves existing journaling and tree guards;
71 native recovery, journal and source-evidence tests passed (one ignored),
including injected failures before and after durable file-operation boundaries.

The internal refresh publisher now retains the exact versioned row-change plan
before canonical writes. Its publication hash binds the starting snapshot,
prepared manifest and retained plan. Rows, search postings, revision ownership
and the next epoch commit in one transaction. Recovery recognizes that exact
publication before opening a starting snapshot that may already be obsolete.
Historical completed retries use the retained outcome before constructing a
session for the current catalog.

The connected catalog/source/recovery gate passed 157 tests (two ignored).
Coverage includes source-title and new-revision publication, held readers,
failures before and after SQL commit, interrupted acknowledgement, retained
terminal receipts with shortened operational journals, and immutable ownership.
An interrupted SQL write preserves its original budget error and rolls back
ordinary rows and search postings together.

This integration is not public activation. Direct admission of a raw row plan is
limited to tests: structural validity and matching checksums do not prove that a plan
updates every affected record. Production admission still requires the bounded
semantic projector, including dependent eligibility and link resolution, and
the selected-proof layout must support authored documents and graph modes.

The normalized proof layout now separates each record's own observed files from
its typed semantic relationships. Expected file states are stored once per
publication. Inventory, support and policy relationships have distinct roles;
a record no longer carries a copied transitive dependency list or every Decision.
Historical revisions do not capture a source's former head as a fixed dependency.

Navigation indexes retain raw destinations and potential path, basename and
alias matches, including missing and ambiguous links. An ambiguous link keeps a
constant number of lookup keys rather than copying every candidate identity.
Selected fact lookups use explicit indexed scopes and cumulative row/byte budgets;
exhaustion returns an error rather than an incomplete dependency group. This
layout has its own version, and older internal catalogs require an explicit rebuild.

The connected catalog, source, recovery and link-resolver gate passed 187 tests
(two ignored). Regressions include dangling evidence with preserved invalid-row
diagnostics, a successful build with 4,100 ambiguous alias candidates, and bounded
lookup work despite unrelated rows. Independent review approved this internal
checkpoint; these are correctness tests, not a large-vault performance result.

These facts are inputs to the affected-record projector. They do not replace
selected evidence verification or authorize a partial graph to pass as a complete
validation. Production refresh admission and all-mode query integration remain
required before public activation.

Version 2 row deltas now maintain local facts, exact semantic edges, raw links and
registry keys in the same transaction as records and search postings. Existing
authored canonical records remain immutable during a source refresh; lifecycle
changes may reuse their facts. New revisions require complete local facts and
registry entries. Old and new rows share finite accounting, and layout/index
preflight runs before canonical file changes. Version 1 plans cannot leave the
new fact tables stale.

Full rebuild and incremental recomputation share the lifecycle, evidence support,
declared dependency and opposition rules. Compact indexes include every authored
accepted opposition member, regardless of eligibility, and potential matches for
all assertions' frontmatter evidence links. Those navigation keys have no invented
byte offsets or candidate-list expansion. Metadata lookups avoid decoding document
bodies. The indexed source planner also no longer enumerates every source assertion,
including on title-only changes; the projector must discover complete affected
groups before applying an update.

The connected gate passed 419 unit tests and 54 catalog-eligibility, source-evidence
and graph-query integration tests, with five unit tests ignored. Two long ledger
recovery matrices passed in the preceding full run; that run's only failure was a
new fixture using an unsupported predicate, corrected before the connected gate.
Tests cover retained v2 recovery before and after SQL commit, held readers,
fact-stage rollback, corrupt selected records, lookup budgets and zero source-wide
assertion queries during indexed planning. These results qualify the internal
rules and publication boundary, not the unfinished production projector or CLI.

The internal source projector now supplies a sealed admission capability to the
publisher. Its connected tests cover title changes, new and reused revisions,
empty and unsupported content, alternative support, opposition, navigation and
interruption on both sides of SQL commit. Recovery preserves held readers and
does not rewrite completed canonical files.

Normalized proof layout 2 stores structural validation effects separately from
the final baseline and requires rebuilding older layouts. Full validation and
incremental code share reference and propagation rules; the full evaluator checks
that recorded effects reproduce its baseline. General authored-record adoption
still needs complete structural and policy recomputation when repairing references.

Capture creates an opaque revision identity; its public request has no requested-ID
field. An allocated identity must be absent from both identity claims and structural
references, and its exact canonical path must be unreferenced. The planner retries
collisions within a finite bound, and sealed admission repeats the checks against
the same publication before accepting generated revision metadata. This prevents
an ordinary capture from accidentally adopting a dangling reference. Historical
reuse authenticates an existing revision. Generic basename and alias links remain
navigation updates and never reserve a generated identity.

Receipt validation limits apply to receipt notes and the distinct records actually
needed for their proofs. Complete discovery still checks malformed, duplicate and
contradictory receipts; exhaustive reference checks retain their actual witnesses.
Exceeding the proof allowance is an operation error, not a semantic invalidity to
cache on a Decision. Unrelated document growth must not change receipt authority.
The semantic fingerprint changes with these rules so earlier cached outcomes need
rebuilding. Explicit authored-operation capture limits remain separate.

The connected freshness and receipt-budget gate passed 451 unit tests and 110
integration tests across catalog eligibility, source evidence, graph queries,
graph review and entity decisions. Five tests were ignored and the same two
unchanged ledger matrices were excluded. This covers collision retries, sealed
admission, full-projection parity and receipt behavior with unrelated document
growth; it does not establish public CLI performance or retrieval completeness.

Normalized navigation now records an exact compact ambiguity state. Bounded
candidate probes preserve path precedence and public resolver behavior. A connected
update regression with 16 versus 4,100 matching filenames used identical selected
row work and matched the full normalized oracle. The combined gate passed 434 unit
and 54 related integration tests, with five ignored and the two unchanged ledger
matrices excluded as described above. Full-build candidate enumeration, actual
100,000-document performance, public activation and unseen answer quality remain
unqualified.

For an already selected normalized catalog, the application routes source refresh
through the indexed planner and sealed projector. An authenticated no-op creates
no changeset or publication. Dry-run remains read-only; staging retains the exact
proof and row changes needed for later application. This does not activate the
normalized catalog by default or migrate the remaining query modes.

`changes apply ID` replays one retained operation and recognizes historical
terminal results before opening old payloads or a superseded catalog epoch.
`recover` remains an explicit maintenance command that enumerates complete
history and staged proposals. Ordinary refresh and query do not invoke that
enumeration. Admitted source refreshes now follow hash-bound published logical
paths without enumerating unrelated source siblings. The private path scope is
bound to the vault and the exact retained before/after dependencies, and survives
staging and replay without treating newly created paths as previously published.
New revision and asset names still undergo physical case-folded collision checks;
selected-file containment, symlink, bytes, ownership and immutable-tree checks
remain. This applies to unchanged source paths in both storage layouts. Generic
writers and relocated operational paths retain their existing validation.

This does not certify the absence of external aliases of old path components.
The [storage contract](technical/storage.md) defines that narrow distinction.
Selected-source revision history and generic operation-directory validation can
still grow with their respective histories. Removing source-root enumeration
does not establish history-independent updates or full-tier performance.

The connected CLI gate passed 458 unit tests and 124 integration tests, with six
ignored and the two unchanged ledger matrices excluded. Six real-executable
scenarios verify refreshed Unicode citations, no-op/title/history behavior,
read-only dry-run, staged apply/retry, and rejection of damaged payloads, replay
proofs and required indexes. Connected recovery checks cover both sides of SQL
commit and historical retries after old payloads expire. The general `read`
command was outside that integration gate; that gate does not qualify default
activation, all query modes or large-vault latency.

On a selected normalized catalog, `read --no-sync` now uses a pinned bounded
lookup for the requested ID or path, followed by that document, its optional
record and its diagnostics. It returns the published cached Markdown with
`index_snapshot` freshness, including when canonical bytes have changed outside
the tool. It does not verify current canonical content or return an evidence
citation. Context retrieval continues to verify selected evidence independently.
Dry-run uses the same read-only path while retaining its existing unknown-freshness
warning. The legacy fallback refuses a result if normalized activation occurs
while its reader is held.

Cached reads preserve malformed-note diagnostics, unsupported schema metadata,
raw bookkeeping Markdown and UTF-8 range/continuation behavior. The existing
non-UTF-8 canonical-note representation remains empty cached text plus its parse
diagnostic. The normalized reader's 8 MiB row admission limit counts both raw and
search text plus metadata; the CLI's 16 MiB output ceiling does not override that
admission limit. Plain normalized reads now verify selected dependencies as described below;
strict global read semantics and remaining query modes still require migration
before default activation.

The cached-read gate passed 48 bounded-query and CLI unit tests plus all 16
offline CLI integration tests. Coverage includes paired legacy/normalized output,
stale canonical edits, malformed and duplicate identities, raw Markdown, UTF-8
continuations, read-only dry-run, and rejection of legacy results after activation.
Identity lookup tests retain identical decoded work after adding 4,096 unrelated
claims. These checks qualify the cached-read route, not default activation.

### General cached discovery

On a selected normalized catalog, `search QUERY --mode lexical` now
searches authored notes and captured text through the same exact ID, title,
alias and lexical ranking channels as the legacy index. Filters apply before
candidate limits. Entity identity remains searchable independently of an
unsupported description; identity-only hits do not return description excerpts.
`context QUERY --scope snapshot --target documents --no-sync --mode lexical`
uses those candidates with the existing passage selection and packing rules.

These commands return `index_snapshot` freshness. Snapshot context can include
historical material with its eligibility label and supplies no verified
citations. External file edits remain invisible until synchronization; a managed
source refresh publishes its changed rows before returning. Use the separate
`indexed-evidence` scope for selected captured-source citation verification.
General discovery and captured evidence use distinct cursor/dependency scopes.
Cursors also remain bound to the physical publication and generation.

The query opens the published catalog read-only, checks selected cached rows,
and rechecks operation authority before returning. It performs no full-vault
audit, synchronization or index repair. Dry-run performs the same cached query
with an explicit cache warning; SQLite shared-memory coordination may still
change. Complete proof layout 2, the general title index and the candidate
metadata index are required.
Missing capabilities refuse rather than create indexes or scan canonical files.

FTS candidate selection reads path, identity, kind, eligibility and source ownership
through a SQLite-maintained index keyed by document row. It excludes body/raw text
and tag arrays, avoiding traversal of large document overflow chains merely to
read eligibility. Optional tag filters read the earlier tag column from the
selected table row; they still incur the cost of parsing the actual tag values.
SQLite maintains the index in the same transaction as document deltas. Older
layouts without the index require explicit reconstruction; query code never
creates it opportunistically. Exact ID/title/alias ranking channels are unchanged.
SQLite describes this table-read avoidance in its
[covering-index documentation](https://www.sqlite.org/optoverview.html#covering_indexes);
the actual tag and no-tag access paths are checked separately in regression tests.

The result cap bounds selected payload, not all query work. FTS scoring and
restrictive filters can depend on matching population; source filters can parse
record metadata. Literal, semantic, hybrid, graph and synchronized normalized
routes remain outside this package. Public normalized reconstruction and default
activation still require the remaining lifecycle work; an internally prepared
fixture does not establish that ordinary users can activate this layout.

The internal correctness checkpoint passed 498 unit tests and 139 integration
tests across context, freshness, lexical, semantic, graph and offline CLI
behavior. Eight tests were ignored and two unchanged ledger stress matrices
were excluded. A subsequent 36-test serial run passed the final capability
preflight, duplicate-ID, Unicode span/alias, lifecycle and query-work additions.
The focused count overlaps the full gate; it is not an additional independent
population. Broader runtime qualification remains separate.

The SQL probes distinguish selected payload from candidate work. With 10 versus
1,000 extra matching notes and a one-hit limit, the exact-alias leg retained 266
VM steps, while exact-title and FTS work grew to 61,226 and 62,688 steps. Both
returned two candidate rows for overflow detection. A negative source filter over
100 records with approximately 10 MB of metadata retained the same 7,869 VM
steps but increased measured statement time from 1.2 to 16.5 ms, admitting no
payload rows. These are component measurements, not CLI latency or capacity
claims. Native JSON processing is one reason VM counts cannot stand alone;
SQLite documents its [JSON input conversion costs](https://www.sqlite.org/json1.html#performance_considerations).

## Incremental validation and navigation

A cached final eligibility state is insufficient for incremental repair. A record
can be invalid because a referenced ID was absent, because its own evidence is
corrupt, or because an independent policy check failed. Adopting the missing ID
must recompute the affected reference and integrity checks while retaining other
failures. Store the necessary stage provenance and traverse the complete affected
dependency closure; compare incremental results with a fresh full projection.
Do not replace this with an ordinary-update requirement to run a full audit.

This follows the dependency-graph approach used by
[TypeScript incremental compilation](https://www.typescriptlang.org/docs/handbook/release-notes/typescript-3-4.html).
Its optional [direct-dependencies-only shortcut](https://www.typescriptlang.org/tsconfig/assumeChangesOnlyAffectDirectDependencies.html)
explicitly trades complete checking for speed; that tradeoff is unsuitable for
eligibility and citation correctness here. These are design precedents, not
performance evidence for this implementation.

Navigation needs the exact resolution outcome, but ordinary indexing does not
need every candidate ID to report ambiguity. Resolve direct paths before extension
fallbacks, and only then consider basename and alias matches. A bounded indexed
probe can establish ambiguity from two distinct adopted entries; keep that compact
outcome distinct from public APIs promising a complete candidate list. New entries
must participate in the same precedence rules. Verify indexed access and work with
large matching buckets, not only unrelated rows; SQLite documents the relevant
[index access paths](https://www.sqlite.org/queryplanner.html) and
[EXPLAIN QUERY PLAN output](https://sqlite.org/eqp.html).

## Why a redesign is necessary

A public 299-source corpus exposed an operational failure before answer quality
could be evaluated: all 64 prospectively selected questions exhausted the maximum
65,536-entry context-verification budget. Repeated nested-vault checks enumerated
an entire sibling directory for each file being checked. Removing that repeated
work addresses one defect, but cannot establish large-vault support.

Several other operations still depend on the entire corpus:

- Opening a strict catalog reader checks the database, deserializes a complete projection and
  reconstructs another catalog and both search tables in memory to compare rows.
- Current-context verification captures canonical files and revision assets
  twice. For 10 GB of captured text, original/content duplication alone implies
  approximately 40 GB of payload reads per query, before metadata work.
- Publication rewrites a complete generation and search tables. Repeating source
  additions compounds whole-vault validation and history scans.
- Semantic retrieval renders the corpus and scans vectors; embedding scheduling
  and prior-accounting discovery also materialize or enumerate global state.

Increasing proof limits or bounding the number of returned passages does not
bound those operations. Ten GB of current text also needs substantially more than
10 GB of disk: original/content copies, retained history, indexes, journals and
temporary rebuild space all contribute.

The whole-catalog JSON value is also a hard storage obstacle, independently of
memory. SQLite defaults to a 1,000,000,000-byte string/BLOB limit and supports at
most 2,147,483,645 bytes for one value; its length limit also applies to a complete
row. A projection containing 10 GB of source text cannot be stored in that single
value. Increasing query budgets cannot repair this representation; publication
must use independently addressable rows.
[SQLite implementation limits](https://www.sqlite.org/limits.html)

## Chosen direction

The ordinary workflow is fast search and incremental updates. Full integrity
auditing belongs in a separate, explicitly requested maintenance command, suitable
for occasional checks or troubleshooting. It must not run implicitly before or
after routine searches or single-document updates. This is the target behavior;
the existing strict default and whole-generation publisher have not yet been
replaced.

Use the existing `lwiki check` command for that explicit full audit, extending it
to cover the selected index. Keep `doctor` lightweight, `index sync` for discovering
external edits, and `index rebuild` for reconstruction. Currently `check` scans
canonical data only; extending it to the selected index remains outstanding.
`doctor` observes the publication header and operation slot without scanning
canonical records or retained change history. It reports unperformed checks and
unknown canonical freshness explicitly. Do not introduce another overlapping
audit command.

Ordinary search uses a published index epoch and validates the evidence returned.
A successful managed update must make its changes discoverable by subsequent
queries. External edits require synchronization for discovery; returned metadata
must explain that freshness boundary. Whole-vault discovery, cache completeness
and full integrity are separate guarantees supplied by explicit maintenance.

A single-document update must do work proportional to the changed content and
its affected dependencies. Update the relevant ordinary rows, FTS entries and
changed retrieval units atomically; preserve reusable embeddings. Do not rebuild
the database, enumerate completed change history or validate unrelated documents.
Measure both latency and work as vault size and completed history grow. The
existing single-edit acceptance ceiling is a failure limit, not a satisfactory
interactive latency target.

For a 100 KiB managed refresh with fixed dependency fanout, target warm end-to-end
p95 of one second and every operation within five seconds on the declared test
host; target a no-op p95 of 250 ms. These are prospective UX targets, not measured
results or a replacement for the existing mandatory scale gates. Time the public
update command through durable publication and immediate discovery by a new
query, not just its SQL transaction. Keep a reader open during the update to
verify that it retains a coherent earlier epoch without delaying new readers.

Refresh planning is part of that budget. Look up the selected source, current
revision, matching retained revision identity and affected dependents by index.
Authenticate the selected inputs and any revision actually reused; inspecting
every unrelated historical payload belongs to explicit auditing. A no-op should
return after those selected checks, without preparing a changeset or rebuilding
the graph. The internal planner implements this boundary; the public command and
apply path still require integration.

Keep immutable revision ownership in indexed rows reconstructed from validated
retained change receipts and manifests. Include committed owners whose canonical
trees have since disappeared. An explicit completed ownership-index version must
gate bounded writes; an empty table does not establish that reconstruction ran.
The active operation record reserves a pending manifest, and its new ownership
rows must commit with the catalog delta. This is the integration design, not an
activated replacement for the public command's history checks. The internal
registry reconstruction and bounded guard passed 112 catalog/authority tests
(two ignored) and eight existing native revision-recovery regressions. Coverage
includes removed committed trees, aborted changes, conflicting retained histories
and interrupted publication. Reconstruction checks cooperative deadlines and a
separate history-work limit even when history emits no ownership rows. The internal
refresh session uses a separate finalization guard after verifying the intended
publication and its exact owner rows; it does not broaden the starting-epoch guard.

Use independently addressable catalog records, source manifests, reverse
dependencies and retrieval units. Query only required rows and bounded candidate
sets. Replace full read-time reconstruction with selected-row checks plus an
explicit full integrity audit. Selected-row checks cannot rule out cache omissions.

The normalized projection must distinguish semantic dependencies from history
inventory. The legacy eligibility projection expands all decision dependencies
and retained revision history into many records. Persisting that expansion, or
merely repeating it during a query, would preserve the same scaling problem.
Store expected file state once per epoch and assemble proofs for selected records
through relevant support and policy relationships. The captured-source reader
already uses a separate selected proof; authored and graph modes still need that
coverage before the default changes. Preserve the legacy full-audit contract while
introducing an explicit normalized proof format.

Publish changed rows, affected dependency closures and search-index changes in a
single SQLite transaction. Reader transactions must retain a coherent published
generation. Reconciliation and rebuild must stream bounded batches, with durable
checkpoints and interrupted-publication recovery. Batch source capture must avoid
one full publication per document while preserving individual immutable revisions.

The next public lifecycle package must bound retained canonical-note state while
connecting normalized rebuild/sync/check. The scanner excludes captured revision
payloads such as `content.md`; ordinary captures are read individually through
named source reads. It does **not** retain the entire captured corpus in
`scan_input`. That input does retain scanned canonical/authored Markdown, and
`input_notes` plus `SourceView::from_input` parse separate copies. Share one
immutable parsed-note authority and account for retained authored bytes, graph
state and temporary projections. Preserve legacy closed-input validation.

Maintenance also needs consistent named reads and a final input recheck before
publication. Detect conflicting observations even if an eligibility check handles
a read error as an invalid source. Use the existing budgeted path resolution;
ordinary ancestor checks already avoid full sibling enumeration when no vault
marker exists. Charge retained path bytes during scanning, before accumulating
an unbounded path list. Additional full-payload passes or new path bypasses need
measured justification; they are not consequences of the note-sharing change.

Retained-layout mapping previously revalidated the original migration receipt
for each managed path. Explicit maintenance should validate that receipt once
through the same metered reader, bind the mapping to the operation, and recheck
its exact raw inputs before publication. Mapping reuse must preserve physical
ancestor and nested-vault checks. Include receipt bytes in input accounting;
payload-only counters do not establish a bound on the entire input phase.

An unchanged explicit sync can preserve the published epoch after comparing the
complete current input commitments and parser/layout compatibility. It need not
audit SQLite internals. Changed external input may initially use full
reconstruction, clearly reported as such. Full canonical/index/FTS comparison
belongs to `check`; `doctor` reports bounded publication/capability status with
unperformed audits labeled. The current in-memory audit backup must also be
replaced or resource-qualified. Forecast scratch together with current and
reader-pinned old databases before choosing a disk-backed copy. These are
implementation requirements, not completed large-vault qualification.

The full-check implementation should combine a read-only native SQLite integrity
check with streamed canonical row comparison and a disk-backed, contentless FTS
reference. SQLite added FTS5 virtual-table integrity checking in 3.44; checking
external-content equality remains a separate obligation. Compare token positions,
complete document-size membership and token totals as well as ordinary metadata
and retained revision owners. This avoids duplicating raw/body text in another
full database merely to audit the selected one. It is a design choice pending
corruption tests and capacity measurements, not proof that compact scratch will
always fit. Include reference postings, temporary merges and any reader-pinned
predecessor in disk accounting.
[SQLite 3.44 release notes](https://www.sqlite.org/releaselog/3_44_0.html),
[FTS5 integrity checking](https://www.sqlite.org/fts5.html#the_integrity_check_command)

Retries must not accumulate full unacknowledged siblings. Dispose only the exact
owned candidate after proving it is neither selected nor acknowledged, and try
to retire the known predecessor after publication. A held reader can defer that
retirement. SQLite documents that long readers can prevent WAL checkpoint
progress, while ordinary VACUUM can require up to twice the original database
size in free space. These costs belong in capacity planning; automatic compaction
is not a substitute for candidate ownership and retirement.
[SQLite WAL](https://www.sqlite.org/wal.html),
[SQLite VACUUM](https://www.sqlite.org/lang_vacuum.html).

Keep SQLite first. FTS5 already supports row updates, internal segments and
incremental merging; the application must maintain row/index consistency.
Measure delta publication before adopting another lexical engine.
[SQLite FTS5](https://www.sqlite.org/fts5.html)

The current migration candidate builds a private normalized database in bounded
batches and switches a durable selector after sealing it. Existing readers retain
the old file through a lifetime lease. This topology is for full rebuilds;
admitted source refreshes already use bounded deltas. Normalization alone does not require
multiple files, so the extra selector, lease and retirement machinery must justify
its resource and concurrency costs in actual workflow tests before activation.

For ordinary updates, use a transaction on the selected database with SQLite WAL:
old read transactions retain their snapshot, and new transactions see committed
rows and postings together. The candidate now validates normal WAL sidecars and
retains them across connection close. Public source refresh uses this path;
public full reconstruction and other writers still need integration.
Bound checkpoint and retained-WAL costs when readers remain open. The successful
public update response must acknowledge index visibility, not merely an accepted
background job. SQLite's [WAL documentation](https://www.sqlite.org/wal.html),
Tantivy's explicit [reader reload policy](https://docs.rs/tantivy/0.26.2/tantivy/enum.ReloadPolicy.html)
and Meilisearch's [asynchronous task completion](https://www.meilisearch.com/docs/capabilities/indexing/tasks_and_batches/async_operations)
support these distinct transaction and visibility mechanisms. They are design
references, not performance evidence for this implementation.

Owned-cache lookups must use bounded, direct generated paths. Enumerating sibling
filenames on every acquisition makes query work grow with publication history,
especially when lease inodes persist. A directory-entry ceiling only changes that
growth into refusal. Preserve canonical user-path validation separately, and test
cache acquisition work with increasing retired-file history and unrelated names.

Strict audit requires canonical equality and actual index consistency on the same
pinned publication. The audit mechanism remains provisional. Compare a bounded
private [SQLite backup](https://sqlite.org/backup.html) followed by native FTS
integrity checks with the contentless-reference approach using identical corruption
cases and measured time, memory and scratch allocation. The backup adds a complete
database copy; the reference adds postings and retokenization work. Include any
old database retained by a reader in disk admission. Neither an earlier audit
receipt nor a clean reconstructed compatibility index proves the selected index
is correct.

Separate graph validation from retrieval-text emission before replacing storage.
Measure control-record, graph-snapshot and dependency memory at the 1k and 10k
tiers; raw envelope size alone cannot establish whether the graph fits in memory.
A streamed document sink should serve an actual rebuild path. Move graph state
to a disk workspace when the measured resource requirements demand it.

Avoid persisting raw and normalized text again inside row JSON, whole-catalog JSON
and FTS content tables. External-content FTS can index the stored text, but the
application must keep postings and content consistent, including during migration
and deletion. Its integrity check must explicitly compare external content.
[SQLite external-content tables](https://www.sqlite.org/fts5.html#external_content_tables),
[FTS integrity checks](https://www.sqlite.org/fts5.html#the_integrity_check_command)

The rebuild publication topology still requires a separate contract and tests.
Building a sibling catalog in bounded transactions and then publishing a durable
selector can control rebuild WAL growth, but introduces selector recovery and
reader-retirement obligations. That filesystem commit point differs from the
SQL transaction used for an in-place delta. Do not combine their atomicity claims
or replace an open database file. Measure old/new catalog and WAL peaks before
qualifying the full tier.

Persist retrieval units and eliminate per-unit lookup overhead before selecting
an approximate vector index. Compare any embedded ANN candidate index with exact
search on frozen vectors, including restrictive filters, deletion and refresh
churn. Exact reranking cannot recover neighbors that candidate generation omitted.
No new database service, local model or ANN dependency is selected by this plan.

## Freshness must be explicit

Retain the existing strict verification behavior. Global identity uniqueness,
current eligibility and arbitrary external edits require full reconciliation;
strict requests must fail without verified output when their budget is exhausted.

A separate fast workflow may search published generation G and recheck selected
source bytes, head records and dependencies. Its output must identify that
generation, the scope of the byte checks and the absence of a global-currentness
or discovery-completeness claim. It must not reuse `verified_snapshot` for this
weaker guarantee. It must reject changed selected dependencies rather than merely
attach a warning to invalid evidence.

Timestamp caches cannot prove arbitrary-edit freshness: same-size writes can
retain indistinguishable timestamps. Watcher barriers can help establish which
events were observed, but lost continuity requires reconciliation. An optional
watcher is an optimization with explicit platform assumptions, not a replacement
for the strict contract. [Git racy index](https://git-scm.com/docs/racy-git),
[Watchman clocks](https://facebook.github.io/watchman/docs/cmd/clock),
[Watchman recrawl recovery](https://facebook.github.io/watchman/docs/troubleshooting)

## Implementation and acceptance order

1. Repair repeated directory traversal and rerun the fixed public questions under
   unchanged budgets. Preserve the failed baseline and all new failures.
2. Introduce bounded catalog reads and the explicit freshness distinction, with
   selected/unselected corruption and external-edit tests. Preserve strict mode.
3. Replace full projection publication with normalized incremental storage,
   streaming rebuild and recoverable batch ingestion. Test readers across commits
   and every publication/restart boundary.
4. Bound embedding scheduling and accounting discovery, preserving unknown holds.
   Evaluate vector-index changes only against a frozen exact baseline.
5. Run the declared scale workloads, then freeze the candidate for independent
   unseen answer-completeness evaluation.

The [scale acceptance protocol](testing-large-vaults.md) freezes prospective
workloads and thresholds separately from the semantic gate. Freeze hardware,
fixtures and work limits before measurement. Record
query latency, peak memory, ingestion/rebuild throughput, bytes read/written,
directory work, storage amplification and failures. Include no-op sync, one-file
and batch updates, deletes, duplicate IDs, same-timestamp edits and interrupted
publication. Synthetic fixtures test operations; public relevance labels and
independently graded evidence test retrieval quality separately.

Neither a fast synthetic benchmark nor a correct citation establishes answer
completeness. The existing [quality gate](rag-quality-targets.md) remains separate;
large-vault support requires an independent critic to assess the complete workflow
at the declared tier, not just isolated query timing.

### Selected document workflow

On an explicitly activated normalized index, plain lexical search uses published
metadata, plain read verifies its selected canonical dependency closure, and an
omitted context scope resolves to `indexed-documents`. The latter combines current
eligible authored passages and captured source text through the existing passage
selection and packing rules. Authored text carries its record/path/hash locator;
only captured source text receives source-span citations. `--scope indexed-evidence`
retains captured-only behavior, and `snapshot` remains unverified cached context.

Selected verification binds exact canonical hashes and expected absences, follows
bounded eligibility/support dependencies, authenticates selected cached rows, and
rechecks its dependencies before returning. It performs no full-vault audit,
implicit sync or retained-history census. Discovery still belongs to the pinned
generation: new external files require explicit sync, and neither selected proof
nor an earlier full `check` establishes current global membership or uniqueness.
Explicit `--scope current` retains its strict meaning and currently refuses on
normalized catalogs. This workflow does not activate the layout by default or
qualify the remaining query/write modes, full scale or unseen completeness.

The selected-proof design follows an established incremental-validation principle:
reuse a derived result only after checking its recorded inputs. Rustc describes
traversing a query's dependencies before reusing its cached result; Salsa likewise
validates prior query inputs. The application here is narrower: authenticate the
selected generation's relevant canonical dependencies, including successful review
decisions that produced no error effect, then replay shared local eligibility
rules. This does not infer that newly added files are absent or rerun the complete
validator over a partial vault. The critical regression is an indexed successful
decision edited from accept to reject: selected context must refuse without a
whole-vault scan. [Rustc incremental queries](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation-in-detail.html),
[Salsa algorithm](https://salsa-rs.github.io/salsa/reference/algorithm.html).

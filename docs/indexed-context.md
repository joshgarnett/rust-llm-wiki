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
semantic, hybrid, literal, graph and host selection remain separate migration
work. Dry-run previews the resolved request without running its proof. Operational
generation-output records currently require explicit cached read because their
selected verification is not implemented.

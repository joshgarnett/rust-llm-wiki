# Sources and providers

Use [capture and refresh](everyday-use.md#capture-and-refresh-notes) for one
note. This guide covers collections, batch updates and optional remote services.
Capturing and searching by words work offline without credentials.

## Import a collection

Make an input list with one JSON object per line. Each object names a file;
its title and media type are optional. Relative paths start from the input
list's folder. For example, `inputs.jsonl` might contain:

```json
{"path":"project-notes.md","title":"Project notes"}
{"path":"meeting-notes/decisions.txt","title":"Meeting decisions","media_type":"text/plain"}
```

Use your own filenames. Prepare an import manifest, choosing an output file that
does not already exist:

```sh
lwiki --offline source import prepare --input-list "/absolute/path/to/inputs.jsonl" --output "/absolute/path/to/import.jsonl"
```

Preparation checks the files and records their hashes. It does not capture them
yet. [Back up an existing wiki](recovery.md#backup-and-restore), then prepare the
search index and run the import:

```sh
lwiki --offline index rebuild --normalized
lwiki --offline source import run --manifest "/absolute/path/to/import.jsonl" --key project-notes --group-size 4 --max-groups 64
lwiki --offline source import status --key project-notes
```

Choose a key that names this import. The default group size is four files; each
group is saved as one change. The invocation processes at most 64 groups in
this example. If status says the import is incomplete, continue it:

```sh
lwiki --offline source import resume --key project-notes --max-groups 64
```

Reuse the same key only for the same manifest and group size. A different key
creates separate captured sources, even when files are identical. If an import
fails before retaining its manifest, follow the error's instruction to rerun
with that manifest. Status reports the result file with the saved source IDs.

Markdown and plain UTF-8 text remain citable. HTML captures keep the raw HTML;
word search omits tags, comments, scripts and styles, while literal search can
match markup. No browser rendering or encoding conversion takes place. Other
formats can retain the original without searchable text. Check the capture
result before expecting a citation. Individual inputs are limited to 64 MiB;
large inputs use their own group.

## Maintain imported Sources

Keep the import's result file so you can match originals to source IDs. In its
JSON Lines output, each `group_committed` event contains `result.items` with
zero-based manifest positions. Use all groups when building the mapping;
`last_group` contains only the last one.

To refresh an imported note, inspect its source and stage an update:

```sh
lwiki --offline --json read --id SOURCE_ID
lwiki --offline --stage source refresh SOURCE_ID --file "/absolute/path/to/updated-note.md"
lwiki --offline changes show CHANGE_ID --operation 0
lwiki --offline changes apply CHANGE_ID
```

Replace the IDs with the actual source ID and returned change ID. Review the
before and proposed text before applying. Apply uses the staged copy, so a later
change to the external input does not silently alter the reviewed update.
An unchanged refresh can reuse the existing captured version.

To stop treating a source as current support:

```sh
lwiki --offline source withdraw SOURCE_ID --reason 'why this note is no longer current'
```

Withdrawal keeps its history and earlier citations. It does not rewrite a page
that used the note, and refresh does not reactivate it. Ask your agent to review
affected pages against the new evidence, then save a draft update.

## Refresh a guarded batch

For an automated update of several sources, `source refresh-batch` updates up
to 16 distinct active sources in one reviewed change. It requires a normalized
index. Inspect the request format first:

```sh
lwiki schema source-refresh-batch
```

Each item names the source, new input file, current source hash, current revision
ID and new input's BLAKE3 hash. For example:

```json
{
  "items": [{
    "source_id": "SOURCE_ID",
    "file": "updated-note.md",
    "if_match": "blake3:CURRENT_SOURCE_HASH",
    "expected_revision": "REVISION_ID",
    "input_hash": "blake3:NEW_INPUT_HASH",
    "media_type": "text/markdown"
  }]
}
```

Replace every placeholder. Source `read` returns the current `data.hash` and
`wiki_current_revision`. Replace each whole hash string with `blake3:` followed
by the actual 64 lowercase hexadecimal digits.
To calculate the new input hash with the `b3sum` tool, run
`b3sum /absolute/path/to/updated-note.md` and use the first value it prints.
Member file paths start from the request file's directory. The total input limit
is 4 MiB. Stale hashes, repeated sources and symlink inputs are rejected.

```sh
lwiki --offline --dry-run source refresh-batch --file REQUEST.json
lwiki --offline --stage source refresh-batch --file REQUEST.json
lwiki --offline changes show CHANGE_ID --summary
lwiki --offline changes show CHANGE_ID --operation 0
lwiki --offline changes apply CHANGE_ID
```

A dry run checks the request inputs but does not check every source guard.
The summary helps you choose operations to inspect; review their exact before
and proposed text before applying. Batch refresh does not update wiki page
prose or prepare embeddings.

## Optional remote providers

Search by meaning uses an embedding model: a remote service turns note text and
questions into numerical vectors that lwiki stores locally. It sends that text
to your chosen provider and can cost money. Keep using literal or word search
if you do not want those calls.

To configure a model, rerun setup for your wiki:

```sh
lwiki setup "/absolute/path/to/Wiki" --select
```

When asked about an embedding model, answer yes. Supply the model name and full
embeddings endpoint URL from your provider. Set vector dimensions only if your
provider requires them. Choose how to provide your API key:

- **Save privately:** enter the key at the hidden terminal prompt. Setup saves it
  in a private file outside your wiki, so later terminals can use it.
- **Private file:** supply an existing private key file; setup records its path.
- **Environment:** supply the variable name. That variable must also be available
  in every terminal or agent process that makes requests.

Setup shows the model and destination, then asks whether it may send wiki text
and search questions there when you request search by meaning. Requests can cost
money. Setup itself makes no network request. `config show` displays the model,
destination and key source without showing the secret:

```sh
lwiki config show
```

After granting access, test the selected provider with a single paid request,
then prepare embeddings with a finite request limit:

```sh
lwiki doctor --probe --role embed --max-requests 1
lwiki embeddings sync --max-requests 20 --quality-target-bytes 3000 --max-input-bytes 8000
lwiki search 'your question in different words' --mode semantic --max-requests 1
```

The sync result reports how much text has embeddings. A request limit can leave
work incomplete; check coverage before increasing it. Saved provider choices
apply to new commands, so you do not need to repeat their paths or profile name.
The full provider configuration remains a separate private file.

A new semantic question needs a query embedding. An identical cached question
can later run offline, but offline mode cannot create a missing embedding.
Use `lwiki --offline embeddings check` to inspect coverage. Long notes may be
split into multiple inputs, increasing requests. Removing the cache loses those
reusable vectors.

Retries count toward request limits. If a send has an uncertain result, inspect
`jobs status --run RUN_ID` before retrying. Unknown cost is not zero, and repeating
a command does not reset an existing run's limits. Use `jobs amend --help` when
you deliberately want to increase a retained run's allowance.

For a manually maintained provider file, pass
`--providers-config "/absolute/private/providers.toml" --profile PROFILE`.
Keep credentials outside notes, command arguments and public reports. Generation
services use a separate model and adapter; configuring embeddings does not
configure generation. The [provider reference](../developer/contracts/providers-jobs.md)
covers advanced adapters, rate cards and accounting.

## Experimental Source relations

Generated relationships between source notes are optional suggestions for
navigation. They are disabled by default and are not reviewed facts. Preparing
them needs a normalized index and a configured generation service.

Inspect `relations prepare --help` for input and limit options. `--quote` shows
a bounded cost estimate without sending requests. Review that estimate and input
coverage before `relations run`. Queries must explicitly pass
`--experimental-source-relations`. Refreshing or withdrawing a source removes
its current generated navigation while keeping the older history.

Use cited source passages to support an answer, rather than treating a generated
relationship as proof. The [advanced relation reference](../developer/contracts/retrieval.md#experimental-source-relation-preparation)
contains the full prepare, run and query formats.

# Sources and providers

Capture files or folders with one command. This guide covers collections,
reviewed batch updates and optional remote services. Capture and word search
work offline without credentials.

## Import a collection

For an ordinary folder of notes:

```sh
lwiki source capture "/absolute/path/to/Notes/project"
```

Capture walks the folder recursively for `.md`, `.markdown` and `.txt` notes
and saves them in groups. Hidden folders and symbolic links are skipped. Its
result shows source IDs, citation status, unchanged files and any skipped or
failed files. Repeat the command to add new files and refresh changed ones. It keeps
earlier copies and leaves your originals unchanged. Use `--offline` when you
want no provider requests.

For notes in several places, pass several paths or make a UTF-8 text file with
one path per line:

```sh
lwiki source capture --files-from "/absolute/path/to/note-paths.txt"
```

Relative paths in the list start from the list's folder. Read a saved note with
`lwiki source read SOURCE_ID`, using an ID from capture. Add `--json` before
`source capture` to get every file result and its exact citation object.
Ordinary capture limits each input to 16 MiB. Split larger notes or use the
explicit import below when you need its larger input limit.

### Use a reviewed import manifest

For a collection that needs a fixed input list and explicit resumable groups,
use the import commands below. They are an alternative to ordinary capture.

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

## Update notes from an advanced import

If you captured a folder with `source capture`, repeat that command to save its
updates. The commands below are for notes added with a reviewed import manifest.
Your agent can use the import's result file to update selected notes without
making you assemble batch requests.

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
and proposed text before applying. Batch refresh does not update wiki page prose. Use
`embeddings sync` when this explicit operation leaves embedding work incomplete.

## Optional remote providers

Search by meaning uses an embedding model: a remote service turns note text and
questions into numerical vectors that lwiki stores locally. It sends that text
to your chosen provider and can cost money. Keep using literal or word search
if you do not want those calls.

### Set up search by meaning

Rerun setup for your wiki:

```sh
lwiki setup "/absolute/path/to/Wiki" --select
```

Choose `skip` at the agent question if your agent is already connected. When
asked about an embedding model, answer yes. Supply the model name and full
embeddings endpoint URL from your provider. Leave vector dimensions empty for
the provider's default, or enter a number if your provider requires one.
Choose how lwiki gets your API key:

- **Command:** enter a credential helper command, such as `sagex auth export`.
  Install the program and follow its sign-in instructions first. Choose `text`
  if it returns only a token, or `json` if it returns a `key` and optional
  `expires_at` expiry time.
- **Save privately:** enter the key at the hidden terminal prompt. Setup saves it
  in a private file outside your wiki, so later terminals can use it.
- **Private file:** supply an existing private key file; setup records its path.
- **Environment:** supply the variable name. That variable must also be available
  in every terminal or agent process that makes requests.

If you already know the model and endpoint, supply them and the helper directly:

```sh
lwiki setup "/absolute/path/to/Wiki" --select \
  --embedding-model MODEL \
  --embedding-url "https://gateway.example/v1/embeddings" \
  --credential-command 'sagex auth export'
```

Replace `MODEL` and the example URL with your provider's values. Use a helper
that returns the required output format. For JSON output, add
`--credential-output json`. Its output must be a JSON object containing only a
`key` string and an optional `expires_at`. The expiry is an RFC 3339 timestamp,
such as `2026-12-01T18:00:00Z`.

Keep the command quoted as one argument. Quotes inside it can preserve spaces
in a program path or an argument. lwiki runs the program directly; pipes,
redirects, variable expansion and command substitutions do not run. Never put
the key itself in the command. Saving helper settings does not run it.
Permitted automatic preparation can run it after setup saves those settings.
When a provider request needs credentials, the helper has a 10-second timeout.
lwiki keeps its token in memory for at most 300 seconds and stops reusing it
60 seconds before a supplied expiry. A later request gets a fresh token.

If the helper reports that you need to sign in again, follow that program's
sign-in instructions, then repeat your lwiki command. You do not need to rerun
setup or change the saved helper settings. You can keep searching locally with
`lwiki --offline search 'your question'` while the connection is unavailable.

Setup also offers a generation model, described below. Skip it if you only want
search by meaning. Before saving, review each model, destination, API format,
credential source and the text it may receive. Embeddings can send wiki text
and search questions. Generation can send wiki text selected for extraction.
Answer yes to permit those requests only if you approve all the models shown,
then confirm applying the settings. Requests can cost money.

Setup also asks whether to allow automatic search by meaning for the displayed
wiki, destination and embedding model. This permits note text and search
questions to be sent during setup, after saves and captures, and during ordinary
lookups. It does not permit generation requests. If you decline, local search
still works and embedding preparation remains an explicit command.

With automatic permission, setup can begin bounded embedding preparation after
saving your settings. It is not a separate credential test. Your agent or
terminal still needs its own access to the files and network.

Check the saved choices:

```sh
lwiki config show
```

The output shows the selected wiki, model, destination and credential source
without showing the key or helper arguments.

### Search automatically or prepare explicitly

With automatic permission, use ordinary commands:

```sh
lwiki source capture "/absolute/path/to/Notes/project"
lwiki search 'your question in different words'
lwiki context 'your question about the project'
```

Eligible changed text is prepared after capture or save. Ordinary lookup can
prepare more and combine word matches with search by meaning. Each invocation
limits its extra work. It can leave preparation incomplete, and lookup falls
back to local search if the remote work is unavailable. Read the result's status
and warnings; a successful capture does not promise complete semantic coverage.

To enable automatic search for an already configured embedding model:

```sh
lwiki config set embedding --auto-embed
```

Review the wiki, destination, model and text scope before confirming. For setup
with supplied model settings, `--auto-embed` requests the same permission.
Setup's older `--grant-embed` grants only explicit embedding requests; it does
not turn on automatic requests. Changing the bound wiki, endpoint or model
requires permission for those new choices. `--auto-embed` never grants generation.

If you prefer explicit preparation or need to finish a larger collection:

```sh
lwiki embeddings sync --max-requests 20 --quality-target-bytes 3000 --max-input-bytes 8000
lwiki search 'your question in different words' --mode semantic --max-requests 1
```

The sync result reports how much text has embeddings. Its request limit can
leave work incomplete; check coverage before increasing it. Explicit semantic
and hybrid modes require compatible prepared inputs and report missing
prerequisites. Saved provider choices apply to commands for that wiki.

For a separate connection test, `lwiki doctor --probe --role embed --max-requests 1`
sends at most one paid request. Local `lwiki doctor` makes no provider request.

A new semantic question needs a query embedding. An identical cached question
can later run offline, but offline mode cannot create a missing embedding.
Use `lwiki --offline embeddings check` to inspect coverage. Long notes may be
split into multiple inputs, increasing requests. Removing the cache loses those
reusable vectors.

Retries count toward request limits. If a send has an uncertain result, inspect
`jobs status --run RUN_ID` before retrying. Unknown cost is not zero, and repeating
a command does not reset an existing run's limits. Use `jobs amend --help` when
you deliberately want to increase a retained run's allowance.

### Add an optional generation model

A generation model lets lwiki call an API to extract information from selected
wiki text. Your agent can extract information without this extra service.
Configuring an embedding model does not configure generation.

Answer yes at setup's generation question. Enter the model and full endpoint URL
from your provider. Choose `responses-v1` or `chat-completions-v1` to match that
endpoint. New generation settings default to `responses-v1`.

You can use separate generation credentials. Setup also offers to reuse your
embedding credentials, after showing both destinations. Choose reuse only if
the same credentials are intended for both services. To add generation to a
wiki that already has embedding credentials:

```sh
lwiki setup "/absolute/path/to/Wiki" --select \
  --generation-model MODEL \
  --generation-url "https://gateway.example/v1/responses" \
  --generation-reuse-auth
```

Replace the model and URL. For a Chat Completions endpoint, also supply
`--generation-adapter chat-completions-v1`. To use a separate helper, replace
`--generation-reuse-auth` with
`--generation-credential-command 'sagex auth export'`. Add
`--generation-credential-output json` if that helper returns JSON. Setup also
accepts `--generation-key-env VARIABLE`, `--generation-key-file PATH` or
`--generation-prompt-key` for the other credential choices.

Review both models before granting permission and applying the settings, then
run `lwiki config show`. This saves configuration; it does not start extraction.

### Choose reasoning effort

If your generation model supports reasoning effort, choose how much work it should
spend thinking before returning a result:

```sh
lwiki config set generation --reasoning-effort low
lwiki config show
```

Expect the generation summary to show `Reasoning effort: low`. Supported choices
are `none`, `minimal`, `low`, `medium`, `high`, `xhigh` and `max`; your provider and
model may support only some of them. Use your provider's documented choices. Setup
also offers this setting, or accepts `--generation-reasoning-effort low`.

To let the provider choose its default again:

```sh
lwiki config set generation --reasoning-effort default
```

`default` removes the setting. `none` sends an explicit choice to the provider.
Omitting this option keeps the saved value. Changing effort asks you to review
permission, saves configuration and sends no generation request.

Reasoning tokens use part of the maximum output allowance, even when the model
returns little visible text. Higher effort can use more tokens and take longer.
If `doctor --probe --role generate` runs out of output tokens, follow its reported
limit adjustment. Effort alone does not increase the output allowance or establish
a guaranteed cost. See [extract and review relationships](retrieval.md#extract-and-review-relationships)
when you are ready to use the model with your notes.

### Change a model or credentials

Use `config set` for the selected wiki. To choose a different wiki, pass the same
`--wiki` folder to both the edit and its readback, as shown below.
The commands below work in a terminal and ask you to review permission and
confirm saving. Omitted settings and the other model role are kept.

Change the embedding model:

```sh
lwiki config set embedding --model NEW_MODEL
```

Change how generation gets credentials:

```sh
lwiki config set generation --credential-command 'sagex auth export'
```

Add `--credential-output json` for a JSON helper. For either role, you can use
`--key-env VARIABLE` or `--key-file "/absolute/private/key"` instead. To
deliberately share embedding credentials with generation, use
`lwiki config set generation --reuse-auth embedding` and review both destinations.

Set generation's maximum output to 8192 tokens:

```sh
lwiki config set generation --max-output-tokens 8192
```

A token is a small piece of text counted by the model. This setting is a ceiling;
an individual request may use a smaller limit. The generation test in
`doctor --probe --role generate` uses this ceiling, subject to your request budget.
If the test runs out of output tokens, it explains how to increase the limit.

Set embedding dimensions to the number required by your provider:

```sh
lwiki config set embedding --dimensions 1024
```

You can also change an endpoint with `--url URL` or its API format with
`--adapter responses-v1`, `--adapter chat-completions-v1` for generation, or
`--adapter embeddings-v1` for embeddings. Use your provider's documented values.
Running `lwiki config set embedding` or `lwiki config set generation` without
flags offers a prompt for one setting.

After an edit, expect `Your model settings were saved.` and a summary of both
models. Confirm the saved value:

```sh
lwiki config show
```

To edit and check another wiki, keep its folder on both commands:

```sh
lwiki --wiki "/absolute/path/to/OtherWiki" config set embedding --model NEW_MODEL
lwiki --wiki "/absolute/path/to/OtherWiki" config show
```

To preview without saving or running a helper, use:

```sh
lwiki --dry-run config set generation --max-output-tokens 8192 --non-interactive
```

For an unattended edit, review the preview first, then rerun without `--dry-run`
and with `--non-interactive`. Add `--grant-provider` when new explicit-request
permission is needed. It approves explicit requests for every final model and
destination shown. Changed generation settings, including credentials or an
output cap, require that reviewed permission. Rotating embedding credentials
under a matching automatic grant keeps that permission; it does not require
`--grant-provider`. Changing the wiki, embedding endpoint or model requires a
new matching automatic grant if you want automatic requests.

Setup's `--grant-embed` approves embeddings only; it cannot approve a new or
changed generation service. With matching automatic permission, setup and
`config set embedding` can prepare text after saving settings.
`config set generation` does not start embedding preparation or generation
requests. `--offline` blocks requests and credential resolution.

If saving is incomplete, follow the reported retry instruction. Completed steps
are preserved; repeating the same edit reuses matching settings.

### Advanced provider settings

The full provider configuration stays in a private file outside your notes.
For a manually maintained file, add
`--providers-config "/absolute/private/providers.toml"` to the command that
makes requests and use `--profile PROFILE` to choose its profile. `config set`
also accepts `--providers-config` after the model role. Keep keys outside notes,
command arguments and public reports.
Setup and `config set` save model choices and the provider file for the wiki you
selected. Other wikis keep their settings. After saving, commands for that wiki
use the saved file and profile without repeating those options.
The [provider reference](../developer/contracts/providers-jobs.md) covers headers,
rate cards and accounting.

Provider files are limited to 64 KiB and 128 services and profiles each. Distinct
edits retain earlier definitions and can reach those limits; repeating identical
settings does not add entries. If an edit reports a size or table-format error,
review the private file using the provider reference before retrying. Setup and
`config set` cannot extend root collections written as closed inline TOML tables.
Changing settings can also reformat your user preferences file and remove its
comments, while retaining the saved values.

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

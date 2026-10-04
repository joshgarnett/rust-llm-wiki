# Import a local collection

`source import` captures an explicit list of local files in bounded committed
groups and retains progress so the same import can resume without duplicating
sources. It works offline without providers. Use a current macOS/Linux binary;
Windows vault writes remain unsupported.

## Prepare a manifest

Create a JSON Lines input list, one object per file. Relative paths use the
list's directory, regardless of the command's working directory:

```json
{"path":"deployment-notes.md","title":"Atlas deployment notes"}
{"path":"runbooks/rollback.txt","title":"Atlas rollback","media_type":"text/plain"}
```

`title` and `media_type` are optional; a missing title uses the filename. Files
must exist. Preparation hashes their original bytes and freezes paths, titles
and extraction policy. It requires no wiki. Existing outputs are preserved;
choose an absent output in an existing directory.

```sh
"$LWIKI" --offline --json source import prepare \
  --input-list "$DEMO/inputs.jsonl" --output "$DEMO/import.jsonl"
```

Add `--dry-run` to compute the proposed manifest without creating a file.
Supported text formats retain exact UTF-8 bytes, as ordinary `source add` does.
Empty inputs are captured without nonempty citations. Unsupported formats retain
original bytes without citable extracted text. No extractor is run.

## Run, inspect and resume

Activate the normalized catalog explicitly before importing a collection:

```sh
"$LWIKI" --wiki "$WIKI" --offline index rebuild --normalized
"$LWIKI" --wiki "$WIKI" --offline --json source import run \
  --manifest "$DEMO/import.jsonl" --key atlas-notes --group-size 4 --max-groups 64
"$LWIKI" --wiki "$WIKI" --offline --json source import status --key atlas-notes
"$LWIKI" --wiki "$WIKI" --offline --json source import resume \
  --key atlas-notes --max-groups 64
```

Reusing a key requires the same manifest and group size. Another key requests
another import with fresh identities, including for identical bytes; imports
do not deduplicate by title, origin or content. Each invocation processes at most
`--max-groups`. Incomplete successful results report `meta.partial` and
`data.completed: false`. Resume uses the owned manifest once its copy is retained.

Groups contain up to 1–8 items, default four. Multiple-item groups are limited
to 4 MiB aggregate original bytes; larger inputs form size-one groups. Each
input is limited to 64 MiB. Membership is frozen before retention, and each group
uses one Change and one catalog publication. Source/Revision identities remain
individual. `--stage` is unsupported for this workflow.

JSON results report `imported_items`, `total_items`, `groups_committed`,
`completed`, `pending_change` and `last_group`. The last group's `items` map
zero-based input ordinals to Source/Revision IDs; `change` names its shared Change.
`results_path` identifies the append-only JSON Lines journal for all mappings.
`group_committed` events carry them under `event.result.items`; `attempt_closed`
events preserve superseded attempts. Missing acknowledged mappings are a conflict.
Completion records import history, not present eligibility after later updates.

Pending progress also reports `pending_group` and bounded `pending_items` with
input ordinals, paths and titles. Input errors identify the failed item and
include progress plus a continuation hint. If interruption occurred before the
owned manifest copy, use the hinted same-key `run --manifest` command with the
original prepared manifest; `resume` cannot replace that missing copy.

Use the returned Source ID with ordinary commands:

```sh
"$LWIKI" --wiki "$WIKI" --offline read --id "$SOURCE_ID"
"$LWIKI" --wiki "$WIKI" --offline search 'atlas_migrate' --limit 5
"$LWIKI" --wiki "$WIKI" --offline context 'Atlas deployment' --max-bytes 6000 --max-tokens 1500
"$LWIKI" --wiki "$WIKI" --offline source refresh "$SOURCE_ID" --file "$DEMO/deployment-notes.md"
"$LWIKI" --wiki "$WIKI" --offline check
```

Refresh and withdrawal operate on individual sources. Guarded rollback of a shared
import Change operates on that entire group.

## Interrupted runs and backups

Retry with `resume`. Known prepared/committed groups retain their identities;
historical commit replay precedes obsolete-base checks. Once a complete preparation
retains its original bytes, resume can use them after external files disappear.
An unapplied preparation made stale by another managed publication is closed and
re-admitted under a new Change, keeping its Source/Revision IDs and capture times.
An active operation must complete or use ordinary recovery before replacement.

Input drift, missing bytes in incomplete preparation, unfamiliar edits, unsafe
aliases and lost operational authority produce explicit errors while preserving
the attempt. Status reveals its ID for `changes show`. Dry-run reads bounded input
or saved progress without locks, writes, cache access or identity reservations;
it does not promise later admission.

Back up the whole vault, including `.wiki/state` and `.wiki/retained`. Progress,
the owned manifest, naming commitments and results are operational state, not
rebuildable cache. The manifest admission limits are 128 MiB, 200,000 items and
64 GiB aggregate original input. These limits do not establish practical capacity;
the [large-vault gates](testing-large-vaults.md) remain separate.

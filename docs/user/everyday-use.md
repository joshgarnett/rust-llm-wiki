# Everyday use

## Choose the wiki

`lwiki wiki show` reports the canonical root, stable ID and selection origin.
Selection uses explicit `--wiki DIR` first, then the nearest ancestor with
WIKI.md, then the saved default. An invalid explicit root or nearer marker fails
without falling through. Nested wikis stay independent. Defaults bind both the
absolute path and observed ID, so replacement with another wiki fails validation.

~~~sh
lwiki init "/path/to/Personal Wiki" --set-default
lwiki wiki show
lwiki wiki default set "/path/to/Another Wiki"
lwiki wiki default clear
~~~

After deliberately moving a wiki, set its new location. Clear preserves other
preferences. Init and default registration are separate durable outcomes: if
registration fails, keep the created wiki and fix configuration before setting it.
For automation, copy the actual ID from `wiki show` into WIKI_ID and pin mutations:

~~~sh
lwiki --wiki "/path/to/Personal Wiki" --expect-wiki-id "$WIKI_ID" --offline index sync
~~~

The wiki ID guards the target; a whole-file hash separately guards author edits.

## Initialize an existing editor folder

~~~sh
lwiki --dry-run init "/path/to/Obsidian Vault" --existing
lwiki init "/path/to/Obsidian Vault" --existing --set-default
~~~

Ordinary init requires an absent final directory with an existing parent.
`--existing` preserves author notes and .obsidian, and requires all reserved
top-level entries absent: WIKI.md, pages, knowledge, sources, changes and .wiki.
Files, empty directories, symlinks and Unicode case-fold aliases collide, including
in dry-run before any mutation. Choose another wiki folder or deliberately rename
the author folder occupying a reserved name. Init never deletes it. Select an
already initialized wiki with `--wiki`.

## Capture and refresh notes

Capture an ordinary note without adopting or rewriting its author properties:

~~~sh
lwiki --offline source add "/path/to/deployment.md" --title "Atlas deployment"
~~~

Keep the returned Source path/ID. Sync discovers external canonical edits but
never recaptures an edited original. After an editor change, inspect its current
whole-file hash and explicitly name the Source and new input:

~~~sh
lwiki --offline --json inspect --path notes/deployment.md --max-bytes 6000
INPUT_HASH='blake3:paste-the-actual-whole-file-hash'
SOURCE_PATH='sources/paste-the-actual-source-directory/source.md'
lwiki --offline source refresh --path "$SOURCE_PATH" --file "$WIKI/notes/deployment.md" --input-if-match "$INPUT_HASH"
lwiki --offline index sync
lwiki --offline context 'Atlas deployment' --max-bytes 6000 --max-tokens 1500
~~~

Set WIKI to your selected root, and replace placeholders with returned values.
Path refresh requires `--input-if-match` and unique Source identity; stale input
or competing identity claims refuse. Refresh retains identity and adds immutable
history. New current citations refer to the new revision; old citations still
refer to old bytes. Never edit a captured revision to update a fact. ID-selected
refresh is also available; see [collection maintenance](sources-and-providers.md).

## Inspect and edit exact bytes

~~~sh
lwiki --offline --json inspect --path notes/deployment.md --max-bytes 65536
lwiki --offline inspect --path notes/deployment.md --start 0 --end 128 --max-bytes 128
~~~

Use ranges within `file_bytes`. Inspection coordinates are zero-based, half-open
physical whole-file bytes, including BOM, frontmatter and CRLF. `data.hash` covers
the whole file; `data.bytes` is exact, and `data.text` requires valid UTF-8.
A damaged envelope can leave `body_range` unknown. Inspection neither establishes
committed currentness nor gives bytes canonical identity or citation authority.
Structured `read` uses its returned body coordinates instead.

For an exact-byte edit, keep a backup such as `cp -p NOTE /path/to/new-backup.md`
with an absent destination. This example inserts a line at byte zero in a plain
UTF-8 note whose first byte is body text. For BOM/frontmatter, choose an observed
body location instead. Replace FILE_HASH with the actual `data.hash`:

~~~sh
cat > insert-review.json <<'PATCH'
{
  "version": "lwiki.literal-edit.v1",
  "edits": [
    {"range": {"start": 0, "end": 0}, "replacement_utf8": "Reviewed locally.\n"}
  ]
}
PATCH
lwiki schema literal-edit
FILE_HASH='blake3:paste-the-actual-whole-file-hash'
lwiki --offline --dry-run edit --path notes/deployment.md --patch insert-review.json --if-match "$FILE_HASH"
lwiki --offline --stage edit --path notes/deployment.md --patch insert-review.json --if-match "$FILE_HASH"
~~~

Use the returned change ID to review exact before/proposed bytes, then apply:

~~~sh
lwiki --offline changes show "$CHANGE_ID" --operation 0
lwiki --offline changes apply "$CHANGE_ID"
lwiki --offline index sync
~~~

Omit `--stage` for preparation and application in one command. Edits preserve
bytes outside their ranges and do not adopt ordinary notes. Valid Pages keep their
IDs and validated metadata. Stale guards require rereading and merging, rather
than replacing the hash blindly. Symlinks, traversal, nested-wiki crossings,
immutable captures and identity/operational files refuse. Invalid envelopes need
[known-byte restoration](recovery.md#restore-known-author-bytes), not guessed IDs.
Inspect the schema for range/order/replacement bounds. Identical retries can reuse
a retained outcome; that does not authorize unrelated edits.

## Create, edit and move a Page

A Page is an authored record, separate from a captured original. Create a new body:

~~~sh
cat > "$DEMO/checklist.md" <<'PAGE'
# Atlas release checklist

Run atlas_migrate before deployment. Review this checklist when the source changes.
PAGE
lwiki --offline page init --file "$DEMO/checklist.md" --title "Atlas release checklist" --path pages/checklist.md
lwiki --offline read --path pages/checklist.md --max-bytes 6000
lwiki --offline search 'Atlas release checklist' --kind page --status draft
~~~

DEMO can be the tutorial's temporary directory or another existing input directory.
Page init preserves its input, allocates metadata once and refuses an occupied
path. Drafts are discoverable but excluded from verified current context until
review/provenance requirements are satisfied. Do not merely relabel unsupported
text to make it accepted evidence. Unknown-frontmatter originals use Source capture.

Edit through your editor while preserving `wiki_id`, then run `index sync`. For
guarded whole-Page replacement, `page put --file DRAFT --if-match AUTHOR_HASH`
requires the full returned envelope plus body and current whole-file hash. Read
again after a stale guard and reconcile unrelated author work. Managed normalized
Page updates publish their selected dependencies; external changes require sync.

On a normalized wiki, move a Page by its actual ID and current read's `data.hash`:

~~~sh
lwiki --offline --json read --id "$PAGE_ID"
lwiki --offline page rename "$PAGE_ID" --to 'handbook/Operations Guide.md' --if-match "$AUTHOR_HASH"
lwiki --offline read --id "$PAGE_ID"
~~~

The absent portable Markdown destination keeps the ID. Known incoming wiki,
inline and reference links are updated with their fragments/labels intact;
ambiguous aliases and required immutable-owner edits refuse. Sync external edits
before moving. Selected incoming bytes are rechecked; excessive dependency fanout
refuses rather than omitting links. Use `--stage` and change review for separate
application. `--dry-run page rename` is an incomplete request preview: it does not
resolve identity, guards, destination or incoming links. Sequential writes are
recoverable, not a simultaneous filesystem transaction. Use the retained change
ID with `recover` after an interrupted apply.

## User configuration

Preferences use absolute `$XDG_CONFIG_HOME/lwiki/config.json`, or otherwise
`$HOME/.config/lwiki/config.json`. Relative XDG_CONFIG_HOME is an error. Preferences
are separate from provider credentials. Explicit `--preferences FILE` replaces
ambient lookup, including when combined with `--no-user-config`; that flag alone
disables ambient preferences/default selection:

~~~sh
lwiki --no-user-config --wiki "/path/to/Personal Wiki" wiki show
lwiki --preferences "/private/path/lwiki-config.json" --wiki "/path/to/Personal Wiki" wiki show
~~~

Malformed, unreadable or symlink config fails with its path. Repair that file or
use the bypass. Help/version/schema discovery needs no valid config. Reading v1
preferences preserves bytes; a supported explicit write migrates to v2 with an
exact backup. Unknown versions refuse writes unchanged. Defaults store path and
wiki_id; vault preference entries remain keyed by ID.

Default writes require the immediate config directory to be owned by the current
user and have no group/other write permission. Reads remain unchanged. Inspect the
exact reported directory first. If it is yours and intended for configuration,
`chmod u=rwx,go=rx /exact/configuration/directory` is a supported mode. Do not alter
an unfamiliar owner or files to force writes. Alternatively use a private directory
you own with `--preferences FILE`. Writes recheck directory binding and file bytes.

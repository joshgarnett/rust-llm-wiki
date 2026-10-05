# Reorganizing authored Pages

On a normalized vault, `page rename` moves one adopted Page while preserving its
stable ID and updating known incoming links. It uses selected catalog lookups and
the existing retained change/recovery engine. Other target record kinds remain
unavailable on this layout; immutable Source/Revision trees never move.

Run commands from any directory with an explicit vault root:

```sh
lwiki --wiki /path/to/wiki --offline --json read --id page_guide
lwiki --wiki /path/to/wiki --offline page rename page_guide \
  --to 'handbook/Operations Guide.md' --if-match 'blake3:CURRENT_FILE_HASH'
lwiki --wiki /path/to/wiki --offline read --id page_guide
lwiki --wiki /path/to/wiki --offline search 'operations guide'
lwiki --wiki /path/to/wiki --offline context 'operations guide'
```

Use the first read's `data.hash` as `--if-match`; the placeholder above is not a
valid hash. The destination must be an absent, portable, vault-relative Markdown
path. A changed author hash, duplicate target identity, occupied destination or
portable path collision refuses before staging. A same-path request still checks
the Page identity, author hash and selected dependencies, then reports reuse
without creating another change or publication.

The move updates wiki, inline Markdown and reference-style links that resolve to
the Page in the before catalog. It preserves fragments, labels, Markdown titles,
unrelated envelope fields and code examples. Destinations containing spaces use
the required Markdown angle brackets. Plain and readable unadopted incoming notes
are supported without adopting their IDs. Ambiguous aliases are not blindly
rewritten. A required edit in an immutable incoming owner refuses the move;
captured payload text remains byte-exact and is not a mutable navigation owner.

Incoming discovery uses published membership. After adding or editing Markdown
outside managed commands, run `index sync` before reorganizing it. Selected bytes
are hash-checked even when size and modification time remain unchanged; an
external edit to an already indexed incoming owner is preserved and refuses the
move until synchronization. The operation does not claim global freshness.

The destination is created before incoming rewrites; the old file is removed
last. Catalog identity, documents, search postings, navigation, policy and
verification facts publish together. Broad affected fanout can exceed the
existing finite row/read/byte/time budgets and refuses before staging rather
than silently omitting owners. These bounds are safeguards, not a 25K capacity
or latency qualification.

To inspect and apply separately, use `--stage` with the same rename request, then
`changes inspect CHANGE_ID` and `changes apply CHANGE_ID`. Apply rechecks retained
author and read guards. If an apply is interrupted, the error retains the change
ID; use the ordinary `recover` or `changes apply CHANGE_ID` route. Repeated apply
returns the committed outcome. This is recoverable sequential file mutation, not
a simultaneous multi-file filesystem transaction.

`--dry-run page rename` previews only the request, before opening SQLite or
resolving the target. JSON marks `plan_complete: false`, returns no sealed
operations/change/snapshot and explicitly leaves the author hash, incoming links,
destination and dependencies unchecked. A preview is not a stageable validated
move. Run without `--dry-run` to perform admission.

The normalized layout remains opt-in. The [independent workflow assessment](validation-page-reorganization.md)
passed at 10/10 with no blockers, including exact reconstruction and reached native
recovery cuts. This does not establish the unseen context-quality, default-activation
or large-vault gates.

# Troubleshoot, back up and restore

Check the selected root before repairing anything:

```sh
lwiki --wiki "/path/to/Personal Wiki" wiki show
lwiki --wiki "/path/to/Personal Wiki" --offline doctor --diagnostics --diagnostic-limit 20
lwiki --wiki "/path/to/Personal Wiki" --offline check
```

Strict check reports malformed metadata, duplicate identities, unsupported schemas
and other structured damage. Healthy selected evidence can remain usable when an
unrelated record is damaged. A contained sync must disclose its issues/exclusions;
its successful observation is not a clean-corpus claim. Failed directory observation
is nonzero and cannot prove files or answers are absent.

Doctor diagnostics page through bounded published issues; they do not perform a
new full audit. Use a returned cursor with `--diagnostic-cursor` for another page,
and choose `--diagnostic-limit` from 1 to 100. Sync distinguishes `completed`,
`completed_with_issues` and `failed`. Published issues, eligible exclusions and
unknown current membership are separate health facts. Read/search/context warnings
can disclose known issues without claiming the whole corpus was checked.

## Choose the repair for the error

| Symptom | Action |
| --- | --- |
| Init requires intent | Preview with `init --existing` for an author folder, or select an initialized wiki with `--wiki`. |
| Init reports a reserved-path collision | Preserve that path. Choose another wiki folder or deliberately rename the author folder occupying the reserved name. |
| Wiki selection/ID mismatch | Use `wiki show` with the intended explicit root. After a deliberate move, run `wiki default set NEW_PATH`; never replace IDs to satisfy a stale default. |
| Bad preferences or relative XDG_CONFIG_HOME | Repair the exact reported path/environment. `--no-user-config --wiki DIR` bypasses ambient preferences; `--preferences FILE` replaces them. |
| Stale author hash | Reinspect current bytes, merge your edit, and prepare with the new whole-file hash. |
| Malformed Page/Source metadata | Preserve exact bytes, inspect without an index, and restore a known-good editor backup or retained before-image. Do not guess a lost ID. |
| Interrupted apply | Retain the change ID and run `recover`; inspect any conflict before explicit resolution. |
| Search misses a new external file/edit | Run `index sync`. Sync does not refresh the captured version of an edited original. |
| Missing/corrupt derived index | After preserving the wiki, run explicit `index rebuild`; vector regeneration may require authorized remote work. |
| Verification budget exhausted | Follow the named resource hint after inspecting size and scope. Raising an output limit does not raise the proof allowance. |
| Missing semantic query/source vectors | Use supported literal/lexical work or explicitly authorize bounded embedding preparation. Offline never creates missing remote embeddings. |

## Restore known author bytes

1. Stop CLI writers and close the editor's pending saves.
2. Make a complete copy before attempting recovery. Use JSON physical inspect to
   retain the current whole-file hash, exact bytes and parse diagnostics.
3. Obtain the intended exact file from an editor backup or `changes show CHANGE_ID
   --operation N` retained before-image. Verify that it belongs to this file and
   desired revision; a singleton ID in damaged YAML is not proof of identity.
4. Restore that known file through your editor or ordinary file tools, preserving
   BOM/CRLF, unknown properties and intended ID. If a change is still applying,
   reconcile it through `recover`/documented conflict resolution first.
5. Run sync, strict check, and a selected read/search for the expected fact. Preserve
   the failed attempt and verify the actual returned evidence.

The CLI refuses unsafe structured writes rather than inventing repairs. Physical
inspection can expose bytes despite metadata damage; it does not make them
trusted canonical or immutable evidence.

## Backup and restore

Back up the whole wiki while writers/editors are stopped. Include WIKI.md, ordinary
notes, Sources, immutable originals/content/revisions, Pages, graph/decision data,
change history, research reports and **all hidden `.wiki/` retained state**. Keep
private provider/user configuration separately with its proper access controls.
Only rebuildable caches may be regenerated; a cache-only backup loses knowledge
and recovery/accounting authority.

On macOS/Linux, a simple complete local copy into a new absent backup path is:

```sh
cp -a "/path/to/Personal Wiki" "/path/to/new-backup-copy"
```

Verify the copy's inventory and exact source hashes using your backup tooling.
Test restoration into another **absent** path, leaving the original untouched:

```sh
cp -a "/path/to/new-backup-copy" "/path/to/new-restore-copy"
lwiki --no-user-config --wiki "/path/to/new-restore-copy" --offline index rebuild
lwiki --no-user-config --wiki "/path/to/new-restore-copy" --offline check
lwiki --no-user-config --wiki "/path/to/new-restore-copy" --offline context 'Atlas deployment' --max-bytes 6000 --max-tokens 1500
```

Compare selected facts/citations with the original. The copied wiki retains its
ID; choose its location explicitly. Refresh a copied Source only with an explicitly
named input file, never by assuming a remembered external path still belongs to
this copy. `storage plan` is a separate read-only preview; take a verified complete
backup before authorized `storage cleanup`. Restoring a pre-upgrade complete copy
also restores the earlier schema and retained undo/accounting state.

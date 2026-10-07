# Maintain individually imported Sources

Use this route to update selected files from a completed local import and revise
a cited draft Page. Keep the original manifest and the import key. A new key
creates fresh Sources; matching paths, titles or content do not establish identity.
The [public route validation](validation-import-maintenance.md) exercises these
existing commands on twelve small inputs.

## Recover the original mapping

Inspect the completed import:

```sh
lwiki --wiki VAULT --offline --json source import status --key IMPORT_KEY
```

`results_path` names a documented JSONL artifact relative to the vault. Open that
file and the original external manifest with ordinary filesystem tools. For each
`group_committed` event, join its `result.items` with manifest items by `ordinal`.
Retain the original path, extraction policy, original hash, Source ID and imported
Revision ID. Require complete ordinal coverage, no duplicates and agreement with
the completed status counts. `last_group` alone omits earlier groups.

This join is a way to consume CLI-produced owned artifacts. Status success is
not a certificate for the entire acknowledged results prefix. Do not silently
accept malformed, incomplete or inconsistent mappings, or hand-edit operational
state to repair them.

For each selected item, inspect the canonical records:

```sh
lwiki --wiki VAULT --offline --json read --id SOURCE_ID
lwiki --wiki VAULT --offline --json read --id IMPORTED_REVISION_ID
```

Check the Source identity and origin, imported Revision owner/original hash and
extraction status. Observe the Source's present status and current revision
separately. If its head changed since import, read that current Revision too.
Compare a candidate file with the current capture and extraction/title policy;
the original manifest hash alone is not an unchanged-file test. Report and skip
missing inputs or withdrawn Sources unless a separate action was intended.
Equal bytes at two paths can belong to two distinct Sources.

## Stage, review, then apply

```sh
lwiki --wiki VAULT --offline --json --stage source refresh SOURCE_ID --file INPUT
lwiki --wiki VAULT --offline changes show CHANGE_ID
lwiki --wiki VAULT --offline --json changes apply CHANGE_ID
```

Stage first and review the resulting Change before applying. Check the Source's
expected before-state and the complete proposed original payload. Resolve any
omitted or unavailable payload before deciding. `changes show CHANGE_ID
--operation NUMBER` can inspect a selected operation. An unchanged refresh can
return reused content without a Change; that is an observation, not a promise
that future input or Source state will stay unchanged.

The reviewed payload is the accepted snapshot. Changing the external file after
staging does not change it. An earlier Source read or dry run does not constrain
a later staging request: if staging exposes a different baseline, stop or
explicitly review that new baseline. Apply checks its retained authority and
dependencies. Keep refused or unresolved operations and use ordinary recovery;
do not discard them to force a write.

## Reconcile the cited Page

Read fresh context or verified search for the selected Source IDs, then inspect
the actual returned text, qualifications, omissions and citations. Assemble the
revised draft from those returned bytes. Preserve the Page record, unrelated
author text and notes; old citations remain references to their old immutable
revisions, even after refresh.

```sh
lwiki --wiki VAULT --offline --json read --id PAGE_ID
lwiki --wiki VAULT --offline --json page put --file DRAFT --if-match AUTHOR_HASH
lwiki --wiki VAULT --offline --json read --id PAGE_ID
```

Use the author hash from the Page read. A stale hash must refuse the update;
read and reconcile the actual current Page instead of overwriting its author.
Inspect the saved Page's facts and citations. This explicit editorial workflow
does not automatically rewrite Pages or reactivate withdrawn Sources.

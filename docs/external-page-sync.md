# Synchronize Pages edited outside the CLI

After creating, editing, deleting or restoring an authored Page in your editor,
synchronize the wiki:

```sh
lwiki --offline --wiki /path/to/wiki index sync
```

On an existing normalized catalog, a supported group of 1–16 changed Pages can
update the selected catalog without reconstructing the corpus. The command still
compares canonical inputs and checks them again before publication. Other changes,
missing before-images or unsupported inputs use ordinary reconstruction. You do
not need to select a different command.

Keep the Page's `wiki_id` stable when editing or restoring it. Restoring its exact
Markdown preserves its identity and authored text. Synchronization updates search
membership and known link relationships while retaining captured Source revisions.
It does not extract assertions or request embeddings.

Draft Pages remain discoverable through an explicit draft search:

```sh
lwiki --offline --wiki /path/to/wiki search 'your terms' --kind page --status draft
```

Default current context excludes drafts. A restored Page's citation may still
refer to a historical Source revision; synchronizing or restoring the Page does
not make that revision current. Use verified Source reads to distinguish current
and historical evidence.

The JSON maintenance response distinguishes selected Page work (`page_sync`)
from reconstruction (`build`). Selected work reports created, edited and deleted
Page counts and measured phases. The
[validation checkpoint](validation-external-page-sync.md) records functional,
recovery and separate 10K timing evidence. Full 25K and release qualification
remain open.

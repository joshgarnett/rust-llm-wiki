# Troubleshoot, back up and restore

Start by checking that you are working with the intended wiki:

```sh
lwiki --wiki "/absolute/path/to/Wiki" wiki show
lwiki --wiki "/absolute/path/to/Wiki" --offline doctor --diagnostics --diagnostic-limit 20
```

`doctor` shows known problems and suggested next steps. It uses the latest
recorded diagnostics. To check all managed files now, run:

```sh
lwiki --wiki "/absolute/path/to/Wiki" --offline check
```

A damaged note can be excluded from results while other notes remain usable.
Read any warnings so you know what was skipped. A successful search or read does
not mean every file in the wiki is healthy.

## Choose the repair for the error

| Problem | What to do |
| --- | --- |
| Setup or init reports an existing folder | Use `setup PATH --existing` for a notes folder, or `setup PATH --select` for a wiki. |
| A managed name is already in use | Preserve it and choose a new wiki folder, or deliberately rename the conflicting item. |
| The saved wiki moved or its ID does not match | Check the intended folder with `--wiki PATH wiki show`. After a deliberate move, set the new path with `wiki default set PATH`. |
| Configuration cannot be read | Fix the reported file. To work without saved settings, use `--no-user-config --wiki PATH`. |
| A file changed after you inspected it | Read it again, merge your edit, and use its new hash. |
| A page has invalid metadata | Keep a copy, inspect the file, and restore a known-good version as described below. |
| A change was interrupted | Run `recover`, then inspect any reported conflict before resolving it. |
| A new page or editor change is missing from search | Run `index sync`. For an edited source original, use `source refresh` too. |
| The search index is missing or corrupt | Back up the wiki, then run `index rebuild`. Remote vectors may need paid regeneration. |
| A result exceeds a verification limit | Follow the error's size or scope hint. Increasing output size alone does not raise the verification allowance. |
| Search by meaning has no usable vectors | Use literal or word search, or [prepare embeddings](sources-and-providers.md#optional-remote-providers). Offline commands cannot create missing remote embeddings. |

If diagnostics return a cursor, pass it with `--diagnostic-cursor` to read the
next page. `--diagnostic-limit` accepts 1 to 100.

## Restore known author bytes

Use an editor backup or a retained before-image when a file's metadata is
damaged. Keep a complete backup before repairing it.

1. Stop lwiki writers and close the editor's pending saves.
2. Preserve the damaged file. `lwiki --offline --json inspect --path PATH` can
   show its exact bytes, whole-file hash and parse errors without using the index.
3. Find a known-good copy from your editor or backup. For a change made through
   lwiki, inspect `lwiki --offline changes show CHANGE_ID --operation N` for its
   retained before-image. Check that it is the right file and version.
4. Restore that copy with your editor or file tools. Keep its original ID and
   metadata. If `recover` reports a change still in progress, resolve that change
   first so it does not overwrite your repair.
5. Run `lwiki --offline index sync` and `lwiki --offline check`. Read the restored
   page or search for a fact you expect to confirm the result.

Do not invent a replacement ID or guess missing metadata to silence an error.
If you cannot find a trustworthy copy, preserve the damage and report the
problem with private content removed.

## Backup and restore

Stop writers and pending editor saves, then copy the whole wiki, including all
hidden files. The `.wiki/` folder contains recovery information as well as search
caches. Keep your private configuration and credentials in a separate protected
backup.

On macOS or Linux, copy the wiki to a new backup path:

```sh
cp -a "/absolute/path/to/Wiki" "/absolute/path/to/new-wiki-backup"
```

The destination must not already exist. Verify the backup with your usual backup
tools. Test restoration to another new folder, leaving the original untouched:

```sh
cp -a "/absolute/path/to/new-wiki-backup" "/absolute/path/to/restored-wiki"
lwiki --no-user-config --wiki "/absolute/path/to/restored-wiki" --offline index rebuild
lwiki --no-user-config --wiki "/absolute/path/to/restored-wiki" --offline check
lwiki --no-user-config --wiki "/absolute/path/to/restored-wiki" --offline context 'a question your notes answer'
```

Replace the paths and question. Check that the restored wiki contains your pages
and that the answer passages match those from the original. The copy keeps the
same wiki ID, so select its location explicitly while testing.

Before `storage cleanup`, make and verify a complete backup. Do not move or
delete hidden folders by hand.

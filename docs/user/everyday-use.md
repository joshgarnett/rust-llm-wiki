# Everyday use

Use your agent for these tasks or run the commands yourself. The examples use
placeholder paths, IDs and questions; replace them with your own. Run
`lwiki wiki show` first if you are unsure which wiki a command will use.

## Choose the wiki

Your saved default lets you use the wiki from any folder:

```sh
lwiki wiki show
```

The result shows the selected folder and its ID. A command with `--wiki` uses
that folder. Otherwise, lwiki uses the nearest wiki containing your current
folder, then your saved default. If the selected folder is damaged or has been
replaced with another wiki, lwiki reports an error instead of choosing a different
one.

To change the saved default or stop using one:

```sh
lwiki wiki default set "/absolute/path/to/Another Wiki"
lwiki wiki default clear
```

After moving a wiki, set its new location as the default. For a single command,
use `lwiki --wiki "/absolute/path/to/Wiki" COMMAND`. Agents can also pass
`--expect-wiki-id ID` to ensure a write goes to the wiki they inspected.

## Initialize an existing editor folder

Use [setup](getting-started.md#set-up-your-wiki) to choose a folder and connect
an agent. For direct initialization of an existing notes folder:

```sh
lwiki --dry-run init "/absolute/path/to/My Notes" --existing
lwiki init "/absolute/path/to/My Notes" --existing --set-default
```

The first command checks whether lwiki can use the folder. The second creates
its wiki files and saves the default. Existing notes and `.obsidian` settings
stay in place.

The names `WIKI.md`, `pages`, `sources`, `knowledge`, `changes` and `.wiki` must
be unused. If initialization reports a collision, preserve that item and choose
a new wiki folder, or rename the item deliberately before trying again. A new
folder still lets you capture notes from your existing editor folder.

## Capture and refresh notes

Save copies of the notes you want the agent to cite:

```sh
lwiki source capture "/absolute/path/to/project-notes.md"
lwiki source capture "/absolute/path/to/Notes/project"
```

Use either command, or give several file and folder paths to one capture command.
Folders are searched recursively for Markdown and plain-text notes. Capture
leaves originals unchanged and prints a summary of saved files. Add
`--markdown-only` to leave out plain-text files and other attachments.
Read a saved note using a source ID returned by capture or lookup:

```sh
lwiki source read SOURCE_ID --max-bytes 12000
```

After editing your originals, repeat the same capture command. It adds new files,
refreshes changed files under their existing IDs and reuses unchanged copies.
Earlier copies remain available so old citations keep their original text.
Check skipped or failed files in the result before relying on complete coverage.

With automatic search by meaning enabled, a successful capture prepares eligible
changed text within finite limits. Add `--offline` to capture, read or lookup
commands to block provider requests. Local capture and word search still work.

For a previously saved source whose input has moved, refresh it explicitly:

```sh
lwiki --offline source refresh SOURCE_ID --file "/absolute/path/to/new-location.md"
```

Use the actual source ID. `source add FILE --title TITLE` remains available when
you deliberately want a separate new source or a custom title.

If you have the managed source path instead of its ID, you can use the guarded
path route. Inspect the input note inside your wiki, then copy the returned
whole-file `data.hash` into the command:

```sh
lwiki --offline --json inspect --path notes/project-notes.md --max-bytes 6000
lwiki --offline source refresh --path sources/SOURCE_ID/source.md --file "/absolute/path/to/Wiki/notes/project-notes.md" --input-if-match INPUT_HASH
```

Replace both `SOURCE_ID` and `INPUT_HASH` with actual returned values. The hash
check stops the command if the input changed after inspection.

`index sync` picks up edits to wiki pages and other files in the wiki. It does
not capture a new copy of an edited original. Repeat capture, or refresh explicitly, when you want
the agent to cite the update. See [collection maintenance](sources-and-providers.md#update-notes-from-an-advanced-import)
for imported notes.

## Browse your wiki in Obsidian

Open your wiki folder as an Obsidian vault. Your pages are in `pages/`, and saved
sources have readable filenames in `sources/views/`. Follow a page's source links
to read the notes behind it. A citation to an older copy still opens that copy,
even after you refresh the source.

Edit your pages normally. To change a saved source, edit its original and repeat
the capture command. The notes in `sources/views/` are maintained by lwiki. If you
edit one, lwiki preserves your changes and asks you to resolve the conflict before
replacing it.

New wikis use this arrangement automatically. For a wiki created with an older
version, close pending editor saves and preview the change:

```sh
lwiki --offline --dry-run storage migrate
lwiki --offline storage migrate
```

Resolve any issues reported by the preview before running the second command.
Migration keeps your notes and history and creates a backup. Existing Obsidian
settings stay unchanged. Keep regular [full backups](recovery.md#backup-and-restore),
including the hidden `.wiki/` folder; editor sync alone is not a full wiki backup.

## Inspect and edit exact bytes

For normal writing, use your editor. Agents and scripts can inspect a file and
make a guarded edit:

```sh
lwiki --offline --json inspect --path notes/project-notes.md --max-bytes 65536
lwiki schema literal-edit
```

Inspection returns the file's bytes and whole-file hash, even when its metadata
cannot be parsed. Edit ranges use physical byte positions, starting at zero;
the end position is excluded. Include frontmatter and line endings when counting
positions. Use `read` when you want the note's text instead.

Create a patch file in the format shown by `schema literal-edit`, then replace
`FILE_HASH` with the inspected `data.hash`:

```sh
lwiki --offline --dry-run edit --path notes/project-notes.md --patch PATCH.json --if-match FILE_HASH
lwiki --offline --stage edit --path notes/project-notes.md --patch PATCH.json --if-match FILE_HASH
```

A staged edit waits for review. Copy its returned change ID into these commands:

```sh
lwiki --offline changes show CHANGE_ID --operation 0
lwiki --offline changes apply CHANGE_ID
```

Review the before and proposed text before applying. A stale hash means the file
changed; read it again and merge your edit. Do not replace the hash merely to
force the write. Captured copies and operational files cannot be edited through
this command. For damaged metadata, [restore a known-good file](recovery.md#restore-known-author-bytes).

## Create, edit and move a Page

Ask your agent to save a cited draft, or write the body in your editor and create
a page from it:

```sh
lwiki --offline page init --file "/absolute/path/to/draft.md" --title "Project summary" --path pages/project-summary.md
lwiki --offline read --path pages/project-summary.md --max-bytes 6000
```

The original draft stays unchanged. The new page appears in `pages/` with its
own ID and draft status. Drafts can be found by search:

```sh
lwiki --offline search 'Project summary' --kind page --status draft
```

Drafts are excluded from default cited context until their review and source
requirements are met. Keep unsupported claims and missing facts visible during
review; changing a status label does not supply evidence for them.

Open the page in your editor to change its text or title. Preserve its `wiki_id`
in the frontmatter, then update search:

```sh
lwiki --offline index sync
```

For guarded replacement, agents use `page put --file FILE --if-match HASH` with
the full page, including frontmatter, and its current whole-file hash.

To move a page and update known links to it, first prepare the search index,
then read the page by its actual ID. Back up an existing wiki before changing
its index layout:

```sh
lwiki --offline index rebuild --normalized
lwiki --offline --json read --id PAGE_ID
lwiki --offline page rename PAGE_ID --to 'handbook/Project summary.md' --if-match PAGE_HASH
lwiki --offline read --id PAGE_ID
```

Replace `PAGE_HASH` with `data.hash` from the read. The destination must be a new
Markdown path. If links are ambiguous or an affected file changed, lwiki stops and reports the
problem. Resolve it before retrying. You can add `--stage` to review the move
before applying it.

## User configuration

See the saved wiki, model and authentication choices without reading a secret
or making a network request:

```sh
lwiki config show
```

Run `lwiki setup` to change those choices. Settings live in
`$XDG_CONFIG_HOME/lwiki/config.toml` when `XDG_CONFIG_HOME` is an absolute path,
or otherwise in `$HOME/.config/lwiki/config.toml`. Provider settings and any
saved key file are separate private files. The configuration display tells you
which files are in use.

Explicit command options override saved preferences. For a separate setup or
automation, choose a configuration file with `--preferences FILE`. To bypass
saved user settings, supply the wiki explicitly:

```sh
lwiki --no-user-config --wiki "/absolute/path/to/Wiki" wiki show
```

If configuration is malformed, fix the reported file or use this bypass to work
with your notes. Older `config.json` settings are read when no `config.toml` is
present. Setup can migrate them, preserving the old file and an exact backup.
Do not delete that backup until you have checked the new settings.

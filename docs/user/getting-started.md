# Use your notes with your agent

Create a wiki you can keep, connect your coding agent, and ask it questions about
your own notes. Start by [installing lwiki](install.md). You also need Codex,
Sagex, Claude Code or Cursor installed to follow the agent steps. You can use the
terminal commands below without an agent.

## Set up your wiki

Open a terminal and run:

```sh
lwiki setup
```

Setup walks you through these choices:

1. Create a wiki or use one you already have. Choose a lasting location, such as
   the suggested `Wiki` folder in your home directory. Give it a title.
2. Save this wiki as your default so later commands can find it from any folder.
3. Choose `codex`, `sagex`, `claude-code` or `cursor` to install the wiki instructions for
   your agent. Accept your home folder unless your agent uses a different one.
4. Choose whether to use search by meaning. If you want it, enter your provider
   model, endpoint and credential source. Review the named wiki, destination and
   model before allowing automatic requests that send note text and questions.
   You can skip it to start without an API key and
   [add it later](sources-and-providers.md#optional-remote-providers).
   Skip generation unless you want a separate extraction service; your agent
   can write answers without it.
5. Review the choices and confirm setup. Automatic search permission applies
   only to the wiki, destination and embedding model you approved.

When setup prints **Your wiki is ready.**, restart your agent session so it can
find the installed wiki instructions. Open the folder shown by setup in Obsidian
or your preferred Markdown editor. `WIKI.md` is its start page.

You can check the selected folder later with `lwiki wiki show`, or inspect model
and connection settings with `lwiki config show`.

To use an existing notes folder, supply its path:

```sh
lwiki setup "/absolute/path/to/My Notes" --existing
```

Setup keeps your notes and editor settings. It needs its own names, including
`WIKI.md`, `pages`, `sources`, `knowledge`, `changes` and `.wiki`, to be unused.
If one is already present, choose a new wiki folder instead. You can capture
notes from the original folder without moving them.

## Give your agent a real task

Restart your chosen agent so it can discover the installed `llm-wiki`
instructions. Tell it which notes to use and what you want to know:

> Use llm-wiki and my default wiki. Capture my notes from
> /absolute/path/to/Notes/project. Summarize the decisions about the next release,
> cite the notes for each decision, and list any missing information.

Replace the notes path and question with your own. You can start with one note
instead of a folder. Ask the agent to capture only the notes you choose.
Capturing means saving a copy that it can cite; it leaves the original file
unchanged. If your agent cannot access those files, use the terminal capture
command in the next section first.

Check that the answer includes citations you can follow to passages from your
notes. Read the passages to see whether they support the answer. If something
is missing, ask the agent to search for that detail and read the relevant notes.

Once the answer is useful, ask for a wiki page:

> Save this as a draft page in my wiki. Keep the citations and the unanswered
> questions so I can review it. Tell me the saved file path.

Open the reported file in your editor and review it. You can keep editing your own notes
as usual. [Capture their updated versions](everyday-use.md#capture-and-refresh-notes)
when you want the agent to use those changes.

## Use the terminal directly

Capture a note or folder, then ask for relevant passages:

```sh
lwiki source capture "/absolute/path/to/Notes/project"
lwiki context 'your question about the project' --max-bytes 6000 --max-tokens 1500
```

Replace the path and question before running the commands. Capture saves the
chosen files and prints their source IDs and citation status. It searches folders
recursively. Repeat the same command after editing your notes: it refreshes
changed files, adds new ones and reuses unchanged copies.

Context prints passages and citations rather than an answer. Read them yourself
or give them to your agent. If search by meaning is enabled, these commands can
use your approved provider; otherwise they use local search. Read any warnings,
and search for a specific phrase or read the saved note if an important detail
is missing. Add `--offline` to both commands to prevent provider requests.

Read a saved note directly using its returned source ID:

```sh
lwiki source read SOURCE_ID --max-bytes 12000
```

Replace `SOURCE_ID` with an ID from capture. The result includes the saved text
and a citation for that text. Follow any continuation instruction to read more.

To find an exact word or command you remember from the note:

```sh
lwiki --offline search 'a phrase from your note' --mode literal
```

You should see the matching text. If you cannot find the fact you
need, use the [search guide](retrieval.md#search-and-inspect-cited-context).
Continue with [everyday use](everyday-use.md) for updates and editing, or
[troubleshooting](recovery.md) if a command reports a problem.

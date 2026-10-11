# Use your notes with your agent

Create a wiki you can keep, connect your coding agent, and ask it questions about
your own notes. Start by [installing lwiki](install.md). You also need Codex,
Claude Code or Cursor installed to follow the agent steps. You can use the
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
3. Choose `codex`, `claude-code` or `cursor` to install the wiki instructions for
   your agent. Accept your home folder unless your agent uses a different one.
4. Skip the optional embedding model to start without an API key. You can
   [add search by meaning later](sources-and-providers.md#optional-remote-providers).
5. Review the destinations and confirm setup.

When setup prints **Your wiki is ready.**, it also prints the wiki location and
what it installed. Check your saved choice:

```sh
lwiki wiki show
lwiki config show
```

The wiki path should be the folder you chose. Open that folder in Obsidian or
your preferred Markdown editor. `WIKI.md` is its start page.

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
> questions so I can review it.

Open the page in your editor and review it. You can keep editing your own notes
as usual. [Capture their updated versions](everyday-use.md#capture-and-refresh-notes)
when you want the agent to use those changes.

## Use the terminal directly

Capture one of your notes, then ask for relevant passages:

```sh
lwiki --offline source add "/absolute/path/to/your-note.md" --title "Your note title"
lwiki --offline context 'your question about that note' --max-bytes 6000 --max-tokens 1500
```

Replace the path, title and question before running the commands. Capture prints
the saved note's ID and path; keep these for future updates. Context prints
passages and citations. It supplies evidence for an answer; your agent writes
the answer.

To find an exact word or command you remember from the note:

```sh
lwiki --offline search 'a phrase from your note' --mode literal
```

You should see the matching text. If you cannot find the fact you
need, use the [search guide](retrieval.md#search-and-inspect-cited-context).
Continue with [everyday use](everyday-use.md) for updates and editing, or
[troubleshooting](recovery.md) if a command reports a problem.

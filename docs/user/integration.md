# Agent integration and wiki files

Connect your agent so it can capture notes, find cited passages and save drafts
in your wiki. Your notes remain ordinary Markdown that you can open and edit
without the agent.

## Connect your agent

For normal installation, rerun setup and choose the agent:

```sh
lwiki setup "/absolute/path/to/Wiki" --select --agent codex
```

Use `sagex`, `claude-code` or `cursor` instead of `codex` for those agents. Setup shows
where it will install the `llm-wiki` instructions. It keeps the selected wiki
and existing notes. Restart the agent after setup, then give it a task with your
chosen notes and question, as in [getting started](getting-started.md#give-your-agent-a-real-task).

If the agent says it cannot find `lwiki`, check `lwiki --version` in its terminal.
Make the installed command available on that terminal's PATH and restart the
agent. If the instructions are not discovered, check the installation location
reported by setup and ask explicitly to use `llm-wiki`.

To connect an agent with a different home folder, pass `--agent-directory`.
This is the agent's home folder, not the final skill directory:

```sh
lwiki setup "/absolute/path/to/Wiki" --select --agent codex --agent-directory "/absolute/path/to/agent-home"
```

To make the instructions available only inside one project, choose project scope:

```sh
lwiki skill install --target codex --scope project --directory "/absolute/path/to/project"
```

Open your agent in that project. Use the same `--scope project` and `--directory`
when checking, updating or removing these instructions. Project installation
keeps existing `AGENTS.md`, `CLAUDE.md` and other agent settings unchanged.
You can also choose this during setup with `--agent-scope project` and
`--agent-directory "/absolute/path/to/project"`.

For a portable copy to inspect or install yourself, export the instructions:

```sh
lwiki --offline skill export --target codex --output "/absolute/path/to/skill-export"
```

The export includes instructions, examples and the command reference for the
running lwiki version. It does not install or launch the agent. Keep one
installed copy per agent so old and new instructions do not compete.

## Check, update or remove agent instructions

For Sagex, the commands in this section use `--target codex`, which manages the
same shared instructions installed by `setup --agent sagex`.

Check the instructions installed for your agent:

```sh
lwiki skill status --target codex
```

After updating lwiki, update its agent instructions and restart the agent:

```sh
lwiki skill update --target codex
```

Use `claude-code` or `cursor` for your other agents. Installed agents share one
copy of the instructions, so an update also updates other agents connected by
lwiki. The command reports the affected paths.

To disconnect an agent:

```sh
lwiki --dry-run skill remove --target codex
lwiki skill remove --target codex
```

Removal keeps your wiki and unrelated agent settings. Codex and Cursor use the
same instruction location; it stays in place if the other agent still uses it.
If you changed an installed file yourself, lwiki leaves it alone and reports the
conflict. Use `--directory` with these commands if you chose a different agent
home folder during setup.

## Work with cited answers

A useful task names the notes, the question and the result you want. For example:

> Use llm-wiki to compare the decisions in these two notes. Cite each difference,
> identify unresolved questions, and save a draft summary for me to review.

The agent finds the selected wiki, saves copies of the notes and reads evidence
before writing. Its answer should distinguish what the notes say from its own
inferences. Check cited passages and review any saved draft before relying on it.

Text inside a captured note is evidence to read. It does not give the agent
permission to follow embedded commands or spend money. Give the agent those
instructions separately when they are part of your task. Your agent's own model
costs are separate from lwiki's optional remote-provider calls.

## Understand the files

You can browse the wiki in Obsidian or another Markdown editor. `WIKI.md` is the
start page. Edit its readable text if you want to customize it; preserve the
`wiki_id` in its frontmatter.

| Location | What you will find |
| --- | --- |
| `pages/` | Pages you and your agent write, including drafts |
| `sources/` | Saved source notes and their earlier captured versions |
| `knowledge/` | Reviewed facts, their evidence and related records |
| `changes/` | Records of changes made through lwiki |
| `.wiki/` | Hidden files needed for search, recovery and remote-call accounting |

Keep writing your original notes wherever you prefer. Creating a wiki in an
existing folder does not automatically capture them. [Capture a chosen note](everyday-use.md#capture-and-refresh-notes)
when you want it available as cited evidence.

Obsidian can change line endings and remove a byte order mark (BOM) when saving.
lwiki captures the saved file exactly.

A page's title and filename can change while its `wiki_id` stays the same.
Preserve the `wiki_*` fields when editing managed files. Do not edit saved source
versions to update a fact; refresh the source from your updated original.

Links work like normal Markdown links. Obsidian can display linked notes, but
lwiki's recorded relationships do not become special Obsidian graph labels.
No Obsidian plugin is needed.

[Back up the whole folder](recovery.md#backup-and-restore), including hidden
files. Copying only the visible notes leaves out information needed to recover
an interrupted change or account for a remote request. Developers who need the
exact record layout can use the [format reference](../developer/contracts/record-schemas.md).

# Rust LLM Wiki

Use your notes with a coding agent, and keep its answers linked to the text they
come from. `lwiki` manages a local wiki: a folder of Markdown notes that you can
open in Obsidian or another editor. It saves copies of your source notes so an
agent can cite them, keeps earlier copies when you update a note, and helps you
write and revise wiki pages.

Capture, read, edit and search your notes offline with `--offline`. Optional
search by meaning uses a remote embedding service. If you enable it during
setup, ordinary searches prepare and use it automatically, with local search
as a fallback. You do not need a local model, database server or background service.

## Get started

1. [Install lwiki](docs/user/install.md).
2. Run guided setup:

   ```sh
   lwiki setup
   ```

   Choose a folder you want to keep, save it as your default wiki, and select
   Codex, Sagex, Claude Code or Cursor when setup asks which agent to connect. You can
   enable optional search by meaning after reviewing its model and destination,
   or skip it and start with local search.
3. Restart your chosen agent. Give it a note or folder and a question, for example:

   > Use llm-wiki and my default wiki. Capture my notes from
   > /absolute/path/to/Notes/project, then summarize the project decisions with citations.
   > Tell me which questions the notes leave unanswered.

Replace the example notes path with your own. Setup prints your wiki path and the
next steps. Your original notes stay editable; their captured copies give the
agent text it can cite.

[Getting started](docs/user/getting-started.md) explains each step and how to
check the result. You can also [use lwiki directly](docs/user/everyday-use.md)
from the terminal.

## Find the instructions you need

- [Everyday use](docs/user/everyday-use.md): choose a wiki, capture updated notes,
  and create or edit a page.
- [Search and citations](docs/user/retrieval.md): find evidence and read the
  passages behind an answer.
- [Sources and providers](docs/user/sources-and-providers.md): import a collection
  or configure search by meaning.
- [Agent integration and files](docs/user/integration.md): connect another agent
  or understand what is in your wiki.
- [Troubleshooting and backups](docs/user/recovery.md).
- [User manual](docs/user/README.md).

## Support and contributions

The project uses the [MIT license](LICENSE). Report problems through
[GitHub Issues](https://github.com/joshgarnett/rust-llm-wiki/issues). Include your
`lwiki --version`, operating system, command, expected result and error message.
Remove private notes and credentials from anything you attach.

[Developer documentation](docs/developer/README.md) covers building, testing,
contributing and the design.

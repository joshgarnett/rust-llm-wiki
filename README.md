# Rust LLM Wiki

Keep a local Markdown wiki, capture source notes, and give people or coding agents
exact cited evidence. `lwiki` works offline for capture, reading, editing, literal
search and context. It needs no model runtime, database server or background service.
Semantic embeddings are optional remote API work; vectors are stored locally.

## Start with your notes

With a suitable `lwiki` binary on your PATH:

```sh
lwiki init "/path/to/Personal Wiki" --set-default
lwiki wiki show
lwiki --offline source add "/path/to/deployment-notes.md" --title "Deployment notes"
lwiki --offline search 'atlas_migrate' --mode literal
lwiki --offline context 'Atlas deployment' --max-bytes 6000 --max-tokens 1500
```

[Getting started](docs/user/getting-started.md) walks you through creating your wiki
and adding your notes. [Everyday use](docs/user/everyday-use.md) covers updating notes
and writing wiki pages. Existing
editor folders require `init --existing`; reserved managed names must be absent.
Open the wiki folder in Obsidian or any Markdown editor. Initialization preserves
ordinary notes and `.obsidian` and does not adopt those notes automatically.

## Choose your task

- [Installation and binary checks](docs/user/install.md).
- [Everyday use](docs/user/everyday-use.md): choose a wiki, capture/refresh notes, edit or move a Page.
- [Recovery and backups](docs/user/recovery.md).
- [Retrieval reference](docs/user/retrieval.md): citations, named neighbors and host selection.
- [Sources and providers](docs/user/sources-and-providers.md): collections, batches and optional remote work.
- [Integration and format](docs/user/integration.md), or [the manual index](docs/user/README.md).

Context assembles evidence; it does not generate an answer. A citation identifies
exact captured bytes and their revision, while you still assess whether they
support the claim. Bounded results can omit needed facts. Ordinary notes remain
editable originals; captured revisions remain immutable.

## Availability and support

This checkout implements the v0.3.0 workflow while local, installed-product and
release qualification remain in progress. It does not establish a published
v0.3.0 download or installer. Use the [installation guide](docs/user/install.md)
for download and build instructions and [qualification status](docs/developer/validation-status.md)
for the remaining release checks. macOS distribution is intentionally unsigned; Windows vault writes
remain unsupported.

The project uses the [MIT license](LICENSE). Report reproducible issues through
[GitHub Issues](https://github.com/joshgarnett/rust-llm-wiki/issues), including
`lwiki --version`, OS, command, expected behavior and redacted errors. Keep private
notes, provider secrets and accounting receipts out of public issue attachments.
There is no promised response time.

## Contribute

[Developer documentation](docs/developer/README.md) routes current contracts,
build commands, architecture decisions and reproducible evaluation protocols.
Development uses disposable vaults and mock providers. Local tests, native
recovery, live interoperability, retrieval completeness and final release bytes
have separate acceptance boundaries; none substitutes for another.

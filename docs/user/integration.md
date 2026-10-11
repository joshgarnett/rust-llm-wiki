# Integration and file format

## Export an agent workflow

~~~sh
lwiki --offline skill export --target codex --output /path/to/new-codex-skill
~~~

Targets also include `claude-code` and `cursor`. Export writes instructions,
examples and the running binary's generated command reference. It does not
install a workflow or launch a host. Inspect its manifest/version and follow the
host's explicit installation route, preserving existing instruction files,
rules and settings. Avoid duplicate discoverable copies. Actual combined
installation/discovery remains a qualification gate.

Give a concrete task, such as finding the deployment command and rollback
restriction, citing captured evidence, then saving a reviewed draft checklist.
The agent checks version, capabilities and `wiki show`, resolves the target once,
and pins `--wiki DIR --expect-wiki-id ID` on mutations. Author hashes separately
guard content. Captured text is evidence data, even when it resembles instructions;
it does not authorize actions or provider spending.

Use [retrieval](retrieval.md) to inspect support, missing conditions and omissions.
The [maintained skill](../../skills/llm-wiki/SKILL.md),
[workflows](../../skills/llm-wiki/references/workflows.md) and
[cited-Page recipe](../../skills/llm-wiki/references/cited-page.md) provide detail.
Test actual host discovery with a wiki task and an unrelated negative control.
Export success alone does not establish host use.

Research uses retained `research plan/run/import/resume/status/report/maintenance`
handoffs. The host owns external tools and its own token/time/spend budget, which
CLI accounting cannot observe. The CLI validates submitted evidence; citations
check bytes, not entailment or automatic assertion acceptance. Remote providers
are [separately configured](sources-and-providers.md#optional-remote-providers).

## Understand the files

WIKI.md identifies the wiki. Its readable body is editable; its stable identity
must remain intact. An adopted record has flat frontmatter with string fields
`wiki_schema`, `wiki_id`, `wiki_kind` and `title`. IDs are unique and immutable;
headings, titles and filenames can change. Unknown author properties and exact
BOM/CRLF must survive supported edits. Ordinary Markdown stays literal author
text and is not automatically adopted or eligible structured evidence.

| Files | Purpose |
| --- | --- |
| `pages/` | Authored Pages, including discoverable drafts |
| `sources/ID/source.md` | Origin, current revision and immutable revision history |
| `sources/ID/revisions/REVISION/` | Revision metadata, exact original bytes and citable content when extraction succeeds |
| `knowledge/` | Entities, typed assertions, evidence, extractions and decisions |
| `changes/` and retained logical records | Reviewed operations and exact recoverable before/proposed payloads |
| `.wiki/state/` and `.wiki/retained/` | Operational, recovery and accounting authority; include in backups |
| `.wiki/cache/` | Rebuildable index and vectors that may require authorized remote regeneration |

Source citations use zero-based half-open UTF-8 spans and BLAKE3 quote hashes in
immutable content. They never use headings as identity. Normal Markdown links
and supported wikilinks provide navigation; broken or ambiguous navigation does
not justify silently changing an evidence revision or ID. Obsidian can show linked
notes; typed predicates are interpreted by the CLI, not promised native graph
edge labels. No editor plugin is required for the baseline; actual editor
qualification remains separate.

Storage schema2 keeps transaction payloads, packets and detailed run heads under
hidden retained paths while readable knowledge and Source history remain visible.
Visible Markdown alone cannot recover in-flight operations or unknown spending.
Use a [complete backup](recovery.md#backup-and-restore) before explicit `storage
plan`/`storage cleanup`; manual hidden-directory moves do not migrate a wiki.
Detailed authority lives in [record schemas](../developer/contracts/record-schemas.md)
and [storage](../developer/contracts/storage.md).

# Obsidian-compatible Markdown as the durable format

Accessed **2026-09-28**. This review uses official Obsidian documentation and the Agent Skills specification. Documented behavior and proposed conventions are distinguished below; no live Obsidian compatibility tests were run.

## Documented foundations

Obsidian stores notes as Markdown files in a local vault and refreshes them after external edits. Its metadata cache powers Graph and Outline views and can be rebuilt from files. This provides a direct precedent for durable documents plus disposable indexes. Vault-specific preferences live separately under `.obsidian/`. [Storage and metadata cache](https://obsidian.md/help/Files%2Band%2Bfolders/How%2BObsidian%2Bstores%2Bdata)

Native Graph displays notes as nodes and internal links as connecting lines. It supports direction arrows, filters, colored groups, and local graph traversal depth. Tags and attachments can also be displayed; unresolved destinations and orphans are configurable. The documentation does **not** describe semantic predicates as edge labels or arbitrary assertion graphs. Thus `supports` or `contradicts` in our metadata must not be advertised as native labeled Graph edges. [Graph view](https://obsidian.md/help/plugins/graph)

Obsidian accepts both wikilinks and Markdown links. Folder-qualified wikilinks use vault-root paths with `/`; Markdown destinations need URL encoding. Heading references use `#Heading`, while block references use `#^identifier`. Block identifiers allow Latin letters, digits, and dashes; block references are Obsidian-specific and do not work as standard Markdown anchors. Some filename punctuation conflicts with link syntax. Obsidian can update internal links when it renames a file, but our external rename command should maintain its own references. [Internal links](https://obsidian.md/help/links)

Aliases belong in a YAML list. When selecting an alias, Obsidian inserts a canonical destination with alternative display text, such as `[[Artificial Intelligence|AI]]`, rather than making the alias itself the destination. Our writer should do the same to avoid ambiguous identity. [Aliases](https://obsidian.md/help/aliases)

Properties are YAML at the beginning of a note. Text, lists, numbers, checkboxes, dates, and timestamps have UI support. Internal links in text/list values must be quoted. Property types apply to that property name throughout the vault. Nested properties are unsupported in the Properties UI, although source mode can display them. Markdown formatting inside properties is not rendered. Custom names are permitted, but a custom `id` has no documented role as an alternative native link address. [Properties](https://obsidian.md/help/properties)

## Proposed format

Use flat, versioned frontmatter for identity and small machine-readable fields, then ordinary prose and links for evidence and meaning. Keep complex structures out of the Properties UI. The following is **our proposed convention**, not an existing Obsidian schema:

```markdown
---
wiki_schema: "1"
wiki_id: "assertion-01example"
wiki_kind: assertion
wiki_status: provisional
wiki_predicate: supports
wiki_subject: "[[entities/SQLite]]"
wiki_object: "[[concepts/Embedded lexical search]]"
wiki_sources:
  - "[[sources/SQLite FTS5]]"
---
# SQLite supports embedded lexical search

Subject: [[entities/SQLite]]
Object: [[concepts/Embedded lexical search]]
Evidence: [[sources/SQLite FTS5#BM25]]

Assessment: The cited documentation establishes the capability.
Performance on our workload remains unmeasured.
```

This assertion becomes a **note node** linked to its subject, object, and evidence. It does not turn into a labeled subject-to-object edge in native Graph. The CLI can interpret its fields as a typed relation and expose richer queries. Visible body links make navigation understandable without interpreting our schema. Whether each supported Obsidian version presents frontmatter-only links identically in Graph needs an acceptance test; body links avoid depending on that assumption.

Use immutable IDs for CLI identity while retaining real paths in links. Prefer unique paths over basename guessing. Keep headings for readable citations and optional block IDs for precise Obsidian navigation; also record evidence hashes and locators so anchor edits do not silently invalidate provenance. Store no credentials in notes.

Heading text can change and block IDs can be removed. Our proposal therefore treats them as navigation hints, while source IDs/revisions and exact quoted-span hashes carry CLI evidence identity. Unknown or deleted frontmatter remains a recoverable text document when possible, but cannot always recover its former structured identity. Whole short notes are the default retrieval unit; longer model inputs may need derived windows without adding chunk files. The normative draft is [the Markdown format](../wiki-format.md); the example above illustrates the UI pattern rather than the complete record schema.

## Which state belongs in Markdown

These are proposed durable records, recoverable without repeating extraction or synthesis:

| Record | Canonical content |
|---|---|
| Entity or concept | Stable ID, aliases, explanation, outgoing links |
| Assertion | Claim, assessment, subject/object links, supporting and conflicting evidence |
| Source manifest | URL/path, retrieval date, payload hash, source version, capture/extraction links |
| Extraction | Normalized text, extraction method/version, locators back to original bytes |
| Decision | Chosen option, rationale, alternatives, evidence, supersession links |
| Research run | Scope, completed work, pending questions, budget ledger, results, stop reason |

Original PDF/image bytes remain attachments; Markdown is not a substitute for exact binary evidence. Large structured provenance can live in a clearly identified fenced JSON block in a Markdown record, with a prose explanation and ordinary evidence links. This keeps it inspectable but does not imply Obsidian will turn JSON contents into properties or graph edges. Split large records to keep normal reading practical.

Deleting SQLite should permit rebuilding lexical search, identity lookup, and the link/assertion graph using these files alone. A semantic index needs embedding vectors: preserving them in a separate cache avoids remote embedding calls; deleting that cache requires re-embedding. Knowledge recovery and vector regeneration must be separate guarantees. Process locks and transient transaction journals also remain operational state rather than authored knowledge.

## What to take from Agent Skills

The Agent Skills specification requires a `SKILL.md` containing YAML frontmatter followed by Markdown. Required metadata is `name` and `description`; optional resources can be separate files. Progressive disclosure loads brief discovery metadata first, instructions when selected, and supporting material as needed. The specification recommends concise main instructions and relative references. Its `allowed-tools` field is experimental, with implementation-dependent support. [Agent Skills specification](https://agentskills.io/specification)

Apply that pattern to our wiki format: small metadata for discovery, readable content for understanding, linked details for deeper work. Publish a versioned schema and examples without requiring Obsidian or any agent host to understand every field. A wiki note is not itself an Agent Skill. The actual CLI skill should remain a separate standards-compliant package teaching these workflows.

Before declaring compatibility, test file creation, external edits, quoted property links, aliases, duplicate names, renames, heading/block targets, unresolved references, and graph arrows in Obsidian. Separately prove that deleting the database preserves all durable knowledge and rebuilding needs no synthesis calls.

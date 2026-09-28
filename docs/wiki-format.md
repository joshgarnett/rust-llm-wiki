# Markdown wiki format, draft version 1

Status: proposed, 2026-09-28. This is a format contract to implement and test, not an implemented standard. It incorporates the user's preference for Obsidian-compatible Markdown, best-effort recovery from files, and resilience to heading edits.

## Design rule

**Put durable knowledge and its interpretation in Markdown. Keep acceleration and transient execution state outside it.** Deleting the search database should not lose entities, assertions, evidence, human corrections, or completed research. It may lose vectors that must be restored from a cache or generated again. We do not claim that every SQLite byte can be reproduced from Markdown.

Borrow the Agent Skills pattern: a small entrypoint, machine-readable frontmatter, a readable body, and linked detail loaded when needed. `SKILL.md` itself has a different purpose and schema; wiki records do not become executable skills. [Agent Skills specification](https://agentskills.io/specification)

## Vault entrypoint and record envelope

`WIKI.md` identifies the vault, format, and conventions. It replaces separate portable `wiki.toml` and `schema.md` files in the earlier proposal. Its body introduces the topics, navigation, evidence policy, and optional links to specialized conventions. It may select a locally trusted provider profile by name; it cannot authorize an endpoint or credential command.

```markdown
---
wiki_schema: "1"
wiki_id: vault-example
wiki_kind: vault
title: Research wiki
description: Sources, findings, and decisions for our research.
---
# Research wiki

Start with [the index](index.md). Preserve source evidence and identify
uncertainty when recording a finding.
```

Adopted records require `wiki_schema`, `wiki_id`, `wiki_kind`, and `title`, all strings. IDs are immutable and unique within a vault; titles, filenames, and body headings may change. The `wiki_` prefix reduces collisions with existing note properties. `aliases` and `tags` use their native list forms. Additional fields stay flat, with one consistent property type per name. Unknown fields and user prose must survive CLI edits.

Ordinary Markdown without this envelope is still readable and searchable. Explicit adoption adds metadata. Malformed frontmatter or a deleted ID does not justify discarding the text or guessing an old identity: report the issue, keep safe text lookup available, and suspend affected structured references. Prior records/cache/history may support repair, but a deleted identity cannot always be recovered from the remaining files alone. Reject duplicate YAML keys, duplicate IDs, and unsupported major schema versions for structured writes.

## Durable files

```text
WIKI.md
index.md                              # generated navigation, safe to rebuild
pages/*.md                            # maintained knowledge and human notes
knowledge/
  entities/*.md                       # identity, aliases, description
  assertions/*.md                     # typed propositions and qualifications
  evidence/*.md                       # proposition-to-source support records
  extractions/*.md                    # completed extraction and model provenance
  decisions/*.md                      # merge/split/reject/correction decisions
sources/<source-id>/
  source.md                           # original location and revision history
  revisions/<revision>/
    revision.md                       # immutable payload/extraction metadata
    content.md                        # exact normalized text snapshot
    original.*                        # original bytes, including PDFs/images
runs/<run-id>/
  run.md                              # scope, current checkpoint, unresolved work
  report.md
  events/*.md                         # durable results, receipts, stop reasons
changes/<change-id>/
  change.md                           # intended operations, hashes, outcome
  proposed/*.md                       # proposed contents or reconstructable patches
  before/*.md                         # recoverable before-images
  assets/                             # referenced binary payloads, if any
```

Original files and exact normalized snapshots need not have a managed-record envelope; their enclosing revision note carries it. Do not mutate captured bytes to insert metadata or block anchors. A revised extraction creates a new immutable revision/extraction identity. A revision record binds original-byte hash, normalized-text hash, and extractor version; source URL alone is insufficient identity.

Use readable paragraphs and normal links for primary knowledge. Preserve bulky structured model responses in a named fenced block within an extraction note when needed for audit/reuse; do not make an opaque JSON blob the only representation of active assertions. The validated entity, assertion, evidence, and decision notes are authoritative for the active graph. Completed run events similarly contain a short explanation and enough structured metadata for recovery, with raw receipts only where necessary.

High-frequency locks, pending I/O, and recovery journals can use a more suitable operational format under `.wiki/state/`. They must remain separate from disposable `.wiki/cache/`. Recovery reconciles them with durable Markdown changesets before reporting completion. Changesets retain actual proposed content or reconstructable patches and referenced payloads; hashes alone cannot recover an unapplied edit. A Markdown-only backup preserves completed knowledge and recorded work, but cannot guarantee perfect recovery of an in-flight request or unrecorded provider charge.

## Assertion and evidence example

One assertion is an ordinary note with flat frontmatter:

```markdown
---
wiki_schema: "1"
wiki_id: assertion-use-sqlite
wiki_kind: assertion
title: The CLI plans to use SQLite
wiki_status: proposed
wiki_subject_id: entity-cli
wiki_subject: "[[knowledge/entities/cli]]"
wiki_predicate: plans_to_use
wiki_object_id: entity-sqlite
wiki_object: "[[knowledge/entities/sqlite]]"
wiki_evidence:
  - "[[knowledge/evidence/design-001]]"
---
# The CLI plans to use SQLite

[[knowledge/entities/cli|The CLI]] plans to use
[[knowledge/entities/sqlite|SQLite]] for its embedded index.

Evidence: [[knowledge/evidence/design-001|Design proposal]].
This describes a plan, not an implemented dependency.
```

For typed references, immutable `*_id` fields establish identity and companion wikilinks provide navigation. If a link is broken, an unambiguous ID can support path repair. If the link resolves to a different ID, flag a conflict rather than silently choosing one. Body links are readable projections; edits that disagree with structured fields require a diagnostic. The format does not parse arbitrary prose into authoritative predicates automatically.

Evidence notes carry `wiki_assertion_id`, an assertion link, `wiki_source_id`, `wiki_source_revision`, a source link, `wiki_stance` (`supports` or `contradicts`), and exact span/hash information. Store locators as flat `wiki_span_start`, `wiki_span_end`, `wiki_locator_kind`, and `wiki_quote_hash` fields, tied to the immutable normalized snapshot. The body shows the cited excerpt and an explanation. Multiple evidence notes avoid nested lists of objects in Properties. An assertion's evidence-link list is a convenience projection; evidence notes' assertion IDs define membership, with consistency checks for missing or stale links.

Version 1 uses `wiki_locator_kind: utf8-bytes`, zero-based half-open `[start, end)` offsets into the exact captured `content.md` bytes, and `blake3:<hex>` hashes of the selected byte slice. The enclosing revision separately hashes the complete snapshot. Do not normalize Unicode or line endings again during verification. The CLI should derive offsets/hashes from a uniquely matched quotation when importing agent output, rather than requiring a model to count UTF-8 bytes. Explicit spans must be valid UTF-8 boundaries and match the supplied quote/hash; ambiguity is an error.

Qualifiers such as date range, negation, modality, and units have explicitly typed flat fields. A literal object has an explicit literal type/value instead of an entity ID. The detailed predicate vocabulary and supported qualifier fields must be settled in M0; do not turn every shared keyword into an arbitrary relation type.

## Obsidian graph compatibility

Obsidian's documented graph connects note nodes through internal links and can show direction arrows. Our assertion note therefore appears as a node connected to its entities and evidence. The CLI additionally interprets its fields as a typed subject–predicate–object assertion. Native Graph does not document rendering these custom predicates as edge labels. [Graph view](https://obsidian.md/help/plugins/graph)

Use both normal Markdown links and supported wikilinks. Generated wikilinks target actual vault paths with optional display aliases. Quoted links in flat YAML are supported; nested property structures are outside the native Properties UI. Keep visible body links so ordinary readers can follow relationships without understanding our fields. [Properties](https://obsidian.md/help/properties)

Filter technical extraction/run notes from a browsing graph when desired; they remain files that can be inspected. No Obsidian plugin is required for the baseline. A future richer graph UI may display typed edges directly. Live Obsidian acceptance tests still need to verify the actual rendering and editing behavior.

## Heading changes and citation repair

Headings are labels, never primary IDs or database keys. Obsidian heading links contain heading text; its optional block references use block IDs. Those links aid navigation, but neither mechanism guarantees that a user will preserve the target. [Internal links](https://obsidian.md/help/links)

For CLI evidence, use these checks:

1. Resolve the stable document/source ID and recorded revision.
2. Check the stored passage against that immutable snapshot using span and hash.
3. For a newer mutable page, try locating an identical quotation with surrounding context. A unique exact match may repair a navigation location, while retaining the original evidence revision.
4. If wording changed or matching is ambiguous, mark the reference stale or unresolved. Never silently reattach it through semantic similarity.

Changing a heading or filename does not change the record's identity. It may change the normalized embedding input, requiring an embedding update, or break a human navigation anchor, requiring link repair. These are different events. Renaming/deleting required frontmatter keys is a metadata error; retain the body and report it. This is best-effort resilience, not protection against arbitrary destructive edits.

## Documents first, segments when useful

Keep documents as the unit people maintain. Start lexical retrieval with a document index and on-demand matching excerpts; heading parsing need not create durable chunk records.

For embeddings, `granularity = "auto"` starts with one input per short note/entity/assertion. Split an input when its formatted text exceeds the provider limit or a configured retrieval-quality target. Prefer headings and paragraphs as boundaries, with a deterministic bounded fallback for long sections. A collection of many short notes does not need chunking merely because its file count grew; one long report may need segmentation immediately.

Smaller passages may improve focused retrieval and reuse on edits, while whole-note vectors are simpler and preserve context. Measure that tradeoff before choosing a smaller universal target. Compare whole-note and section-based retrieval under the same evidence budget. Provider limits include title/context prefixes and depend on the configured model/tokenizer.

Segments live in derived indexes as retrieval units with text hashes and source mappings, not user-managed chunk files. Mutable heading text is not a segment identity. Reuse vectors by actual formatted input and embedding-space identity; unchanged segment text can be reused after a heading change only when the heading was not part of that input. Long-document entity extraction may need bounded windows independently of embeddings. Factual citations still point to source revisions and passages, never only to transient retrieval-unit IDs.

Default semantic inputs are selected knowledge pages, source text, and readable entity/assertion representations. Exclude bookkeeping IDs, credentials, extraction transcripts, duplicated evidence notes, and run-event logs unless explicitly selected. Do not count an entity summary and its original source as independent corroboration.

## Recovery contract

| State | Markdown-only recovery |
|---|---|
| Valid pages, entities, assertions, evidence, recorded decisions | Recover from managed notes and their links/IDs |
| Completed extraction/research outputs and recorded receipts | Recover from their notes; no repeated synthesis needed |
| FTS, backlinks, graph adjacency, lookup maps, retrieval units | Rebuild locally from files with the pinned parser/configuration |
| Embedding vectors and ANN structures | Restore retained vectors or re-embed remotely, then rebuild search structures |
| Credentials, endpoint trust, machine preferences | Reconfigure from private user settings; never put secrets into notes |
| In-flight locks, network responses, unrecorded spending | Reconcile operational journals; not a Markdown-only guarantee |

The test is semantic recovery of recorded knowledge, not a byte-identical SQLite database. Missing/damaged notes, ambiguous IDs, unsupported schema versions, and absent attachments produce a recovery report. The CLI must not hide incomplete recovery behind a successful index build.

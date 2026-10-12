# Canonical record schemas, version 1

Implementation contract; [storage](storage.md) owns parsing and validation. Frontmatter is flat. Required common string fields are `wiki_schema: "1"`, `wiki_id`, `wiki_kind`, and `title`. Optional `aliases`, `tags`, and `wiki_depends_on_ids` are string lists; the last authoritatively names supporting assertions for page/entity descriptions. `description` is an optional string. IDs are immutable; filenames and headings are editable.

The Vault marker additionally supports storage schemas 2 and 3. These select
physical storage arrangements; other canonical records retain schema 1. New
wikis use storage 3. Catalog schema and storage schema are independent.

In the table, all fields are strings unless marked **integer**, **boolean**, or **list**. All shown fields are required unless marked `?`. Names omit the common `wiki_` prefix. Date/timestamp values must be strings: dates use `YYYY-MM-DD`, timestamps RFC3339 UTC. Hashes use the storage BLAKE3 spelling. Every typed ID field has a same-purpose optional quoted path wikilink for navigation, except lists of operational identifiers. CLI-generated records include the corresponding readable links in their body.

| `wiki_kind` | Kind-specific fields and allowed values |
|---|---|
| `vault` | `profile?` selects a private trusted profile by name, never grants endpoint/command trust |
| `page` | `status`: `draft`, `reviewed`, `deprecated` |
| `entity` | `status`: `active`, `superseded`; `entity_type`: `person`, `organization`, `project`, `component`, `concept`, `place`, `event`, `other`; `superseded_by_id?` |
| `assertion` | `status`: `proposed`, `accepted`, `rejected`, `superseded`; `subject_id`; `predicate`; exactly one `object_id` or (`literal_type`, `literal_value`); qualifiers below; `evidence?` **list** of path links, a nonauthoritative convenience projection |
| `evidence` | `status`: `active`, `retracted`; `assertion_id`; `source_id`; `source_revision`; `stance`: `supports`, `contradicts`; `locator_kind`: `utf8-bytes`; `span_start`, `span_end` **integers**; `quote_hash`; `supersedes_id?`; `extraction_id?` |
| `source` | `status`: `active`, `withdrawn`; `origin_kind`: `local-file`, `url`, `agent-report`; `origin`; `current_revision`; `revisions` **list** of revision IDs; `withdrawn_at?`, `withdrawal_reason?` |
| `revision` | `source_id`; `captured_at`; `original_path`; `original_hash`; `extractor`; `extractor_fingerprint`; `extraction_status`: `complete`, `unsupported`, `failed`; `content_path?`, `content_hash?`; `media_type?`, `published_at?` |
| `extraction_packet` | `source_id`; `source_revision`; `packet_fingerprint`; `output_schema`; `created_at`; body retains exact packet/schema JSON; record ID equals its deterministic wire packet ID |
| `extraction` | `status`: `completed`, `partial`, `failed`; `packet_id`; `input_hash`; `extractor_fingerprint`; `executor`: `agent`, `api`; `source_ids`, `source_revision_ids` **lists**; `completed_at`; `model?`, `model_revision?`; body retains raw response and allocated ID/mention mappings |
| `decision` | `status`: `active`, `superseded`; `action`: `merge`, `split`, `reject`, `accept`, `correct`, `bind_mention`, `create_entity`, `reject_mention`, `add_alias`; `input_ids`, `output_ids` **lists**; `created_at`; `supersedes_id?`; `extraction_id?`, `mention_ids?` **list**; body rationale and exact change manifest |
| `change` | `status`: `prepared`, `applying`, `conflict`, `committed`, `aborted`; `created_at`; `manifest_hash`; body contains `lwiki-change-v1` manifest referencing recoverable payloads |
| `run` | `status`: `planned`, `running`, `paused`, `completed`, `failed`, `stopped`; `created_at`; `checkpoint_event_id?`; body stores scope and limits |
| `run_event` | `run_id`; `sequence` **integer**; `event_type`; `occurred_at`; `request_id?`; provider/research design owns bounded event payload schemas and accounting fields |

Companion links use `wiki_subject`, `wiki_object`, `wiki_assertion`, `wiki_source`, `wiki_revision`, and `wiki_supersedes` for the corresponding typed references. Other typed reference links use the ID field name with `_id` removed. `wiki_source_revision` refers to a revision ID, never an inferred timestamp or heading. Revision local payload paths are relative to its revision directory and may not escape it. Complete revision extraction requires both content fields; unsupported/failed revisions omit both. Revision, extraction-packet, and run-event records are immutable; fixes create successors. Extraction source ID/revision lists are validated as sets through revision ownership, not zipped parallel arrays.

Assertion qualifiers are `wiki_negated` (**boolean**, default `false`), `wiki_modality` (`asserted`, `possible`, `planned`, default `asserted`), optional `wiki_valid_from`/`wiki_valid_until` (dates, half-open interval), and optional `wiki_unit` (numeric literal only). Literal types are `string`, `decimal`, `boolean`, and `date`; `wiki_literal_value` is always a string. Decimal grammar is `-?(0|[1-9][0-9]*)(\.[0-9]+)?`; boolean strings are `true`/`false`. No NaN, infinity, exponent notation, or float conversion. An end date must be later than its start.

The initial directed predicate registry is `is_a`, `part_of`, `depends_on`, `maintains`, `works_for`, `uses`, `plans_to_use`, and `located_in` with entity objects; `has_property` takes a typed literal and required `wiki_property` string. Subject is always an entity. Direction is literal as written; no inverse, symmetry, or transitivity is inferred. Unknown predicates/qualifiers block activation until a schema change supports them. Negation and modality remain part of proposition identity, not extraction confidence. An asserted `plans_to_use` proposition asserts the existence of a plan; it does not prove actual use.

An assertion's normalized identity key includes subject, predicate, object/literal, property, negation, modality, unit, and date interval; it assists duplicate proposals but never replaces its immutable ID. No automatic merging follows equal labels. Decisions store explicit affected canonical IDs and exact before/after operations; mention actions also require an extraction ID and packet-local mention IDs. Reject supersession cycles and silent assertion retargeting. Applying a decision updates affected canonical records in the same changeset, so rebuild needs no model interpretation. Active conflicting decisions invalidate affected structured records. Agent import creates proposals; only explicitly requested acceptance can mark assertions accepted.

A graph review covers every active evidence ID/hash associated with the assertion, checking membership again on apply. `insufficient` retracts that evidence; a changed stance creates successor evidence for the same verified source span and retracts its predecessor. Unchanged supported/contradicting assessments retain their stance. Record all corrections, retractions, and assertion/decision edits in one changeset. Acceptance requires at least one current supporting evidence record after those edits; evidence omitted from the review cannot silently remain usable.

Evidence bodies contain exactly one fenced `text` quotation plus explanation. Its raw interior equals the source slice plus exactly one separator newline in the note's newline convention, even when the slice already ends in a newline. Verification removes only that separator; source CRLF and Unicode remain exact. The importer chooses a fence length exceeding embedded backtick runs. Human wording outside that block is not parsed as a predicate. Mechanically checkable body/projection disagreements produce diagnostics; arbitrary prose cannot be proven equivalent to structured fields.

## Editor navigation and evidence coordinates

A Source may contain `wiki_presentation`, a flat JSON string with strict version 1
ownership metadata for its generated readable notes. It binds the Source ID,
current path, revision, optional content hash and generated-file hash, plus a map
of published revision routes and any saved historical note hashes. The encoded
field is limited to 64 KiB and 4096 published revisions; the complete Source still
must fit its existing 256 KiB envelope limit. Paths are restricted to allocated
`sources/views/` notes and `sources/views/saved/` historical notes. Display files
are not canonical records or additional searchable captures. Their revision
anchors are navigation only; exact immutable SourceSpanRef bytes remain evidence.

Native Obsidian graph nodes follow internal links; typed predicate labels are CLI
interpretation, not a promised native edge-label UI. Flat Properties and quoted
wikilinks remain navigation projections. Keep visible body links for readers;
code links are not graph authority. Live editor qualification remains separate.

Evidence coordinates are zero-based half-open UTF-8 bytes in exact immutable
content. Preserve line endings and Unicode; do not inject anchors into captured
text. Headings and optional editor block IDs are navigation labels, never primary
identity. A unique exact quotation can repair mutable navigation while preserving
its recorded evidence revision. Changed or ambiguous quotations remain stale or
unresolved; semantic similarity cannot silently reattach them.

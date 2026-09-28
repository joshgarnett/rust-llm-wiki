# Storage, indexing, and recovery design

Status: implementation contract proposed on 2026-09-28; no code, crash test, or platform guarantee has been demonstrated. This specializes [the format](../wiki-format.md) and [architecture](../architecture.md). [Record schemas](record-schemas.md) defines the flat properties.

## Ownership and Rust boundaries

Canonical knowledge lives in Markdown, captured original files, and immutable normalized source snapshots. `.wiki/cache/index.sqlite` contains replaceable projections and reusable vectors; losing vectors can require explicitly authorized API work. `.wiki/state/` contains locks and operational journals, never disposable cache. Credentials and endpoint trust belong to private configuration.

One library, initially divided into `vault`, `records`, `sources`, `changes`, and `index`, exposes these conceptual Rust contracts; this is pseudocode:

```rust
struct RecordRef { vault_id: RecordId, record_id: RecordId, expected_kind: RecordKind }
struct DocumentLocator { record: Option<RecordRef>, path: VaultRelativePath,
                         observed_hash: Blake3Hash }
struct ByteSpan { start: u64, end: u64 } // [start,end), UTF-8 boundaries
struct SourceSpanRef { source_id: RecordId, source_revision: RevisionId,
    span: ByteSpan, quote_hash: Blake3Hash }
struct EvidenceRef { evidence_id: RecordId, assertion_id: RecordId,
    source_id: RecordId, source_revision: RevisionId,
    span: ByteSpan, quote_hash: Blake3Hash }
enum CitationRef { Source(SourceSpanRef), Assertion(EvidenceRef) }
struct ReadSnapshot { generation: u64, parser_fingerprint: Blake3Hash,
    control_manifest: Blake3Hash }
enum Eligibility { Current, Historical, Stale, Invalid, Withdrawn, Unsupported }

trait RecordStore {
    fn read(ref_: RecordRef) -> Result<ParsedRecord>;
    fn scan(policy: ScanPolicy) -> Result<ScanManifest>;
    fn prepare(ops: Vec<ExpectedWrite>) -> Result<PreparedChange>;
    fn apply(change: ChangeId) -> Result<ApplyReport>;
    fn recover() -> Result<RecoveryReport>;
}
trait IndexStore {
    fn sync(scan: ScanManifest) -> Result<ReadSnapshot>;
    fn snapshot() -> Result<ReadSnapshot>;
    fn verify_dependencies(snapshot: &ReadSnapshot, deps: DependencyClosure)
        -> Result<VerifiedDependencies>;
}
```

Newtypes validate construction. All hashes use `blake3:<64 lowercase hex digits>`. IDs are case-sensitive, match `[A-Za-z0-9][A-Za-z0-9._-]{0,127}`, and are unique across kinds within one vault. Generate `<kind>_<lowercase UUIDv7>` except immutable extraction packets, whose ID is `packet_<64 hex fingerprint digits>` for repeatable lookup. Never derive identity from title, path, or headings. `RevisionId` is the revision record's ID. Missing IDs produce path/hash locators, not invented durable identities.

Vault paths use relative slash-separated components; reject absolute paths, `..`, NUL, and platform-reserved names. Managed writes reject symlink components and recheck destination containment. Initial scans skip symlinks, `.wiki`, `.git`, generated `index.md`, and changeset payload copies. Revision payloads never supply managed envelopes: captured frontmatter cannot introduce canonical IDs. Index normalized text through its revision owner; do not index original binaries. Case-folded path collisions are errors for portable writes, even on a case-sensitive host. Hostile concurrent symlink replacement is outside the initial local-vault concurrency model.

## Parsing and references

Parse UTF-8 bytes without line-ending or Unicode normalization. Preserve an optional initial BOM. Frontmatter exists only when the first logical line after that BOM is exactly `---`; terminate at the next exact `---` line, accepting LF or CRLF. An unterminated envelope is invalid, while all text remains available to literal search. An adopted envelope must be a YAML mapping. Reject duplicate keys at every depth, aliases, anchors, merge keys, and explicit YAML tags for structured operations; bound envelope size and nesting. Known fields use the schema's exact types, with no string/number/date coercion.

Use a lossless token/range representation around a validating YAML parser. Retain original bytes for unknown properties, comments, key order, quoting, whitespace, and body. Change only requested known-field ranges; append new fields before the closing delimiter using the existing newline convention. Preserve unknown nested values as opaque ranges; do not generate nested Properties. If safe range editing is unavailable, refuse the structured edit rather than reserializing the whole note. Body replacement is allowed only as an explicit expected-hash write. The parser adapter needs a fixture spike before choosing/pinning its implementation.

Malformed metadata, unsupported schema, and duplicate IDs exclude affected records from the typed registry; searchable text remains. Resolve a typed reference by exact ID and expected kind, then check its companion path link. A missing/stale path can be repaired explicitly from a unique ID; a path resolving to another ID is a conflict. Untyped links resolve exact vault-relative paths, then unique basename/alias matches; ambiguity remains unresolved. Never resolve ID conflicts through title similarity. Links inside code are ignored. Heading/block fragments are navigation hints, never evidence or entity identity.

`wiki_schema: "1"` fixes the canonical schema. Unknown properties survive; unknown `wiki_` properties receive diagnostics. A future major version is readable text but read-only structured data. Explicit `migrate` prepares ordinary before/proposed changesets, supports dry-run, and retains identity; indexing never migrates notes. SQLite's `user_version` versions cache SQL independently; migrate in a transaction or rebuild, reporting any vector-cache loss before optional regeneration.

## Capture and lifecycle

Capture into `sources/<source-id>/revisions/<revision-id>/`: `revision.md`, exact `original.*`, and `content.md` when extraction succeeds. Text/Markdown v1 accepts valid UTF-8 and preserves its bytes, recorded as `utf8-preserve-v1`; extracted output is thereafter immutable. Hash original and normalized bytes separately, even when equal. Binary originals are allowed, but unsupported extraction produces a revision without text and no eligible passage. A later extractor produces a new revision, never fills or rewrites an old capture.

`source.md` records the observed locator and current revision. A refresh writes all new revision files before advancing this pointer in the same recoverable changeset. Reuse an existing revision only when original bytes, normalized bytes, and extractor fingerprint all match for that source. URL equality alone does not merge sources. Importing an agent report preserves that report's origin; its links are not verified underlying captures.

Evidence verifies both the complete snapshot hash and the selected byte-slice hash. Require `0 <= start < end <= byte_len`, UTF-8 boundaries, and exact quotation equality. An imported quotation must have one exact match in its declared window; zero or multiple matches require correction. Do not normalize before matching. Revalidation against a refreshed source creates successor evidence for the new revision; retain old evidence for history.

Direct captured passages use `CitationRef::Source(SourceSpanRef)` without creating assertion/evidence notes. Validate their source/revision ownership, revision manifest, complete snapshot, span, and quote hash. Current scope also requires an active source and its current revision; historical scope verifies the bytes but labels historical/withdrawn state. `CitationRef::Assertion` additionally verifies the durable evidence/assertion chain. Its `EvidenceRef` keeps the flattened wire fields shown above, equivalent to composing a `SourceSpanRef`. Ordinary note excerpts retain `DocumentLocator` and a note-text label, not an invented source citation.

Authored `wiki_status` and derived eligibility are separate. Current assertions are accepted and have at least one active, intact supporting evidence record bound to an active source's current revision. Contradiction remains visible as a dispute; it does not erase support. Proposed assertions are discoverable only when requested and never silently promoted. A head advance makes old evidence historical and unsupported-by-current-revision assertions stale. Withdrawal disables that source's support immediately; losing all support suppresses the assertion from current evidence, with the cause reported. Rejected/superseded assertions and retracted evidence cannot supply current support.

Pages and entity descriptions use authoritative `wiki_depends_on_ids` for their supporting assertions. Missing or ineligible dependencies invalidate the entire derived description/note in v1; identity and ordinary text discovery remain available. Recompute the transitive closure, rejecting dependency cycles. No paragraph-level freshness is promised. `--include-proposed` and `--include-historical` permit labeled discovery, not promotion to current evidence. Purging stored bytes is a separate operation.

Eligibility carries reason codes; authored status remains separately visible. Structural invalidity takes precedence over the following otherwise-valid cases:

| Record/state | Derived eligibility and use |
|---|---|
| Source active/current revision; active evidence on that revision | `Current`; verify bytes before citation |
| Source withdrawn; active evidence from it | `Withdrawn`; historical scope only |
| Older revision/evidence, retracted evidence, rejected/superseded assertion | `Historical`; no current support |
| Proposed assertion | `Unsupported` with `proposed` reason; explicit proposed discovery only |
| Accepted assertion with current support | `Current`, including when disputed |
| Accepted assertion with only older-revision support | `Stale`; requires revalidation |
| Accepted assertion with no usable support | `Unsupported`; distinguish withdrawn, retracted, or absent support in reasons |
| Reviewed page with all declared dependencies current | `Current` note text; dependencies supply evidence separately |
| Reviewed page without declared dependencies | `Current` verified note text, never source evidence |
| Draft page | `Current` discovery text if otherwise valid; excluded from default context |
| Deprecated page | `Historical` |
| Page/entity description with an ineligible declared dependency | `Stale`, or `Invalid` for unresolved/invalid references; excluded from current derived context |

Entities have separate `identity_eligibility` and `description_eligibility`: an active valid identity remains `Current` while its description is stale/unsupported; superseded identities are `Historical`. An entity description with no declared support is `Unsupported` note text. Name/alias lookup may seed an active identity independently; current description ranking excludes unsupported descriptions. The document projection's eligibility describes its text, while graph identity lookup uses the entity column.

Persist immutable packet notes at `knowledge/extractions/packets/<packet-id>.md` before returning agent work. Their fenced JSON contains the exact packet/schema; repeated identical export reuses the same note. Completed extraction notes link that packet, retain the raw response and allocated ID mapping, and make repeated identical imports idempotent. Pending packets survive Markdown-only restoration; operational scheduling state can be reconciled without regenerating the packet or requesting new model output.

## SQLite projections and generations

Use bundled SQLite, foreign keys, WAL, a bounded busy timeout, and short write transactions. Keep the live database on a local filesystem. No network request runs inside a database transaction. Initial SQL skeleton follows; derived columns and checks are expanded during implementation:

```sql
CREATE TABLE generations (
  gen INTEGER PRIMARY KEY, state TEXT NOT NULL,
  manifest_hash TEXT NOT NULL, parser_hash TEXT NOT NULL);
CREATE TABLE index_meta (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1), published_gen INTEGER);
CREATE TABLE documents (
  doc_row INTEGER PRIMARY KEY, gen INTEGER NOT NULL, path TEXT NOT NULL,
  record_id TEXT, kind TEXT, file_hash TEXT NOT NULL, title TEXT, body TEXT,
  source_id TEXT, owner_revision TEXT,
  eligibility TEXT NOT NULL, UNIQUE(gen,path));
CREATE UNIQUE INDEX record_ids ON documents(gen,record_id)
  WHERE record_id IS NOT NULL;
CREATE VIRTUAL TABLE documents_fts USING fts5(
  title, aliases, headings, tags, body, gen UNINDEXED, doc_row UNINDEXED);
CREATE VIRTUAL TABLE graph_fts USING fts5(
  name, aliases, endpoints, predicate, qualifiers, description,
  target_kind UNINDEXED, target_id UNINDEXED, gen UNINDEXED);
CREATE TABLE entities (
  gen INTEGER, id TEXT, entity_type TEXT,
  identity_eligibility TEXT, description_eligibility TEXT, PRIMARY KEY(gen,id));
CREATE TABLE assertions (
  gen INTEGER, id TEXT, subject_id TEXT, predicate TEXT, object_id TEXT,
  literal_type TEXT, literal_value TEXT, qualifiers_json TEXT, status TEXT,
  PRIMARY KEY(gen,id));
CREATE INDEX outgoing ON assertions(gen,subject_id,predicate);
CREATE INDEX incoming ON assertions(gen,object_id,predicate);
CREATE TABLE evidence (
  gen INTEGER, id TEXT, assertion_id TEXT, source_id TEXT, revision_id TEXT,
  stance TEXT, span_start INTEGER, span_end INTEGER, quote_hash TEXT,
  eligibility TEXT, PRIMARY KEY(gen,id));
CREATE TABLE dependencies (
  gen INTEGER, owner_id TEXT, path TEXT, expected_hash TEXT, role TEXT,
  PRIMARY KEY(gen,owner_id,path,role));
CREATE TABLE links (
  gen INTEGER, from_path TEXT, byte_start INTEGER, target_id TEXT,
  target_path TEXT, resolution TEXT);
CREATE TABLE diagnostics (
  gen INTEGER, path TEXT, code TEXT, details_json TEXT);
```

Also project aliases, source/revision manifests, and decisions. Source-text rows have null `record_id`, their source/revision ownership, and a locator referencing the revision record with the payload path/hash. [Retrieval](retrieval.md) owns retrieval units, embedding spaces, cached vectors, and memberships. Cache vectors by space/input hash independently of generation; memberships are generation-specific. Every candidate query, including FTS, filters its pinned generation. Structured duplicate IDs become null `documents.record_id` rows with diagnostics for every conflicting path, never a winning row. Add composite foreign keys where appropriate; unresolved references stay diagnostics rather than violating constraints. JSON columns here are derived, not canonical authority.

For sync, enumerate paths in bytewise order, read/hash bytes, parse, resolve the complete ID registry, validate evidence, apply decisions, compute eligibility, and project rows. Use explicit tie-breaking and a parser/config fingerprint. Metadata hints may accelerate scanning, but full verification hashes content, detecting same-size/same-timestamp edits. A rebuild repeats the same algorithm without extraction or embedding calls. Canonical graph equivalence, not row IDs or byte-identical SQLite, defines determinism.

Build an unpublished generation, leaving the prior generation readable. Before publishing, acquire the vault lock, reconcile apply journals, and recheck the captured manifest. Retry a changed scan once, otherwise return a conflict. In one SQLite transaction replace both FTS rowsets, mark the new generation complete, and update `published_gen`; failures preserve the previous view. Keep only published-generation FTS rows so retained history does not distort BM25 statistics. Readers hold one database read transaction, retaining their old FTS view through SQLite snapshot isolation. Garbage-collect old ordinary rows after readers finish; rebuild never replaces an open database file.

Before verified output, compare a control manifest covering canonical path membership, managed envelopes, and complete decision records; this detects newly introduced decisions/duplicate identities, not merely edits to known dependencies. Then hash the exact dependency closure: selected records, statuses, evidence, decisions, source manifests, revision notes, and quoted snapshots. Initially hashing all canonical Markdown is acceptable. On mismatch, resync/retry once or return freshness conflict. Exhausted verification budgets cannot claim `verified_snapshot`; explicit unverified output is `index_snapshot`. Return generation and verification time. These checks describe observed bytes, not a globally atomic filesystem snapshot; a non-cooperating edit after verification remains possible.

## Recoverable changesets

An OS advisory lock serializes CLI writers and publishers. PID files are diagnostic, never lock authority. Reads check for incomplete applies and recover under the lock, or refuse verified current-evidence output until recovery; historical/index-snapshot access is explicitly labeled.

Preparation writes `changes/<id>/change.md`, full `before/*.md` and `proposed/*.md` payloads, plus binary payloads under `assets/`. A fenced `lwiki-change-v1` JSON manifest in the note body lists sorted operations, relative targets, old/new hashes, payload paths, and absent-file expectations. Graph imports also record `origin: {operation: "graph_import", packet_id, response_hash}` as an idempotency key. Search prepared and completed manifests before allocating IDs; an identical retry returns the existing change/outcome even before its extraction note has been applied. Recovery scans these manifests separately; `changes/**` is excluded from semantic/control-manifest inputs. Creation, replacement, deletion, and rename-as-create/delete are supported; immutable captures cannot be replaced. Validate the complete proposed graph before application.

The optional `read_preconditions` manifest list retains original observed states of unmodified notes/assets that authorize a proposal, including a revalidation predecessor's expected hash. Empty lists are omitted; existing version1 manifests remain valid without rewriting. Preparation checks these base states before allocating retained files. Identical conditions on retained write targets are covered by their guarded before-images; requested conditions on dropped no-op writes remain. The union of operation/read paths must have no duplicates, ancestor overlap or portable case collisions. Apply and forward recovery recheck guards around mutations and publication; they cannot rebind to newer bytes. Graph-validator dependencies describe the proposed final graph separately. Source plans bind base dependencies directly into their drafts; overlay bytes are verified against the same proposal rather than treated as original reads. Terminal historical outcomes skip obsolete read checks, and a known Prepared proposal remains safely abortable.

The operational journal is append-only length-framed JSON with sequence and checksum under `.wiki/state/changes/<id>.journal`. Sync each state transition; ignore only an incomplete trailing frame. The state machine is:

```text
Prepared -> Applying -> FilesApplied -> Indexed -> Committed
                 \-> Conflict (requires explicit resolution)
Prepared -> Aborted (before any target mutation)
```

Persist and sync all Markdown payloads and manifest before `Prepared`. At `Applying`, recheck expected target hashes, stage and sync replacement files in each destination directory, record intent, recheck immediately before replacement, perform the tested platform replacement, sync the containing directory where supported, then append completion. Deletes retain before-images. After all targets match proposed hashes, record `FilesApplied`; build/publish the index, record `Indexed`, finalize the changeset outcome note, then `Committed`. The publisher accepts only this fully applied journal under its held writer lock; any other unresolved apply blocks publication. Changeset outcome records are bookkeeping, excluded from semantic/control-manifest inputs.

Recovery never trusts a completion flag alone. Inspect every target: old hash means pending; new hash means applied; any third hash means preserve unfamiliar bytes and mark conflict. For creates/deletes, absence is an explicit old/new state. Resume forward only when every target is classifiable and applying intent is recorded. If SQLite committed before its journal event, rebuild/publish idempotently. Rollback is explicit and replaces only targets still matching the recorded new hash. Lost operational state is reconstructed from retained manifests and hashes; all-old prepared changes remain staged, and ambiguous intent/missing payloads block completion.

Individual replacements can be atomic on tested platforms; a multi-file changeset and SQLite are not one filesystem transaction. An editor can modify a file between final hash check and replacement; the recorded before-image preserves what the CLI observed, not an unseen intervening edit. Before-images, locks, and hashes reduce risk without claiming universal compare-and-swap. Keep changeset payloads until explicit pruning under a retention policy. Crash safety and durability after power loss remain release gates, particularly Windows replacement and directory syncing.

## Implementation dependencies and validation

| Ticket | Depends on | Required evidence before acceptance |
|---|---|---|
| S1 lossless format/IDs | schema fixtures | Unknown fields/comments/body unchanged; Unicode, CRLF, BOM, malformed YAML, duplicate keys/IDs, unsupported versions diagnosed |
| S2 capture/evidence | S1 | Exact byte hashes; multibyte span boundaries; ambiguous quote rejected; binary without extractor ineligible; tampered snapshot rejected |
| S3 changes/recovery | S1, S2 | Inject crash at every journal/sync/replace/SQL boundary; old/new/conflicting targets preserved; competing CLI writers serialize |
| S4 projections | S1–S3 | Rename/delete, full rebuild, identical timestamps, copied IDs, decisions and parser changes produce deterministic graph/FTS results |
| S5 verified reads | S4, retrieval contract | New decision detected; source advance/one versus all support withdrawals invalidate graph, descriptions, vectors, cached context |
| S6 release filesystem gate | S3–S5 | macOS/Linux/Windows replacement, locked files, failed fsync, disk-full and truncated journal tests; actual Obsidian edits preserve schema/navigation |

These are acceptance obligations, not completed checks. Markdown-only recovery covers recorded knowledge and retained changesets; it cannot reconstruct deleted identities, vectors, secrets, an unrecorded provider response, or a charge absent from durable receipts.

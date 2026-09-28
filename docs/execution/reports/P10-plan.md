# P10 planning proposal: durable packet-bound extraction import

Status: **planning only**. No P10 source/test implementation, dependency addition, Cargo execution, or acceptance evidence. P09 is not accepted by this proposal; root freezes interfaces only after its dependency gate. Documentation lease: `/root/p02_vault`; root owns shared types/modules/schemas/CLI/Cargo and future integration. No delegation, provider access, production-vault access, or commits performed.

## Root-owned types and strict wire boundary

Proposed shared file `src/graph/extraction_types.rs` supplies:

- `PacketLocalId`: validated existing ID grammar, explicitly packet-local; never authorizes canonical identity.
- `ExtractionLimits { max_mentions, max_assertions, max_output_bytes }`: ceilings 64, 128, 262144. Packet ceilings: 16 windows and 12000 exact source bytes per window. Bound packet metadata, schema, headings, strings, nesting, and JSON expansion separately.
- `PacketWindow { id, span: ByteSpan, text: String, headings: Vec<String> }`.
- `ExtractionPacket`: exactly contract fields `schema`, `packet_id`, `packet_fingerprint`, `source_id`, `source_revision`, `snapshot_hash`, `windows`, `registry_version`, `output_schema`, `limits`, `instructions`, optional `candidate_context`. Schema `lwiki.extraction-packet.v1`; proposed `output_schema` is the complete bounded extraction JSON schema, while note frontmatter `wiki_output_schema` is its string identifier. Root must explicitly freeze this field interpretation. Candidate context is typed/bounded IDs, kinds, and labels, with context-only semantics.
- `ExportRequest { source_id, revision_id?: RecordId, windows: Vec<ByteSpan>, limits }`. Default revision is current; explicit retained revision is verified without converting historical evidence into current support. Omitted windows use deterministic source-boundary segmentation.

Packet fingerprints hash recursively key-sorted UTF-8 JSON, no insignificant whitespace, preserved array order, and exact specified integer values. Exclude only `packet_id` and `packet_fingerprint`; `created_at` belongs to note metadata, not task identity. Fixed instructions include prompt/segmenter versions; registry/schema/context/limits and every effective instruction influence identity. `packet_id = packet_<fingerprint hex>`. Windows equal snapshot slices exactly, with valid UTF-8 offsets. Persist immutable packet notes under `knowledge/extractions/packets/<packet-id>.md` before stdout; restore/reuse checks envelope, fingerprint, schema and exact snapshot windows. Export launches no host.

Strict response DTOs use schema `lwiki.extraction.v1`:

```text
ExtractionResponse {
  schema, packet_id, packet_fingerprint,
  mentions: Mention[], assertions: Assertion[], unresolved: Unresolved[]
}
Mention { id, window_id, label, type, quote, span?, description? }
Assertion {
  id, subject, predicate, object: MentionObject | LiteralObject,
  negated, modality, evidence: EvidenceQuote[],
  valid_from?, valid_until?, unit?, property?
}
MentionObject { kind: "mention", mention_id }
LiteralObject { kind: "literal", type, value }
EvidenceQuote { window_id, stance, quote, span? }
Unresolved { window_id, quote, reason }
```

Every object and tagged-object arm rejects unknown fields. Reject duplicate JSON keys recursively before DTO conversion; bound response bytes before parsing, with string/item/depth limits. Validate schema/version, unique local IDs, declared window/mention references, predicate/object compatibility, exact literal grammar and qualifier intervals. Reuse canonical proposition validation with internal synthetic endpoints solely for validation, never persisted. Decimal values remain strings without floating conversion. Model `accepted` flags or durable bindings are unknown fields and cannot activate facts.

`VerifiedPacket` and `ValidatedExtraction` are sealed types with private constructors and no public deserialization. Use existing `SourceView::revision_content`, `unique_quote_span`, exact span/slice/hash verification and retained `ReadDependency` values. Optional absolute spans disambiguate quotations only when exact and inside the declared window; omitted spans require one exact match. Full source/revision/payload ownership and hashes are checked. Snapshot or packet-note tampering refuses import. Traceability validation does not establish entailment.

## Frozen durable extraction body proposal

One exact fenced JSON artifact uses fence info `lwiki-extraction-state-v1` and schema `lwiki.extraction-state.v1`:

```text
ExtractionArtifactV1 {
  schema, extraction_id,
  packet_id, packet_fingerprint,
  source_id, source_revision, snapshot_hash,
  response_hash, raw_response: String,
  mention_spans: BTreeMap<PacketLocalId, TraceSpan>,
  evidence_spans: BTreeMap<PacketLocalId, Vec<TraceSpan>>,
  allocations: ExtractionAllocations,
  bindings: BTreeMap<PacketLocalId, MentionBinding>,
  materialized_assertions: Vec<PacketLocalId>
}
TraceSpan { window_id: PacketLocalId, span: ByteSpan, quote_hash: Blake3Hash }
ExtractionAllocations {
  assertions: BTreeMap<PacketLocalId, RecordId>,
  evidence: BTreeMap<PacketLocalId, Vec<RecordId>>
}
MentionBinding =
  { state: "pending" }
  | { state: "resolved", entity_id: RecordId, decision_id: RecordId }
  | { state: "rejected", decision_id: RecordId }
```

`evidence_spans` and evidence allocations are keyed by local assertion ID; their vectors preserve the exact order of the response's evidence arrays. No durable mention ID or guessed entity is allocated. All nested artifact fields/tag arms reject unknown fields.

`raw_response` retains exact accepted UTF-8 input bytes as a JSON string; `response_hash` hashes those original bytes, including whitespace. Restoring the extraction strictly parses those bytes again, reconstructs typed source-local propositions, recomputes verified spans, checks exact mapping coverage/cardinality/vector lengths, and validates unique reserved durable IDs. This avoids a second mutable copy of the source-local graph. Artifact envelope ID must equal `extraction_id`; packet/source/revision/snapshot identities must agree. Typed references to materialized records and explicit decisions are verified when present. Note path: `knowledge/extractions/<extraction-id>.md`.

Raw response/hash, packet/source identity, verified proofs, and reserved allocation maps are immutable across resolution/remap edits. P10 creates every mention binding as Pending and an empty materialized assertion list. Extraction `completed` metadata means response import completed; it grants no factual acceptance.

## P10/P11/P12 boundary and idempotency

Canonical assertion records require durable entity endpoints. **P10 therefore creates the readable extraction note retaining source-local proposals and reserved assertion/evidence IDs; it does not fabricate global entities or write malformed unbound canonical assertions/evidence.** P10 import/apply yields the extraction's real ID/hash and retained allocations for P11.

P11 takes an expected extraction-note hash and explicit decisions. Pending transitions to Resolved only with an explicit entity bind/create decision, or to Rejected with an explicit reject decision. The binding map always covers every response mention. Only assertions with fully resolved subject and mention-object endpoints may materialize canonical assertion/evidence notes; literal objects require only a resolved subject. Materialized assertions start proposed and use the immutable reserved IDs. Unresolved/rejected endpoints stay retained source-local proposals. No raw model field can perform a binding transition. Future corrections/remaps require explicit retained decisions; P12 supplies exhaustive remaps and preserves allocation and response history. Note/binding/materialization/decision changes remain one guarded changeset; P13 review alone may explicitly accept supported assertions.

Import origin is existing `ChangeOrigin { operation: GraphImport, packet_id, response_hash }`. Under the held writer, call `ChangeEngine::prepare_or_reuse`: default `ReuseOrConflict`, explicit `--new-extraction` uses `AllowNewResponse`. Search canonical extraction notes and prepared/completed origin keys before any new ID allocation. Allocate extraction/assertion/evidence IDs **only inside the `build_once` closure**, retaining the maps in the change manifest and extraction artifact. Identical byte responses reuse IDs and staged/completed changes before and after apply; conflicting responses refuse by default. Explicit new extraction preserves prior records and all prior acceptance/rejection decisions. Corrupt/duplicate identity evidence refuses rather than allocating another history silently.

Carry packet-note/source/revision/original/content expected states into `ChangeDraft.read_preconditions`, so staging/apply cannot silently rebind to changed authorizing inputs. Source history remains labeled; proposals never establish current acceptance. Coverage reports omitted source windows, response unresolved entries, and Pending/Rejected endpoint counts; gaps do not create evidence.

## Leaf API and root integration proposal

```text
packet::build_packet(view, request) -> Result<PacketPlan>
packet::load_packet(view, packet_id) -> Result<VerifiedPacket>
wire::validate_response(packet, view, response_bytes) -> Result<ValidatedExtraction>
import::stage_import(engine, writer, validated, origin_policy) -> Result<ImportOutcome>
```

`PacketPlan` contains the exact packet, dependencies, immutable expected-absence `ChangeDraft`, and coverage. Root application code routes persistence/apply through existing recovery, graph validator and publication backend before returning packet stdout. Repeated tasks reuse an identical durable packet. Dry-run builds a read-only plan and performs no lock/directory/cache/provider writes.

`ImportOutcome` contains extraction locator/ID, prepared change, reserved allocations, coverage, and reused status. Import stages by default; worker does not hide canonical application. Root later wires graph extract/import CLI adapters, schema registry and tests. No new dependency is proposed; existing serde/JSON, BLAKE3, pulldown-cmark and source/changes APIs suffice.

## Required checks and unresolved design risks

Required P10 gate names:

- `packet_exact_window_fingerprint_and_markdown_restore`
- `wire_duplicate_unknown_reference_qualifier_and_quote_rejections`
- `identical_staged_import_reuses_ids_before_apply`
- `conflicting_response_requires_new_extraction`
- `model_accepted_flag_cannot_activate`

Additional meaningful cases: CRLF/Unicode absolute offsets; ambiguous quotes with exact explicit spans; nested duplicate/unknown keys and tagged-arm fields; registry/literal/unit/date rules; malformed versions and byte/item/string/depth caps; source/packet tamper; read guards drifting before apply; exact schema/instructions/context changes affecting identity; reuse before/after apply; explicit new extraction preserving decisions; `.wiki` deletion/restoration retaining pending packets/raw response/maps; Pending/Rejected endpoints never producing graph authority.

Root must freeze full `output_schema` wire interpretation and the typed candidate-context shape, then integrate authoritative artifact/schema definitions. Bounds must account for JSON escaping and artifact raw-response retention, not only selected source bytes. Reserved-ID/materialization semantics must remain identical across P10–P12. No gate passes follow from this document; implementation, named tests, review and root integration remain future work after P09 acceptance.

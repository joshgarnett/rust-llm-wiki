# P11 planning proposal: explicit mention resolution

Status: **planning only**. This document and `/tmp/lwiki-p11-types-proposal.rs` are the entire P11 lease. No P11 source/test implementation, Cargo execution, schema/module/CLI registration, dependency addition, commit, or readiness/acceptance claim. Root freezes shared interfaces after P10 acceptance. P10's final integration gate was pending when this proposal was written. Root owns accepted import helpers, shared types, schemas, canonical classifier, app/CLI, Cargo, Git, and execution state. Future leaves are `src/graph/resolution.rs`, `mention_state.rs`, and `tests/graph_resolution.rs`; no nested delegation.

Selected contracts read: WORK-PACKAGES P11; retrieval “Identity resolution and invalidation” through explicit resolution/alias requirements; record-schemas decision invariants and exact evidence quotation rules; CLI “Mutation and dry-run behavior”. Existing P10 artifact/type/import/wire APIs, changes origin reuse, bounded SourceView, and Catalog decision classification were inspected to identify concrete shared deltas. The execution playbook/checkpoint were read on resume.

## Smallest strict public boundary

Root adds `ResolutionRequest { schema, extraction_id, expected_hash, mappings }` for exact schema `lwiki.graph-resolution.v1`. Each mapping is one strict internally tagged `operation` arm, with exact PascalCase spelling:

- `BindMention { mention_id, reason, entity_id, expected_entity_hash }`.
- `CreateEntity { mention_id, reason, title, entity_type }`.
- `RejectMention { mention_id, reason }`.

All object/arm fields reject unknown fields; no nullable fields or aliases/confidence/model-global-ID alternatives are accepted. Reuse P10's bounded strict decoder and its duplicate-key, depth/node/string checks; no unchecked secondary JSON parser. Proposed ceilings: input 256 KiB, 1..64 mappings, unique packet-local mention IDs, nonempty reason up to 4096 UTF-8 bytes, nonempty title up to 1024 bytes, entity_type up to 64 bytes and exactly the canonical entity registry. Depth32/nodes65536 remain existing upper bounds; tighter operation/cardinality limits apply first. Canonical request JSON sorts object keys and normalizes mappings into mention-ID order, so reordering the same explicit decisions preserves the task identity. Reasons and every expected hash participate in the hash.

Requests may resolve a **subset** of Pending mentions. “Complete map” means every response mention remains represented in the durable artifact; omitted Pending/Resolved/Rejected bindings stay unchanged. No duplicate mappings, unknown mentions, or silent whole-map replacement. A fresh request may transition only Pending→Resolved or Pending→Rejected. Already decided mentions require proven identical-origin reuse; another decision/remap is P12, not a P11 overwrite. Reject zero-work requests unless an independently verified identical receipt proves reuse.

Names, pronouns, mention entity types, packet candidate context, aliases, and future vectors never select an entity. BindMention requires the exact current active entity ID/hash. CreateEntity explicitly uses the caller's title/type and allocates a new identity even when another entity has equal labels. AddAlias stays a separate P12 decision.

## Sealed plans and guarded publication

Proposed shared types are `ValidatedResolution`, `ResolutionPlan`, `ResolutionSummary`, `ResolutionAllocations`, `ResolutionReceiptV1`, and `ResolutionOutcome`; detailed draft is in the temporary Rust proposal. The validated type has no public deserializer/unchecked constructor, holding verified extraction, normalized request/task fingerprint, original read dependencies, and optional independently verified canonical receipt.

Leaf APIs:

```text
validate_resolution(view, request_bytes) -> ValidatedResolution
plan_resolution(validated) -> ResolutionPlan
stage_resolution(engine, writer, validated) -> ResolutionOutcome
```

Validation/planning is read-only, bounded and allocation-free. Root creates the production view with canonical total64MiB/files4096 bounds and uses revision payload caps before allocation. It verifies the current extraction envelope/body/raw-response/proofs/reserved maps and explicit entity hashes. Dry-run returns deterministic counts/reserved IDs, creates no directories/locks/indexes, and never prepares a changeset. There is no implicit staged canonical overlay for the next CLI operation.

Under the held writer, stage rechecks authorizing inputs and calls `prepare_or_reuse`. Entity and decision IDs are allocated **only inside build_once**; assertion/evidence IDs come exclusively from P10's immutable reservation maps. One decision per mapped mention has active status, exact extraction/local mention identity, caller rationale, explicit affected input/output IDs, and a durable batch receipt. The extraction update preserves unknown author frontmatter through the existing surgical editor and retains raw response/hash, packet/source identity, span proofs, reserved maps, and every untouched binding exactly. No hidden apply occurs.

Only an assertion not previously materialized with every required endpoint now Resolved may produce canonical records. Mention objects require both subject and object; literal objects require subject alone. Any Pending/Rejected endpoint retains the source-local proposal. New assertions start `proposed`; new evidence retains exact source/revision/span/hash/stance and original quote bytes. No model acceptance/confidence or resolution choice activates an assertion. Existing materialized records/statuses and successor evidence remain unchanged. The artifact materialized set grows deterministically and cannot contain duplicates or shrink.

Stage all decision/entity/assertion/evidence/extraction writes in one guarded changeset with exact expected absence/hashes and all original source/packet/revision/payload/entity/decision dependencies. Validate the complete bounded closed proposed view, including the new artifact, before preparation. Apply uses the existing engine/journal and validator; another editor changing any authorizing input conflicts. No claim of multi-file filesystem atomicity or external-editor CAS follows.

## Concrete root-owned helper and classifier deltas

1. Expose a narrow `import::verify_extraction_note(view, path, parsed_note) -> VerifiedExtractionArtifact` wrapper around private `load_note`, preserving envelope/raw/source/span/allocation/binding/materialization checks on caller-supplied proposed notes. Also supply one immutable/transition validator comparing before/after artifacts, permitting only legal binding transitions and newly materialized members. Do not expose a mutable verified constructor or reimplement weaker restore validation in the leaf. Existing `artifact_body`, `wire::proposition`, `SourceView::from_input`, `note_dependency`, bounded revision content, `sources::revision::{common,record_bytes}`, and `exact_quote_body` already suffice for construction. Do not call SourceStore.plan_evidence: it allocates a different evidence ID and uses its legacy unbounded read path.
2. Extend origin support for GraphResolve without colliding with GraphImport. Minimal compatible proposal retains existing serialized fields: operation=`graph_resolve`, packet_id=`resolution_<hash(extraction_id, expected_hash)>` as explicitly documented task scope, response_hash=canonical normalized request hash. Root may rename/generalize the origin DTO if desired while preserving old manifests. Same request/scope reuses; different mappings on the same expected extraction base conflict; later current expected hashes permit a new batch. Existing ReuseOrConflict then remains meaningful; no blanket AllowNewResponse bypass or import/new-extraction semantics. New IDs never precede origin lookup.
3. Refine `catalog::eligibility::apply_decisions`: per-mention actions conflict by `(extraction_id, mention_id)`, not merely overlapping canonical input/output IDs. Different mentions may share an extraction or bound entity and commute; contradictory outcomes for the same mention invalidate both decisions and affected binding authority. Preserve explicit extraction input IDs and entity output IDs. Preserve global merge/split/alias/review conflict and supersession rules. Root validates active mention decisions against strict artifact/receipt mappings, not prose; RejectMention must correspond to a retained Rejected binding. Empty input-ID workaround is prohibited.
4. Root schema registry/app/CLI adds resolution and receipt schemas, sealed getters/outcome, module declarations, bounded read-only plan adapter, and held-writer staging adapter. No new dependency is proposed.

## Durable receipts and restoration

Proposed fence `lwiki-graph-resolution-v1`, schema `lwiki.graph-resolution-receipt.v1`, unique actual Markdown fenced block with no decoys/multiples, maximum2MiB. Each batch decision copies the same strict bounded receipt: task ID, normalized request/hash, immutable extraction proof hash, exact decision/entity allocation maps, Pending→Resolved/Rejected transitions keyed by local mention, newly materialized local assertion IDs, and exact before/after operation hashes/paths for non-decision writes. It names every decision in that batch; each decision envelope/action/input/output/extraction/mention list must match its assigned mapping. Maps contain exactly one decision per mapping and one entity per CreateEntity; all IDs unique and disjoint from immutable reserved IDs.

The retained changeset manifest remains the complete exact operation manifest including decision notes. Receipt operation summaries exclude decision-note hashes to avoid recursive self/mutual hashes; they describe exact extraction/entity/assertion/evidence writes, while the embedded request, binding transitions, allocation maps and canonical decision envelopes authoritatively describe each decision operation. This deliberate recursion rule needs root freeze and review, rather than claiming a self-referential receipt is the full journal manifest. Before-images remain in the retained changeset; canonical receipts do not invent lost before-images.

Canonical-only identical retry after `.wiki` loss can reuse only when bounded restoration verifies the complete matching batch receipt, immutable P10 proofs/maps/raw bytes, every canonical decision/entity/materialized reserved record, and current artifact bindings. A stale expected_hash is otherwise a conflict. A proven identical receipt authorizes reuse of that historical request, never an unrelated new request against an old hash. Do not manufacture PreparedChange when its retained manifest is absent: `prepared/status` are optional, disposition CanonicalRestored, original entity/decision allocations retained. Retained reuse also compares manifest allocations and proposed receipt to canonical receipt; no canonical-B/Prepared-A pairing. Later explicit review/status changes and evidence successors preserve original allocations; P12 binding remaps require an explicit validated successor/history extension, not guessed labels or reset Pending state. Duplicate, partial, mismatched or corrupt origin evidence refuses before allocation.

## Mandatory gates and additional risks

Every required named gate remains mandatory:

- `homonym_and_pronoun_require_explicit_binding`.
- `resolution_expected_hash_and_complete_mapping`.
- `rejected_mentions_never_activate_endpoints`.
- `resolve_apply_preserves_raw_source_local_output`.

Meaningful additions: strict duplicate/unknown/null/tag/size/depth/cardinality rejection; conflicting duplicate local mappings; equal-label distinct explicit creates; shared-entity and separate-mention decisions commuting; same-mention conflicting decisions invalidating authority; stale extraction/entity/source hashes before stage and before apply; partial batches preserving all untouched bindings; no materialization until the final required endpoint; literal-subject-only materialization; rejected endpoint never producing assertion/evidence; exact CRLF/Unicode/embedded-fence quotation bytes; allocation once across retries before/after apply; reserved assertion/evidence IDs and raw response unchanged; staged/canonical mismatch refusal; canonical-only restore without invented manifest; reviewed accepted/rejected assertions and historical/retracted/successor evidence retained; pure dry-run membership/bytes/mtime invariance; malformed proposals leave no partial graph authority.

Outstanding root freeze choices: compatible origin task-scope representation; strict durable receipt schema/recursion rule; narrow import wrapper; mention-specific Catalog classifier and P12 successor authority extension. No P11 test has run and no acceptance evidence is asserted by this plan.

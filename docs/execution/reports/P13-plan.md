# P13 bounded interface proposal

Planning only, 2026-09-28. P12 forward/inverse leaf handoff is complete; root acceptance and interface freeze remain prerequisites. No P13 source, tests, Cargo, schema, CLI, or Git edits or runtime checks were performed. This document proposes contracts for root integration; it does not register capabilities or claim implementation readiness.

Selected contracts: WORK-PACKAGES P13; record-schemas graph-review/quotation paragraphs; retrieval bounded extraction/review and current support/invalidation; CLI mutation/dry-run; implementation-handoff M2 fixture path. Relevant current APIs inspected: SourceView bounded capture/closed reads, evidence verification/revalidation/rendering, import/restoration, resolution/entity decisions, Catalog eligibility/decision policy, retained graph and inverse engine interfaces. Root remains owner of shared types, schemas, engine, Catalog, app/CLI, modules and acceptance evidence. Intended later worker lease: graph/review.rs, tests/graph_review.rs, tests/m2_workflow.rs, fixtures/p13/**, reports/P13.md.

## Root DTO proposal

```rust
pub const GRAPH_REVIEW_SCHEMA: &str = "lwiki.graph-review.v1";
pub const GRAPH_REVIEW_RECEIPT_SCHEMA: &str = "lwiki.graph-review-receipt.v1";
pub const GRAPH_REVIEW_FENCE: &str = "lwiki-graph-review-v1";

#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub schema: String,
    pub decisions: Vec<AssertionReview>,
    pub supersedes: Vec<ExpectedRecord>, // required, [] when none
}
#[serde(deny_unknown_fields)]
pub struct AssertionReview {
    pub assertion_id: RecordId,
    pub expected_hash: Blake3Hash,
    pub decision: ReviewDisposition, // accept | reject
    pub reason: String,
    pub evidence_checks: Vec<EvidenceCheck>,
}
#[serde(deny_unknown_fields)]
pub struct EvidenceCheck {
    pub evidence_id: RecordId,
    pub expected_hash: Blake3Hash,
    pub assessment: EvidenceAssessment, // supports | contradicts | insufficient
}
```

Enums use exact snake_case strings. Confidence, free-form acceptance flags, paths, endpoint edits, quote changes, revision replacements, and entity assignments are not inputs. Typed IDs are resolved against actual record kinds and unique canonical IDs. Root may reuse the existing ExpectedRecord type. All arrays have unique keys; sort assertion IDs, evidence IDs and supersession IDs before canonical sorted-key JSON hashing. Duplicate keys, unknown fields at every level, null, floating-point numbers, invalid schema versions and surplus JSON are rejected through the existing strict bounded parser. A pure `normalize(ReviewRequest)` or `preflight(bytes)` runs before writer/recovery in the app; it cannot allocate IDs or touch providers/indexes.

Proposed ceilings: request 256 KiB, 16 assertion reviews, 256 active evidence checks total, 128 declared predecessor decisions, nonblank reasons <=4096 UTF-8 bytes, JSON depth32/nodes65536. Receipt <=2 MiB; canonical capture <=4096 files/64 MiB; selected source payloads <=64 MiB each with an aggregate 64 MiB capture ceiling enforced before reading/cloning. Final documents/overlay and retained before+after totals obey explicit checked byte meters before allocation; existing engine limits remain additional constraints. Worst-case mutable operations are 16 assertion edits +256 evidence retractions +256 successors +16 review Decision creations +128 predecessor status edits =672. Refuse over-limit complete membership rather than truncate it. Actual receipt-copy/proposed-byte totals must be estimated before allocation and checked again before preparation (64 MiB proposed overlay; 128 MiB retained review payloads). No new dependencies are proposed.

Task scope is `review_<canonicalhash({assertion_ids_and_expected_hashes})>` over sorted assertion/hash pairs. Origin GraphReview uses this typed deterministic task ID and the canonical complete normalized request hash. Different assessments/reasons/supersessions for the same base scope conflict; an explicitly new review uses current hashes and a new scope. Exact repeated requests reuse allocations and retained status; aborted/conflicted/committed outcomes remain honest. Allocation happens only inside prepare_or_reuse's build_once closure.

## Sealed API packet

```rust
validate_review(view: &SourceView<'_>, bytes: &[u8]) -> Result<ValidatedReview>;
plan_review(validated: &ValidatedReview) -> Result<ReviewPlan>;
stage_review(engine: &ChangeEngine, writer: &WriterPermit,
             validated: &ValidatedReview) -> Result<ReviewOutcome>;
load_review_receipt(view: &SourceView<'_>, task_id: &RecordId)
    -> Result<Option<VerifiedReviewReceipt>>;

verify_review_overlay(fs: &VaultFs, input: &ValidationInput,
                      retained: Option<&RetainedGraphInput>)
    -> Result<Option<VerifiedReviewOverlay>>;
verify_review_policy(notes: &BTreeMap<VaultRelativePath, ParsedNote>)
    -> Result<Option<VerifiedReviewPolicy>>;
```

ValidatedReview contains normalized request/hash/task ID, dependencies, bounded verified captured input and optional verified restored receipt. Constructors/fields stay crate-private; no Deserialize/public unchecked constructor. ReviewPlan exposes estimates/counts and borrowed validated getter without allocating IDs. ReviewOutcome mirrors P11/P12: decision locators, allocations, summary, reused, prepared Option, status Option and RetainedChange|CanonicalRestored disposition. Canonical-only acknowledgement does not invent a manifest or reapply historical bytes.

VerifiedReviewOverlay exposes `accepted_assertions()` and exact supersession edges. It does not grant proposition retargeting or blanket graph validity. VerifiedReviewPolicy exposes proven review supersession edges and bounded historical review authority; root unions edges with existing Decision/receipt edges before cycle checks. Supply `relevant_decision_ids(notes)` for diagnostic attribution so malformed unchanged baseline review receipts do not abort unrelated operations. Relevant metadata plus an actual unique supported Markdown fence selects receipts; unrelated rationale mentioning a fence name is ignored. All copies and generated decision envelope identities must match.

## Durable receipt and exact transition proof

Root-owned ReviewReceiptV1 should retain schema, task ID, normalized request and request hash, allocations keyed by assertion and changed-stance predecessor evidence ID, original record_paths, per-assertion prior status/proposition invariant, complete prior active evidence membership/hash map, exact evidence assessments/transitions, source/span/quote proofs, complete declared predecessor Decision scope/hash/action/status, explicit supersession edges, and non-Decision operation before/after hash proofs. Source proof includes source/revision IDs, immutable original/content hashes, absolute UTF-8 span and quote hash; it never treats prose as entailment. New Decision envelope timestamp/rationale/actions/inputs/outputs/IDs and every creation are deterministically recomputable from receipt data; the receipt does not recursively hash its own Decision note bytes. Retained witness still proves actual complete Decision bytes independently.

One Accept/Reject Decision per reviewed assertion has input/output assertion ID only, action matching requested outcome and reason as readable prose. Evidence edits are authorized by the same strict receipt; no separate invented Correct decision is required. Complete receipt is copied identically into each new review Decision. Preserve existing assertion/evidence/old Decision author body and unknown ordinary frontmatter through guarded lossless status splices. Changed stance creates a new evidence ID, retains the predecessor's exact body including quotation/explanation and ordinary metadata, and changes only identity, active status, stance and explicit supersedes ID/link. Preserve source/revision/span/hash/assertion/extraction trace fields; regenerate navigation links using saved original paths, resolve current stable IDs separately. Predecessor is retracted in the same changeset. Insufficient retracts without successor. Same stance makes no evidence write. No review rewrites captured revisions, source head, P10 raw response, reserved maps, or mention bindings.

Bounded source proof uses revision_content_bounded, evidence_reference and exact note_quote, then compares the declared absolute source slice/hash; repeated matching quotes elsewhere do not invalidate an already explicit span. Do not call legacy SourceStore::plan_evidence/build_evidence: it allocates before build_once and performs unbounded fallback reads. Rendering may be private in review.rs using already verified fields/body and assigned ID. Damaged source/quotation integrity is conservatively refused; historical or withdrawn intact evidence may be assessed/retracted, but cannot supply Current acceptance. Root can separately authorize repair semantics later without weakening this packet.

Preparation proves physical all-before hashes and complete active evidence membership by canonical assertion_id association, independent of convenience evidence links. Closed validation captures every selected evidence source chain as assets with exact observed guards; missing unrelated assets never become synthetic physical dependencies or fallback reads. At actual apply/final/recovery, engine-sealed retained bytes supply logical-before membership when current files already contain some after states. Require current operation targets equal exact before or after bytes, complete overlay equals declared writes, every Decision creation/predecessor edit equals exact generated/status-only bytes, and exactly one matched GraphReview origin/request/allocation receipt. Enumerate actual current AND retained logical-before AND final associated evidence; allow only declared transitions/allocated successors. New active evidence, reassociation, duplicate IDs, extra writes, hidden qualifiers/status/body changes, stale hashes, or mismatched origin conflict before further publication. Partial recovery never borrows an edited wiki_status as commitment proof.

Catalog must test every accepted_assertions ID for accepted + Current after review, regardless of baseline status. This includes re-review of accepted assertions and already-written acceptance during FilesApplied recovery. One intact current active support is required after retractions/successors; contradictions remain visible and disputed. Neither historical revisions, withdrawn sources, confidence nor closed-overlay estimates grant acceptance. Actual application performs full current source/dependency reproof before publication. No multi-file atomicity or protection against an external editor's post-hash race is claimed.

## Explicit supersession, P12 and restoration

Review must not silently override existing active Accept/Reject decisions. The proposed required supersedes list hashes explicitly authorized predecessors. Each must be active, action Accept/Reject, have a nonempty entirely assertion-typed authoritative scope input_ids union output_ids, and its entire scope must be included in this review batch. All overlapping active Accept/Reject predecessors must be declared, including same-outcome re-reviews; spurious/missing/stale predecessors are refused. Only status changes to old decisions are allowed. Receipt edges connect each predecessor to new decisions covering its complete scope; optional wiki_supersedes_id is emitted only where a single predecessor is unambiguous. Root must freeze this multi-edge convention and integrate it into cycle/conflict authority. Correct/identity decisions are not automatically superseded; incompatible overlaps fail under the existing classifier. Supersession chains remain bounded64 and ID-order-independent.

P12 D37 continues to recognize these Accept records by their exact assertion scope: endpoint-changing remap resets accepted to proposed and can supersede an obsolete Accept only when its ENTIRE scope actually changes under ONE governing Merge/Split. New review must consume the resulting current assertion/evidence hashes. No old review can silently reaccept a remapped proposition. Pure review policy must tolerate predecessor status superseded only with a complete verified successor chain; root P12 receipt edges and P13 receipt edges join one cycle graph, without generic compatible-family transitive closure.

Retained reack validates exact manifest payloads/allocations/immutable receipt identity. Canonical-only reack uses saved original paths and semantic historical transitions, not reconstructed lost before-images; current stable IDs/path/title/prose may evolve. Later explicit P13 reviews, P04 revalidation and P12 remap may change statuses or supersede original evidence/decisions, while exact quote/source trace and immutable receipt facts remain provable. Original extraction reserved evidence IDs stay unchanged. A bounded read-only cross-package evolution hook must validate these descendants without recursively calling import/Catalog; unknown changes refuse reack. Acknowledgement preserves current bytes/status/prose. Full later-history helper signatures should be frozen by root before implementation, rather than inferred during restore.

Review rollback uses the ordinary guarded inverse workflow, including undo-of-inverse. Existing engine inverse authentication terminates only at GraphDecide, so root must extend sealed ancestry dispatch for GraphReview and add a review committed-anchor verifier. It verifies exact original review semantics from retained bytes while skipping only obsolete unmodified historical guards under engine commitment seal. Current final references, Decision conflicts and restored-accepted Current eligibility are still freshly checked. Deleting review-created successor evidence cannot leave undeclared canonical successors/references. No public unchecked bool mode or broad acceptance exemption.

## Root integration requests and gates

Root freezes review_types.rs/request+receipt schemas; adds OriginOperation::GraphReview to origin schemas and engine retained dispatch/payload limits; wires sealed review overlay/policy into Catalog and applies accepted-set Current checks initial/final/recovery; integrates review edges with P12/global cycle and supersession rules; extends authenticated inverse anchor dispatch and Current restoration; owns app/CLI graph review --file, pure dry preflight, capability/schema registry. Production graph input and source helpers retain bounded closed behavior. Canonical restoration/evolution hooks are shared root deltas. These are necessary integration requests, not worker source permission.

All five P13 named gates remain mandatory:

- review_omitted_or_new_active_evidence_conflicts: omission, duplication, wrong assertion membership, stale evidence/assertion hash, new evidence after stage and during partial recovery.
- changed_stance_successor_and_insufficient_retraction_atomic: exact Unicode/CRLF/body/unknown-frontmatter preservation, same revision/span/hash/trace, predecessor retraction, allocation-once retry, actual native injected interruption and recovery.
- accept_requires_post_review_current_support: insufficient removes last support, changed supports->contradicts loses support, contradicts->supports succeeds only with intact Current source, disputes remain visible, already accepted re-review and already accepted partial recovery still reprove support.
- review_cannot_restore_withdrawn_or_old_revision_support: withdraw/head advance after stage and after partial writes; P04 explicit revalidation successor supplies new current support without automatic acceptance.
- m2_packet_import_apply_resolve_apply_decide_apply_review_apply: actual materialized command stages under one authorized local workflow, homonyms remain distinct, explicit P12 merge/split/alias and P04 revalidation, then supported review; query/context cite verified bytes; cache deletion/rebuild gives equivalent canonical semantics with zero providers/helpers.

Additional meaningful gates: strict schema/unknown/null/duplicate/depth/count/size boundaries before allocation; direct engine.apply hidden Decision/evidence/proposition writes and zero-match origin; complete mixed batch supersession/cycle/stale/shared-scope refusal and valid full scope; actual P13 Accept->P12 D37 remap plus mixed scope refusal; real apply/recovery failure before publication and source withdrawal after FilesApplied; retained and canonical-only reuse after cache/changes deletion, author/path edits and legitimate later review/revalidation/remap; malformed relevant fences versus unrelated prose; review inverse/undo/partial recovery, new evidence references and stale accepted restoration; dry-run recursive bytes/membership/mtime invariant with no writer/index/provider/helper use. Root integration reruns accepted P10/P11/P12/source/catalog/context and CLI/schema gates. Native injected faults demonstrate the exercised boundaries only, not power-loss, unavailable-platform, host or live-provider qualification.

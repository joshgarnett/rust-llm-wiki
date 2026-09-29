# P13 independent invariant review

Independent Sol fallback because fresh Astra review runtime remains unavailable at the thread limit; root keeps separate source/runtime approval and every gate. Read-only review of the isolated accepted-P12 plus P13 candidate named by /tmp/lwiki-p13-acceptance-path.txt, selected P13 contracts and D42, graph review/types/remap, engine retained/inverse factories and Catalog delegation. Only this report is edited. No Cargo, source/test/Git/network edits, nested delegation or runtime reproduction by reviewer.

Status: **R1/R2 and origin-omission fixes source-reviewed; additional source-read bound R3 reported to root; runtime acceptance pending**. Worker forward tests are running; authored tests are coverage proposals, not execution evidence. Source changed during this bounded review; the fingerprints below identify the final inspected workspace sample, not a claimed frozen/runtime-tested candidate.

## R1 — retained acknowledgement mixes original proposals with later active Decisions

stage_review canonical receipt branch calls retained_receipt for each matching retained origin (review.rs2144–2157). That helper overlays the original before/proposed payloads onto current v.input, then calls load_review_receipt on the synthetic proposal (2075) and expected_writes with ordinary fresh validation (2094). Later real Decisions remain in this mixed view. Apply Accept A, then an explicit superseding Reject A, retain changes/, and stage the original Accept request again: synthetic A and its old Decision become accepted/active while the later Reject remains active. Policy rejects that mixed graph, even though the actual canonical descendant is legitimate. Fresh predecessor/current-support checks can independently reject old committed acknowledgement after later valid history/source changes. Existing canonical_review_reack_preserves_author_prose_and_later_decisions deletes changes/ and does not cover this retained path.

Root accepted the fix direction: authenticate exact original retained manifest/payload/receipt creation separately, then acknowledge actual canonical descendant history without activating the synthetic old graph or requiring historical support to become Current. Keep engine current/final Current checks for real forward acceptance and accepted inverse restoration. Regression must retain changes and prove original allocation reuse/no writes through later reviews, author edits and valid P12 evolution; forgery/canonicalB-versus-retainedA still refuses.

## R2 — forward GraphReview input bounds apply after allocation

changes/apply.rs validation_input (653–680) selects64MiB/4096 bounded canonical scanning and declared overlay cap only for inverse_of. Forward GraphReview uses scan_documents and usize::MAX overlay allowance. Leaf review::bounded rejects the complete input only after those bytes have been allocated. Stage a valid review, then add4097 ordinary tiny Markdown notes or aggregate canonical bytes above64MiB before apply. The advertised capture bounds must stop enumeration/reads before allocation rather than rely on leaf refusal. Intrinsic128MiB retained-manifest checks correctly bound retained payloads, but do not bound current canonical scan or64MiB proposed overlay.

Recommended root correction: select the existing bounded graph-input path for forward GraphReview too, including pre-read declared overlay aggregate. Test a sparse/aggregate hostile canonical input or enumeration boundary at the real engine entrypoint; do not merely test bounded on a caller-owned already allocated ValidationInput.

## Origin omission boundary requiring explicit root policy

verify_review_overlay(None) supports pure planning, but ordinary Catalog validate also invokes it without a retained witness. A copied exact prepared review operation set with origin cleared/allocated_ids empty can take that path; current forged-origin test mutates a Some(GraphReview) hash instead. Root must distinguish intentional ordinary complete-review policy from runtime engine-witness authority. Suggested probe clears origin on a real prepared proposal. This is a source architecture question pending root disposition, not a claimed runtime bypass.

## Other inspected invariants and correction delegation to rereview

Fresh review checks exact assertion hashes, all active evidence IDs/hashes, typed companions, verified original/content/span/quote bytes, source-head ownership and post-assessment supporting Current source before acceptance. Changed stance allocates a new ID and preserves immutable span/extraction/body via exact predecessor template; insufficient retracts without allocation. Exact expected write membership includes allocated Decisions, predecessor status-only bytes and non-Decision receipts. Full predecessor union is explicit; combined review/entity edges use memoized longest-suffix64-hop/cycle checking, independently of ID order.

Runtime retained and inverse witnesses are engine-created, non-Deserialize and actual input.documents remain separate from historical logical reconstruction. Inverse proofs bind authentic committed anchors, exact mutable reversal and retained payload hashes; new typed references to deleted evidence/Decision and author edits refuse. Catalog separately requires every accepted/restored assertion Current, including already-restored Applying/FilesApplied targets; pure derived eligibility alone never proves review authority.

Root's pending correction delegates runtime source/original/content/span validation to actual full Catalog projection plus selected exact read dependencies, while the leaf validates structure/trace/receipt/membership without filesystem fallback. Rereview must check actual evidence quotation/body comparison, typed references and Source ownership are still performed, asset guards include all selected originals/content and final verification uses the unchanged actual scan; source-delegation assertions alone do not close that obligation. Fresh local validation continues full proof. All runtime/publication/inverse tests remain mandatory.

## Final bounded rereview and R3

R1's revised retained_receipt (review.rs2127 onward) checks each original allocated Decision receipt from exact retained proposed bytes against the separately validated actual canonical descendant. It regenerates original expected writes from retained before bytes without fresh current-support/predecessor validation, and compares the complete manifest operation role/before/after/proposed byte set. The canonical-known path no longer loads policy from the synthetic original proposal. This closes the reported mixing mechanism at source level; retained historical regressions and forgery controls still need actual runtime evidence.

R2's revised validation_input (apply.rs652 onward) selects bounded_graph for forward GraphReview/GraphDecide and inverse manifests, checks declared proposed aggregate before payload reads, and uses bounded64MiB/4096 canonical scanning. prepare::plan performs its corresponding pre-clone proposed aggregate check. Source closure is conditional on real forward enumeration/aggregate entrypoint tests.

The origin question now has explicit source closure: eligibility.rs89–99 refuses a non-closed review overlay without authenticated GraphReview witness, while validate_closed remains preparation-only. A real missing-origin regression remains required.

Runtime delegation preserves exact evidence authority: source_metadata and retained_semantics check typed ID/companion resolution, retained revision/head ownership, exact span/quote fields, fenced quote hash and original immutable body. Full actual Catalog projection validates every Evidence, including retracted Evidence, through SourceView::verify(Historical); that verifier checks original/content integrity, the exact UTF-8 slice/hash and equality to note_quote. proposed.dependencies flow into ValidatedGraph; engine verify_dependencies runs around final publication. Actual documents remain separate from logical retained reconstruction, and accepted/restored sets require Current even on already-restored recovery scans.

**R3 remains a source bound gap in that delegation.** Full normal Catalog projection calls revision_content/verify without read limits. sources/revision.rs295–333 read_limited(None) falls through to fs.read_before for physical assets. Replace a selected reviewed original/content asset with a sparse file larger than64MiB after staging: normal projection can allocate it before hash refusal, even though runtime leaf now intentionally avoids source reads and canonical/retained inputs are bounded. Root was notified to bound selected assets before/full projection, or supply a bounded SourceView/project for this invocation. No reviewer runtime reproduction or final root disposition is claimed.

Final inspected SHA-256 sample:

| File | SHA-256 |
| --- | --- |
| src/graph/review.rs | 4a521696f7043e2134ba7b115a666f0678e5a9032f2ffd28ea317ba7368e809b |
| src/graph/review_types.rs | 822dda0dbeea223cd9107648f370f692bc05a052c8f591073394b1d773a70298 |
| src/graph/remap.rs | c34705092c30f7e575ca2c4fcdde82ba38b230d81484c75f7e87c11a47fd85dd |
| src/changes/types.rs | ff8200d1bad1d7f604ad96966ab1a4e294d403787a2160972222025dc055f05d |
| src/changes/apply.rs | f89b147ba2a8dec8564849af6f9ae7192a93ff3465757afc055f95f5f1314258 |
| src/changes/prepare.rs | cc450f2f195f01fb9ce3f05c6ea7aa0bdf1ad4c4a9593eaa3b1ea2309007c591 |
| src/changes/rollback.rs | 1f232708bbef9eb314947e0b12dda835a4760b6b93083583b1a0471831f12ac1 |
| src/catalog/eligibility.rs | 8fefea06bc5776820d173edf329446138a5ec0dc4c5eced1168b4dfedb4fe77d |
| src/sources/revision.rs | 8568f47c648302b217820577fff101b1ae24dbe947b93d3b6d82f2f3f4e70b13 |
| src/sources/evidence.rs | d6d3f4e4f68fb701b72fbd8d6b34d91f5e6c721cadd1ea60473df00553e0d4ba |
| tests/graph_review.rs | 8f4c7e7aa58e90e4593157288ce00a65aa80f58aac76c35e31529c6c21073794 |
| tests/m2_workflow.rs | f72f0341977059be4c62c05d941d7034832baaebf519e922d4cc5d1273213aa3 |

Reviewer ran only bounded source reads, searches and hashes. No Cargo/test execution, runtime reproduction, native crash or live qualification is inferred. Report lease returned to root; further fixes/runtime acceptance belong to root and worker.

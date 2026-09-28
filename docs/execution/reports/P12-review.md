# P12 independent invariant review

Status: architectural findings open/pending implementation and runtime gates. P11 accepted5ebca61; P12 is unaccepted. Astra `/root/p12_invariant_review` performed bounded read-only source/contract review (no files/Cargo/tests). Runtime rejected its followup at thread limit; independent Sol `/root/p05_scan_eligibility` reviewed the new witness shape and obsolete acceptance rule. Root records their findings here, with separate attribution; no reviewer runtime claim.

## Astra architectural findings

1. Preserve actual ValidationInput.documents. Supply verified retained before-images separately. Enumerate references in actual and projected final views, reconstruct declared old states from payloads, and classify each current target strictly as old/new; unfamiliar references or third states conflict. A final-view-only verifier can miss an undeclared old reference hidden by an unrelated overlay edit.
2. Merge A→B may overlap B's independently verified unchanged P11 mention scopes. Compatibility cannot be limited to allocated replacement IDs. Permit exact distinct current mention authority on active outputs; retain unrelated review/correct/reject conflicts.
3. AddAlias(A) before merge/split A must preserve explicit historical label authority or be explicitly superseded. Root D37 preserves exact retained historical aliases, with no current binding authority. Broad entity-outcome bypasses remain forbidden.
4. Assertion-ID authorization requires every proposition difference to match exact declared field transitions. Reject hidden object/predicate/literal-kind/qualifier/status/evidence/companion edits alongside a declared subject remap.
5. Canonical receipt/family verification belongs below projection/import restoration; it must not call either recursively. Bounded historical mention chains bind exact scope, predecessor/successor IDs, governing receipt and immutable artifact proof; reject forks/cycles/missing links. Fresh resolution remains Pending-only.
6. Include every verified receipt supersession edge in cycle checks, including historical decisions. Validate all allocated receipt copies; exclude arbitrary decisions from compatibility exemptions.

## Sol followup on D37/D38

The opaque RetainedGraphInput shape is architecturally sound: engine-only pub(super) fields, no Deserialize/Clone, getters only. Apply verifies before/proposed retained refs with aggregate128MiB preallocation ceiling; ordinary default validates without granting an exception. Exact GraphDecide policy still owns semantic proof, with unchanged actual scan at initial/final calls. No architectural blocker found in the witness shape; runtime unproven.

Obsolete active Accept authority can be superseded only when one governing entity operation changes its entire nonempty authoritative assertion union from accepted to proposed. Require every target to be an assertion, exact before hashes, invariant proposition/evidence trace, status-only predecessor edits and exact supersession edges. Refuse mixed untouched scope or several governing operations rather than silently disabling unrelated acceptance. Enumerate active acceptance predecessors again at apply/publication so newly introduced review authority conflicts.

Required regressions: actual accepted/current-evidence remap loses current factual status; partial shared Accept scope refuses; all assertions of one predecessor changed by one governing operation resets all; stale hashes/missing/spurious edges/no-op remaps/unrelated decisions refuse; accept added after staging conflicts. P13 must later exercise the real review CLI→entity-decision path. No fixture-only result may claim P13 compatibility.

## Root implementation disposition

D36–D38 recorded; leaf verifier and shared Catalog/history integration in progress. Root added separate engine witness plus two authored production-engine factory tests for FilesApplied interruption/recovery with actual current scan and retained original bytes, and tampered before-payload refusal. No Cargo has run on these new paths while P15 holds the exclusive lease. Full P12 six named gates and relevant recovery/hidden-edit/new-reference/schema/CLI/restore tests remain mandatory.

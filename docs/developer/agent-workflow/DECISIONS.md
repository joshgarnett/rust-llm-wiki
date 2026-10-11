# Implementation decisions

User instructions and implemented contracts determine scope. Root maintains this
reusable decision procedure; a local task records temporary choices and evidence.

## Decision procedure

| Situation | Action |
| --- | --- |
| The contract decides it | Implement it without reopening a routine preference question. |
| A reversible detail is unspecified | Choose the simplest compatible behavior; record material interface choices and validate relevant invariants. |
| Contracts conflict | Identify their precedence and invariants, request bounded independent review where useful, update all affected contracts. |
| A core correctness/recovery check fails | Fix it and replay the public failure; do not classify missing implementation as external qualification. |
| Credentials, host GUI or native platform are unavailable | Finish safe local implementation; record the exact unqualified external boundary. |
| A destructive/paid/publishing step lacks authority | Prepare a concrete reviewable result; preserve progress and respect current platform permissions. |
| Whole-task quality stalls | Stop local tuning after two unsuccessful interventions, trace stage information loss and request fresh architecture review. |

## Current choices

| Choice | Rationale and revisit trigger |
| --- | --- |
| One Rust library/CLI, bundled SQLite | Canonical Markdown and rebuildable projections without required external services or a local model runtime. |
| Remote embeddings, host-agent research, optional remote extraction | Separate provider authority/accounting from host tools and unobserved usage. |
| Immutable Source revisions and heading-independent IDs | Retain exact citations and history through edits, refresh and withdrawal. |
| Lossless supported author mutations | Keep unknown properties/comments/BOM/CRLF and refuse unsupported structured changes. |
| Sealed validation/publication/replay | Preparation is never application authority; guards and retained proof survive recovery and undo. |
| Exact retained change payloads and journal intent | Matching current bytes alone cannot authorize interrupted replay or reconstruct lost before-images. |
| Retained unknown accounting | An uncertain attempt remains reserved until explicit authenticated reconciliation; cancellation does not imply zero cost. |
| Explicit normalized activation and schema2 cleanup | Preserve old identities and operation authority; take complete backups before authorized migration. |
| Selected dependency proof plus explicit global reconciliation | Generation-scoped discovery cannot claim global freshness or corpus absence. |
| One identity-pinned user default | Resolve explicit/nearest/default once; no registry or silent wrong-target fallback. |
| One bounded private preferences snapshot | Explicit replacement/bypass, guarded v1→v2 writes and exact backup; no provider permission grant. |
| Strict existing-folder initialization | Every reserved name absent before writes, ordinary author/editor files untouched, complete marker before managed state, no hidden-state adoption. |
| Narrow literal note editing | Exact existing-target whole-file guard, no canonical allocation, immutable/control exclusions; malformed identity needs known bytes. |
| Output envelope v2 | Selection metadata changes the published closed v1 shape; retain v1 for older output. |
| Bounded grouped maintenance and compact embedding descriptors | Reduce repeated work without discarding per-owner history, supplier authority or invocation-wide budgets. |
| 25K/about-2.5GB first capacity target | Public complete-workflow/resource qualification remains open; 100K is deferred. |
| MIT; GitHub Issues; unsigned macOS without notarization | Owner choices do not waive notices, normal OS trust checks, native final-byte qualification or publication authority. |

[Design rationale](../design-decisions.md) explains architectural tradeoffs.
[Contracts](../contracts/README.md) specify exact formats and invariants;
[validation status](../validation-status.md) states measured limits rather than
promoting historical component scores into whole-product acceptance.

# Large-vault architecture

The first qualification target is 25,000 distinct current captured documents totaling
about 2.5 GB of UTF-8 content; 100,000/about 10 GB is deferred. These are targets,
not measured supported capacity. [The capacity protocol](testing-large-vaults.md)
freezes controls, budgets, churn and correctness gates before execution.

## Current implementation boundary

Explicit normalized activation (`index rebuild --normalized`) stores published
per-document facts, search postings, navigation and operation state. Queries read
publication facts and verify selected dependencies instead of reconstructing completed
history. Authored Pages, captured Sources, grouped imports/refresh, Page moves,
selected reads/context and compatible semantic/hybrid vectors share this publication
boundary. Default activation and full command parity remain separate integration work.

Retained historical authority survives derived-index removal. Schema2 moves operational
records under `.wiki/retained` without changing Source/Revision IDs; readable reports
remain available. Rebuild is explicit, respects active operation authority and preserves
compatible vectors where available. Missing complete vector caches cannot be recreated
offline from receipts. Follow [storage](contracts/storage.md) and
[retrieval](contracts/retrieval.md); no scaling optimization weakens these invariants.

## Costs that shape the design

Legacy full projection scans, repeated history validation and per-document durable
publication can dominate collection/update time. Whole-corpus embedding rendering and
per-query exhaustive vector scanning add independent costs. Optimize only the measured
stage, keeping logical work, physical I/O, memory, disk, latency and answer completeness
separate. A small faster component or a finite successful import does not demonstrate
a useful integrated full-tier workflow.

Use resumable explicit input manifests and bounded publication groups. Preserve each
Source's original bytes, immutable revisions, author guards and individual outcome.
Use affected-row updates when operation authority admits them; fall back to bounded
reconstruction for unsupported changes. Reconcile canonical input/final bytes before
publication. Joining independent work may reduce elapsed time, but worker service sums
and admitted logical workspace are neither command latency nor native process RSS.

Policy-bound compact embedding descriptors retain owner IDs, selected authenticated
suppliers and changed-dependency invalidation. Repeated inputs can share paid content
while retaining individual ownership. A single invocation's call/deadline allowance
must span task partitions and Runs; durable unknown reservations remain binding.
Approximate vector indexing is a future measured choice, requiring exact-neighbor
comparison and migration before promotion; it is not an existing scale guarantee.

## Freshness must be explicit

A published generation answers what was discovered at that boundary. Selected proof
answers whether returned canonical evidence/dependencies still authenticate now.
Strict global reconciliation detects omissions, duplicates and changed corpus state.
None can silently substitute for another. Sequential verification does not claim a
filesystem-wide snapshot against arbitrary editors, and an empty bounded result is
not proof that no answer exists.

Healthy selected evidence should remain usable when unrelated records are damaged,
while failed observation remains nonzero and contained outcomes disclose issues and
eligible exclusions. Correctness blockers include wrong-target writes, lost author or
immutable bytes, guessed identity, stale evidence labeled Current and accounting loss.

## Integrated workflow and migration

Qualify ordinary public commands in sequence: explicit/default selection, author-note
capture, inspect/edit, path-selected refresh, sync, literal/lexical context, current/history
citations, author Page reconciliation, restart, cache reconstruction and backup restore.
Maintain source identities and exact old citations through both storage layouts.

A candidate changes shared interfaces only after independent architecture challenge
of the measured problem, benefit, simpler alternatives, cost and migration path.
After two whole-task interventions without clear gains, stop local tuning and trace
information loss through the complete workflow. Freeze a distinguishing experiment;
never replace a failed capacity/quality gate with a narrow component score.

[Validation status](validation-status.md) records enduring failures/limits;
[testing](testing-large-vaults.md) specifies new measurement controls. Raw attempts
and working architecture notes remain local rather than becoming a published diary.

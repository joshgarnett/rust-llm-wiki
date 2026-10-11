# Validation and limits

Choose checks that match the behavior you want to establish. A local fixture can
verify a transaction or citation, while an installed agent, live provider or
release archive needs a separate replay with its actual environment. Keep run
results and unfinished checks in local task evidence; maintain reusable procedures
and established limits here.

## Workflow and recovery

Test complete public-command workflows, including capture, refresh, history,
page editing, change review and restoration. Use disposable wikis and mock
providers for correctness tests. Inspect the returned text and saved files as
well as exit codes. A safe refusal must explain how the reader can proceed.

Native fault, kill and restart tests establish recovery only at the interruption
points reached on that platform. They do not establish power-loss safety.
Cross-builds establish compilation, not native filesystem recovery. Windows wiki
writes are unsupported.

Recovery must retain pending apply intent and uncertain remote charges through
conflicts, cleanup and retry. A complete backup includes hidden retained state;
visible Markdown alone cannot restore an interrupted operation or its accounting.
See [storage invariants](contracts/storage.md) and the [recovery guide](../user/recovery.md).

## Retrieval quality

Measure task completion, citation integrity, relevance and answer completeness
separately. A correct quote hash establishes the captured bytes, not whether the
passage answers the question. Search ranking or finding the right source does
not establish complete evidence for all required facts.

Freeze the binary, source revisions, cache, budgets, expected facts and error
policy before exposing unseen acceptance questions. Any question used to guide a
fix becomes development data. Retain every failure, regression and exclusion.
See [context evaluation](evaluating-context.md) and [quality targets](rag-quality-targets.md).

Assisted selection and complete-original reading add agent work and budget.
Evaluate them as separate workflows, including additional input, calls, observed
latency and known costs. Their results do not establish the quality of automatic
context under the original limits.

## Capacity

Qualify the complete workflow at the intended scale: import, retrieval, update,
recovery and resource use. A successful large import does not establish search
quality or maintenance cost. Use the [capacity protocol](testing-large-vaults.md)
and [full-check benchmark](benchmark-full-check.md) to define inputs, limits and
required stages.

Pin a release executable with its compiler settings and hash before measuring.
Record hardware, source/input hashes, physical disk use, process-tree memory,
monotonic timing and failed or unrun stages. Unoptimized diagnostics cannot
establish shipping performance. Protocol thresholds are targets until the
corresponding complete measurement supports them.

## Qualification

Before claiming support, replay the intended user task with the final executable
on each advertised native platform. Check fresh installation, PATH, reinstall and
removal, editor round trips, backup restoration and ordinary OS trust controls.
Use the [external qualification procedure](qualification.md) for installed agents,
live providers and final release downloads.

The installer targets Apple silicon macOS and x86-64 GNU/Linux. macOS releases
are unsigned and are not notarized. Hashes check integrity against the supplied
manifest; they do not independently authenticate the publisher. Packaging must
retain license and dependency notices and preserve normal OS security controls.

Mocks establish local provider mechanics. Live interoperability and billing need
an authorized endpoint, finite limits and retained receipts. A successful probe
for one role does not establish other adapters, extraction schemas or model
quality.

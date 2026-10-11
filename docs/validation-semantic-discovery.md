# Experimental semantic discovery checkpoint

Status, 2026-10-11: implemented behind the default-off
`experimental-semantic-discovery` feature. Native base-graph publication failed
on the tested macOS host. Do not promote this candidate or infer retrieval,
capacity or release acceptance from its affected checks.

## Implemented behavior

Cache-only embedding preparation can stage a derived semantic graph from retained
paid vectors. A pinned USearch 2.26.4 child constructs the base topology; the parent
owns durable writes. Complete descriptor, vector and topology audit precedes
manifest selection. A bounded delta supports changed owners. Query discovery,
selected-owner reranking and exact evidence use shared work counters and recheck
publication freshness. Canonical Markdown and paid provider accounting remain
independent of this disposable graph.

Admission derives from actual node count, dimensions and observed filesystem
allocation geometry. The experimental policy caps child address space at 8 GiB,
aggregate graph-artifact reservations at 32 GiB and an invocation at 120 seconds.
A separate persistent SQLite ledger reserves complete attempts and debits artifact
credits before writes. Failed and unknown holds are retained. These allocation
reservations are not proof of physical disk peaks or resident-memory bounds.
Failed-attempt recovery and lifetime reservation exhaustion remain open usability
issues; there is no automatic refund or reclamation claim.

## Validation and actual failure

The native optimized CLI and library tests compiled with release optimization
level 3 and debug information disabled. The affected check set comprises 97 valid
passes: 88 unchanged passes plus nine corrective replays. Initial evidence retains
two compiler errors, seven failing fixtures and two audit negatives whose original
passes did not reach the intended assertion. Corrections use ordinary owner
acknowledgement, phase-specific deadlines, valid per-record byte charging and the
actual cache-loss error. All three durable-audit tests were replayed. Production
source stayed unchanged during the corrective fixture build. This is a composite
affected-check result, not a full green suite.

The first real cache-only smoke used a disposable copy of the retained public
994-Source corpus: 1,429 passage vectors and 40 query vectors. Staging all passages
completed in 50.998 seconds. The next invocation refused after 0.117 seconds with
`CAPABILITY_UNAVAILABLE`: semantic builder address-space limit installation or
readback failed at 8 GiB. No graph was selected and the context query was unrun.
The original vault was verified unchanged, no provider calls occurred, and failed
artifacts and reservations were retained. The current error does not distinguish
set failure, read failure or unequal readback; allocator causality is unproved.

To reproduce the affected library checks after building with the feature, select
`retrieval::discovery`, `catalog::semantic_discovery`,
`app::semantic_discovery_workflow_tests` and the affected inventory/vector tests.
A real lifecycle additionally requires complete corpus/query cache coverage and
pinned publication identity before running offline `embeddings sync`; a passing
small delta fixture does not qualify the isolated native base builder.

## Remaining gates

Independent review requires a bounded startup-mapping diagnostic before another
native graph run, followed by actual memory-limit enforcement qualification if
that diagnosis supports a change. The observed 51-second staging time has no
fsync attribution. The leaf-per-node representation and duplicated input/output
vectors also need architectural reassessment before scale execution: at a
conditional 325,246 passages and 1,536 dimensions, raw duplicated vectors alone
require 3,996,622,848 bytes. Actual corpus segmentation and complete peak occupancy
must be measured; the current experiment's remaining disk allowance does not fit
that lower bound.

Native quality remains 13/32 complete positive tasks and 79/129 required facts.
Development acceptance requires at least 17/32, preservation of every baseline
fact, all eight absent controls and exact citations under unchanged evidence and
proof budgets. Graph recall, bounded query cost, ordinary update/search/rebuild,
crash/corruption recovery, unseen/HIGH evaluation and representative 25K/~2.5 GB
qualification remain open. Practical 0.2.0 still requires independent at least
9/10 review and no correctness blockers across the complete product workflows.

# Design decisions and tradeoffs

Use these decisions to understand the storage and retrieval tradeoffs.
[Contracts](contracts/README.md) specify implementation requirements;
[validation and limits](validation-status.md) explains which checks support each
kind of claim.

## Markdown knowledge and rebuildable indexes

Keep canonical records, immutable captures, evidence, decisions and readable research
reports in Markdown. SQLite supplies metadata, FTS, graph projections and local vectors.
One Rust executable and bundled SQLite avoid required external database services,
Python/Node runtimes, local models and a resident daemon. Remote embeddings remain
API work. A cache is not a replacement for retained revisions, undo payloads or
accounting authority; losing vectors may require explicitly authorized regeneration.

Titles/headings are editable labels; immutable IDs carry identity. Companion path
links help navigation while typed references authenticate IDs, kinds, source/revision
ownership, byte spans and hashes. Homonyms must not merge because their labels match.
Portable paths use full Unicode case folding while IDs remain case-sensitive.

## Guarded mutation and recovery

Exact old-file hashes and read dependencies preserve intervening author edits.
Preparation retains intended operations without granting application authority.
Validation and catalog publication are sealed together, including retained replay,
recovery and inverse authority. No permissive validator or no-op publication backend
is a safe shortcut for ordinary-note editing.

A literal edit admits one existing ordinary target, an exact expected hash and no
new canonical identity/property claims. Valid Pages use Page authority. Unknown
non-wiki properties and untouched BOM/CRLF/prose survive byte edits. Damaged canonical
envelopes require known-byte restoration, not guessed identity repair. A general
repair registry is unnecessary until actual public inputs demonstrate a failure
that stronger input/allocation invariants cannot prevent.

Initialization preflights every reserved name before writer acquisition or directory
creation. It refuses existing hidden state, files, even empty managed directories and
portable aliases; deliberate existing-folder intent permits ordinary author material
and editor settings. A new wiki is prepared in a unique sibling folder and published
without replacing an existing destination. In an existing author folder, a complete,
synced marker is published before working directories, so ordinary commands can
finish creating those directories after interruption. Failed setup reports retained
paths and can be retried with the same choices. Unknown temporary files are preserved.
Init and default registration remain separate durable outcomes; an I/O failure does
not imply whole-tree rollback.

## Selection and configuration authority

Resolve one canonical root/ID/origin context per command. Explicit and nearest roots
win over one user fallback; invalid chosen targets never fall through. Pin the selected
identity on mutations and the exact file hash independently. Do not introduce an identity
registry or automatic adoption layer when a small immutable context suffices.

Load one bounded private preferences snapshot. Explicit preferences replace ambient
lookup, and a bypass disables ambient configuration. Defaults pin absolute root and ID;
vault settings remain keyed by ID. Preferences cannot authorize endpoints, credentials,
TLS changes or executable helpers. Guarded configuration replacement is separate from
vault mutation because HOME must never be treated as a wiki.

## Measured retrieval and scale boundaries

A relevant source can reach retrieval yet lose necessary conditions during passage
selection or final packing. Repeated sparse/packing/query-signal tuning produced small
gains and regressions in representative development tasks; those rejected experiments
did not justify promoting a new default. Diagnose information loss at each pipeline
stage with returned bytes, then run a bounded distinguishing experiment before adding
another heuristic. After two unsuccessful whole-task interventions, obtain a fresh
architecture review.

Selected verification authenticates chosen evidence against a published discovery
generation. It does not claim all current files are indexed or prove an absent answer.
Separate explicit synchronization, strict global reconciliation and selected dependency
proof. Bounded host selection or complete-original reading can improve an explicit
workflow, but includes additional input/calls/latency and cannot qualify the one-command
native default.

Grouped source import/refresh and affected-row publication avoid repeating full work
for each member while retaining individual immutable histories. Small publication facts
and policy-bound embedding descriptors reduce unnecessary history/material rendering.
Unchanged full scans, compatible-vector scans, history enumeration, reconstruction and
mixed-workflow latency remain measurable scaling costs. Do not infer supported capacity
from tiny fixtures or unoptimized diagnostics. Current target: 25,000 documents/about
2.5 GB; 100,000/about 10 GB is deferred. Neither is an established shipping limit.

## Distribution and support

The owner selected MIT, GitHub Issues support and unsigned macOS distribution without
notarization. No trust bypass or publication follows from those choices. Verify final
archive/executable identities, normal OS controls, final-byte native workflows and
post-publication downloads before advertising an installer or supported release.

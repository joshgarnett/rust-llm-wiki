# P05 integrated review

Status: findings resolved and root integration gate passed; accepted tree ready for local commit. Requested Astra creation and restart failed with the runtime agent thread limit. The playbook fallback used independent Sol cross-reviews of disjoint components, plus a separate root review of storage/publication invariants. No review gate was waived.

[P05-eligibility-review.md](P05-eligibility-review.md) covers malformed/future duplicate IDs, typed companions, full source/evidence/decision closure, cycles, entity identity/description and whole-overlay validation. [P05-SQL-review.md](P05-SQL-review.md) covers held permits, current-read recovery guards, pinned WAL readers, both FTS rowsets, migration, typed rows and logical-cache integrity. Both reviewers returned their leases; final reviewed hashes and actual execution attribution are retained in their reports.

Resolved findings:

- Malformed YAML copies reserve every safely readable root ID through the existing bounded parser, including comments, quoted/spaced keys and block scalar continuations. No conflicting path becomes the winner. The same adapter detects invalid managed-envelope intent.
- Evidence successors retain assertion/source identity; same-revision successors preserve the verified span/hash. New-revision revalidation remains allowed by the accepted P04 contract. Empty accept/reject decisions are invalid.
- Conflicts cover explicit input/output IDs; invalid superseders have no authority. Mechanical outcomes use canonical fields, never rationale interpretation. Exact mutation/import/review operation contracts still belong to later P09–P13, which remain pending.
- Unsupported/stale entity descriptions are omitted from graph description ranking while active identity names/aliases remain available. Assertion endpoints render resolved names/aliases in subject/object order; immutable IDs and qualifier/direction fields remain canonical.
- Same-version rebuild preserves the old published view on precommit failure. Old/building ordinary generations are reclaimed on successful publication and idempotent reuse after canonical reversion; held readers retain their WAL snapshot.
- Logical SQL cell drift is checked against a regenerated in-memory canonical projection before snapshots or idempotent reuse. Explicit rebuild repairs derived cells without canonical mutation. This checks projected rows, not arbitrary hostile SQLite shadow-table internals.
- Cache migration resets schema and publishes the new generation in one transaction. Retained vector-loss/uncertainty notices commit with the index and survive a stopped/failed return. No vectors are regenerated. Invalid u64 evidence spans outside SQLite's signed range remain exact in canonical JSON/raw text and diagnosed, with null scalar columns.

Root targeted gate: 31 parent tests (14 scan, 15 generations, two enabled process-kill wrappers). Native wrappers explicitly execute the ignored child at 11 boundaries: five publication and six migration. Precommit migration faults preserve schema/version/vector rows; postcommit retries surface retained loss notices. All-target Clippy and formatting pass. The broad integration gate passed 127 parent tests, including 250 native filesystem boundaries × before/after = 500 fault cases; [P05-checks.json](P05-checks.json) records results against exact source hashes.

D27 retains ordinary SQLite read-only WAL lock/sidecar semantics. Strict CLI dry-run must bypass SQLite opens and use canonical scans/planners with unknown cache facts; P06 must prove unchanged vault/cache/journal trees. No immutable mode shortcut is used for a concurrently published cache.

No test claims power-loss safety, other native operating systems, live providers, host compatibility, semantic entailment or full M4 completion. Root integration/build/format/lint/seed checks passed; local P05 commit completes M0. The complete authorized run continues afterward.

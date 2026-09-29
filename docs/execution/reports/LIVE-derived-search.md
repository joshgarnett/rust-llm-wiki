# Derived artifact search follow-up

## Trigger and red evidence

An API source extraction retained its source passage in three places: the captured revision, `knowledge/extractions/extraction_*.md`, and `runs/run_api_*/outputs/run_event_generation_*.md`. Before the fix, a real mock-provider API extraction made all three searchable as current text. The new `api_extraction_and_generation_output_never_supply_current_search` regression failed on the unchanged source with three default literal hits instead of the one captured-source hit (`.artifacts/live-derived-red.log`, `//:retrieval_lexical_test`, 17 pass/1 fail). The first attempted red run stopped at an unrelated fixture import wiring error; adding the existing `library` alias resolved that compile issue without changing production source.

## Implementation

- Catalog eligibility treats an extraction summary as operational, using its validated source/revision set to report `Unsupported`, `Historical`, or `Withdrawn`. A generation output gets source lifecycle status only when its bounded retained body binds to an adopted packet by ID and fingerprint, run ID, task key, and response hash. Unbound output is `Unsupported`; no run ID alone is treated as source evidence. Other typed run, run-event, and change records are operational.
- Scan recognizes readable operational `wiki_kind` declarations even when malformed metadata or a duplicate ID prevents adoption. Their normalized body is empty for FTS/embedding, while raw text remains available for audit. The parser fingerprint advances for this projection change; `index_snapshot` already refuses a prior fingerprint until sync/rebuild.
- Default search SQL excludes these operational kinds and managed artifact paths before candidate caps. An explicit historical search retains literal audit access. The path guard protects malformed managed artifacts and older compatible rows. Ordinary invalid author notes outside those declared/managed forms remain discoverable.

## Validation

Root's focused Bazel gate passed `//:catalog_scan_eligibility_test` (14 cases, 0 failures, 0.9 seconds reported by Bazel) and `//:retrieval_lexical_test` (19 cases, 0 failures, 8.3 seconds reported by Bazel). Case counts come from the corresponding `bazel-testlogs/*/test.log`; the grouped invocation is recorded in `.artifacts/agent-research-focused.log`. Focused tests cover real mock API output and imported summary before/after source withdrawal, historical state, embedding corpus exclusion, copied valid artifacts with duplicate IDs, malformed copied and managed artifacts, and ordinary invalid-note discovery. No live provider call or credential is used. The API fixture uses a synthetic `Ada` passage; the separate malformed-artifact fixture uses `CEDAR-731`.

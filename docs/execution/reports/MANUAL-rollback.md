# Manual source-add rollback follow-up

The user's `source add` rollback reached graph validation and failed with a generic `RECORD_INVALID`: the inverse deletes the mutable source note while retaining the immutable revision tree, so its revision records would lose their source reference. This rejection is correct; `source withdraw` retains the history and removes its current support.

`src/changes/rollback.rs` now recognizes that exact source-deletion/retained-revision combination after the existing read-only `self.plan` preflight. It returns `RECORD_INVALID` with a `source withdraw <id> --reason ...` hint before staging. The source note recheck consumes the existing aggregate inverse read budget and returns `CONTENT_CONFLICT` if its bytes changed after planning. Graph validation and immutable-asset rules are unchanged.

Disposable tests in `tests/offline_application.rs` cover the hint, unchanged source/revision bytes after rejection, successful withdrawal with revision bytes retained, and edited-source rollback still returning `CONTENT_CONFLICT`. Root captured the pre-fix failure in `.artifacts/manual-rollback-red.log`: `source_add_rollback_explains_retained_history_and_withdraws_safely` failed because the generic error lacked the hint. This worker ran `rustfmt` and `git diff --check`; no Cargo/Bazel tests were run by this worker. Root owns post-fix results.

Read-only triage of the separate page-put/rename report found that the literal `missing retained manifest` error originates in `load_manifest_with_budget`, before inverse path checks. Without the earlier temporary vault or exact change ID, that message does not establish a path-conflict cause. No change to that workflow was made.

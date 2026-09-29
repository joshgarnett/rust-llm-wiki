# P20 existing-ledger extraction adapter

Status: implementation awaiting targeted Cargo lease/verification.

Public API: `graph::research_extract::plan_task(run_id, verified_packet, trusted_service, max_output_tokens, priority, dependencies) -> GenerationTaskPlan`; `retain_input(fs, writer, plan)`; `OfflineApp::execute_existing_task(ledger, task, verified_packet, trusted_service, dispatcher, new_extraction) -> ApiExtractionOutcome`.

The caller starts/resumes/completes the containing run. Fresh extraction requires Running. Retained output repair uses the same admitted task and original attempts. The adapter does not create a run, alter lifetime limits, or complete the containing run. It stages the existing source-local importer changes and leaves review/resolution/acceptance explicit. Descriptor identity includes its durable `runs/<run>/inputs/extraction_<hash>.json` path, canonical generation data, schema/prompt/model/settings/source proofs. Retention uses bounded expected-state immutable storage; outputs and receipts already live under `runs/<run>/outputs` and `events`.

P18 now calls a shared task executor plus shared receipt/acknowledgement helpers, with standalone completion enabled only for its original whole-run API. Current-source proof is reloaded immediately before publication; local integrity/freshness failures preserve paid responses instead of issuing rejected schema receipts. Research validates ledger vault, exact admission/current active membership, canonical descriptor, trusted service and current packet; app dry/offline policy remains authoritative.

Seven new tests cover containing accounting/state, five paid response/receipt/settlement boundaries followed by descriptor loss/restoration and zero resend, pending/completed cache deletion, staged changeset availability, changed-source rejection (including an in-flight source change), prior explicit resolution/accept/reject decisions, and app dry/offline precedence.

Checks completed: leaf `rustfmt --edition 2024 --config skip_children=true` on three Rust implementation leaves and the new test. Cargo checks not yet run (root serializes shared-target leases). All tests use disposable fixtures/mock dispatchers; no live-provider or other-platform claim.

Root integration closure: current 7/7 cases passed in the frozen P20 gate; P20-checks.json records exact source, command and log identity. Worker-only checks above remain historical.

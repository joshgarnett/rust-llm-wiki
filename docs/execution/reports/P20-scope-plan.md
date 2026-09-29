# P20 pure scope/task planning

Status: implemented, formatted; targeted tests not yet run. Shared Cargo target lease remains with other workers/root.

Owned paths: `src/research/plan.rs`, `tests/research_planning.rs`, this report. Root owns module exports/shared DTOs/Cargo/schema/CLI.

Implemented pure `validate_scope`, `plan`, `generation_task`, `search_task`, and `capture_task`. They perform no filesystem, authorization-helper, retrieval/index, ID-allocation or dispatch operations. Caller-supplied trusted service summaries provide production wire fingerprints without live reauthorization. Scope validates current local literal/lexical retrieval, positive bounded research/search/token ceilings, lower schema/fetch/extraction bounds, public URL syntax, controls, exclusions and normalized duplicate URLs while preserving distinct query strings.

Initial immutable genesis retains exact canonical scope bytes at `runs/<run>/inputs/scope.json`, zero binding epoch, source/read/service proofs, caller's original lifetime allowances/deadline, and empty legacy profile map. It creates only local InspectExisting and its dependent round-one PlanFrontier task. Task descriptors live at `runs/<run>/inputs/<inputhash>.json`, use production RemoteInput/wire fingerprints and canonical task keys, and retain exact passage plus parent-output hash dependencies. No task creation grants frontier/source admission authority.

Strict public `ResearchInspectInput` and `ResearchStageBinding` bind scope, round/stage, citation references, current records and retained parent outputs. Generation schemas come directly from frontier/gaps/synthesis. Instructions treat sources as untrusted, prohibit limit/exclusion/apply changes and accepted truth, and keep derived assertions unassessed. Extraction remains the separate existing-ledger adapter.

Eight authored tests exercise full fixture tree/mtime equality for pure planning, genesis lifetime preservation, durable descriptors/exact wire/schema/key identity, one profile serving Generate/Search, wrong generation proofs, explicit URLs without Search, source/parent dependency retention, bad quote/hash/source proof, controls/exclusions/public URLs/query identity, structural/size ceilings and binding validation. Fixture setup uses disposable vault/private configuration; no credentials, mock dispatch, network calls or production vaults.

Actual check: `rustfmt --edition 2024 src/research/plan.rs tests/research_planning.rs` exited 0. Cargo targeted tests await serialized lease; no runtime correctness or live-provider qualification claimed.

Root integration closure: current 8/8 cases passed in the frozen P20 gate; P20-checks.json records exact source, command and log identity. Worker-only checks above remain historical.

# Independent provider and accounting audit

Date: 2026-09-29. Astra read-only follow-up at checkout `63b2439f5cd57211e2a2bc5587ac5bace3d03ea2`, using packaged `.artifacts/012-macos/lwiki` 0.1.2 for reproductions. This worker read the current provider contract and user guide, then traced private configuration, credentials, authenticated transport, wire validation/history, admission/replay, and embedding/API/probe application boundaries. It did not restore or propose restoring built-in research.

Three additional recovery defects were reproduced, plus confirmation of root's diagnostic-loss finding. The observed failures preserve accounting and refuse new sends; no overspend, secret disclosure, duplicate automatic send, or fabricated valid output was demonstrated.

The separate storage finding **S2 is High** after the graph worker's cross-module reproduction: invalid duplicate source identity can still permit one API generation request, followed by a catalog panic. See `AUDIT-storage.md`. This worker inspected `/private/tmp/lwiki-s2-api-iadmx46f/vault` read-only: `run_s2_audit` has `received` and `reconciled` events and a surviving exact response spool, but no output commit or settlement. The reconciliation retains unknown billable classes/cost and observed 6627 request bytes/489 response bytes. The failure is premature dispatch and broken materialization, not demonstrated loss/refund of accounting. No repeat send was attempted by this worker.

## P1 — High: a rejected embedding probe blocks subsequent sync for the same space

**Locations:** `src/app/embeddings.rs:925`, `src/app/embeddings.rs:936`, `src/app/embeddings.rs:429`, `src/app/embeddings.rs:747`; underlying receipt validation at `src/app/embeddings.rs:887`.

A paid `embeddings check --probe` response rejected by the wire validator is correctly committed and settled with `output_disposition: rejected`. On a later invocation, `dispatch_embedding_task` treats a settled/output-committed attempt with empty cache outputs as reusable whenever `probe` is true. It then demands a *validated* receipt. The rejected receipt necessarily fails that test. The run marker is not retired because the retirement condition excludes probes.

This is not limited to repeating the probe: `embeddings sync` first traverses unfinished same-space markers in `recover_embedding_jobs`, recognizes the old marker as `embeddings_check`, and enters the same impossible validated-receipt path. A failed diagnostic probe therefore poisons ordinary synchronization for the unchanged space.

**Native reproduction:** local mock returns HTTP 200, correctly shaped vectors, and malformed usage (`prompt_tokens: "bad"`). `embeddings check --probe` sends exactly one request and returns `PROVIDER_RESPONSE`. Change the mock to a valid response and explicitly rerun the probe: it returns `RECOVERY_REQUIRED`, `vector cache references lack exact settled validated receipt`, with zero requests. `embeddings sync` on that vault also returns the same error with `network_used:false`. No cache or operational files were deleted to force the result.

**Contract:** `providers-jobs.md`, embedding wire and job persistence sections: invalid output is accounted for and remains pending; validated output can be reused, unsuccessful output cannot be presented as completed. An explicitly requested future sync must not be permanently disabled by a failed compatibility probe.

**Suggested work:** inspect the receipt disposition before taking the successful-probe reuse path. Treat accounted rejected probes as failed/retired work; allow an explicit new probe or normal sync without losing their immutable receipt or unknown-charge hold. Preserve no-automatic-resend behavior. Add an end-to-end failed-probe → corrected-provider → probe/sync regression, including crash/replay of the rejected receipt.

## P2 — Medium: explicit larger deadlines/limits cannot amend retained provider jobs

**Locations:** `src/app/embeddings.rs:763`, `src/graph/api_extract.rs:322`, `src/jobs/ledger.rs:248`, `src/cli/remote.rs:13`.

The ledger still implements `resume_with_requested_limits`, but it has no production call sites. Embedding and API-extraction resumes call `resume(None)`. Existing-run branches do not apply the caller's newly supplied limits or deadline. There is no provider-job amendment CLI after the built-in research removal. The defect is missing provider lifecycle wiring, not a reason to restore the old research executor.

**Native reproduction:** run `embeddings sync --deadline-ms 1` on a disposable source and trusted loopback service. It creates a genesis/marker, then returns `BUDGET_EXCEEDED` before HTTP. Immediately retry the identical sync with `--deadline-ms 900000 --max-requests 100`. It returns the same error, `clock/deadline or invalidated budget blocks restart`, for the exact same run ID; zero HTTP requests occurred in either invocation. The retained run has only genesis, so this case requires neither a failed provider nor unknown billing. The marker forces reuse of the expired run and the command exposes no run selector or explicit new-run override for embeddings.

**Static extension:** existing API-extraction runs similarly ignore `ApiExtractionRequest.limits` and the requested deadline, and resume paused work without an amendment. API extraction at least exposes a different explicit run ID as a workaround; that starts distinct accounting rather than amending the old run. This API-extraction variant was not independently sent to a mock in this pass.

**Contract:** provider jobs are bounded, resumable work retaining lifetime accounting; explicit changes must preserve/raise cumulative ceilings instead of resetting or silently ignoring them. The existing ledger amendment API provides the required checked mechanism. The public remote flags describe caller-selected lifetime ceilings/deadline, but their new values presently do not affect retained jobs.

**Suggested work:** expose a concrete provider-job resume/amendment interface, or explicitly bind changed remote flags to a retained-run amendment after validating monotonic limits, deadlines, input/configuration identity, and accounting completeness. Handle expired `planned` genesis too: current amendment accepts paused/stopped states only. Do not reset consumed slots or unknown money holds. Test deadline exhaustion before first send, exhaustion after a paid attempt, explicit raised limits, rejected lower limits, and unchanged flags preserving the old budget.

## P3 — Medium: `--retry-uncertain` cannot resume an uncertain embedding send

**Locations:** `src/app/embeddings.rs:475`, `src/jobs/ledger.rs:289`, `src/cli/remote.rs:51`.

Embedding recovery unconditionally rejects an attempt whose remote exposure is `PossiblyInFlight`, before consulting the caller's explicit retry-uncertain policy. The CLI advertises `--retry-uncertain` as opting into uncertain retries under retained accounting. Dispatcher retry logic supports this within an active invocation, but the ordinary resumed embedding path never reaches it. Paused ledger resume also requires reconciliation of remote in-flight work, and no corresponding provider reconciliation command is exposed.

**Native reproduction:** the loopback mock reads the complete embedding POST then closes the connection without a response. Initial sync sends one request, returns `PROVIDER_UNAVAILABLE`, and durably records `outcome_unknown` followed by a run transition. Correct the mock and explicitly run `embeddings sync --retry-uncertain`: it makes zero requests and returns `RECOVERY_REQUIRED`, `prior embedding send outcome remains unknown; no repeat send authorized`. The missing authority is not user consent: that invocation supplied the advertised opt-in.

**Impact:** a transient ambiguous network failure strands same-space embedding work despite explicit retry authorization. The refusal correctly avoids an unauthorized duplicate and retains the prior unknown attempt; the operational recovery capability is missing.

**Suggested work:** provide an explicit retained-accounting retry/reconciliation path that honors the opt-in while preserving the old unknown charge and applying concurrency, elapsed remote-exposure, attempts, lifetime budget, and source/configuration checks. Do not simply remove the guard or refund the original attempt. Test process restart before/after possible send, both opt-in states, zero versus partially observed response bytes, timeout of remote exposure, and a final remaining request slot.

## P4 — Medium, shared with root: embedding errors erase safe diagnostic reason codes

**Locations:** `src/app/embeddings.rs:988`, `src/app/embeddings.rs:1002`.

Both fresh-dispatch and retained-decode failure branches replace `WikiError.details` with run/attempt/spool context, dropping the allowlisted `reason` established by the wire layer. The same malformed usage response yields `usage_invalid` through `doctor --probe`, but not through `embeddings check --probe` or `embeddings sync`. Root owns the primary reproduction and synthesis of this issue: `.artifacts/audit-provider-diagnostics.json` and `.artifacts/audit_provider_diagnostics.py`; this worker independently reproduced the missing reason in P1's first request.

**Contract:** `docs/providers.md` and `providers-jobs.md` promise fixed reason codes without copying provider error bodies. Nest or merge the original safe details under `cause`/`context`, while preserving retained change IDs as well as run/attempt information.

**Accounting clarification:** dropping the `DispatchFailure.materialization` field at this application boundary does **not**, by itself, lose settlement for an ordinary malformed successful response. `src/providers/dispatcher.rs:704` calls `commit_receipt`, whose implementation at line 930 publishes, acknowledges, and settles before returning the failure. The native P1 journal contains `received`, `reconciled`, `outputs_committed`, and `settled`; settlement retains `billing: unknown_reserved` and unknown input/cost, rather than inventing zero. Partially received transport failures and receipt-write failures have different replay paths and were not exhaustively fault-injected in this audit.

## Evidence

Worker reproduction script: `/private/tmp/lwiki-provider-audit.py`. Native invocation log: `/private/tmp/lwiki-provider-audit-5_yi654q/results.json`. Parsed read-only journal events: `journal-events.json` beside it; poisoned-sync envelope: `poisoned-sync.json`. The script used separate disposable vaults for deadline, rejected probe, and uncertain send; private TOML files contained only a synthetic key and an explicitly allowed `127.0.0.1` endpoint. The initial sandbox denied binding a listener; the normal sandbox escalation approved the same local-only script, which completed successfully. No external endpoints, real credentials, production vaults, helper commands, or provider bills were involved.

Exact request observations: expired deadline first/retry = 0/0; rejected probe first/retry = 1/0; uncertain send first/opt-in retry = 1/0. The poisoned sync reports no network use. Read-only journal inspection establishes settlement for the rejected probe and preservation of the unknown attempt for the interrupted connection. It does not measure actual provider billing.

## Audited mechanisms and limits

Selected source review covered:

- Private provider-file size/permission checks, root-plus-vault binding, explicit capability/profile selection, endpoint/TLS/query/header validation, configuration rechecks, and secret-free service summaries.
- Offline/dry-run gates before credential resolution, direct argv helper launch, closed stdin/discarded stderr, bounded output/deadline/TTL, refresh serialization and one-command-auth refresh after 401.
- Dispatcher reservation and intent before credential/transport entry, last-moment authority checks, redirect/proxy/automatic retry suppression, bounded headers/body, response spooling, invalid-response settlement, timeout/uncertain handling, and retry admission.
- Responses versus Chat wire construction, schema/local-validator separation, refusal/incomplete/tool-output rejection, exact indexed embedding batches, usage partitions/unknown extensions, and unknown-cost handling.
- Retained codec snapshots bound to original task/input/schema/model/bound fingerprints; replay using historical codecs rather than silently rebinding paid responses to changed configuration.
- Fixed-point checked money, applicable-rate completeness, request/byte/token/concurrency admission, complete-history journals, cumulative allowances/unknown holds, terminal receipt acknowledgement, and no budget reconstruction from Markdown alone.
- Embedding corpus/query/probe application paths, same-space marker recovery, settled cache proof, API extraction retained generation/import seam, and explicit provider probes. Local research's separation from paid-provider dispatch was preserved.

No additional concrete secret leak, silent live-endpoint fallback, automatic schema-repair request, missing-usage-to-zero conversion, or automatic duplicate send was established in the reviewed code. This is not exhaustive proof of absence. The review was concentrated on consequential cross-layer invariants and reproduced application paths, not a line-by-line certification of every provider module or platform branch.

Root alone runs the full Rust/Bazel suite and owns its results. This worker started no build/test job, changed no product source/test, and wrote only its leased audit reports plus disposable scratch evidence. Native helper descendant cleanup, Windows ACL/runtime behavior, TLS/redirect edge cases, budget races, kill matrices, and all wire combinations were not freshly executed by this worker. No live Responses/Chat/gateway interoperability, returned model quality, real billing, or cross-platform durability claim follows from these mocks. Windows write refusal remains the known deliberate external limitation.

## Live-report follow-up: P5–P8

The user supplied further 0.1.2 live-gateway observations during this audit. Those observations are owner-reported; this worker made no live call. Bounded local reproductions and source checks below separate confirmed product behavior from unknown gateway causes. Script: `/private/tmp/lwiki-provider-followup.py`; final native evidence: `/private/tmp/lwiki-provider-followup-sytlokuj/results.json`. Earlier run `/private/tmp/lwiki-provider-followup-2iy2whfs` retains the rejected receipt before a successful explicit retry; its journal was inspected read-only. Both runs used disposable bootstrap copies, one synthetic credential, and a sandbox-approved loopback mock.

### P5 — Medium: generation output limit is rejected late, opaquely, and not qualified by dry-run

`src/providers/generation_wire.rs:333` imposes `service.max_output_tokens.unwrap_or(4096)`. A CLI `--max-output-tokens` above that effective service ceiling is rejected with `WikiError::invalid("generation options invalid")`; dispatcher sanitization removes the useful cause. The normal API path has already retained the packet/task/run and transitioned it to running before pure wire preparation reaches this check (`src/graph/api_extract.rs:151–204,318–340`). No request is sent, but the run remains running with pending work that cannot execute under that configuration.

Native reproduction with no service `max_output_tokens`: API dry-run at 8192 exits 0; actual API extraction at 8192 exits 9, `RECORD_INVALID`, `bounded provider operation failed`, no reason, zero requests. The retained journal contains only genesis and planned→running. This matches the owner's N1 default4096/4097+ observation; the worker specifically exercised 8192.

Dry-run is deliberately a **local packet preview**, routed through agent export at `src/cli/dispatch.rs:497–505`; it does not load provider configuration and must remain usable without credentials/configuration. Its success is not proof of provider-limit validity. The defect is late/opaque validation and absent disclosure, not that dry-run should covertly resolve secrets or a provider. Normal execution already has an explicitly authorized nonsecret `TrustedService`, so validate its pure output/schema/request options before job allocation/start. Return a safe typed diagnostic naming requested and effective maxima and the private service setting to adjust. Dry-run should disclose that provider-specific limits/schema compatibility remain unchecked. Document the local4096 default; do not infer that the gateway itself has that limit.

### P6 — Medium: correct accounting retry refusal hides the usable recovery action

After a syntactically valid provider response fails extraction quotation validation, the application settles a rejected receipt and may keep billing `UnknownReserved`. `src/jobs/ledger.rs:700–727` then correctly requires explicit retry policy before another allowance: its original message is `uncertain retry requires explicit retry policy and new reservation`. Dispatcher redaction reduces this to opaque `RECOVERY_REQUIRED`, `bounded provider operation failed`, with no hint. This is a distinct case from P3: resumed **API extraction can honor** `--retry-uncertain`; P3 concerns embedding recovery refusing before that policy is consulted.

Native reproduction: the mock returned a schema-valid mention quoting `NOT IN THIS SOURCE`. First default-run extraction sent one request and returned `EXTRACTION_INVALID`. An identical retry and a retry changing only `--max-requests 100` both sent zero requests and returned opaque `RECOVERY_REQUIRED` for the same run ID. After correcting the mock response, repeating with **`--retry-uncertain` succeeded in that same run**, sending one new request. Accounting retains the earlier unknown charge; no reset/refund is required. Therefore the owner's N3 is confirmed as poor recovery diagnostics, **not** a demonstrated permanently stuck terminal API job.

The default API run identity (`src/graph/api_extract.rs:52–78`) contains packet/dependencies/service fingerprints and `max_output_tokens`, but not `max_requests`. The owner's observation that a run ID changed alongside a request-ceiling change cannot be attributed to that flag alone; the isolated local check kept the ID unchanged. Preserve a fixed safe reason such as `uncertain_retry_requires_opt_in`, explain possible prior billing, and give a concrete same-run retry action without automatically resending. Ordinary `recover` concerns changesets, so an empty recovery result does not resolve this accounting choice.

### P7 — Medium: semantic extraction rejection is not durably diagnosed and its raw output is discarded

`src/graph/api_extract.rs:360–381` handles semantic importer rejection only after successful transport/schema decoding. It creates a rejected receipt with no output records; `src/jobs/checkpoint.rs:462` copies `failure_code` from the successful wire metadata, which is null. `commit_generation_task` then unconditionally removes the sensitive response spool after settlement (`src/graph/api_extract.rs:589`). The canonical receipt records paid rejection but not its semantic cause or rejected text.

The local quotation-failure reproduction confirms `output_disposition: rejected`, `failure_code: null`, followed by `spool_removed` and a canonical checkpoint. No generated output or assertion was accepted. The failure cause is available only in the first CLI envelope, so restarting loses the ability to inspect which paid response failed and why. The graph reviewer owns the separate missing item/window-specific quote diagnostic; this finding concerns durable rejection provenance and response availability.

Add a bounded durable semantic rejection outcome with a fixed reason and safe item/window identifiers, distinct from immutable transport/accounting metadata. Retain the rejected model response privately for an explicit inspection/repair workflow under documented size/access/retention limits; never make it canonical accepted evidence. Do not rewrite an old receipt or sealed response metadata merely to insert a later importer error. Keep the unknown billing hold and require separate explicit authorization/budget for another model attempt. Tests should reopen after rejection and verify inspectable reason, no promoted facts, preserved accounting, and explicit cleanup behavior.

### P8 — Product observability enhancement: bounded private HTTP error-body retention

The owner reports a successful Haiku Responses/json-schema probe but HTTP400 for extraction, with379 observed response bytes and an empty `response.bin`. `src/providers/dispatcher.rs:642–646` intentionally replaces all non-2xx or malformed transport bodies with empty bytes before spooling, while recording observed usage/byte metadata and a safe failure code such as `http_400`. This matches the existing `providers-jobs.md` contract to reduce error bodies to safe codes. Empty stored bytes do **not** establish that the gateway returned no body, that bytes were unaccounted, or that schema projection caused the rejection.

A successful small probe does not demonstrate acceptance of the much larger extraction schema/request. Gateway schema support is a plausible hypothesis only; request/model/output-limit policy and other gateway validation remain possible. This audit did not diagnose or reproduce the real gateway's400 cause.

The owner's request for private bounded debug retention is a reasonable explicit contract enhancement. Define an opt-in protected diagnostic artifact for bounded error-body bytes plus safe endpoint/model/request fingerprints, with strict access permissions, truncation labeling, retention/cleanup controls and explicit inspection. Keep raw provider bodies out of normal JSON/human errors, canonical notes, public reports, and logs; providers can echo sensitive inputs. Retain safe status/reason and observed byte accounting regardless of debug mode. Do not add speculative fallback requests, automatic schema repair, or a live-provider compatibility claim. A local HTTP400 mock can verify retention/redaction mechanically; actual gateway diagnosis needs the owner's authorized real response evidence.

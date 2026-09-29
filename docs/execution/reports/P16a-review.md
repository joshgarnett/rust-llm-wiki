# P16.a independent trust and credential review

Independent Sol fallback because Astra is unavailable at the runtime thread limit; root retains separate source/runtime acceptance. Accepted baseline P15 `44f3da7368e52e070abc37a2871eb2af8da9fb8d`. Read-only review initially covered config/providers.rs, providers/credentials.rs, public/private trust tests, P16 report, selected P16 contracts and D39. Root then explicitly authorized the narrow credential corrections below; those fixes are **reviewer implementation**, requiring independent root source/runtime approval. No Cargo, actual secrets, helpers, network, Git or nested delegation were used by this reviewer.

Status: initial source findings addressed in candidates; updated runtime/root approval pending. No remaining concrete P16.a source blocker found in this bounded review.

## Pending P16.b / P15 retry admission question

Read-only inspection finds jobs/ledger.rs admission classifies every UnknownReserved predecessor as uncertain. The transport contract permits a new reservation after a retryable terminal HTTP response while retaining unknown prior billing. Prefer a narrow exception requiring TerminalConfirmed plus loaded hash-bound received_meta with terminal_response=true and status 401/429/500/502/503/504. TerminalConfirmed alone is insufficient: public reconcile can mark arbitrary possible-send attempts terminal without received HTTP status evidence. The dispatcher still owns one command401 refresh, status policy, backoff and committing/verifying the paid failure receipt before scheduling another attempt.

Existing replay accounting already retains unknown allowance while releasing terminal concurrency; leave that arithmetic and locked new reservation checks unchanged. Admission examines every prior attempt, so preserving metadata only until the next reservation fails retry3 after cleanup of retry1. Retain prior unknown retry-status spools until the task no longer needs retry authority, or add authenticated bounded durable status evidence. Current UsageReceipt and Reconciled omit status; missing metadata cannot be inferred from either.

Required focused checks: restarted429/503 unknown-billing retries retain old/new allowances; mixed ambiguous predecessors still block; manual terminal reconciliation and malformed200/400/403 do not gain default retry; missing/tampered metadata refuses; retry3 retains proof for retry1; competing processes cannot acquire the final reservation twice. This is a source-policy recommendation only: no jobs source changes or tests run, and no P16.b runtime closure claimed.

## R1 — fresh command token could expire on monotonic time before its first lease

Original fresh resolution records completed-helper acquisition, then resolves additional secret headers. Its final check considered only UTC expiry, while cached resolution also checked monotonic elapsed time. An injected extra-header input advancing monotonic time beyond TTL with frozen UTC could return an already expired fresh lease/cache entry.

Authorized correction: enforce both UTC expiry and elapsed monotonic validity before cache installation; final policy sample follows trust recheck and epoch mutex work. Static and cached return paths likewise sample closing policy after trust recheck. Refused resolution installs no new token cache entry. The added deterministic extra-header fixture advances monotonic time by TTL+1 without UTC movement; additional cases cross the UTC deadline or regress monotonic time and assert no cached token installation. No test has yet run on this correction.

## R2 — private input permissions were checked only before opening

Root and reviewer agreed inode/device continuity does not imply Unix mode continuity. Config/key permissions or protected CA mode can become unsafe on the same inode during opening/reading. Root factored protected_permissions and applies it to initial, opened and final metadata, while retaining regular-file/size/symlink/inode checks. Reviewer re-read the changed function and source-closes this finding conditional on root's runtime checks. No stronger ancestor-symlink rule was invented; the current contract is an explicit absolute bounded regular final file.

## P16.b handoff — opaque lease validity must survive durable send recording

Resolution-time validity alone cannot certify a later HTTP handoff. Root authorized extending the opaque lease with original acquired ClockReading, valid_for_ms and private auth-key association. The broker's crate-private validate_lease checks execution policy/regression, exact service association, current root/marker/config/CA trust, then samples closing policy and enforces UTC and monotonic expiry. Cached leases retain original acquisition/TTL; they cannot extend validity when leased later. Static leases have a monotonic duration bounded by the original invocation deadline. No public header getter, constructor, Debug/serde or secret hash is introduced.

Added post-resolve production-broker fixtures cover fixed-UTC monotonic expiry, UTC expiry/deadline, both clock regressions, changed config/marker trust, cached original validity, static monotonic deadline and cross-service lease refusal. P16.b must invoke this helper after durable begin_send and before transport, and classify a refusal without a transport call through its trusted NotSent accounting path. Actual .b dispatcher/retries/HTTP are not implemented by this packet.

## Other inspected boundaries

Strict bounded TOML rejects duplicate/unknown entries, capability mismatches and conflicting credential sources. Authority binds actual canonical root, valid marker ID/hash, explicit allowed profile, private configuration content and CA hash; portable/cloned IDs grant no authority. Full configured URL is retained; no path is appended. HTTPS is required except numeric explicit loopback HTTP; userinfo/fragments/backslash forms/known credential query fields refuse. Other secret query values cannot be detected perfectly and are forbidden by configuration contract. Header names and case-folded duplicates/managed fields are checked; additional secrets use environment/file references.

Public summaries export nonsecret endpoint/profile/config fingerprints; literal token bytes, helper argv and auth prefixes are omitted from those projections. Actual content and auth-cache guards remain private. Secret wrappers have no Debug/serialization; parser/source/runner errors redact values and details. Static sources resolve on each lease for rotation, while command tokens have bounded serialized process-local TTL/expiry cache and stale-epoch401 coalescing. Offline/dry/cancel/deadline checks precede credential reads/cache work and follow helper/waits; warm cache cannot bypass policy.403 handling and once-per-accounted-attempt401 retry belong to .b.

Unix helper uses direct explicit argv, closed stdin, private config cwd, discarded stderr, nonblocking bounded stdout and its owned process group. WNOWAIT retains PID ownership until descendant cleanup; timeout/cancel paths kill the group and bound reap waiting, with no unbounded reader thread. Helpers are trusted user code rather than a sandbox; their independent network/spend is not an observed dispatcher charge. Windows source uses explicit inheritable handle list, suspended creation, kill-on-close job assignment before resume, available-pipe polling and bounded cleanup. This is source inspection only; native Windows behavior/ACL/platform qualification is unproven. No native/cross-build claim follows.

TLS verification, private CA installation, disabled redirects/ambient proxies, exact HTTP URL execution, wire validation, final transport authority, Retry-After/uncertain retry accounting and provider compatibility remain .b/.c work. Config flags/fingerprints alone do not implement those mechanisms or prove hard dollars from DTO estimates.

## Runtime attribution and candidate hashes

Reviewer read the actual original logs without running tests: `/tmp/lwiki-p16a-public.log` has6 passed (3.96s compile/0.01s tests); `/tmp/lwiki-p16a-private.log` has15 passed (9.89s compile/1.27s tests). These include injected policy/refresh tests and four native macOS helper parents; they certify the **pre-correction** source only. Logs contain expected future-dispatch dead-code warnings and are not strict lint evidence. The corrected credential/permission and lease-validation cases require an isolated rerun/root approval.

Current correction candidate SHA256s:

```text
43e4c7189b9ee8bb44068e171ca852905cab72b00283e4016ad55b8312bef29b src/config/providers.rs
401a305446dce71f853c83483c3070832468dc13a8fcbe5c1a0fa045d07baf00 src/providers/credentials.rs
4570fab90bf10853b999d0b128395fe320545c96e300a0e7c50854b2368c3fde src/providers/credential_tests.rs
```

Only the two authorized credential leaves were edited/formatted by this reviewer, then source leases returned. Root owns config correction, future HTTP machinery, Cargo and all final integration/lint/acceptance.

## Root independent correction approval

Root independently read static/cached/fresh resolution and validate_lease: final policy follows trust checks, fresh cache installation follows BOTH clock expiry checks, cached leases preserve acquisition validity, and the private auth key binds service association. Initial/opened/final mode checks share the same predicate. Corrected actual18 private and6 public tests passed in the coherent .a-only checkout, logs /tmp/lwiki-p16a-{private,public}-corrected.log. R1/R2 and the lease API are locally source/runtime closed. This does not certify future HTTP/TLS/dispatcher accounting or native Windows and is not whole P16 acceptance.

Root final lease-clock wrapper gate: copied latest credentials.rs/credential_tests.rs into the165-package a-only checkout and ran cargo test --locked --offline --lib providers::credential_tests. Actual18/18 passed, compile4.58s/run1.27s (tool transcript; no standalone captured log for this last run). Existing post_resolve regression exercised lease-owned clock check_validity expiry/regression. One expected unconsumed later-provider configuration-field warning remains before b/c integration; no lint claim. No HTTP/live/other-platform qualification.

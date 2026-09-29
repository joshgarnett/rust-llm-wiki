# P16 final integration delta review

2026-09-28. Independent Astra bounded static review of root's final dispatcher boxing, not-entered branch folding, generation-schema/usage/cost condition folding, and private authority/deferred-method lint attributes. Compared the four scoped files directly with the previously reviewed isolated P16 candidate, then checked current hashes against root's combined snapshot `lwiki-p16-integration-n10pjymt`. Read the final closure in Windows-callers-review for the separately reviewed storage guards; did not repeat that audit. No source edits, Cargo, test execution, nested delegation, live provider access, or Windows execution. Only this report written.

**Disposition: no concrete semantic regression or omitted integration behavior found in the scoped deltas.** The previous paid-attempt retention, refund/retry, usage-breach, and private-authority source conclusions remain supported. Combined runtime acceptance remains root-owned and was still in progress at this review.

## Dispatcher error ownership and release

`src/providers/dispatcher.rs:41` and `:71` now return `Result<DispatchOutcome, Box<DispatchFailure>>`; `failure` at `:571` allocates the same fields with the same initial values. This changes the Rust return type/representation, not the contained failure meaning. Callers receive the same error, disposition, attempt, spool, materialization and retry values through the box. No new serialization or public authority constructor is introduced.

`RetainedPaid::finish` at `:543` takes and returns the owned box, mutating its contained failure. Its field-retention logic is unchanged: preserve an already populated current attempt; otherwise attach the last retained paid attempt/disposition; fill only absent spool/materialization fields; retain the safe paid-attempt identity list in error details. The outer async result still passes every resulting error through `retained.finish` at `:514`. Existing `retained.response(&out)` calls continue to borrow the boxed value through dereference rather than move or discard its payload. No new retry, receipt-commit, settlement, or refund branch was introduced by boxing.

The folded condition at `:272` remains left-to-right short-circuit evaluation:

```text
not_entered && observed_body_bytes == 0 && release_not_sent(...).is_ok()
```

The ledger release is called only after both transport observations match, just as with the previous nested `if`. Successful release returns the same NotSent failure with the same attempt. Failed release falls through to retained OutcomeUnknown handling; it does not imply a refund. Entered/observed-byte failures never reach that release call. Incomplete-byte spool retention and uncertain retry decisions are unchanged.

## Folded schema and usage conditions

`generation_wire.rs:185` and `:190` only combine each optional `$schema`/`format` lookup with its previous rejection predicate. Missing fields retain their prior behavior; present null/nonstring/unsupported values still refuse. No reference, schema, or decoder policy changed in this delta.

`wire_json.rs:324` combines the parsed-document and object-usage guards without moving any observation outside or inside a different condition. Model mismatch initializes the marker first; independently valid raw-total breach can replace it only with another breach; unsupported billable class likewise sets a breach. Missing partitions continue to produce Unknown counts. These predicates and marker order are unchanged from the scoped P16.c closure.

`wire_json.rs:362` preserves the exact conjunction for computing money: no violation, valid parsed usage, and a present rate card. The fold still requires every class to be Known and priced, applies checked class charges, and adds the request fee once. Any violation, missing partition/rate, malformed usage or arithmetic failure keeps computed cost Unknown. The unchanged dispatcher still persists the safe violation marker before refusing output. No new path clears a breach or restores the lifetime guarantee.

## Private provenance and storage boundary

The `src/jobs/types.rs` delta removes stale whole-type/trait dead-code expectations now that P16 consumes them, adds narrow expectations to retained opaque provenance fields, and identifies still-deferred reconcile/cleanup methods. The fields remain present and private to their original module scope. No authority type gains Clone, Deserialize, a public constructor, or a weaker clock/price check. The `SendAuthorization` owned clock, live floor comparison, cancellation, bound and deadline logic are byte-for-byte unchanged by this delta.

Windows-callers-review records source closure of private directory enumeration/cleanup guards, same-handle stage verification, creation-time private adapter routing, and the helper creation-error marker. Those findings are not reopened by dispatcher boxing. Its qualification limits remain: native ACL/sharing/reparse effects and crash behavior were not executed, and Windows directory durability remains Unsupported where required. This report does not convert that caller source review into runtime evidence.

## Candidate identity and validation limits

The following reviewed current files match root's combined snapshot byte-for-byte:

```text
src/providers/dispatcher.rs 6e2a65088149f062b09a9e9e465f3e754978fe0fdd8676c19679936424b1e975
src/providers/generation_wire.rs a042459053e229e74a072f903f77293a3021f632cfba74f5a94df872d1dcbb1b
src/providers/wire_json.rs 59df866f8c8d150484590fdb234d697fe49d42ccbc0081bcae635e9784d4f832
src/jobs/types.rs 38931ea88f1ed89db6e151b5a83365062503e8500e64227f61b993a6dba4f5ef
```

Snapshot: `/var/folders/g0/jdy1y8615k30fs6sbf4x1wt40000gn/T/lwiki-p16-integration-n10pjymt`, described by root as accepted P14 HEAD `f1cef3b1` plus38 P16 overlays. No Git verification was performed. Prior scoped tests remain evidence for their recorded candidates; this report does not attribute them automatically to the combined candidate. Strict combined lint and focused root runtime gates were pending when the static review completed. Their final logs and source inventory must supply acceptance evidence. No new regression test was requested solely for the behavior-preserving folds/boxing; the existing retained-payment, not-entered, retry and usage-breach gates are the relevant checks.

Root owns final acceptance. Report lease returned.

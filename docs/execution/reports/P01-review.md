# P01 independent gate review

Date: 2026-09-28. Reviewer: assigned independent storage/parser reviewer. Base accepted P00: `77ef9b7`; P01 implementation frozen after worker handoff, with root's test-only Clippy correction. Read-only implementation review; only this report, P02 review, and leased temporary reproduction test were written. **Disposition: one correction required before acceptance.** Root owns finding disposition.

## R1 — P2: overflowed radix integers become strings

Location: `src/records/parse.rs:299–300`, `scalar` delegation to `Yaml::from_str`.

Trigger: a valid page envelope with `title: 0x8000000000000000` or `title: 0o1000000000000000000000`. Both unquoted values have YAML core integer syntax. The dependency's signed-i64 radix conversion fails, its floating parser cannot parse the radix spelling, and it falls back to `Yaml::String`. The adapter accepts that result as a canonical string title. Expected: reject the wrong type; preserve integer classification independently of overflow, optionally accepting supported unsigned values in integer fields. The no-number-to-string-coercion contract cannot depend on representable magnitude.

Actual reproduction: `numeric_classification` in temporary `tests/p01_p02_review_repro.rs`, run with `cargo test --locked --offline --test p01_p02_review_repro -- --nocapture`; both titles reported `Valid`. A huge decimal title correctly reported `Invalid`; decimal evidence spans `9223372036854775808` and `18446744073709551615` remained exact. Thus the confirmed bug concerns radix overflow, not decimal u64 handling. Add permanent regressions for overflowing hex/octal in string fields and supported/unsupported integer boundaries, quoted numeric strings, and update the parser fingerprint if validation changes.

## Reviewed evidence and limits

Static inspection found no additional blocker in exact raw-byte/BOM/CRLF retention, iterative depth admission before child allocation, duplicate-key rejection before map insertion at every depth, anchor/alias/tag/merge refusal, byte conversion of YAML character markers, conservative multiline/range-edit refusal, or malformed/unsupported text availability. Schema exact typing delegates to the accepted P00 validator. Link extraction excludes Markdown code spans/blocks; resolution preserves exact ID/kind checks, companion conflicts, and ambiguity including duplicate IDs. The existing seven P01 tests and root's integration evidence cover these paths; this reviewer executed the three focused combined repro tests, not a redundant full suite.

Temporary repro tests deliberately assert observed bugs and must be converted or removed before integration. No live-provider, P03 transaction, cross-platform, or crash-safety claim follows.

## Final bounded re-review disposition — 2026-09-28

**R1 resolved; no remaining actionable finding in the reviewed P01 gate.** The scalar adapter now recognizes decimal/hexadecimal/octal integer syntax before conversion and returns an exact supported integer or a range error. Radix overflow cannot fall through to strings. Nonnegative values retain the full `u64` range; negative decimal values use `i64`; quoted numeric strings bypass implicit resolution. The parser fingerprint advanced to `lwiki-lossless-v2`.

Inspected permanent regression `numeric_syntax_never_falls_back_to_string_and_spans_remain_exact`: reproduced radix titles reject, quoted counterparts remain exact strings, decimal/hex/octal `2^63` and `u64::MAX` spans remain exact, and overflowing/quoted/float spans reject. No new issue was found in this changed invariant. The temporary expectation-of-bug test is now absent.

Re-review was static; no additional Cargo run was needed. Worker recorded 8/8 targeted tests and library Clippy passing; root reports the final 30-test all-target suite, all-target Clippy with warnings denied, formatting, debug build and diff checks passing. Reviewed SHA-256: `src/records/parse.rs` `8e828e54160e067f9c7efaa92b54f785fc5a1fba00139f7142cf5332a7807aa0`; `tests/records_lossless.rs` `6835c5c94273061ea5b25f51c8ab0b35dbabdf6314b26cd2455ab7683e189bf2`. Initial findings/evidence above remain historical; root retains package acceptance authority.

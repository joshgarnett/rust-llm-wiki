# P16.b terminal HTTP retry accounting

Independent bounded Sol source review substituted for unavailable Astra runtime. Root accepted the narrow retry policy and owns the ledger change; this worker edits only accounting tests and this report. No Cargo, source changes outside the leased test, Git, network, real helpers or credentials were used.

Status: all five new private ledger tests passed in the root-created isolated accepted-P15 plus jobs2 checkout. The file contains 31 parent tests; only the five new tests were selected in this gate. Root retains integration and source acceptance.

The admission exception requires both TerminalConfirmed and loaded hash-bound response metadata reporting a terminal status401/429/500/502/503/504. Unknown prior billing keeps its entire quoted allowance; terminal completion releases concurrency rather than refunding spend. Public reconciliation alone is insufficient HTTP proof. All prior unknown-billing retryable response spools must remain until the task no longer needs retries, because each reservation checks every predecessor. The dispatcher remains responsible for one command401 refresh, retry/backoff policy and actual paid receipt commit before scheduling another attempt.

Added tests use disposable existing full JobLedger fixtures, priced bounds, durable sends and owned response spools, actual ChangeEngine/Catalog receipt application, settlement and reopened ledgers:

- terminal_http_unknown_billing_retries_keep_full_charges_after_reopen:401/429/503 preserve exact request/byte/unit/money reservations while allowing a second request at concurrency1; after its paid receipt, the exhausted hard money ceiling refuses a third reservation.
- terminal_http_third_retry_needs_every_prior_status_spool: retry3 succeeds with both retained predecessors; removing retry1's settled spool prevents it despite intact retry2 proof.
- terminal_http_retry_does_not_hide_an_older_possible_send: a503 after an explicitly uncertain retry does not erase an older timeout's possible exposure or authorize another default retry.
- terminal_http_retry_requires_received_status_not_public_reconciliation: terminal manual reconciliation releases the slot while retaining money and refusing default retry without HTTP metadata.
- terminal_http_default_retry_refuses_invalid_status_or_missing_corrupt_proof: malformed200,400,403, absent status, nonterminal503, deleted metadata and valid-JSON metadata with a changed hash all refuse default retry.

No new race harness is added: reservation locking and the accepted P15 final-slot race are unchanged; these tests specifically exercise the new proof eligibility boundary. They do not qualify live HTTP or claim transport automatic retries are complete.

Actual authorized command: `CARGO_TARGET_DIR=/Users/jgarnett/Devleopment/rust-llm-wiki/target cargo test --locked --offline --lib jobs::accounting_tests::terminal_http_`, run in `/var/folders/g0/jdy1y8615k30fs6sbf4x1wt40000gn/T/lwiki-p16b-accounting-jku0i379`. Exit0; compile10.14s; actual5 passed,0 failed,31 filtered; runtime10.92s. Full output: `/tmp/lwiki-p16b-accounting-final.log`. No fixes or candidate copies were needed. One dead_code warning identifies the dispatcher_bindings method awaiting .b integration; no Clippy or whole-suite gate claimed.

Matching isolated/workspace SHA256 at this gate:

- src/jobs/accounting_tests.rs: `38f8410ff0ac189ecc887ffad9449aed20fd6e1bb225ebbd97b3b5ce058387fe`.
- root-owned src/jobs/ledger.rs: `97321eb725bd351b6476f6f7b62e56259cf8c003997aa14d22fe9c0700768865`.

Cargo/test/source leases returned to root after completion. This worker returns the report lease with this actual-outcome handoff.

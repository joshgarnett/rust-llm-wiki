# P16.c durable wire-contract breach accounting

Bounded test-only packet. Root owns the ledger's exact safe metadata.failure_code=wire_contract_violation recognition, existing Received metadata binding and BoundViolated/Reconciled replay. This worker edits only src/jobs/accounting_tests.rs and this report. No ledger/shared edits, Cargo, Git, live calls, secrets/helpers or nested delegation occurred.

Status: both new tests passed in the authorized isolated accepted-P15 plus jobs2 checkout. The accounting test file contains33 parent tests; only the two new tests were selected. Root retains broader .b/.c integration acceptance.

wire_contract_violation_survives_received_interrupt_and_cannot_resume exercises four disposable actualJobLedger paths: known usage/cost entirely within numeric admission and unknown usage/cost, each uninterrupted or interrupted at actual AfterReceived. It reopens the ledger, checks owned metadata bytes against the Received spool hash/length and exact metadata, and confirms no BoundViolated event yet exists in the interrupted case. Inspection must still expose a paused permanently invalid guarantee while retaining request/money allowance. Two replay calls must produce exactly one actual BoundViolated plus reconciliation, without resending or double charging. The test then actually commits the paid failure receipt with ChangeEngine/Catalog, settles known charges or retains the full unknown allowance, and refuses fresh-task reservation and resume both before and after an attempted money/request-limit increase.

wire_contract_ordinary_paid_output_failures_do_not_invalidate_bounds covers MALFORMED, REFUSAL and the nonmatching wire_contract_violation_suffix label, each with complete within-bound usage/cost or missing observations. Actual paid receipt commit, settlement and reopened replay must preserve the guarantee. A separate ready task remains admissible; known paid cost stays settled and unknown paid cost stays reserved. This demonstrates that unsuccessful knowledge or incomplete metadata is not silently recast as a semantic contract breach or free work.

Actual command: `CARGO_TARGET_DIR=/Users/jgarnett/Devleopment/rust-llm-wiki/target cargo test --locked --offline --lib jobs::accounting_tests::wire_contract_`, run in `/var/folders/g0/jdy1y8615k30fs6sbf4x1wt40000gn/T/lwiki-p16b-accounting-jku0i379`. Exit0; compile3.95s;2passed,0failed,36filtered; runtime8.01s. Log `/tmp/lwiki-p16c-accounting-final.log`. No test corrections needed. One unconsumed dispatcher_bindings dead_code warning remains until .b integration; no lint or whole-suite claim.

Native interruption is the existing deterministic LedgerFault at the real fsynced Received boundary, not a new process SIGKILL qualification. The tests exercise real owned spools/canonical commits rather than fabricated operational history; they do not validate the future observer's selection of the semantic marker or live provider billing.

Source SHA256 at authored handoff:

- src/jobs/accounting_tests.rs: `b09f5a90cd97c6f68acc6239b061c7ff072b886ca26589c9a1e3862d9dd34869`.
- inspected root-owned src/jobs/ledger.rs: `2771a6aa63da09e4265c7033583f8daaf9ca212471397734328e3c4c790c30e9`.

Worker returns test/source/report leases to root. Root retains all Cargo sequencing and actual acceptance evidence.

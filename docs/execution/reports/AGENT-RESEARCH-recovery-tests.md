# Agent research recovery and concurrency tests

`tests/research_recovery.rs` adds four isolated tests for the new local handoff protocol. The fixtures use disposable vaults and no network or provider credentials.

- Two threads submit different responses for one packet at a barrier. The test requires one import success, one content conflict, one captured source, one receipt, and one increment of durable run counters.
- A `DurableIo` adapter interrupts the head replacement once, after research children and the receipt are written. Exact retry must recover the pending change, reuse the committed submission, and retain one source, receipt, and counter increment.
- The same interrupted import is followed by ordinary `research resume` in a reopened app. Resume must recover before returning an answer packet ready for import; subsequent exact retry must be idempotent.
- A batched fixture capture fills the initial packet with 32 source passages. Importing a new source must put its verified passage first in the answer packet, omit one older candidate with an explicit warning, and preserve remaining source and byte limits.

The test source is formatted. Root registered and ran `//:research_recovery_test`; all 4 cases passed with 0 failures (24.9 seconds reported by Bazel). Case count comes from `bazel-testlogs/research_recovery_test/test.log`, with the grouped invocation in `.artifacts/agent-research-focused.log`. This worker ran no build.

The release workflow fixture was also migrated in `tests/release_workflows.rs` to the native CLI agent handoff: run with an active source, import a host-supplied source, exact retry, answer with a verified passage ID, status and report. Its loopback provider now covers only embeddings, direct graph extraction, and generation probe. Legacy research provider, budget-amendment, and research-specific SIGINT routines were removed; generic signal coverage remains in `remote_cli` tests. The parent still checks retained paid accounting for unrelated operations. The first grouped gate stopped at the maintained package-manifest fixture mismatch; root regenerated that fixture and owns the rerun. No release workflow product failure is inferred from that initial mismatch.

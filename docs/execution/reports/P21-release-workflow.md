# P21 unified native CLI workflow

Status: historical targeted native parent PASS; P20 accepted and matching final same-vault parent PASS55.15s at final source fingerprint325e425accaad00f69d658c68121d46469cac5e69d7fc3c2ef44d6b07912678a. P21 is locally accepted; external qualification pending. Leases returned; no production delta.

Exclusive lease: `tests/release_workflows.rs`, this report. No production source, interface, Cargo, registry, schema, shared documentation, state, fixture, or Git edits. No nested delegation.

Named parent: `m0_m4_full_local_acceptance` (native Unix). One disposable vault executes the binary's actual exported, maintained recipe, checking its manifest/file hashes and returned IDs/hashes. Packet/import/apply/resolve/apply/review/apply includes separate Ada identities, quoted counterevidence, idempotence, author conflicts, withdrawal, and explicit assertions that import/resolution cannot implicitly accept knowledge. Cache deletion and CLI rebuild preserve literal hits while withdrawn support remains absent.

The same vault then receives a fresh source through CLI capture and trusted loopback mock provider operations: embeddings sync, semantic query, cached offline reuse, graph/context; strict generation extraction with retained offline reuse; research budget stop after two requests, deterministic offline partial report, unchanged-limit refusal, dry-run amendment preview, explicit three-request amendment/resume that pays only synthesis, and explicit proposal application. Research provenance verifies through production `SourceView::verify(Current)`; claims remain unassessed.

A complete native HTTP request reaches a channel barrier before SIGINT. JSONL has exactly one sequenced terminal event/exit130; durable stopped state retains dispatch intent, unknown reservation, no receipt/spool, and no follow-up request. Online resume refuses reconciliation-required history; offline status/resume/report preserves records. A subsequent native probe must disclose that hold and all earlier unknown histories without mutating budgets, receipts, genesis, or restored Markdown.

Actual command: `cargo test --locked --offline --test release_workflows -- --test-threads=1`, reviewed loopback escalation. Second run exit0, compile2.04s, parent56.95s: **1 passed/0 failed/0 ignored**. Log `/private/tmp/lwiki-p21-release-workflow-second.log`. First run exit101, compile2.65s/parent26.05s, solely omitted required `--profile primary` on cached API extraction; corrected test invocation. Failed evidence retained at `/private/tmp/lwiki-p21-release-workflow-first.log`. Leased-file `rustfmt --edition 2024` passed; no worker full fmt/lint/release claim.

Limits: deterministic local providers establish production adapters, accounting, CLI, and provenance behavior; they do not establish live-provider interoperability, model entailment/quality, public native fetch compatibility, host-app installation, native other-OS crash behavior, or power-loss safety. Separate root-owned fault/counting/public-fetch/artifact/full tests and independent review remain required.

## Matching evidence identities

The historical targeted run used accepted HEAD `809e119aefa781ad7737c575d43acbaae104ad2b` plus then-unaccepted P20 source. P20 is now accepted at f4dcf08; current final evidence is in P21-checks.json. Root final source inventory remains authoritative. SHA256 observations after passing target:

```text
1ab52c2ca6b4f2218a4cc33eff26a44fa7cf6803f27e13cdaab6fd1cb52c563b  tests/release_workflows.rs
d9cbbcd33b5272ac090b646fca804c825c0db2887c26e0dac1ee7bead2d7ef69  target/debug/lwiki
76f93eb57dd461f0d28f36ad87bf1f77a6e4112511b2b4c58c729057c43ecbb4  tests/fixtures/p14/package-manifest.json
63f9dc7c4a3073d388258e93351bb802a6ef2d5e70aceff3b6e2915f7c2757fb  Cargo.toml
e1b6f04e0fd93af2c758fb1210d90627033f20df4b66b0a3c0eba9a968f72def  Cargo.lock
495c69d1db93f25f53e3d0d2d40d812313366cc368de317871c2ace6c6ffafa4  rust-toolchain.toml
```

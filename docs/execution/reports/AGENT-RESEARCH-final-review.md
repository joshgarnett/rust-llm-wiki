# Agent research final review

Independent read-only review of the pending 0.1.1 replacement, 2026-09-29. Scope: current research types/codec/storage/inspection/engine, public schemas and test guide; generic provider codec retention/replay and receipt-only settlement. Historical research compatibility is not an acceptance requirement. No source edits, builds, tests, Git operations, provider calls or delegation were performed by this reviewer.

## Disposition

No new blocking authority, accounting, transaction or replay defect found in the inspected implementation. Runtime acceptance remains with root's focused and broader gates. This source review does not establish native crash behavior, live gateway compatibility or release completion.

The prior report-only freshness issue is closed: completed outcomes without an outstanding packet now say `retained`; they do not claim freshly verified evidence or issue import readiness. Prior R1–R8 fixes remain present.

## Verified source boundaries

- Research is a separate local protocol. It never invokes providers, credentials, HTTP, host tools or a shell. Agent-provided sources retain AgentReport origin; full provenance remains in the retained submission. Host tool activity and spending are expressly unobserved.
- Strict bounded submission parsing validates stage, fields, duplicate keys, source/claim counts and byte limits. Answer claims resolve only packet-local passage IDs to verified citations and remain unassessed. Imports neither accept graph assertions nor publish authored knowledge pages.
- New sources, next packet/report, normalized receipt and updated head use one changeset. Receipt publication follows its outputs; head publication follows all operations and compares the prior head hash. Current source/citation dependencies become read guards. The vault writer and normal recovery path run before mutable state is consumed.
- Identical retries authenticate their receipt and immutable outputs before new random source allocation. Conflicting submissions for a consumed packet fail. Later mutable source changes do not invalidate the historical receipt, but stale outstanding packets are withheld until explicit refresh. Report-only results remain retained history.
- Lifetime source counts/bytes are carried in the guarded head and checked against the outstanding packet's advertised remaining allowance. Rounds, generations, receipts, packet passages and submission content have independent finite ceilings. New captures precede old passages during bounded packet selection. Answer gaps replace the current unresolved set while earlier submissions remain retained.
- Generic provider preparation retains a content-addressed codec under `.wiki/state/provider-codecs` before reservation, then rechecks the same prepared identity after credentials. Restore authenticates descriptor, task, bound, codec hash and exact contract, without invoking transport/auth or substituting current provider settings. Responses grammar and the original local validator remain separately bound.
- `settle_receipt` checks receipt-only identity, holds the vault writer for recovery/publication, reconciles prior acknowledgment, regenerates the receipt from retained accounting and compares it with the requested receipt. It does not authorize arbitrary caller draft writes or replace unknown billing with zero. Already acknowledged receipts settle through the original attempt rather than another publication.

## Concrete documentation and schema follow-ups

1. The live ranking section of `docs/testing-0.1.1.md` asks for Cedar plus four distractors “from the previous report,” but does not supply those notes. This prevents a fresh test agent from reproducing the prescribed corpus. Include exact synthetic text and capture commands before calling the guide self-contained.
2. Disclose that newly captured and explicitly selected sources contribute only their first 4,096 UTF-8 bytes as a packet passage. Captures may contain more text, but claims can cite only the spans actually supplied in the packet; selecting the same source ID does not itself expose its later text.
3. `research-submission-v1.json` requires nonempty string provenance (`minLength: 1`), while `parse_submission` accepts an empty optional provenance string. Align the schema or parser. This is a machine-contract mismatch, not an authority/accounting failure.

The guide otherwise accurately separates draft/download verification, no-network local handoffs, unassessed citation integrity, retained report freshness, uncertain paid requests, and unsupported Windows vault writes. Release links/source claims still need the actual validated 0.1.1 build and draft-release evidence before final handoff.

Root was notified of all three follow-ups. This report records source observations; authored regression assertions and in-progress root gate results were not independently executed here.

## Retained API fixture follow-up

Root's full gate subsequently reported two failures in `api_extraction_test`. Both arose from the obsolete assumption that changing the current provider model must prevent decoding an already received response. The new unconditional immutable codec intentionally retains the original model and contract; local recovery does not need current configuration to match. One test expected an error from successful historical decode; the second called `.err().unwrap()` on successful recovery for its `wrong_service` case. No source invariant defect was found.

Root leased this reviewer only `tests/api_extraction.rs` for corrections. The replacement asserts the original model/value still decode after current model changes, then alters the saved codec's model and requires a recovery failure with no receipt/materialization, unchanged paid budget, Received state and one transport call. Restoring the exact codec restores local recovery without output publication or a second send. The fault matrix replaces obsolete `wrong_service` with missing/corrupt codec cases, alongside missing descriptor/body and changed body, and strengthens paid-state/no-send assertions before restoring files and completing ordinary API recovery.

These are authored test corrections, not runtime acceptance. The leased file was formatted with `rustfmt --edition 2024 --config skip_children=true tests/api_extraction.rs`, without formatting fixture modules. No tests or builds were run by this worker; root owns the final affected gate. The test-source lease is returned.

## Root integration response

The self-contained test guide now includes the exact five-note ranking corpus and commands, and explicitly documents the 4096-byte UTF-8 passage prefix plus honest excerpt capture. The submission schema now accepts empty optional provenance, matching runtime validation. Packet-schema validation was added to the handoff fixture helper. Full runtime gates remain root-owned.

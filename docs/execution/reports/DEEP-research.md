# Research handoff follow-up

Leased paths: `src/research/inspection.rs`, `src/research/engine.rs`, `tests/research_handoff.rs`, and `tests/research_recovery.rs`. No shared schema/type, Cargo, Git, or build changes.

## Changes

- Inspection now returns every explicit and lexical candidate. Packet assembly owns both the 32 passage and 65,536 quotation-byte limits, so omissions are visible and the warning names each omitted source/revision/span or assertion identity.
- Packet assembly keeps supplied order, suppresses exact duplicates, and suppresses source citations wholly contained in an earlier selected passage of the same source and revision. Newly imported passages lead; explicit sources lead lexical hits. Collection imports reconsider every explicit source, including those omitted from the earlier packet.
- Answer reports set `partial` only when a follow-up was requested. They set `completion_reason` to `agent_finished`, `follow_up_requested`, or `round_limit`. The root owns the corresponding optional report type/schema field and retained-report compatibility.

## Regressions

- `exact_byte_limit_names_every_omitted_explicit_source_on_start_and_import`: twenty 4,096-byte explicit sources fill exactly 65,536 bytes at sixteen passages; the four initial and five successor omissions are identified; two newly imported passages retain priority.
- `explicit_sources_cover_lexical_subsets_on_start_and_refresh`: two explicit sources produce one passage each before and after a source revision changes.
- `finished_answer_with_honest_gaps_is_not_marked_interrupted`: a finished cited answer with unresolved gaps has `partial=false` and `agent_finished`.
- Existing offline follow-up recovery test now asserts `follow_up_requested` and `round_limit` reasons.

Files were formatted with `rustfmt --edition 2024`. No Cargo tests or builds were run under the worker lease; root serializes those checks. B6 requires no engine keyword filter. B13 source metadata remains root-owned.

## Timestamp and diagnostics follow-up

Added `host_claimed_retrieval_time_and_exact_bytes_survive_capture_and_replay`: a two-source collection retains the supplied RFC3339 timestamp and `agent-claimed` marker on only the dated immutable revision, preserves the original UTF-8 payload bytes, and returns the same source IDs and counters on replay. Added `handoff_validation_names_invalid_field_or_selector_before_writes`: invalid timestamp, a 65,537-byte source, NUL content, empty claim text, quote hash and source ID selectors each produce a field-specific diagnostic without canonical changes. The root owns the codec, source capture implementation and integration run. This test-only follow-up was formatted; no build or test was run under the lease.

## Packet context reconstruction follow-up

Packet assembly now replaces an earlier selected source subset with a later same-revision full span when the replacement fits the byte bound; otherwise it retains the useful subset and reports the omitted full candidate. Refresh and collection import reconstruct local source candidates from hash-verified retained receipts, then original scope and prior packet, deduplicating IDs and filtering only explicitly withdrawn sources. Invalid or missing source records remain errors. This restores captures previously evicted by packet caps without changing the stored scope hash. Newly selected existing passages and receipt artifacts contribute read preconditions to publication; generated capture targets remain proposed writes rather than current read prerequisites.

Regressions: engine unit `later_full_span_replaces_earlier_subset_only_when_it_fits`; handoff `refresh_recovers_captured_source_evicted_by_packet_limit`; `withdrawn_explicit_source_stays_out_of_refreshed_import`. Leased files were formatted; root owns serialized compilation and tests.

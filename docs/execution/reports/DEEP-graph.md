# Opposing accepted assertion visibility

Leased paths: `src/catalog/eligibility.rs`, `src/graph/traverse.rs`, `tests/graph_queries.rs`. The root owns `GraphAssertion` type additions, catalog projection fingerprint, schemas and integration checks.

`OppositionKey` compares canonical subject, predicate, object identity or literal type/value, property, unit, modality, and validity dates. Polarity is the only differing field. Both catalog eligibility and traversal use this helper and restrict groups to accepted, currently eligible assertions. Catalog marks each member disputed with `opposing_accepted_assertion`; it does not alter authored status or decisions. Grouping uses ordered maps and takes O(n log n) for the assertions.

Traversal builds one ordered opposition index per walk and emits opposing assertion record references up to `contradictions_per_assertion`, plus an omitted count. `contradictions` continues to contain only actual contradicting evidence. Record-ID order makes truncation deterministic. Current catalog `row.disputed` continues to drive query, neighbors and context flags.

Regressions added: `opposing_accepted_assertions_are_bounded_and_match_exact_qualifiers` checks three opposites, the one/two-link limits, catalog/query/neighbor flags, and excludes different object/date/modality/property and rejected/proposed assertions. `withdrawn_support_clears_opposing_assertion_dispute` verifies a source withdrawal removes current opposition and leaves no historical link.

`rustfmt --edition 2024` and `git diff --check` passed for leased source/tests. No Cargo build or tests were run under the worker lease.

## Fixture correction

The focused graph test exposed an invalid fixture: `wiki_property` is valid for `has_property` literal assertions, not the `depends_on` entity relation used for the main opposing pair. The relation cases now cover object, date and modality differences with valid fields. A separate pair of accepted `has_property` decimal literal assertions compares `cost` with `sector` and verifies the property mismatch does not create opposition. The bounded result also asserts `truncated` and its opposing-link warning. This test-only correction preserves record validation; `rustfmt` and `git diff --check` passed. Root owns the rerun.

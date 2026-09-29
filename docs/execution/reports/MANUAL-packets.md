# Manual packet search follow-up

## Trigger and cause

An exported `graph extract --executor agent` packet retains exact source windows in `knowledge/extractions/packets/packet_<fingerprint>.md`. Catalog scanning intentionally keeps the note's `raw_text` for audit and packet import. The default literal leg searched that raw text while the packet row inherited the generic `Current` eligibility. The source's captured passage and its operational packet therefore both matched `CEDAR-731`; after source withdrawal the captured passage ceased to qualify, but the packet still appeared as `Current`/`note_text`. The normalized FTS body was already empty for valid packets, and valid packet kinds were already absent from the embedding corpus and current context allowlists.

## Change

- `src/catalog/eligibility.rs`: classify an intact packet as `Unsupported` (`operational_packet`) while its source/revision is current, `Historical` for an older revision, and `Withdrawn` after source withdrawal. Structural invalidity still takes precedence.
- `src/retrieval/filters.rs`: ordinary search SQL excludes extraction packet rows and the managed packet directory before candidate limits. This runtime guard also protects an older cache where packet eligibility was `Current` and malformed packet notes that have no adopted kind. Explicit `include_historical` search retains literal packet audit access; direct record read and packet import are unchanged. Other invalid notes retain default literal discovery.
- `src/catalog/scan.rs`: bump the parser fingerprint for changed packet eligibility projections. Rebuild/sync uses the new projection; the SQL guard also excludes packet rows in compatible current snapshots before candidate limits.

Independent review found a second route: copying a valid packet outside its managed directory creates a duplicate ID, so neither copy has an adopted record row; a readable malformed packet outside that directory has the same missing-kind problem. Scan now recognizes a readable packet kind from top-level fields independently of adoption, sets the document's packet kind, and leaves its normalized FTS body empty. Ordinary malformed nonpacket notes still have literal discovery. The parser fingerprint advances to `packet-source-lifecycle-v2`, and `index_snapshot` refuses a published generation with an older parser fingerprint before exposing its rows. A caller with a writer can sync and retry; shared SQL validation stays structural so sync and rebuild can repair the cache.

## Regression and checks

`tests/retrieval_lexical.rs::exported_packet_windows_never_supply_default_source_search` persists a real packet and checks one source hit before withdrawal, zero default hits after withdrawal and rebuild, the index snapshot, explicit historical packet/source lookup and packet reload, and packet exclusion from embedding corpus. `malformed_packet_path_requires_explicit_historical_audit` checks a malformed packet-path note against an ordinary invalid note. The existing literal audit test now opts into history for packet text.

Root captured the red test on the unmodified implementation: `exported_packet_windows_never_supply_default_source_search` failed with two default literal hits where one source hit was expected (`.artifacts/manual-packets-red.log`). No worker build or test was run. Root owns subsequent target results and integration.

Root's grouped Bazel run of `//:retrieval_lexical_test`, `//:graph_resolution_test`, and `//:machine_contract_test` passed all three targets after the fix (`.artifacts/manual-targeted-green.log`, 101.091 seconds). This is local fixture coverage; packet embedding nonmembership is checked through the rendered corpus, without a live provider call.

The second regression failed before its production fix: a copied valid packet outside the managed directory made two default literal hits where one captured-source hit was expected, and the malformed packet outside that directory also appeared in default results (`.artifacts/manual-packets-copy-red.log`). The new tests also exercise no-sync refusal of a consistently stamped prior-parser projection and writer-assisted sync recovery. Second-fix gate result is pending root execution.

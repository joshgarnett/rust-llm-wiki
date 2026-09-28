# P06 independent review

Independent Sol review of offline application/config/discovery, root shared types and migration helper, CLI argument/dispatch/main adapters, stream schema, and application/CLI tests. Fresh/restarted Astra threads were unavailable; this follows the playbook's independent Sol fallback without reducing gates. Base is accepted P05 `6158b5810908dffbea8251ddd386831143bc24ba`. Read-only code review; reviewer wrote only this report, ran no Cargo, and used disposable temporary vaults for executable probes.

## Findings

**P06-R1 — resolved:** direct `execute_draft` prepared a durable changeset, then propagated `engine.apply` failure without returning its ID. Reproduction: initialize a fixture, write invalid bytes to `.wiki/cache/index.sqlite`, then submit a valid page through stdin. Original result: exit 5 `INDEX_CORRUPT`, `data:null`, `details:null`; a new retained `changes/<id>/change.md` existed and the page did not. The caller could not identify the preparation from the result.

Root fixed direct and explicit apply failures with `retained_error`, attaching `PreparedChange` ID/manifest hash while preserving the original error code, message, and prior details. CLI failure envelopes expose `data.change`, set `partial:true`, and human errors print the retained ID and inspection command. Re-review confirms correct source paths. Independent executable reproduction now returns exit 5 with the retained ID/hash; `changes show` succeeds with `prepared`, then `changes abort` succeeds with `aborted`. Root subprocess regression covers the same failure and inspection path. No blocking finding remains in the reviewed scope.

## Reviewed invariants

- Strict dry-run routes read/plans/index/doctor/recovery through canonical reads only, never SQLite or writer acquisition. Source plans have no provider/extractor execution. Normal verified reads acquire a writer and explicitly recover incomplete application; `--no-sync` reads pinned cached rows, labels unverified freshness in JSON and human presentation, and does not substitute live bytes.
- Config precedence is explicit options, trusted explicit private JSON/vault overrides, flat portable preferences, then defaults. Duplicate/unknown JSON fields and unsupported portable permission fields are refused. Profiles require local permission; offline remains final. There is no ambient credential, helper, endpoint, DNS, HTTP, pager, or editor dispatch in these paths.
- Replacements require expected hashes and preserve page ID/kind. Renames preserve canonical identity, classify known incoming links through the registry, retain labels/fragments/reference titles, leave ambiguous links/code alone, bind dependencies, and refuse immutable incoming edits. Graph validation precedes retained/application activation.
- Read paths must belong to visible canonical documents or captured content; private/generated/original payload paths are refused. Observed bytes bind scan hashes, and UTF-8 ranges/truncation remain exact. Captured frontmatter supplies no identity authority.
- Compatible `0 -> 1` migration is explicit, staged, hash-guarded and lossless; future versions and immutable records are refused. Bootstrap preserves excludes and commits the durable marker last, with no SQLite bootstrap.
- Retained payload inspection verifies exact hashes; default aggregate is bounded at 16 MiB with explicit omitted operation indices, and an explicit operation uses P03's per-payload ceiling. JSON/error/exit mapping is consistent; JSONL started/completed events share invocation ID and monotonic sequence, with durable journals remaining recovery authority.

## Evidence and limits

Independent dry-run executable probes on a cacheless fixture: `read`, lexical `search`, `search --no-sync`, index sync/rebuild, doctor, recover, check, and initialization of a new final directory all returned 0, empty stderr, and unchanged recursive membership/bytes/mtime. Reviewed binary SHA-256 was stable throughout: `a7c1c0fd29cd915e8ebfea62ab411e62babbc686c1bb6b6d2036d5449c2a1c86`. R1's resolved executable reproduction used that same binary.

Root-owned `/tmp/lwiki-p06-p07-targets-final.log` was inspected: machine contract **6 passed** (0.64s), offline application **14 passed** (7.64s), offline CLI **9 passed** (3.13s), retrieval **13 passed** (1.96s), 0 failed; compile 3.69s. This is root execution evidence, not reviewer-run Cargo. The later `normal_cli_search_recovers_committed_sql_with_incomplete_change_journal` regression was reviewed statically: injected post-SQL-commit return failure leaves `FilesApplied`; normal CLI search must recover to `Committed` and return verified hits. It was absent from that nine-test CLI log; root's next broader gate must supply its runtime evidence.

No counting network transport was installed by this review. Static absence of network/helper handles and tree invariance support the local guarantees but do not prove live-provider compatibility, native power-loss durability, other operating systems, or E01–E05 qualification. Those remain separate gates.

Reviewed source/test SHA-256:

| Path | SHA-256 |
|---|---|
| src/app/offline.rs | `8fd13217fabaf690a6380211a695b33e6d92afc3be6c6f32eecb0b0c12c5d2a0` |
| src/config/local.rs | `ce6257fd8e5550d1ec4d5c77e9509ff1d56af5f02b9ad03a6d70233f850a4a89` |
| src/vault/discovery.rs | `3d4a6b3d9c21c54d89cc579a490526f8bcd85ccc4bf53eb7cbaa49a827f72eb8` |
| src/app/types.rs | `fb52808559feab8760fb2300202c32628e3cb0d3cf63317c340aa95d4e41fa16` |
| src/records/edit.rs | `d8fa5b3b63aa091b58daf3b221b088e699e2e29f3e07134c3b7811de56a4c06b` |
| src/cli/arguments.rs | `02c6159cfbb8138ac666586aac169f37170830d87960117a43415ca6bd74f259` |
| src/cli/dispatch.rs | `111e358290a680775b8db7aa4968ed0501d8d82940e71b5e5f523cdc3a5aeb25` |
| src/main.rs | `13b84c74fd24b65c3740d2fe0d30869e7a221cb20da788d4c68a08d37f8d7c3d` |
| schemas/stream-v1.json | `dcbe9c30bbc651bebab9e4ecf1b3698b0a2a1a58a05a0b2e50c2a14bfd46e7fe` |
| tests/offline_application.rs | `d717a1d67e05318980d967aff2e614909f6944ef220d0824f0ca42419e20c7b3` |
| tests/offline_cli.rs | `0c9f0b8fa9bce546893520a0c5d44eb5ec724e2d92d448dce0ade1bdee788fcb` |

## Root final integration disposition

Selected final integration passed139 parent tests, including offline CLI10 (with actual AfterCommit/FilesApplied recovery coordinator regression), offline application14 and retrieval13. Required-name-only CLI/machine rename rerun passed16; all-target Clippy, fmt, debug build and seed/diff passed. All findings closed. Exact final integrated hashes/fingerprint are in P06-P07-checks.json; earlier review hash tables identify the reviewed revision.

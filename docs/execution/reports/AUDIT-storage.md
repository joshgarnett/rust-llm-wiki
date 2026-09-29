# Independent storage and canonical-authority audit

Date: 2026-09-29. Read-only implementation audit by the assigned Astra reviewer. Checkout: `63b2439f5cd57211e2a2bc5587ac5bace3d03ea2`; behavioral reproductions used the downloaded `.artifacts/012-macos/lwiki` release 0.1.2 executable (release source `d8a82a5`). No product code, tests, Git state, real vaults, credentials, or live services were changed. This report is the only repository file written by this worker. Root owns the separate full-suite invocation; no build/test runner was started by this worker.

Three actionable findings were reproduced. Existing accepted reports were treated as historical evidence, not proof that these cases work.

## S1 — High: a durable conflict has no supported resolution and indefinitely blocks the vault

**Locations:** `src/changes/apply.rs:37`, `src/changes/recover.rs:41`, `src/changes/rollback.rs:30`, `src/changes/journal.rs:220`, `src/catalog/publish.rs:76`, `src/app/offline.rs:921`.

The engine correctly preserves an unfamiliar target and persists `Conflict`. However, that state has no outgoing journal transition and no explicit resolution API/CLI. `changes apply` refuses it, `recover` refuses it, `changes abort` refuses it, and `changes rollback` goes through `execute_draft` and the vault-wide unresolved-change guard. Restoring every target to recorded before or proposed bytes does not help: the durable status still blocks publication and ordinary writes. Deleting recovery evidence or editing checksummed operational state is not a supported resolution workflow.

**Contract:** `docs/technical/storage.md:155` explicitly labels Conflict as requiring explicit resolution; line 161 provides guarded rollback and reconciliation obligations. Refusing automatic recovery is correct; the defect is absence of the required explicit way forward.

**Reproduction:** initialize a disposable vault, stage a page creation, save and remove that change's operational journal to simulate lost operational state, and create unrelated author bytes at its target. `recover` returns `CONTENT_CONFLICT` and retains the author bytes. Save those bytes outside the vault and remove the target, restoring its exact old state. `recover`, `changes apply`, `changes abort`, and `index rebuild` all return `RECOVERY_REQUIRED`; rollback cannot match its expected proposed state. Then restore the exact proposed page bytes: both explicit apply and rollback still return `RECOVERY_REQUIRED`. This uses simulated journal loss, not a claim that a native crash was observed. Existing `tests/changes_recovery.rs:958` separately exercises genuine applying-state third-hash transitions, but stops after checking that Conflict was persisted; it never exercises resolution.

**Impact:** one interrupted/conflicted change can leave otherwise intact canonical knowledge inaccessible to normal verified/current operations indefinitely. This affects local recovery, not merely an external platform qualification.

**Suggested work:** add an explicit, manifest-bound conflict-resolution operation with fresh expected hashes and retained author bytes. At minimum support user-selected completion when all target/read guards are reconciled, and a guarded inverse/abandonment path for partial application. Record the resolution durably, preserve immutable revision ownership, validate the final graph, and do not bypass the vault guard for unrelated changes. Test recovery from conflict to a terminal outcome, restarts at every resolution boundary, and refusal of unfamiliar bytes throughout.

## S2 — High: malformed duplicate IDs bypass packet authority and permit paid work followed by a panic

**Locations:** `src/sources/revision.rs:249` (`SourceView::resolve`), `src/catalog/scan.rs:138` (`readable_ids` and isolated-field fallback), `src/app/extraction.rs:94` and `src/app/extraction.rs:110` (preview/reuse branches).

Catalog identity discovery reserves safely readable top-level IDs even when another YAML field is malformed. `SourceView::resolve` only counts IDs in successfully parsed `note.fields`. A note containing a valid `wiki_id` line followed by `title: [broken` has no parsed fields and therefore disappears from SourceView's duplicate count while the catalog correctly removes that ID from the canonical registry.

**Reproduction:** copy `tests/fixtures/bootstrap/vault`; export an ordinary agent packet for `source_00000000-0000-7000-8000-000000000005`. Add `broken-duplicate.md` with:

```markdown
---
wiki_schema: "1"
wiki_id: "source_00000000-0000-7000-8000-000000000005"
wiki_kind: source
title: [broken
---
Broken duplicate.
```

`check` correctly diagnoses `REFERENCE_AMBIGUOUS`. Repeating the same ordinary `graph extract --source-id ...` nevertheless exits 0 with `persisted: true`, `ready_to_import: true`, `reused: true`, and `snapshot: null`. Its reuse path checks only the packet's original dependencies and reopens it through the same weaker SourceView resolver. With no prior persisted packet, dry-run also succeeds, but actual new publication rejects the invalid packet; the latter still leaves a staged change. Thus this finding is not a claim that new invalid graph records successfully publish.

**Contract:** `docs/technical/storage.md:51` requires duplicate identities to be excluded from typed resolution; lines 139–141 require newly introduced duplicate identities to affect authority rather than checking only old dependencies. `ready_to_import` should not advertise a packet whose canonical source ID is presently ambiguous.

**Cross-module reproduction:** the graph audit subsequently ran API extraction against a copy of this persisted-packet/duplicate fixture with a local mock generation service. Exactly one request was received; the CLI then exited 101 with a panic at `src/catalog/eligibility.rs:865`, `expect("valid packet source")`. The generation-output lifecycle branch follows a packet whose source was removed from the canonical registry, without checking the source's presence. Shared evidence is `/private/tmp/lwiki-s2-api-iadmx46f/result.json`; script `/private/tmp/lwiki-audit-graph-dAXyAR/repro_s2_api.py`. This independently establishes a real dispatch across invalid canonical identity, followed by an unstructured process failure.

**Impact:** packet export/reuse and direct source verification disagree with canonical checks. An agent can receive apparently ready work against an invalid source identity; API execution can spend before crashing while attempting publication. Public SourceView consumers share the weaker resolver. This worker inspected the shared API vault's accounting: the response spool and `received`/`reconciled` events survive, with unknown charges retained, but there is no `outputs_committed`/`settled` event or completed generation receipt. No lost charge, automatic duplicate send, actual provider billing, or accepted assertion corruption is claimed.

**Suggested work:** centralize canonical identity reservation so SourceView and catalog agree for syntactically broken YAML, duplicate keys, unsupported schemas, and ordinary valid duplicates. Validate present canonical membership before returning persisted/reused packet authority and before paid dispatch; preserve dry-run purity. Replace the generation-output lifecycle assumption with explicit invalid/missing-reference handling so malformed vault records cannot panic the checker/publisher. Expand `tests/sources_evidence.rs:1101`: its existing malformed-copy test changes a valid scalar to an invalid enum, leaving `note.fields` populated, so it misses syntactically broken YAML. Add reused-packet, new-packet preview, fail-before-dispatch, panic-free catalog, and retained-paid-response replay negatives.

## S3 — Medium: required evidence-navigation consistency diagnostics are absent

**Locations:** `src/domain/records.rs:340`, `src/catalog/eligibility.rs:362`, `src/catalog/scan.rs:360`.

`wiki_evidence` is validated as a list of syntactically valid wikilinks, but no catalog check resolves its members or compares their authoritative `wiki_assertion_id` ownership. It is not included in the companion-reference iteration. This correctly prevents the convenience list from granting support, but omits the promised diagnostic channel.

**Reproduction:** copy the bootstrap vault; baseline `check` returns zero diagnostics. Add this field to `knowledge/assertions/forward.md`:

```yaml
wiki_evidence: ["[[knowledge/evidence/nonexistent]]", "[[knowledge/evidence/reverse_support]]"]
```

One target does not exist; the other is evidence for the reverse assertion. Both `check` and `index sync` still exit 0, and `check` reports `diagnostics: []`, `error_count: 0`.

**Contract:** `docs/wiki-format.md` (assertion/evidence example) explicitly calls for consistency checks for missing or stale evidence links; `docs/technical/record-schemas.md:33` requires mechanically checkable body/projection disagreements to produce diagnostics. This is a concrete machine-checkable mismatch, not an attempt to infer arbitrary prose semantics.

**Impact:** a clean health check can conceal broken or misleading evidence navigation in an Obsidian-readable assertion. Current support still comes from authoritative evidence IDs; no incorrect support activation was observed.

**Suggested work:** resolve supplied evidence links deterministically and report missing, ambiguous, wrong-kind, and wrong-assertion targets. Keep the list nonauthoritative; do not require the optional list to exist or turn extra prose into graph authority. Add diagnostics tests independently from current-support eligibility tests.

## Evidence and audited surface

Full command arguments, exit codes, and JSON envelopes are retained at `/private/tmp/lwiki-storage-audit-d1pjhas8/log.json`. Disposable vaults (`conflict`, `evidence`, `duplicate`, `reused-duplicate`) and saved author/journal bytes remain beside it. No live network calls occurred; all CLI invocations included `--offline` and reported `network_used: false`. Reproductions used existing native release bytes, not an unbuilt source assumption.

The audit read AGENTS, execution playbook/state, `storage.md`, `record-schemas.md`, and the actual `docs/wiki-format.md` (there is no `docs/technical/wiki-format.md`). It inspected the implementation and relevant test cases for:

- Bounded YAML events, exact types, duplicate keys, BOM/LF/CRLF handling, immutable kinds, conservative lossless field edits, and typed/untyped reference handling.
- Vault containment, portable path collisions, nested-vault exclusion, symlink refusal, normalized-source ownership, and captured frontmatter exclusion.
- Capture/refresh/revision reuse, original and content hash validation, withdrawal, exact quotations and UTF-8 spans, evidence successors, source ownership, and same-source revision retention.
- Before/proposed payload retention, manifest checks, read preconditions, journal framing/checksums, guarded apply order, final validation, immutable revision-tree ownership, terminal receipts, forward recovery, abort, and inverse planning.
- Catalog canonical membership, duplicate reservations, companion references, source integrity, support freshness, transitive description dependencies, entity identity versus description, SQL projection reconstruction, pinned generations, FTS publication, and cache rebuild/migration gates.

No additional concrete defect was established in those reviewed mechanisms. In particular, inspected code rechecks source bytes rather than timestamps; source payloads in managed revision trees do not supply canonical IDs; SQL rows are compared against their pinned projection and verified reads against canonical files; current support and historical labels remain distinct; unfamiliar target bytes are preserved by recovery. These are bounded source observations, not a substitute for the root's test results.

**Coverage limits:** this worker did not rerun recovery fault matrices, process-kill tests, broad Rust tests, native Windows/Linux tests, permission-adversary races, power-loss tests, large-vault performance tests, or Obsidian GUI behavior. Root serializes the test suite and will report its actual result. Provider/accounting, retrieval ranking, research handoffs, and graph-decision semantics have separate audit ownership. Windows write refusal is the known deliberate durability limitation, not a new finding. A noncooperating editor changing bytes after verification and hostile concurrent path replacement remain explicitly outside the initial concurrency model. None of these limits waive S1–S3.

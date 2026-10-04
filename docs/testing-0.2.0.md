# Try the 0.2.0 candidate

This build adds [selected search citations](validation-verified-search.md), normalized Page authoring, individual source capture/refresh/
withdrawal, bounded catalog reconstruction, context-selection replay and a
[resumable local collection importer](source-imports.md). The first capacity
target is now 25,000 documents / roughly 2.5 GB of text. Capacity and unseen
retrieval completeness remain unqualified; the version number does not certify
those gates.

The candidate now has an actual [1,000-document collection control](validation-1k-collection.md):
import, interrupted resume, refresh/withdrawal, recovery and complete-cache-loss
rebuild succeeded. The broader frozen assessment is 8.5/10 and remains failed:
cached search supplies no source citations, and the initial preparation preview
was unrun. Context citations passed; two distant-fact tasks still missed required
information after churn. The new explicit `search --verify-selected` workflow separately passed at 9.5/10
with no observed correctness blocker; it preserves search contents and does not
fix those missing facts. This is a local trial with those limits visible.

The current candidate is a native macOS ARM64 release build, compiled with Rust
optimization level 3 on macOS 26.5.2, with minimum macOS deployment target 26.5.
Other native platforms require their own qualification. Windows vault writes
remain unsupported. Set `LWIKI` to the supplied executable's absolute path.

## Disposable collection walkthrough

Run this against a new fixture, then inspect the returned Source/Revision IDs:

```sh
"$LWIKI" --version
"$LWIKI" --offline --json capabilities
DEMO=$(mktemp -d)
printf 'Atlas shipment contains 17 amber crates.\n' > "$DEMO/shipment.txt"
printf 'Atlas inspection begins at 06:40.\n' > "$DEMO/inspection.txt"
printf '%s\n' '{"path":"shipment.txt"}' '{"path":"inspection.txt"}' > "$DEMO/inputs.jsonl"
"$LWIKI" --offline init "$DEMO/wiki" --title '0.2.0 walkthrough'
"$LWIKI" --wiki "$DEMO/wiki" --offline index rebuild --normalized
"$LWIKI" --offline --json source import prepare \
  --input-list "$DEMO/inputs.jsonl" --output "$DEMO/import.jsonl"
"$LWIKI" --wiki "$DEMO/wiki" --offline --json source import run \
  --manifest "$DEMO/import.jsonl" --key atlas --group-size 4
"$LWIKI" --wiki "$DEMO/wiki" --offline --json source import status --key atlas
"$LWIKI" --wiki "$DEMO/wiki" --offline search 'Atlas shipment' --verify-selected
"$LWIKI" --wiki "$DEMO/wiki" --offline context 'Atlas shipment' \
  --max-bytes 6000 --max-tokens 1500
"$LWIKI" --wiki "$DEMO/wiki" --offline check
```

The two originals retain separate immutable histories and share one committed
Change. Repeating `source import resume --key atlas` returns the existing
mapping. Use the [import guide](source-imports.md) for updates, interrupted runs
and backups, and [indexed context](indexed-context.md) for supported query modes
and verification boundaries. Normalized catalogs currently support plain
lexical discovery and document context; other modes can refuse explicitly.

## Validation boundary

The grouped importer checks cover 168 distinct native unit tests, supplemented
by three adapter/presentation checks, thirteen focused recovery/replay checks
and the existing offline CLI and source-evidence integration targets. Passing
unchanged checks were reused rather than rerun as a full suite. An independent
walkthrough passed 109 public CLI calls and 289 assertions on disposable vaults
in both storage layouts. Its interrupted-state extension reached 173 calls and
524 assertions with one public recovery failure after a successful resumed
import. The narrow historical replay correction passed its native checks;
independent replay of that exact preserved failure passed eighteen further CLI
calls and seventy valid assertions. The scoped assessment is **9.5/10 with no
observed correctness blocker**; resource qualification remains open. The earlier
plain-text diagnostic omission was corrected and publicly replayed.

A controlled four-input comparison preserves exact manifests, inputs, original
baselines and cited readiness. Grouping four inputs uses one publication rather
than four, and intercepted file/directory sync calls fall from 1,124 to 708.
This is one operation-count comparison, not a latency distribution or capacity
claim. SQLite and direct sync paths are excluded from those counters. Peak RSS
and launch free-space observations were unavailable; this comparison does not
qualify resource bounds.

The [scale protocol](testing-large-vaults.md) and retrieval evaluation retain
their own acceptance gates. Strict Clippy remains failing. Native returned-error
reopen tests do not establish power-loss safety, and local fixtures do not
qualify a live provider.

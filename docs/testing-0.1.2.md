# lwiki 0.1.2 test-agent guide

Use an **explicit path to the 0.1.2 binary** and a fresh disposable vault. Keep the binary, `build-info.json`, JSON envelopes and test vault until findings are reviewed. This guide covers B1–B13 from the deep manual report and retains the earlier provider/graph regression workflow through the links below. Windows vault writes remain unsupported.

## Download and verify

Choose `aarch64-apple-darwin` for Apple Silicon, `x86_64-apple-darwin` for Intel macOS, `aarch64-unknown-linux-gnu` or `x86_64-unknown-linux-gnu` for Linux GNU, and `aarch64-pc-windows-msvc` or `x86_64-pc-windows-msvc` for Windows. All archives use `.tar.gz`; no musl builds are included. While the release is a draft, authenticated GitHub access as a repository collaborator is required.

```sh
TARGET=aarch64-apple-darwin
DOWNLOAD=$(mktemp -d "${TMPDIR:-/tmp}/lwiki-0.1.2.XXXXXX")
gh release download v0.1.2 --repo joshgarnett/rust-llm-wiki \
  --pattern "lwiki-0.1.2-$TARGET.tar.gz*" --dir "$DOWNLOAD"
cd "$DOWNLOAD"
shasum -a 256 -c "lwiki-0.1.2-$TARGET.tar.gz.sha256"  # Linux: sha256sum -c
tar -xzf "lwiki-0.1.2-$TARGET.tar.gz"
export LWIKI="$DOWNLOAD/lwiki"
"$LWIKI" --version
cat build-info.json
codesign --verify --strict --verbose=4 "$LWIKI"     # macOS only
```

Expect `lwiki 0.1.2`, matching package version/target, `source.dirty: false`, and the release source commit in `build-info.json`. Use the downloaded file before installing it. On Windows, use `gh release download` for the chosen target, `Get-FileHash -Algorithm SHA256`, `tar -xzf`, then `lwiki.exe --version` and `lwiki.exe --json capabilities`; compare the hash to the companion checksum. Do not attempt vault writes there.

From a checkout containing this release, run `python3 scripts/manual_smoke.py --binary "$LWIKI"`. Expect 30 offline commands to pass, with `network_used: false`. The script creates a disposable vault and prints its retained JSONL log path. Then run the deeper checks below. For full source/refresh/extract/import/resolve/review/withdrawal instructions and the four live gateway probes, follow the [earlier lifecycle and provider checks](testing-0.1.1.md#live-gateway-regression-tests) with **this 0.1.2 binary**; use the download steps above, not the older release assets.

## Disposable local setup

On macOS/Linux:

```sh
export LWIKI=/absolute/path/to/0.1.2/lwiki
"$LWIKI" --version
"$LWIKI" --json capabilities
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/lwiki-deep.XXXXXX")
VAULT="$TEST_ROOT/vault"
"$LWIKI" --json init "$VAULT"
lw() { "$LWIKI" --wiki "$VAULT" --json --offline "$@"; }
```

Use `lw` only for local checks. A live embedding test needs an explicit, separately authorized provider config and commands without `--offline`. Windows vault mutation remains unsupported; test native help/capabilities there, not write workflows. For each failed command retain the exact invocation, exit status, JSON `error`, and whether canonical files changed. `meta.network_used` should be `false` in every offline command.

| Finding | What to verify below |
| --- | --- |
| B1–B2 | Explicit/lexical passage selection, omission warnings, duplicate coverage |
| B3 | Original-only and empty capture indication |
| B4 | Complete lexical phrase wins over early partial matches |
| B5–B6 | Honest completion reason and local-only offline follow-up |
| B7–B8 | Refresh title and intentional byte-exact literal search |
| B9–B10 | Specific submission validation and distinct lock timeout |
| B11–B13 | Accepted assertion opposition, embedding bounds, claimed retrieval time |

## Offline source and retrieval checks (B3, B4, B7, B8)

```sh
printf '<html><body>HTML-ONLY-731</body></html>' > "$TEST_ROOT/page.html"
printf '\377\376' > "$TEST_ROOT/invalid.txt"
printf '\000\377\000' > "$TEST_ROOT/data.bin"
: > "$TEST_ROOT/empty.txt"
printf 'OLD-CONTENT-731\n' > "$TEST_ROOT/first.txt"
for name in page.html invalid.txt data.bin empty.txt first.txt; do
  lw source add "$TEST_ROOT/$name" > "$TEST_ROOT/$name.add.json"
done
```

`page.html`, `invalid.txt`, and `data.bin` must report `data.extraction_status: "unsupported"`, `data.citable: false`, and a warning that only original bytes were captured and the content cannot be searched or cited. Their revision has `wiki_extraction_status: unsupported`, exact `original.bin`, and no `content.md`. HTML parsing is still unsupported; do not expect HTML body extraction. `empty.txt` must report `complete`, `citable: false`, an empty-source warning, and zero-byte original/content files. `first.txt` must report `complete`, `citable: true`, without an extraction warning. `lw --dry-run source add FILE` should give the same indication but leave the vault's canonical bytes and modification times unchanged.

```sh
SOURCE=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["data"]["allocated_ids"]["source"])' "$TEST_ROOT/first.txt.add.json")
printf 'NEW-CONTENT-928\n' > "$TEST_ROOT/renamed-file.txt"
lw source refresh "$SOURCE" --file "$TEST_ROOT/renamed-file.txt" > "$TEST_ROOT/refresh.json"
lw read --id "$SOURCE"
lw search OLD-CONTENT-731 --mode literal
lw search NEW-CONTENT-928 --mode literal
lw source refresh "$SOURCE" --file "$TEST_ROOT/renamed-file.txt" --title 'Chosen source title'
lw read --id "$SOURCE"
```

The first refresh must retain the source's original title on both the head and new revision despite the new filename. The old revision stays immutable; default current search has zero hits for `OLD-CONTENT-731` and one verified current content-path hit for `NEW-CONTENT-928`. Explicit `--title` with unchanged bytes reuses that revision and changes only the source head title; the immutable revision title stays as captured. A title-only query may return distinct source/revision manifest paths. Those are metadata hits; a unique content token must not duplicate the current payload. Literal search intentionally matches **exact UTF-8 bytes and case**, including punctuation; use lexical mode for tokenized matching. Do not treat a literal case or punctuation miss as a regression.

For B4, make a long text source with at least 70 early partial mentions of `Hungry`, followed by a later contiguous `Hungry Howie` phrase:

```sh
python3 - "$TEST_ROOT/restaurant.txt" <<'PY'
import sys
with open(sys.argv[1], "w", encoding="utf-8") as out:
    out.write(("Hungry visitor stopped here.\n" * 70) + "The exact name is Hungry Howie.\n")
PY
lw source add "$TEST_ROOT/restaurant.txt"
lw search 'Hungry Howie' --mode lexical --excerpt-bytes 400
```

The selected excerpt should show the complete phrase, even though early partials exceed the 64-match candidate cap. This is lexical excerpt selection; literal mode has different exact-byte matching semantics.

## Host-agent research handoff (B1, B2, B5, B6, B9, B13)

Create a separate fresh vault and follow [the 0.1.1 research workflow](testing-0.1.1.md#exercise-a-real-agent-research-workflow) to save `research run`, collection import, answer packet, answer import, status and report envelopes. The host, not lwiki, obtains material through authorized tools. For a fully offline follow-up, use already acquired local material and do no external acquisition; a new offline research run and local follow-up are valid persisted handoffs. A suggested URL is a host claim, not a built-in fetch instruction.

Use multiple explicit `--source-id` values, including a long source whose relevant answer occurs after byte 4096. Inspect the returned packet's `passages` and `warnings`. Selected passages must carry distinct packet-local `p1`, `p2`, … IDs and exact source/revision/span/quote hashes; answer claims cite passage IDs, never source IDs or quote hashes. The packet is capped at 32 passages and 65,536 quoted bytes. Omitted candidates must be named in warnings, including explicit sources excluded by the cap; after withdrawing higher-priority sources and refreshing, previously omitted captures must become candidates again; the same source must not appear twice as both a covered explicit passage and a lexical subset. Refresh a source and explicitly request a new packet to check selection against the new revision. Also withdraw an explicitly scoped source, refresh, and import a valid new collection; this must succeed without reintroducing the withdrawn passage. A valid newly imported current source should not disappear merely because earlier candidates filled the packet. Retained old revisions are historical, not current evidence.

For collection provenance, add an optional field to one inline source:

```json
"retrieved_at": "2026-09-29T10:11:12-04:00"
```

After import, its **new immutable revision** should contain `origin_retrieved_at` with that exact string and `origin_retrieved_at_kind: agent-claimed`. `wiki_captured_at` is the local capture clock and need not equal it. A sibling source without the field should have no claimed retrieval time. The original text bytes must match the submission exactly. For B9, an invalid timestamp, more than 65,536 inline source bytes, NUL content, empty claim text, unknown passage ID, quote hash used as a passage ID, or source ID used as a passage ID should fail with a field-specific validation message before new canonical captures. Replay an identical accepted submission: expect the same IDs and no duplicate capture. The claimed timestamp does not prove the host actually visited a URL.

Finish one answer with an honest nonempty `gaps` list and `follow_up: null`. The report should retain the gaps, set `partial: false`, and say `completion_reason: agent_finished`; terminal status has no next packet. A requested follow-up should say `follow_up_requested`, be partial while work remains, and expose a next collection packet. A round-limit stop should say `round_limit` and be partial with a limit gap. These flags describe requested continuation, not whether the answer is perfect. A local-only offline follow-up may continue with existing material; it must not trigger network use. Check that a gap resolved in a later answer leaves the current report while remaining in retained history.

Run `lw research status RUN_ID`, `lw research report RUN_ID`, and `lw research resume RUN_ID` after import. Failed validation or a stale packet must not silently create another source. Preserve exact error/warning codes and IDs. The CLI does not run a provider for research, and local packet/source bounds do not account for host tool costs.

## Lock and graph checks (B10, B11)

The default writer-lock wait is 5,000 ms. A genuine contention timeout should return `LOCK_TIMEOUT`, exit 4, `retryable: true`, and identify the timeout; it should not return an opaque content conflict. Exercise this with two disposable processes or run the repository's lock integration regression from the candidate checkout. Do not force or remove another process's lock. This is distinct from a stale expected-hash conflict.

For graph opposition, use the [0.1.1 graph lifecycle](testing-0.1.1.md#live-gateway-regression-tests) or a local agent extraction to create two **accepted and currently supported** assertions with the same subject, predicate, object/literal, property, unit, modality, and validity interval, differing only in negation. Query and inspect their assertion edges. Both should be `disputed`; each should list the other's exact record reference in `opposing_assertions`. `contradictions` remains for explicit contradicting evidence and should not be fabricated by the opposite proposition alone. A different object/date/modality, proposed or rejected assertion, or withdrawn support should not create a current opposing link. For `has_property`, read the property name from `qualifiers.property`; there is no top-level property field. If more opposites exceed the query's contradiction limit, inspect `omitted_opposing_assertions` and top-level truncation/warning.

## Optional live embedding check (B12)

Only if live provider use is already authorized, use a separate small vault, a private provider file, and the [0.1.1 live gateway setup](testing-0.1.1.md#live-gateway-regression-tests). Do not put credentials in reports. Before any paid sync, run `lw embeddings check` and `lw --dry-run embeddings sync --max-input-bytes 12000 --quality-target-bytes 3000`; neither should call the provider. A dry-run shows **requested** settings and cannot establish what the earlier active generation used. Check the effective settings in `data.settings` on the local check/active report; keep the active saved settings, source bytes, membership spans and input hashes when diagnosing a surprising unit count.

The owner observed six units from **five current sources and no pages**, with no `--max-input-bytes` override. The default is 12,000 bytes and no quality target, so a 15,254-byte source could legitimately contribute two units. The cache was later deleted, preventing inspection of that generation's saved settings and memberships. There is **no confirmed truncation**. B12's product preference is to consider more heading-aware splitting for a multi-section document; the separately confirmed heading-boundary/ancestry bug has been fixed in the local renderer.

For a new disposable approximately 15,254-byte UTF-8 source with 17 H2 sections and a unique final-tail marker, request `--max-input-bytes 12000 --quality-target-bytes 3000` consistently for check and authorized sync. Expect several bounded units, complete contiguous source-body byte spans without gaps/overlap, the final marker represented, and no formatted input over 12,000 bytes. Do **not** require exactly 17 vectors: short sections can share a unit, and equal complete inputs can reuse one vector. A larger effective bound can legitimately render one complete unit. Repeat sync should reuse unchanged vectors; compare the active saved policy with the policy requested now. A mock/local renderer result does not prove live gateway compatibility or explain the owner's earlier six-unit observation without that run's original settings and artifacts.

## Return evidence

Report the candidate full source commit and dirty flag once supplied, binary path/version/hash, OS/architecture, commands and JSON envelopes, exact source/revision/run/change IDs, passed/failed checks, unexpected current versus historical paths, and tests not run. Redact credentials and private content, but retain a synthetic reproduction and the relevant byte counts, timestamps, settings, and warning/error text. Mark live-provider behavior separately from offline results. This handoff itself records planned checks, not completed test results.

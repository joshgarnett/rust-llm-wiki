# Occupied 10k workflow diagnostic

A frozen native 0.2.0 diagnostic **failed** its final dry-run read deadline.
Completed import, update, cited search/read and recovery stages are useful
measurements; they do not turn the whole diagnostic into a pass or qualify the
first 25,000-document / approximately 2.5 GB target.

## Scope and completed workflow

The release executable came from accepted source `d4565cd`, compiled at
optimization level 3 for native macOS ARM64. Its SHA-256 was
`6f2c88a2da472d3f685880d354d3842a939ebb92ebc4786d0212239509e5467b`.
One supervised owner ran 1,422 CLI calls in 1,094.934 seconds. No providers,
model-generated answers or production vaults were involved.

Sixty-four independently authored, varied UTF-8 notebooks totaling 6,419,197
bytes were publicly imported into owned 1k and generated 10k fixtures. The
occupied fixture reached 10,064 Sources. Its generated seed demonstrates
occupancy, not public import of 10k representative documents. Expected facts
and task labels stayed outside indexed content.

Seven positive and one absent-information task ran three times before changes,
three times after changes and once after complete cache loss/reconstruction.
All 49 positive outcomes were complete; all seven absent outcomes were safe.
Two tasks exercised identity/navigation and five exercised captured evidence.
The fixed continuation recipe used five lexical hits, 80 candidates, 1,024-byte
excerpts and at most 32 reads of 8 KiB / 256 KiB per task. Broad tasks often used
all 32 reads. This larger manual evidence workflow is separate from automatic
HIGH and generated-answer evaluation.

Independent audit checked 1,500 citations, 1,313 captured read ranges and 1,183
continuation warnings against exact canonical bytes, source/revision ownership,
UTF-8 boundaries, full hashes and quote hashes. The interruption/resume preserved
all eight pending IDs and capture timestamps. No-op/title edits preserved
revisions; content refresh created a revision; withdrawal excluded Current
search while retaining historical access. Complete backup, cache-only removal,
offline rebuild and five full checks passed their recorded mechanics; each
check returned zero diagnostics. Human output and quoted continuation passed.

## Observed cost

| Operation | Observed elapsed time |
| --- | ---: |
| Successful warm task calls, p95 | 0.0601 seconds |
| Maximum successful query call | 0.4112 seconds |
| No-op / title / content / withdrawal | 0.033 / 0.342 / 0.554 / 0.293 seconds |
| Cache-loss rebuild | 82.458 seconds |
| Occupied full checks | approximately 101–103 seconds |
| Failed final dry-run read | 15.0208 seconds; no returned result |

These are owner-monotonic, supervised intervals on a MacBook Pro M1 Max,
32 GiB RAM, macOS 26.5.2. Successful-call statistics exclude the failed dry-run.
Observed native bulk peak RSS was 693,305,344 bytes; sampled process-tree peak
was 694,517,760 bytes. Exact tree peaks and native peak RSS for the killed
preview are unavailable. Python-owner memory is outside those figures.
Accounted allocation reached 17,913,069,568 bytes; minimum observed free space
was 78,517,633,024 bytes. Logical work counters were unavailable, so no scaling
ratio or projected 25k performance is claimed.

## Failure and acceptance boundary

The final `--dry-run read --path ... --max-bytes 8192` exceeded its unchanged
15-second deadline. Its legacy fallback projected the whole vault before
reading the selected path. The following same-size/same-mtime external-edit
refusal control was unrun. Independent endpoint inventories found the timed-out
preview left the whole owned tree unchanged, including cache, and both original
fixtures retained exact membership, bytes and mtimes. Preservation does not
establish completion. The frozen diagnostic remains **FAILED**.

The correction is a separately assessed request-only CLI preview that returns
before catalog/target access and explicitly leaves body, target resolution,
UTF-8 endpoints and freshness unknown. Its acceptance must replay the failed
and remaining controls under the same 15-second ceiling. The prior diagnostic
will remain failed after a correction passes. See the
[reading contract](indexed-context.md#continuing-a-captured-source-read).

Actual 25k representative public import, full task/churn/resource gates,
automatic HIGH, semantic/hybrid parity and generated answers remain open.
Detailed receipts and independent reports are retained locally; this summary
works in a fresh checkout without those ignored artifacts.

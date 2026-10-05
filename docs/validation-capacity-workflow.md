# Capacity workflow checkpoint

The preserved 0.2.0 native macOS ARM64 candidate completed the setup and import
portion of a new collection control on 2026-10-05: **1,000 captured Sources and
1,000 authored Pages**. This checkpoint precedes its frozen retrieval, churn and
cache-loss tests. It does not establish 10k/25k capacity, HIGH retrieval acceptance
or a whole-workflow score.

Executable SHA-256:
`c9eaf97e93f905300861175c3fe8319b5560dc16bb8d8e048913a9ea2cf6ba78`.
Compiled source: `38b723bda4e80068a550f71e17709fe565e74762`; native release
optimization level 3, Rust edition 2024. Hardware: Apple M1 Max, 32 GiB RAM,
APFS, macOS 26.5.2. No build, provider calls or production vaults were involved
in this run.

## Completed public workflow

The corpus contains 998 varied synthetic UTF-8 documents and two pinned licensed
Cargo documents, totaling **100,302,609 bytes**, with 1,000 distinct content
hashes. Synthetic documents vary around 80–120 KB and place facts at separated
positions. Question labels and expected answers remain outside indexed content.

Sixteen public captures supplied the graph bootstrap. Its 160 public commands
created 32 Entities and 16 reviewed literal-property Assertions; an independent
audit checked their 16 exact citations and 48 decision records. This qualifies
fixture storage and review mechanics, not ownership navigation or graph relevance.

After a complete backup, guarded storage migration and normalized activation
passed. Import preparation preview reported the remaining 984 inputs while
preserving all 866 existing vault files' bytes, sizes and modification times,
and creating no manifest. Actual preparation then succeeded.

The importer committed 62 four-item groups before the planned SIGKILL, retaining
four pending allocations at ordinal 248. Three same-key resume windows completed
all 984 inputs in 246 groups. All four pending Source/Revision IDs and capture
timestamps survived. All 2,000 canonical originals and extracted content files
match the generated input sizes and SHA-256 hashes.

All 63 Page batches passed. The 1,000 Pages have distinct stable identities and
byte-exact authored input, including the separately counted navigation overlay.
An unchanged sync reused publication 310. Full `check` completed with zero
diagnostics, canonical/cache agreement and revision-owner history checked.

## Measurements and next gate

| Operation | Observed supervised elapsed time |
| --- | ---: |
| Interrupted import plus three resume windows | 995.437 seconds |
| 63 Page batches | 213.283 seconds total |
| Unchanged sync | 2.923 seconds |
| Full check | 16.043 seconds |

These locally owned monotonic intervals include process supervision. The separate
aggregate clock also includes generation, reviews, audits and waits; summed
command times do not describe the whole session. A controller terminal input
limit was corrected before the affected product command launched; the same
aggregate clock and pinned executable were retained.

All 253 setup CLI attempts were recorded, including intentional process
termination. No memory-sampling errors occurred. Largest native main-process RSS
was 197,558,272 bytes; largest sampled process-tree RSS was 197,492,736 bytes.
Sampling does not establish an exact process-tree maximum. Account allocation
after import was 1,631,723,520 bytes, including retained inputs and the backup.

Import throughput projects beyond the four-hour 25k target; this is an
extrapolation, not a completed large-tier measurement. Unchanged sync also read
406,750,732 bytes across 5,149 inputs, despite reusing the publication. Its large
vault cost needs qualification. A bounded comparison of supported four- and
eight-item groups is the next performance investigation after this control,
before considering storage changes. No larger corpus is admitted automatically.

Frozen search/context/read tasks, source updates, withdrawal, complete-cache-loss
rebuild and independent acceptance remain pending. The earlier
[collection control](validation-1k-collection.md), accepted
[selected search](validation-verified-search.md), and failed
[allocation experiment](validation-location-allocation.md) remain separate.

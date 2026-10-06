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

The frozen initial query trial subsequently stopped on its 44th task. Forty-three
tasks completed their search/context/conditional-read sequence; the next selected
search returned `CAPABILITY_UNAVAILABLE` for an ordinary deployment-lead question.
The failed command returned no data, and the supervisor stopped without a retry.
All 127 query attempts are retained: 44 searches, 43 contexts and 40 reads. The
126 successful commands and one refusal took 37.865 supervised seconds total;
the longest took 0.477 seconds. These are command intervals, not session turnaround
or warm p95 qualification. The owning clock terminated after 5,738.717 seconds,
including setup, reviews, audits, host transport and waits.

A separate one-call diagnostic on a complete disposable copy identified the
trigger: the ten discovery hits included nine Current source payloads and one
Entity with Current identity and an unsupported description. The Entity excerpt
was empty and uncited. The selected-search guard conflated identity navigation
with body-evidence authority and rejected the whole page. This question is now
development data; the failed run remains immutable.

Independent partial assessment found all 547 returned citation occurrences and
40 selected read ranges byte-correct and bound to their current revisions.
Evidence was complete for 33 of 37 completed positive tasks; including the failed
positive gives 33 of 38 attempted positives. Six completed negative tasks were
safe. Four completed positives still missed facts despite correct citations.
The successful prefix does not qualify the frozen workflow or admit another tier.

Remaining repetitions, unstarted questions, Cargo overlays, source updates,
withdrawal and complete-cache-loss rebuild are unrun in this control. The earlier
[collection control](validation-1k-collection.md), accepted
[selected search](validation-verified-search.md), and failed
[allocation experiment](validation-location-allocation.md) remain separate.

## Separate native 256-current lifecycle

A separate staged control on 2026-10-06 uses the packaged candidate013 0.2.0
CLI, SHA-256
`64744ed8c8f0bf236c0dc169db4f6a7fbc79327bb015dcb931861f7c2faecaf1`,
on native macOS ARM64 with release optimization level 3. It extends a complete
clone of the accepted 64-Source draft workflow; the earlier 1,000-Source control
and its failed query trial above remain separate.

The starting vault has 64 retained Sources, 63 current and one withdrawn. Import
of 193 distinct frozen UTF-8 originals creates **256 current Sources and 257
retained Sources**. Both captured original and extracted Markdown copies match
every input's full size and SHA-256. A controlled, same-size edit of one newly
imported Source creates a distinct immutable revision; withdrawal then leaves
255 current and two withdrawn Sources. The full original vault remains unchanged.

All 72 continuation commands complete: accepted current/historical range reads,
the existing cited draft's read and title discovery, all twelve ordinary lexical
search/context pairs, guarded unchanged draft replacement, actual obsolete-hash
refusal, source refresh and withdrawal, complete-cache relocation and normalized
rebuild, plus full checks before and after rebuild. The draft's identity, body,
author notes and hash survive. All existing peer/history files remain unchanged
through the new Source's churn. Exact range-read data and same-tier post-churn
D01/D09 search/context packets match after reconstruction, apart from declared
publication/time/work fields. This does not qualify cursor continuation across
reconstruction.

| Operation | Owning supervisor elapsed time |
| --- | ---: |
| Import 193 originals in 49 default four-item groups | 66.733 seconds |
| 24 ordinary lexical search/context commands | 0.057–0.128 seconds each |
| Refresh one 110,426-byte Source | 0.653 seconds |
| Withdraw that Source | 0.290 seconds |
| Complete-cache-loss normalized rebuild | 1.704 seconds |
| Full occupied / rebuilt checks | 2.739 / 2.530 seconds |

The continuation finishes in 97.402 owning seconds. One preserved predecessor
read returns correct application output but its optional `time -l` wrapper
fails because the sandbox denies `sysctl kern.clockrate`. A prospectively admitted
continuation removes that wrapper, reuses the verified clone and repeats only
that failed invocation: **73 cumulative CLI invocations**, including the original
measurement failure. Cumulative owning execution is 100.874 seconds; external
review/launch gaps are separate. The original deadline and cumulative resource
limits remain in force. RSS and physical I/O are unavailable.

Sampled new owned allocation peaks at 397,766,656 bytes, including the complete
clone and retained/rebuilt caches; sampling is not an exact instantaneous peak.
The final free-space observation is 58,310,987,776 bytes. Supervisor logical
inspection totals 2,388,645,742 bytes across both attempts. Public decoder work
and canonical verification counters remain separate from that inspection.
These bytes are logical work, not measured physical I/O.

Independent actual acceptance passes at **9.2/10**, every scoped mandatory
observation and zero correctness blockers. The critic authenticates 164 SourceRef
occurrences (105 distinct references), including all ten references embedded in
the saved draft, against exact immutable source slices and history membership.
The original instrumentation failure remains failed. The critic completed its
report 28 seconds beyond the separate 240-second review allowance; that process
timing gate failed, and the scoped correctness judgment is reported separately.
This control uses lexical commands,
no embedding reacquisition, answer actors or new build. Ordinary relevance is
observational; the previously failed default completeness gate, semantic
readiness, unseen HIGH and practical 25K/~2.5GB capacity remain open. Import
accounts for most measured execution time; query latency supplies no current
reason to optimize decoder counters. Growth to 1,024 current Sources requires
separate admission using the complete workflow and measured resource headroom.

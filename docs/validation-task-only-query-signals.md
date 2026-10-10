# Task-only query reformulation: rejected development experiment

One question-only model reformulation improved aggregate evidence coverage but
**failed acceptance**. It completes 16/32 positive tasks, below the required 17,
and loses three baseline-supported facts. The method is closed without tuning,
retry or promotion. Default native context remains 13/32 complete and 79/129
supported facts; no production retrieval behavior changed.

The fixture contains 994 captured public documents and 5,582,095 UTF-8 bytes from
WixQA, ConditionalQA and QASPER. All 40 development questions remain in the
evaluation: 32 positive tasks, 129 required propositions and eight absent controls.
Six withheld Sources were not imported; the 24-question holdout remains sealed. The independent
Astra critic froze the gate and grading rules before candidate output.

One fresh `gpt-6.1-sol` planner received only the original question IDs/text and
fixed instructions. It returned one query per question, without source contents,
labels, previous answers, selectors, answer generation or fan-out. The original
question remained the critic's task. Native context changed only its query
argument: offline lexical discovery, automatic indexed-documents scope, ten
owners, 80 candidates, 1,024-byte excerpts, 12,000 rendered bytes and 3,000
estimated tokens. Proof limits stayed at 64 MiB, 4,096 files, 16,384 entries and
2,000 milliseconds. Generated vocabulary earned no evidence credit.

| Observation | Original native baseline | Assisted query candidate |
| --- | ---: | ---: |
| Complete positive tasks | 13/32 | 16/32 |
| Complete support tasks | 11/20 | 13/20 |
| Complete long-document tasks | 2/12 | 3/12 |
| Supported facts | 79/129 | 97/129 |
| Baseline facts preserved | 79/79 | 76/79 |

The candidate gained 21 facts and lost three; four tasks became complete and one
formerly complete task regressed. Losses concern product approval duration,
the automation editing procedure and the radically altered vehicle definition.
All eight scoped absent requests remain unsupported. No answer actor was used,
so this does not measure answer-level abstention. A separate planner deviation
introduced answer-like tool names absent from one question; the acceptance gate
fails independently of that interpretation.

All 40 native commands succeeded. All 330 emitted citations authenticate exact
original bytes, immutable Source/Revision identity, ranges and BLAKE3 hashes.
Publication generation 126 and all 8,834 canonical/retained files remained
unchanged. Freshness verifies selected documents only; global membership remains
unverified. Baseline binary, source mappings, original outputs and invocation
bounds were authenticated, so unchanged baseline commands were not repeated.

The existing macOS ARM64 executable used release optimization 3, debug 0 and
source `3e2d2b4`; SHA256
`ba2b19d6032e55570b4cb7b1ccabf5e7b5ce7b1bc685498c7309720faf27c238`.
Native owner-process intervals total 3.361609 seconds, with nearest-rank p95
0.106852 seconds and maximum 0.161046 seconds on this small fixture only.
The entire 40-query planner batch took 101.00834 observed UTC seconds, including
queue, tools and orchestration; this is not per-question inference latency.
Known prompt/question/dispatch payload was 13,553 bytes, and the unmodified query
file was 6,655 bytes. Hidden harness/tool overhead, tokens and monetary cost are
unavailable. Citation authentication separately used 330 helper calls over
7.776159 owner-process seconds. Experiment storage was below its 128 MiB ceiling.

The bounded artifact adapter passed nine synthetic checks for strict IDs/JSON,
UTF-8 and reply caps, refusal before execution, output limits and immutable
receipts. No Rust build, provider call, source mutation or unchanged test repeat
was needed. Native correctness and authenticated citations do not waive the
completion/no-regression failures.

For a new experiment, use the tracked [context protocol](evaluating-context.md)
and [dataset reference](eval-datasets-quick-reference.md), freeze the model stage
and all additional costs separately, and keep labels outside indexed content.
These known development results do not qualify native HIGH, unseen retrieval,
answers, Pages, representative 25K capacity or the full 0.2.0 release.

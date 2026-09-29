# lwiki 0.1.2

This patch addresses the deep research and edge-case testing against 0.1.1.

- Research packets name omitted material, remove covered duplicate passages, and recover previously omitted captures when refreshed. Publication keeps current citation checks and atomic handoffs.
- Completed research with honest gaps now reports `partial: false`; `completion_reason` identifies finished, follow-up and round-limit outcomes. Errors identify bad passage IDs, content bounds, NULs, empty claims and timestamps.
- Source capture warns when only original bytes are retained or text is empty. Refresh keeps the source title unless `--title` is explicit. HTML still requires host-extracted UTF-8 text.
- Lexical excerpts prefer a complete query phrase over an earlier isolated matching word.
- Writer contention returns retryable `LOCK_TIMEOUT`, separate from content conflicts. The default wait is 5 seconds.
- Matching accepted/current assertions with opposite negation are marked disputed and linked through bounded `opposing_assertions`. Explicit contradiction evidence stays separate; properties remain in `qualifiers.property`.
- Embedding segmentation fixes heading-boundary/ancestry errors, preserves all original bytes and enforces the formatted input bound. Local embedding checks expose effective settings. `--quality-target-bytes` remains available for finer units.
- Research captures accept optional host-claimed RFC3339 `retrieved_at`, retained on the immutable revision.

Offline follow-ups remain local-only, and literal search remains byte- and case-exact. The reported six embedding units for five sources were consistent with splitting the long article; no live truncation was established. Research still delegates tools and reasoning to the host agent.

Use the [full 0.1.2 download and test-agent guide](https://github.com/joshgarnett/rust-llm-wiki/blob/v0.1.2/docs/testing-0.1.2.md). It includes changed expectations, the offline smoke, all B1–B13 follow-up checks and links to the provider/graph lifecycle regressions. Windows artifacts support read-only checks; native vault writes remain unsupported. The older installed-copy macOS SIGKILL was not reproduced or diagnosed.

Local validation and six-platform release evidence are recorded in the execution reports. Live gateway quality remains owner-tested separately. This release stays a draft for artifact inspection.

# Refreshed-source graph review follow-up

The reported manual path refreshed a source, imported extraction from the new head, resolved its mentions, then failed to accept the assertion at graph review: `acceptance requires intact current supporting evidence`. The original manual vault is unavailable here; the regression uses a disposable vault and the existing CRLF graph fixture.

Review's closed validation input contained every canonical revision note but only the original/content assets for the evidence's selected revision. Catalog projection therefore classified an uncaptured, valid older revision as invalid. The source's retained-revision reference propagated that invalidity to otherwise current supporting evidence. The root captured the pre-fix failure in `.artifacts/live-review-red.log`: the new end-to-end regression failed with the reported review acceptance error.

`src/graph/review.rs` now includes original/content payloads for all revisions retained by each source named in the review evidence proofs. The extra reads share the existing bounded capture and become prepared-change read dependencies. This preserves rejection of absent or altered sibling assets and apply-time drift. No source freshness or eligibility rule was relaxed.

`tests/graph_review.rs` exercises refresh, head extraction/import, resolution, review acceptance, current eligibility, and graph query. It also checks that evidence from the old revision remains historical and cannot be accepted, and that a tampered old payload rejects both before review staging and after a successful stage without canonical writes.

Worker checks: `rustfmt --edition 2024 src/graph/review.rs tests/graph_review.rs` and `git diff --check` passed. The worker ran no Cargo or Bazel tests. The root's first post-fix Bazel attempt stopped before tests because sandbox policy denied local port binding and `sysctl`; the integrated gate was still pending at worker handoff.

Root integration: the approved native Bazel gate subsequently passed all 21 graph-review tests in 85.21 seconds (`.artifacts/agent-research-full.log` and `bazel-testlogs/graph_review_test/test.log`). The first green attempt exposed only a new negative fixture that had not resolved its old assertion; the fixture now resolves before refreshing. The standalone 30-command manual smoke also passed the user's exact Cedar refresh → extraction → resolve → review → query flow. These are disposable local tests, not live-provider qualification.

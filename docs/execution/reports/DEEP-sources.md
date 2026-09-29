# Deep source capture and refresh follow-up

Scope: `src/sources/**` and `tests/sources_evidence.rs` on the `c320da5` baseline. Root owns CLI/app wiring and build gates.

`SourcePlan.capture_state` now reports `Complete`, `Empty`, or `Unsupported` for capture and refresh; withdrawal reports `None`. The state exposes `extraction_status()` and `citable()`. Existing immutable `original.bin` and revision `wiki_extraction_status` behavior remains: unsupported extraction stores exact original bytes without `content.md`; valid empty UTF-8 stores zero-byte `content.md` but has no citable passage. The root should publish this state in mutation data and warn for unsupported and empty captures. HTML is deliberately unsupported by the CLI's file classifier; no HTML extractor was added.

`plan_refresh` now keeps the source's canonical title when a replacement filename differs. New `plan_refresh_with_title(id, request, explicit_title)` lets the CLI pass `--title` separately. An explicit title updates `source.md`; a new revision uses that title. If bytes/extraction are unchanged, a title-only edit changes only `source.md` and reuses the immutable revision. No title option plus unchanged content remains a no-op.

Regressions cover HTML/binary/invalid-UTF-8 unsupported capture, empty text, exact original bytes and manifest status, implicit filename preservation, explicit title-only update, and explicit title with new revision. `rustfmt --edition 2024` and `git diff --check` passed. This worker did not run Cargo/Bazel or mutate shared app/CLI/catalog files.

Duplicate-hit investigation: managed revision `content.md` is excluded from the ordinary Markdown note scan by `canonical_path`; the verified payload becomes one owned document row. Source and revision manifest rows share its title and are each indexed, so title-only search may show multiple current hits from one source. This is a catalog/retrieval presentation question, not a duplicate revision payload. The earlier two body-text hits in MANUAL-packets came from an operational extraction packet and were fixed there. No catalog change is proposed without reproducing a current body-token duplicate.

## Agent-claimed retrieval time follow-up

`plan_agent_capture(request, retrieved_at)` requires `AgentReport` origin and validates an optional RFC3339 timestamp before allocating the plan. It delegates to `plan_capture`, then adds `origin_retrieved_at` and `origin_retrieved_at_kind: agent-claimed` to the newly planned revision frontmatter. These are nonreserved provenance fields; `wiki_captured_at` remains the local capture time. No refresh path inherits a claimed time, and original/content bytes are untouched. A focused test checks the metadata, exact payloads, absence when omitted, invalid timestamps and wrong origin. Root owns `SubmittedSource` and app-engine wiring plus execution of the test gate.

## CLI regression follow-up

`tests/offline_cli.rs` now exercises the public offline JSON envelope for HTML, binary, invalid-UTF-8 `.txt`, empty `.txt`, and ordinary text. It checks extraction status, citable indication, warnings, immutable original bytes, absent/present content, and dry-run tree purity. A refresh test checks filename changes preserve source and revision display titles, explicit `--title` updates only source metadata when bytes are reused, and a unique current content token yields one payload hit while the old token yields none. A title-only literal query checks that source/revision manifest hits have distinct paths, clarifying the reported apparent duplicate. These tests have not been run by this worker; root owns the build gate.

## Test-agent documentation

Drafted `docs/testing-0.1.2.md` for the B1–B13 candidate. It gives an explicit binary path, disposable setup, source/refresh/search commands, research packet and provenance checks, submission negatives, lock and graph expectations, and optional authorized embedding qualification. It treats B8 literal matching as byte-exact, B6 offline follow-up as valid local work, and B12's six units for five sources as inconclusive for truncation. It does not name a new release, claim a run, or assert live compatibility; root will stamp the integrated candidate commit and review the guide after builds.

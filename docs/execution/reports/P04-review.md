# P04 independent source/evidence review

Reviewer: `gpt-6-astra`; prerequisite baseline `74cfb5838d32f1889bb8a094bc7fe0643390d85e`. Scope: source capture, revision views, exact evidence, lifecycle and original/proposed dependency separation. Read-only source review; exclusive write lease on this report. No Cargo commands, code edits, commits, provider calls, or nested delegation.

## Finding and resolution

**Final disposition: Q1 resolved after the second correction below. No remaining blocking finding in the reviewed P04 scope.**

### Q1 — Medium: nested Markdown quotation escapes the exactly-one fence check

- **Location:** `src/sources/evidence.rs:89–149`, especially the raw-prefix scan at lines 93–99; used by both `exact_quote_body` and citation verification.
- **Trigger:** call `exact_quote_body(b"exact", "\n", "> ```text\n> second quote\n> ```")`, or append that blockquote to an otherwise valid evidence body. CommonMark recognizes the added block as a second fenced `text` code block. Each added line starts with `>`, so `quotation()` skips every line and returns the first quote successfully. List-contained fenced quotations produce the same class of mismatch.
- **Expected:** reject evidence containing more than one fenced `text` quotation, including quotations nested in Markdown containers. The record-schema quotation contract makes this cardinality mandatory.
- **Actual:** the writer and read verifier count only raw fences beginning after zero to three spaces. They accept the second displayed quotation and therefore claim a valid evidence body despite its structural disagreement.
- **Minimal regression:** assert writer rejection for the exact explanation above; append it to a valid, applied evidence record and assert historical/current verification both reject it. Add a list-contained case. Preserve the existing CRLF/Unicode/embedded-backtick exact-byte tests. A structural pass using the already available Markdown parser can count fenced `text` blocks while the raw-byte pass continues to extract the exact separator-preserving quote; no rendered-text normalization is needed.
- **Evidence:** direct control-flow inspection. No unrun regression is represented as executed.

### Q1 bounded re-review — first correction incomplete

Root added a `pulldown_cmark` structural count and the blockquote/list regression. Inspected `/tmp/lwiki-p04-q1.log`: `nested_markdown_text_quotation_is_rejected_by_writer_and_verifier` passed (1 passed, 13 filtered, 1.81s). This reviewer ran no Cargo command.

The count must also bind the exact raw block selected for extraction to that structural block. The following body still passes both independent checks while quoting different displayed text:

````markdown
<!--
```text
exact
```
-->
> ```text
> different
> ```
````

The Markdown parser counts only the nested `different` block; the raw scan extracts `exact` from the HTML comment and skips all blockquote-prefixed lines. For a source slice `exact`, citation verification therefore succeeds. **Q1 remains open** pending correspondence between the sole structural code-block offset range and the raw quotation range. A conservative rejection of sole nested blocks is acceptable; a raw-looking fence inside an HTML/comment container must not substitute for the structurally selected quotation. Add the displayed body as an evidence-verifier regression.

### Q1 final bounded re-review — resolved

Root now uses `Parser::into_offset_iter()` to collect the actual fenced-quotation range, requires exactly one structural `text` block, and restricts raw extraction to that range. The HTML/comment decoy outside the selected block can no longer provide quotation bytes. Raw interiors and separator removal remain byte-based; nested container prefixes are not silently normalized. Inspected both new regressions: nested blockquote/list additions and the hidden-comment/alternate-displayed-quote body are rejected in both current and historical scopes.

Inspected root log `/tmp/lwiki-p04-q1-range.log`: `cargo test` source suite **15 passed, zero failed, zero ignored** in 8.52s (compile 2.27s), including the two new cases and existing Unicode/CRLF/embedded-fence exactness, source lifecycle, predecessor guard, and recovery cases. This reviewer independently inspected code/tests/log and ran no Cargo command. **Q1 resolved; no remaining blocker identified.** Repository-wide integration and the native fault matrix remain root-owned checks.

## Other reviewed behavior

No additional blocking finding identified in the inspected scope. Capture keeps exact originals and separately hashes immutable extracted content; unsupported bytes do not invent passages. Revision ownership, complete original/content hashes, byte boundaries, quote hashes, source head/withdrawal state, and durable evidence/assertion references are checked. Overlapping exact matches are counted. Revalidation preserves predecessor bytes and assertion authored status, and includes the expected predecessor hash in `ChangeDraft.read_preconditions`; refresh/evidence/withdrawal plans similarly bind original reads to their drafts. Proposed `SourceView` dependencies describe final overlay bytes and are not copied into the original-read channel by these physical planning methods. Payload frontmatter is excluded from record adoption. Duplicate canonical IDs plus malformed records retaining an ID are refused.

Lifecycle invalidation exposes source/revision/assertion seeds, not a complete dependency closure. P05 must derive the final graph and enforce whole-registry freshness/membership at validation/publication; that remains an acknowledged dependency rather than a P04 implementation claim. No source interface blocker to that binding was found.

## Checks and limits

Read the selected execution and technical contracts, `src/sources/{types,mod,capture,revision,evidence,lifecycle}.rs`, P04 tests/report, and narrow supporting record/path/read-guard code. Worker/root test outcomes are recorded in their reports; this review independently ran no tests under the report-only lease. Existing tests use mock graph/publication backends and one actual native original-rename recovery fault. They do not establish P05 SQL publication, power-loss safety, other native platforms, or live-provider compatibility. Q1 correction and root's executed regressions are recorded above; final package acceptance belongs to root.

## Root final gate evidence

P04-checks.json records96 parenttests passed onthe correctedfrozen tree, including15 source,8 originalreadguard,250boundary/500faults andenabledSIGKILLwrappers. All-targetlint/fmt/debugbuild/diff/seed checks passed. All findings resolved; acceptance doesnotclaim reviewer-run tests orP05SQL/externalqualification.

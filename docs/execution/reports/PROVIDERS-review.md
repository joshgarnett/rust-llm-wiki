# Provider compatibility and accounting review

Design review against the gateway samples in `.artifacts/provider-gateway-scope.md`, existing wire/history code, and `docs/technical/providers-jobs.md`. No provider calls, credentials, builds or tests used by this reviewer. This is design guidance, not acceptance of the pending implementation.

## Usage acceptance and accounting

Keep output validity, observed usage validity, and complete monetary accounting separate. The present usage key allowlists reject harmless gateway extensions before valid embeddings or JSON can be returned. Relaxing those allowlists must not turn an incomplete bill into known settled cost.

- Embeddings may report `completion_tokens: 0` and null detail objects. Nonzero completion remains unsupported. Present standard counts must still be nonnegative integers, checked totals must agree, and contradictory/overflowing core values must not become valid accounting.
- Chat and Responses have the same accounting partition: input includes cached input; output includes reasoning. Subtract only explicit, valid subsets. Missing/null cached or reasoning counts remain Unknown, never zero. Preserve independently trustworthy observations, but keep computed cost Unknown when a required partition is unknown.
- Recognized `text_tokens` may be treated as informational subsets after integer/type and parent-total checks; do not add them again. Explicit zero cache-write/creation annotations are harmless. Positive cache-write/creation classes, including top-level aliases, cannot disappear from a complete four-class charge. Unless their exact billing mapping is established, accept valid output but leave cost Unknown. Positive cache-read aliases must not silently substitute for missing cached counts or contradict them.
- Bounded unknown usage extensions may coexist with valid output. They cannot authorize settlement: unrecognized potentially billable extensions leave accounting Unknown. `usage.cost` is not a trusted rate card or settlement authority; null does not mean zero. Known zero/null extension handling should be explicitly enumerated, not generalized from arbitrary provider fields.
- Preserve full-document duplicate-key/depth/node/byte checks, recognized core type/subset consistency, documented model-bound violations, and paid observations when knowledge output is incomplete/refused/invalid. Never resend automatically to obtain cleaner JSON or missing usage.

A small shared interface can select `Embedding`, `Chat`, or `Responses` usage field names, normalize only recognized counts/details, and return independent validity, supported billable classes, accounting completeness and fixed diagnostics. Unknown extension presence must survive normalization. Both observation and decode should use the same analysis; do not strip extensions and then mistake the remainder for a complete bill.

## Responses and request boundaries

Use a distinct sealed Responses contract/codec with a shared local schema validator and generation settings where applicable. Keep the output parser separate from Chat. UnknownCompatible bounds are appropriate for the new endpoint/model combinations: the existing documented Chat proof binds an exact endpoint and model and must not be inherited through a shared generation struct.

Encode bounded `instructions`, user data, `stream:false`, `store:false`, `max_output_tokens`, and optional `text.format` schema. No tools, previous-response state, speculative fallback, or automatic endpoint conversion. Preserve request serialization limits, timeout/response caps and the caller's output budget. Probe default 256 is bounded by both configured and explicit caller ceilings; it does not raise either limit.

Accept only completed responses with no error/incomplete condition, exactly one completed assistant message, and output-text parts whose ordered concatenation is one complete JSON value satisfying the unchanged local schema. Ignore reasoning items and Chat reasoning metadata for output without displaying their contents. Refusals, tool calls, unknown output item kinds and incomplete/failed/cancelled statuses remain unsuccessful but paid. Narrow single-outer-fence normalization in text-json mode must require the entire trimmed answer to be that one fence; no prose extraction or fragment salvage.

RFC token characters in header names can be accepted without changing count/byte/value-control limits. Distinguish invalid headers from size bounds with fixed diagnostics.

## Diagnostics and retained replay

Use a closed, bounded diagnostic enum or allowlisted codes. Do not retain provider-controlled field names, text, bodies, reasoning, helper output or credentials in diagnostic metadata. `WikiError.details` can expose these codes. Preserve `wire_contract_violation` exactly where the ledger uses it to stop further dispatch; cosmetic renaming would weaken accounting enforcement.

Do not put harmless accounting diagnostics into `failure_code`: retained decode currently rejects every nonempty failure code. Add a separate optional/default-empty diagnostic field if required, propagate it through spool metadata and receipts, and skip serialization when absent/empty. Historical metadata and receipt bytes are hash-bound and strict; defaults alone are insufficient if reserialization adds fields.

Retain a distinct historical Responses codec so offline replay uses the original wire surface, schema and limits, without current configuration, helpers or transport. Preserve existing Chat codec tags/encoding. Adding defaulted fields to an existing codec requires `skip_serializing_if` or explicit legacy decoding because history checks canonical reserialization against original bytes. Do not recompute or rewrite already settled historical usage to benefit from a more tolerant parser. Received but unmaterialized output may decode locally under its original authenticated contract, without another attempt; rejected/settled output must obey the existing lifecycle.

Required checks include the supplied successful samples, null/missing partitions, nonzero creation extensions, inconsistent aliases/totals, duplicate keys, incomplete paid Responses with reasoning tokens, refusal/tool output, bounded one-fence behavior, legacy Chat codec replay, new Responses replay, and crash recovery with diagnostic metadata. Mock results establish these contracts, not live gateway compatibility.

## Strict provider schema follow-up

The built-in extraction schema deliberately exceeds the current provider subset: metadata keywords, const, scalar/array limits, oneOf, conditionals and optional fields. Frontier/gaps research schemas are closed all-required objects with removable constraints; synthesis additionally uses const-tagged alternatives. Reusing `compile_schema(original, true)` means explicit json-schema extraction/research fails before dispatch even though the simple probe works.

A bounded provider-grammar projection can preserve the unchanged local contract: remove unsupported validation-only constraints/annotations, map const to a one-value enum, and relax oneOf to anyOf. Continue validating the returned JSON against the original full local schema and semantic importer. This is weaker provider guidance, not weaker acceptance. Do not claim a generic transformation for open objects, dynamic properties or arbitrary recursive schemas.

Optional extraction fields require an explicit design. Turning them into required nullable fields changes the wire output language: the original schema rejects null. Blindly making them nonnullable required instead invents dates, units or properties. Dropping optional properties entirely makes some legitimate output (notably has_property) impossible. Avoid all three as silent fixes.

If json-schema extraction must work in this change, omission-preserving alternatives avoid output normalization: for each closed object, enumerate allowed optional-property presence sets as nested anyOf variants, each with exactly its present properties, all required, and additionalProperties false. Extraction has at most four optional fields on an assertion (16 variants), two on a mention (four), and one on evidence (two). Reuse property definitions via bounded local refs to limit duplication. Keep the all-required root object. The provider projection may omit conditionals because unchanged local validation still enforces them. Cap variants, expanded nodes and bytes; unsupported schemas fail clearly before send, with no mode fallback.

Retain the exact projected grammar or its reproducible versioned contract alongside the original schema in the new Responses historical codec. Its restore path must compile the original with local rules and the projected grammar with strict-provider rules separately. Existing history restoration currently compiles the original with strict=true for json-schema: changing only request preparation would leave successful paid responses unreplayable. Preserve legacy Chat encoding.

Defaulting new Responses services to text-json is the smallest immediate interoperable option, but document that explicit json-schema still requires a compatible grammar until projection is implemented. It does not by itself satisfy json-schema graph extraction. No silent fallback or local validation reduction is needed under either scoped choice.

## Specialized projection implementation

Root subsequently leased this reviewer `src/providers/generation_schema.rs` to implement the bounded omission-preserving helper. The module provides `project(&Value) -> Result<Value>`, generates local definitions for repeated optional-object properties, and limits input/output to 4,096 nodes, depth 32 and 64 KiB. It permits at most four optional properties per object (16 presence variants), with 64 optional variants across the whole projection. Unsupported open/dynamic/recursive schemas and optional root properties are rejected explicitly; all built-in extraction/research roots are all-required.

Module tests cover projection/strict compilation of the actual extraction and three research schemas, omitted and present optional properties without null normalization, unchanged local rejection of negative spans, invalid dates and conditional-property errors, and bounded branch expansion/unsupported shapes. No tests or builds have been run by this worker. Root owns formatting, execution and integration; this specialized helper is implementation by the reviewer and therefore needs independent review rather than being described as independently accepted here.

Independent review identified that a final output ceiling alone allowed many references to duplicate a large constant before rejection. The follow-up charges cumulative serialized bytes and nodes before copying caller strings, constants/enums/types, property names, required names and optional-variant subtrees; generated anyOf arrays move owned values instead of serializing copies. The adversarial regression uses a bounded source with 100 references to a 48 KiB constant or enum and requires failure at the second target visit, before its payload clone or the final output check. Work/variant limits continue to bound fixed grammar overhead. The fix is source-frozen for root's formatting and execution; this worker ran no checks.

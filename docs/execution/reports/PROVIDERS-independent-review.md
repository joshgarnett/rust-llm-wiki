# Independent provider gateway integration review

2026-09-29. Astra bounded review of `.artifacts/provider-gateway-scope.md`, PROVIDERS-review, the relevant provider/config/probe diff, and the landed schema projection helper. Read the existing ledger allowance/observation and retained decode boundaries only where needed to trace changed accounting behavior. No builds, tests, provider calls, credentials, source changes, or nested delegation. Source was changing during review; hashes below identify the observed candidate, not a frozen accepted build.

## R1 — high: embedding output subsets could settle an input-only bill

The initial new `wire_json::assess_usage` recognized `completion_tokens_details.reasoning_tokens` and `text_tokens`, but compatible embeddings only refused a positive parent completion total. With `{"prompt_tokens":6,"total_tokens":6,"completion_tokens_details":{"reasoning_tokens":1}}` and no completion total, assessment remained valid/supported, the sole Input class became Known6, and complete Input rates produced Known cost. Positive `text_tokens` took the same path. The newly recognized output subset independently demonstrates output not represented in that input-only bill. This was reported promptly to root.

**Source correction inspected:** `wire_json.rs:349` now treats any positive recognized output-detail field as forbidden on Embedding, independently of the parent completion total. Null/zero details remain compatible with the supplied embedding sample. The observer emits `UnexpectedBillableClass`, keeps computed cost Unknown, and the embedding decoder refuses output. Root added both nested assessment cases and `wire_tests::embedding_output_details_without_total_never_settle_input_only_cost`, which uses a complete Input rate and asserts unknown cost, a violation marker, and decode refusal for positive reasoning/text subsets. The test was authored, not executed by this reviewer; matching runtime evidence remains root-owned.

## R2 — medium: schema expansion exceeded its byte bound before checking it

In the first landed `generation_schema.rs`, local `$ref` resolution at `:158` recursively inlined each target. Source bounds and schema-visit work were checked, but the projected tree's node/64KiB limits were checked only after `Projection::run` returned. A closed all-required root with hundreds of properties referencing one large scalar const/enum can fit the source64KiB/4096node limits while materializing many MiB before the final projected-byte rejection. Optional-property references did not prevent this for all-required repeated targets.

Concrete fixture: `$defs.large = {"type":"string","const":"x" repeated30000}`, with hundreds of required `xN: {"$ref":"#/$defs/large"}` properties and `additionalProperties:false`. Keep the original serialized document below64KiB. The schema-visit counter charges each target visit but not the emitted scalar bytes, so it cannot enforce the intended output-allocation bound. Root acknowledged and assigned a pre-clone/memoized expansion correction to the helper owner. Status at this write: **pending correction rereview**. A regression should exercise the repeated large target and assert bounded sharing or rejection before repeated materialization, not merely observe that final serialization eventually rejects.

## Accounting and retained lifecycle observations

Unknown usage extensions survive analysis as `uncertain_cost`; positive cache-write/creation fields do likewise. Recognized partitions are kept separate from monetary completeness. The cost fold requires valid analysis, no uncertainty, no contract violation, and all applicable classes/rates Known. Missing cached/reasoning subsets remain Unknown; cache-read aliases are checked for contradictions and are not substituted into missing partitions. `jobs::budgets::actual_allowance` additionally requires Known cost and every applicable class, so known partial token observations cannot release an Unknown monetary reservation. Malformed core types/totals/subsets do not authorize a known bill.

The Responses adapter uses UnknownCompatible bounds; the documented Chat basis is selected only for its original exact surface/endpoint/model/CA/limit combination. The new surface is sealed against adapter selection and recorded in history; no new public token/send authority constructor is introduced. Probe's256token default is reduced by configured output maximum and explicit Output/Reasoning ceilings, including zero-budget refusal. This review does not infer a tokenizer guarantee for compatible models.

The dispatcher observes paid usage independently of knowledge decoding. It now decodes before sealing spool metadata, so actual decode failures retain a fixed failure code. The closed diagnostics allowlist accepts only enumerated reasons or three-digit HTTP codes in range; arbitrary provider messages, field names, reasoning, or helper text are not copied to error details. `wire_contract_violation` keeps precedence and its exact spelling, preserving the ledger's durable guarantee-invalidation trigger. Harmless accounting uncertainty is not installed as a failure code, so valid output with an incomplete bill remains locally usable while its reservation stays unknown.

An HTTP200 Responses `status:incomplete` produces a paid unsuccessful result with a fixed reason, including the specific max-output reason. It is not a successful JSON fallback, a new request, or permission to resend for cleaner output. Existing status retry logic does not retry200. The saved original usage/cost metadata drives receipt/replay accounting; retained decoding uses the current observer only to detect contract violation, not to replace historical billed quantities. Recovery checks an existing receipt's output disposition before returning a validated recovery result; rejected historical receipts are not promoted by parser tolerance. No changed path recomputes or rewrites an already settled receipt's charge.

## Historical hashes and provider grammar

The historical Generation codec adds default Chat surface and absent provider grammar with `skip_serializing_if` for both legacy defaults. Therefore old Chat encodings do not acquire new fields during the existing strict canonical reserialization check. Explicit Responses codecs retain their distinct surface and projected grammar; restore does not consult current credentials/config/transport. Restore requires Responses to retain UnknownCompatible bounds, reproduces the projection, compares it to the retained grammar, compiles that grammar strictly, and separately compiles the unchanged original schema locally. Snapshot/task/descriptor/bound fingerprints and exact original schema fingerprint remain checked.

The projection's optional-property alternatives preserve omission rather than injecting null or invented required values. Root remains all-required; open/dynamic/recursive unsupported shapes fail. Const-to-enum and oneOf-to-anyOf plus removed validation constraints widen provider guidance while original schema validation still rejects invalid local output. Scalar/property limits, optional-variant count, local-reference recursion and compiled expanded-schema visits are checked; R2 concerns the order of allocation versus the projected size check. The retained grammar/reproduction algorithm forms part of version1 history and must remain reproducible if the implementation later evolves.

No additional concrete acceptance or history-hash defect was identified in this scope. Configuration currently defaults an omitted adapter to Responses and an omitted response mode to TextJson; explicit json-schema uses projection. This is the observed policy, not a claim that omission selects structured output.

## Evidence

`.artifacts/providers-first.log` reports the first selected unit-test target passed. That build began before the R1 correction and while R2 work was pending; it is not final candidate evidence. No test/build was run by this reviewer. Required final evidence includes the supplied usage cases, the new complete-rate embedding regression, Responses incomplete paid/no-resend lifecycle, fixed diagnostic persistence/recovery, legacy Chat canonical history, Responses grammar history, and the corrected projection expansion bound. Existing authored fixtures cover several of these; root owns exact command/filter/hash attribution and acceptance.

Observed SHA256s before R2 correction:

```text
src/providers/wire_json.rs 18b9c6da6d15944726343f18496839f112283919424376e8d9173e92a03cf923
src/providers/generation_schema.rs 5190b319f4a41808933dcf8d78f448014b7dcef13d3c7f5aa1a478bb264733a5
src/providers/generation_wire.rs 99a6415fea407f9ab06f1a6f897dd704bc45211353d34c1136565d4ebfb64f0d
src/providers/dispatcher.rs 43439ae2737332a960178a06316a120e20a80bf5d34c2973ad77b854e3ba53ba
src/providers/history.rs 3af518620d2bf378c8c2c3520e759c2557954efd72242dd60a5ce1a65e508d32
src/providers/types.rs ba18572ad9282006191e454f733e70744bf7ed721dde91c8db50c64fa4d3a5f9
src/providers/wire.rs 9a88fe123b2524acfde83fe5deef2d77d3bdf9dbf18d0955290c499eeec7175a
src/config/providers.rs b694df7bd6928b01c60e71afe14779b47a6efd5e9995287b074ad52748f71bef
src/app/probe.rs 116219f33ed98ddb58755d6b1b6fd9c479a7f7d2a797925e8c2dedcc8ba6eb0d
src/providers/diagnostics.rs 649b89c47cab2374a5dfb846edd8d5820803b84dd982bc484d6d15de2ab3e881
src/providers/wire_tests.rs ba18b945f5ac101229b891d84e65b5e2e7e3c7f70f9f6f61b0e09c047a30e9f4
src/providers/history_tests.rs 420f2dc0b7c178383e226ab8add6f373c5ccb2cd5f1513f5758553d2e54760ff
```

## Correction rereview and final static disposition

Root requested rereview after the helper correction was frozen and formatted. **R1 and R2 are closed in the reviewed source; no additional concrete consequential defect was found in this bounded review.** Final integrated runtime acceptance remains pending root's gate results.

R2 now has cumulative `copied_nodes`/`copied_bytes` accounting in `Projection`. `charge_copy` counts serialized bytes through a bounded writer before allocation; `copy` checks subtree node extent before cloning; `text` charges before string allocation. Caller-controlled const/enum/type/annotation values, property names, required names and optional-alternative copies use these helpers. Fixed grammar punctuation and small generated reference names remain separately bounded by the existing schema-work/variant limits plus the final output check. This is a bound on amplified retained input data, not a claim that total process memory equals64KiB.

The new `repeated_large_reference_stops_before_second_payload_copy` regression uses a source below64KiB with100references to a48KiB const or enum. It invokes the projection directly and asserts a budget error at work5, before the second large payload copy and before final projected-output validation. This tests the actual pre-allocation boundary, rather than simply checking eventual rejection. R1's complete-rate embedding observer/decode regression remains present after formatting.

Final changed hashes relative to the inventory above:

```text
src/providers/generation_schema.rs 697e507d01cea0fff2e0ff766a350cf00fed88e54997591a97ba51f780d48e37
src/providers/wire_tests.rs 224d751933794697d2224a4da6c777976578825e38cc45ba94819fe065fcefbe
```

All other listed hashes were rechecked and remained unchanged. `.artifacts/providers-integrated.log` shows the combined unit, remote CLI, and provider-trust targets building/testing; it was not complete at the final source observation. The earlier filtered first-unit pass must not be presented as this complete final gate. Root owns the eventual command/count/hash attribution, paid-lifecycle/replay evidence and acceptance. No builds or tests were run by this reviewer. Report lease returned.

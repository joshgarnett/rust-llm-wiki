# Independent P16.c invariant review

2026-09-28. Bounded Astra static review of STATE, P16c-plan, actual wire/types/dispatcher/embedding/generation/JSON leaves and associated authored tests. No Cargo, runtime reproduction, live network, credentials, Git, source fixes, or nested delegation. Only this report edited. Root and Sol changed source during review; final sampled hashes below identify the observed candidate, not a frozen runtime acceptance.

## R1 — high: proven priced requests were missing their quoted allowance

Initial `src/providers/wire.rs:318` set rate_card but always left quoted_allowance=None before fingerprinting. Production dispatcher immediately invokes `budgets::quote_bound` (`dispatcher.rs:142`), which compares the complete computed Some(cost) against quoted_allowance. Thus an exact documented official model with complete supplied rates failed admission even without max_cost; compatible profiles masked the issue because their token bounds/cost allowance stayed unknown. Reproduction: actual production dispatcher + exact official embedding model/endpoint + complete Input rate card + mock transport; expect one admitted call, initially obtain quote disagreement before transport. Repeat with exact chat and a hard-cost cap to exercise the intended capability.

Root fixed this during review. Final `wire.rs:322` checks that every applicable bound is Exact/ProvenUpper and every corresponding rate is present, computes one request fee plus checked upward-rounded class charges through existing Money/rate helpers, then sets quoted_allowance before the final bound fingerprint. This closes the source mechanism; actual production complete-rate and hard-cost tests remain necessary. Incomplete/UnknownCompatible bounds correctly leave the quote unknown so requested hard guarantees still refuse.

## R2 — high: missing usage subsets hide independently observed total-bound breaches

`src/providers/wire_json.rs:302` partitions Input/CachedInput only when prompt total and cached subset both exist, and Output/Reasoning only when completion total and reasoning subset both exist. Keeping missing partitions Unknown is correct. However, the observer does not independently compare raw reported totals against the sealed documented aggregate ceilings. `supported_usage` (`:239`) only examines unsupported classes; the resulting ledger usage contains no known partition count that can reveal the breach. Retaining totals in the response spool alone does not stop future authority.

Concrete valid-JSON reproductions with exact official identity and otherwise valid knowledge output:

- Chat completion_tokens exceeds the actual requested completion cap, with completion_tokens_details omitted. Output and Reasoning become Unknown and no violation is reported.
- Chat prompt_tokens exceeds1047576, with cached details omitted. Both input classes become Unknown.
- Embedding total_tokens alone exceeds min(8192×actual_items,300000), with prompt_tokens omitted. Input becomes Unknown, although the supplied aggregate itself contradicts the documented upper bound.
- Chat total_tokens alone exceeds a safe checked aggregate of the documented prompt ceiling plus requested completion cap. Neither partition must be invented to recognize that contradiction.

Root acknowledged this and assigned independent raw-total checks and production regressions to Sol. Compare each independently valid reported aggregate against its sealed proven aggregate ceiling; preserve missing subsets Unknown; force computed cost Unknown and the durable safe violation marker on a definite breach. Test actual production dispatcher, retained metadata and reopen with guarantee_intact=false, plus under-cap/missing-detail controls that remain unknown without a fabricated violation. Malformed or contradictory fields must not become trusted counts. No runtime reproduction was performed by this reviewer.

## R3 — medium: forbidden reasoning can disappear when completion total is absent

`wire_json.rs:266` zips reasoning_tokens with completion_tokens before checking positive reasoning against the documented nonreasoning identity. An independently valid `completion_tokens_details:{reasoning_tokens:1}` with completion_tokens absent therefore produces supported_usage=true and no violation, despite the frozen contract forbidding a positive observed reasoning class for this snapshot. The audio/prediction checks already recognize a valid positive count when the parent total is absent.

Use the exact returned model and otherwise valid generation response with the omitted completion total. Preserve the Output/Reasoning partition counts as Unknown, but recognize the independently valid forbidden positive class and keep cost Unknown. A malformed/negative/string detail is a separate unsuccessful observation, not positive-class proof. Root was notified to include this case with the R2 observer correction.

## Other scoped findings

The documented-bound factory is private and reproduces the frozen plan: exact full official endpoint, selected exact chat snapshot and max_completion_tokens; explicit representation revision for undated embedding IDs; no custom CA. Root's common seal repeats endpoint/model/custom-CA checks. Compatible endpoints, arbitrary tokenizers, aliases and legacy output-limit fields do not inherit ProvenUpper. Embedding count arithmetic is checked and capped300000; separate conservative chat partition bounds may overreserve but do not purport to be exact tokenization. This is implementation conformance to the previously documented provenance, not independent requalification of current provider behavior; no prices or live calls were used.

Task fingerprints remain data-only. PreparedWire/BoundBasis/WireContract construction stays private; helpers cannot mint public send authority. Exact request hash binds method/full URL/nonsecret headers/body/profile, and token proof fingerprints additionally bind class/count/revision. The later root production descriptor check requires canonical JSON bytes, consistent with the ledger's physical input hash and the helper's canonical logical hash. Existing cfg(test) synthetic routing is distinct from external integration tests compiling production routing.

No additional concrete defect found in the inspected schema resource/reference mechanism. It meters raw nodes/depth before schema serialization/compiler work; caps bytes; restricts keywords/draft/formats; walks local JSON pointers and rejects nonlocal/percent-encoded references, nested IDs and active recursion; meters expanded visits/depth so small reference fanout cannot evade the raw-node limit; and configures offline jsonschema with bounded linear regex compilation/DFA sizes. Strict provider mode rejects unsupported constraints instead of deleting them, requires a root object and closed objects with all properties required. Local validation retains the original schema. These source controls are not a claim of exact process-memory or provider schema compatibility on a live endpoint.

The full-document parser rejects duplicate keys and trailing content, checks byte limits before parsing, and meters retained value nodes/depth. Embedding decode checks complete unique index membership, order restoration, established/fixed dimensions, finite f32 conversion and nonzero finite norm, with per-vector and aggregate coordinate bounds. Generation requires one index-zero stopped assistant choice, rejects actionable/unknown message extensions and malformed content, parses the entire assistant string and validates the unchanged local schema. Probe reduction follows the same decoder. Independently valid usage observation precedes knowledge decoding, and root preserves the violation marker through response persistence. No new decoder completeness defect was identified beyond the usage observations above.

## Evidence and handoff

P16c.md at review time described source-ready authored private/public gates and explicitly did not claim successful runtime execution. Root subsequently authorized worker targeted Cargo, but this review did not inspect completed matching logs. Source corrections and authored tests therefore remain distinct from runtime acceptance.

Final sampled SHA256s:

| Path | SHA256 |
| --- | --- |
| src/providers/wire.rs | 272264c78c6bb954645ef2b16706d49417c9697b38f8d2e98bfd4d3f1c4f796c |
| src/providers/types.rs | 072b17f188898c79083dd2d3701afd708d451062bb31e1722aa2f4d9de5ba713 |
| src/providers/dispatcher.rs | 91e1f7de57061af4469b34d5208be0a0e6439e11b93d52052a1750ca9c351bb4 |
| src/providers/embedding_wire.rs | 1e11214a6717383475aef6c20327eea5294d487fc7aa79088c249f4ad0263781 |
| src/providers/generation_wire.rs | 28f84ed21f1985e6add33d64d6e478aec678cf92723abc5ccea7f972930975f1 |
| src/providers/wire_json.rs | afb34776910a2757fc7c988e1e995122c3c28a1c67829b2e9f953988fa6f65e8 |
| src/providers/wire_tests.rs | f6bdbf285142299de40d4c609af89f26ee63b58596d05e4672aacd2274f7fd66 |
| tests/provider_wire.rs | f866ab583d58eb47134c9a2c7be553ac555af4c26d02b4455f4c2e759f340d50 |

R1 source correction observed; R2/R3 pending worker correction and matching tests. Further rereview should target those resolution predicates, not repeat closed transport work. Root retains package acceptance. Report lease returned.

## Resolution rereview — 2026-09-28

**R1, R2, and R3 are closed for the reviewed resolution predicates.** This addendum supersedes their pending status above. Bounded static rereview only: no Cargo, new tests, source edits, live calls, or broader package review. Existing isolated runtime logs were inspected, not executed by this reviewer.

- **R1:** `wire.rs:341` requires every applicable class to have a proven/exact bound and a corresponding configured rate. It starts with the request fee once, adds checked upward-rounded class allowances, assigns `quoted_allowance`, then fingerprints at `:369`. The production test at `tests/provider_wire.rs:568` admits the exact official generation contract with a complete rate card and a hard money ceiling. Both successful and invalid-output responses account for exactly42nanounits; invalid output auto-settles, while successful materialization reaches `OutputCommitted`. The common factory also serves embedding contracts; the production model-mismatch embedding case has a complete Input rate and reaches transport.
- **R2:** `wire_json.rs:272` validates reported usage and independently compares present raw prompt, completion, and aggregate totals against the documented chat ceilings, and both embedding prompt/total against `min(8192 × items,300000)`. These checks do not depend on cached/reasoning subsets. Arithmetic is checked; the sealed contracts already bound item count and requested completion count, so their overflow fallbacks are not reachable through normal preparation. `observe` at `:327` emits the private total-breach marker; missing partition fields remain Unknown. The generation variants at `tests/provider_wire.rs:451` cover each omitted-partition ceiling and reopen with `guarantee_intact=false`. The embedding case at `:513` covers total-only overrun, unknown Input/cost, and an absent-usage control, including reopen. Malformed/contradictory usage does not become trusted aggregate proof.
- **R3:** `supported_usage` at `wire_json.rs:266` accepts a valid positive reasoning observation when the completion total is absent, while rejecting a subset that contradicts a supplied total. The generation test's fourth variant at `tests/provider_wire.rs:467` supplies only `reasoning_tokens:1`; it refuses knowledge output, persists the violation, retains unknown partitions/cost, and reopens with the guarantee lost. The private malformed-subset control at `wire_tests.rs:517` remains unknown and decoder-rejected without inventing a positive-class proof from contradictory usage.

Marker handling is preserved: `dispatcher.rs:362` gives `wire_contract_violation` precedence over ordinary HTTP status; `:385` persists metadata before `:402` refuses output on the observed marker. All three private violation variants prevent cost calculation (`wire_json.rs:362`), and later observations can replace one violation kind only with another, never clear it. Public tests inspect the retained safe marker and unknown cost. The private enum remains inaccessible to public callers (`types.rs:252`). Direct embedding/generation decoders additionally call `totals_within_contract`; the dispatcher already refuses marked observations independently of those decoder calls.

Under-cap missing partitions do not fabricate usage or release the money reservation: `tests/provider_wire.rs:626` covers both missing detail and absent usage with a complete admitted quote, intact guarantee, `UnknownReserved`, and the full outstanding allowance. Successful fixtures use real `ChangeEngine::prepare/apply` and `JobLedger::outputs_committed` at `tests/provider_wire.rs:149`, not a public test-only settlement API.

Runtime evidence inspected:

- `/tmp/lwiki-p16c-public6.log`: actual external production integration target, **10 passed**, compile2.52s/run13.57s. It exercises production routing with mocked transport and disposable local storage.
- `/tmp/lwiki-p16c-private5.log`: **5 passed**, compile4.00s/run5.42s,92filtered. These are private adapter/decoder tests, not independent live compatibility evidence.
- `/tmp/lwiki-p16c-checked-hashes.json` identifies the isolated snapshot `lwiki-p16b-acceptance-j5ayayh1`. The seven resolution/fixture files below match both this manifest and the current isolated snapshot byte-for-byte. This directly ties the reviewed predicates and regression assertions to the recorded candidate.

| Path | SHA256 |
| --- | --- |
| src/providers/wire.rs | 40996de71d138a79ca3528198d45bfa7b66fe7bf5ab16688627124ade4f97314 |
| src/providers/wire_json.rs | 434180e9289c567e93e9bffa009eba8384b4f474560bfbd5b966f05d8b38a319 |
| src/providers/types.rs | 072b17f188898c79083dd2d3701afd708d451062bb31e1722aa2f4d9de5ba713 |
| src/providers/dispatcher.rs | 70dc7550f6ce262276296ad71a2debb67d6bf33f358a1ebc7dbda9dd6203f43d |
| src/providers/wire_tests.rs | 1a626bf6c0ad9f0d0e860881bd4f884b1ce7564fb32df94ba2c792ccd9f1bc89 |
| tests/provider_wire.rs | 4270b5c17b92ca165b38432990c71178fa53fed9eaa95a8f6059f2fbf45ee470 |
| tests/fixtures/p16c/common.rs | 8c9177ec263290d1fb9220286c67f9220cf38fca0a4819baf27f2a84e49b1c10 |

Evidence limitation notified to root: the current embedding decoder (`3ebd4d67c75a53d7a21519d416e5b4151eddf9464b5e589c47f868f37c2ab7aa`) and generation decoder (`09800f08213a03faad33929d929fa360219f0ad7f9fdeea03a941ba77ab932b9`) match the current isolated snapshot but differ from the checked manifest's older entries. Root should reconcile the last decoder-change timing and refresh those entries before accepting the entire final package; this does not reopen the matching shared observer/dispatcher resolution predicates. No remaining concrete defect found within this bounded rereview. Package acceptance remains root-owned; report lease returned.

Root evidence reconciliation: Sol final same-source private5 (compile6.07s/run6.02s) and public10 (compile3.97s/run13.65s) ran after formatting the two decoder files; logs /tmp/lwiki-p16c-private-final.log and /tmp/lwiki-p16c-public-final.log. Frozen32 overlay manifest /tmp/lwiki-p16c-checked-hashes.json SHA2563e24aa7be61d19818eba2db6d1150f31762f7890e5967008c2c7a121d06cff03. All six leased wire/test source files equal tested snapshot bytes. The previous decoder hash mismatch is closed by fresh same-source gates; later root shared annotation/test-only accessor cleanup is explicitly unqualified until integrated checks. This is root evidence attribution, not additional Astra-executed testing.

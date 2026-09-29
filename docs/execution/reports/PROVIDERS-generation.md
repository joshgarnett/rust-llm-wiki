# Generation wire follow-up

## Scope

The `responses-v1` generation adapter encodes bounded `model`, `instructions`, one user `input`, `stream:false`, `store:false`, and `max_output_tokens`. It sends no tools or prior-response state. Explicit `json-schema` mode sends a separately projected strict provider schema under `text.format`; the original schema remains the local validator and task fingerprint. The existing Chat request shape and documented exact Chat bound remain separate.

Responses decoding accepts only `status:completed`, no error or incomplete detail, and exactly one completed assistant message. It ignores reasoning items, joins ordered `output_text` parts, parses the whole bounded result as one JSON value, and validates against the original local schema. Refusals, tool/unknown output items, additional assistant messages, and incomplete/failed/cancelled status return fixed reason codes in `WikiError.details`. Observation of token usage occurs independently of output success through the shared usage adapter. Text JSON mode also accepts one exact outer ` ```json ` fence, with no prose or fragment extraction. Chat reasoning metadata is ignored for output.

## Fixture coverage and limits

`src/providers/wire_tests.rs` covers the supplied GPT and Claude Responses bodies, an incomplete paid response, gateway Chat usage/reasoning metadata, fenced JSON, tools/refusal/multiple messages, and actual 1024/3072 coordinate embedding decode with the reported usage extension shape. The response samples are in `tests/fixtures/gateway/`. Tests are local mock/codec checks and do not establish live gateway interoperability. Root owns the adapter/config/historical codec, dispatcher diagnostics, and execution gates. No worker build or provider call was made.

Local validation and the strict provider projection remain distinct. A schema that cannot be projected while preserving local acceptance and optional-field omission fails before dispatch; the adapter does not silently weaken it. Any final claim of explicit structured-output coverage depends on the separate schema-projection gate and historical replay check.

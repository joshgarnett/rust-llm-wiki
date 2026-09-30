# Provider dispatch and durable jobs

Implementation contract for authenticated embeddings, generation for extraction, probes, and shared budget accounting. Research uses separate local host-agent packets and does not dispatch search, fetch, or generation through this subsystem. Local tests do not establish live-provider interoperability. [Retrieval](retrieval.md) owns representations/extraction schemas; [storage](storage.md) owns records and commits; [CLI](cli-and-skills.md) owns public envelopes/errors.

## 1. Interfaces and ownership

CLI-owned provider requests for embeddings, generation, and probes pass through one dispatcher, including retries and token-count requests if later supported. Authenticated transport constructors remain private to this module. Conceptual Rust interfaces:

```rust
struct DispatchContext {
    run_id: RecordId, task_key: Blake3Hash,
    policy: ExecutionPolicy, cancel: CancellationToken,
}
enum RemoteRequest {
    Embed { profile: TrustedProfileId, space_id: SpaceId,
            inputs: Vec<EmbeddingInput>, expected_dimensions: Option<u32> },
    Generate { profile: TrustedProfileId, prompt: GenerationInput,
               output_contract: SchemaRef, max_output_tokens: u32 },
}
struct EmbeddingInput { input_hash: Blake3Hash, utf8: String }
struct GenerationInput { instructions: String, data: String }
enum TokenBound { Exact { count: u64, tokenizer: String },
                  UpperBound { count: u64, method: String },
                  Estimate(u64), Unknown }
async fn execute(ctx: &DispatchContext, request: RemoteRequest)
    -> Result<RemoteOutcome, DispatchFailure>;
```

`RemoteOutcome` contains typed embeddings or generation text, plus a `UsageReceipt` and durable output reference. `DispatchFailure` includes a safe cause, receipt when available, and `NotSent | Rejected | OutcomeUnknown` disposition. Central CLI mapping selects existing error codes. Receipts accompany invalid output too: malformed JSON can still cost money. Fixed allowlisted `details.reason` and retained `failure_code` identify validation failures without copying provider strings or bodies.

Adapters implement pure `encode`, `decode`, and `upper_bound` methods; only dispatcher transport sends requests. Jobs supply run identity, limits, reservations, and checkpoints. Retrieval supplies `SpaceId`, actual rendered bytes, and schema validation; generation cannot bypass its importer. Use an injectable transport, clock, jitter source, and credential runner for deterministic tests.

## 2. Private profiles and trust

Load `providers.toml` from the platform's user configuration directory, never by upward traversal from a vault. A profile maps capabilities to endpoint-bound services:

```toml
version = 1
[profiles.primary]
embedding = "embed-main"
generation = "generate-main"

[services.embed-main]
adapter = "embeddings-v1"
url = "https://gateway.example/v1/embeddings"
model = "embedding-deployment"
revision = "deployment-2026-09"
max_batch_items = 32
max_batch_bytes = 262144
# dimensions = 1024
# max_input_tokens = 8192  # only after verifying this deployment
# tokenizer = "verified-tokenizer-name"

[services.embed-main.auth]
kind = "static"
key_env = "WIKI_EMBEDDING_KEY"
header = "Authorization"
prefix = "Bearer "

[services.generate-main]
adapter = "responses-v1"
url = "https://gateway.example/v1/responses"
model = "generation-deployment"
revision = "deployment-2026-09"
response_mode = "json-schema"

[services.generate-main.auth]
kind = "static"
key_file = "/private/path/generation-key"
header = "Authorization"
prefix = "Bearer "

[vault_bindings.example]
root = "/absolute/path/to/wiki"
wiki_id = "vault-example"
allowed_profiles = ["primary"]
```

Each service uses the exact full URL, including non-secret query parameters; append no inferred path. Reject URL userinfo, fragments, and known credential query fields; configuration forbids secrets in other query values, which cannot be detected perfectly. A trusted local binding includes canonical vault root and vault ID: cloning an ID or adding `wiki_profile` cannot grant access. Shared content cannot override URLs, executables, auth, TLS roots, or bindings. Explicitly creating/editing private configuration establishes those choices; routine use needs no repeated approval. This schema supersedes the earlier illustrative single-role `[embeddings]` example in [embeddings.md](../embeddings.md).

Require HTTPS except an explicit loopback test setting; verify certificates, with optional private CA file. Provider redirects are disabled. Standard headers cannot overwrite managed auth/content headers; additional secret headers use explicit environment/file references. Reject duplicate keys and unknown settings. Diagnostics expose capability/model and endpoint fingerprint, never keys, helper output, or provider error bodies. Changing credentials does not change embedding space; changing endpoint/model/rendering does.

## 3. Credentials

`AuthConfig::Static` requires exactly one `key`, `key_env`, or `key_file`. Literal keys are allowed only in private configuration; initialization favors references. Missing/empty credentials fail before dispatch. Use secret wrappers without `Debug` serialization and bounded file reads.

`AuthConfig::Command` replaces that block:

```toml
kind = "command"
command = ["/absolute/path/get-token", "--audience", "embeddings"]
output = "json"                # or "text"
timeout_seconds = 10
ttl_seconds = 300
refresh_skew_seconds = 60
header = "Authorization"
prefix = "Bearer "
```

JSON stdout is exactly `{"key":"token","expires_at":"2026-09-28T18:00:00Z"}`; expiry is optional only with bounded TTL. Text mode removes one terminal LF/CRLF. Reject empty tokens, control characters, malformed expiry, or expiry inside the refresh skew. Cache in memory by endpoint/auth configuration until the earlier expiry-minus-skew or TTL; never persist tokens. Serialize refreshes within the process; independent processes may each refresh.

Spawn argv directly, with stdin closed, private-config working directory, ten-second deadline, and 16 KiB output ceiling. Discard stderr rather than risk exposing secrets. The helper is trusted user code, not sandboxed code. Initially support native executables and explicitly configured interpreters; reject implicit Windows `.bat`/`.cmd` launching. On 401, command auth refreshes once, then retries only within attempt/request/cost limits. Static auth fails; 403 does not imply expiry.

`--offline` and `--dry-run` prevent helper execution, DNS, probes, and HTTP even if a cache misses. Dry-run also prevents journals, credential caches, index refresh, and file writes. Check those modes before resolving secrets. Ordinary local operations never enter the credential path.

## 4. Embedding wire contract

POST JSON `{model,input:[string,...],encoding_format:"float"}`; add `dimensions` only when configured. Decode response `data[].index`, `data[].embedding`, `model`, and optional usage. These fields follow the official [embedding reference](https://developers.openai.com/api/reference/resources/embeddings/methods/create); compatible endpoints need contract tests rather than inherited OpenAI limits.

Require exactly one vector for every requested index; accept reordered entries, reject missing/duplicate/out-of-range indices. Validate finite float32-representable values, nonzero norms, equal dimensions, configured dimensions, and the space's established dimension/model metadata. If dimensions are configured as `auto`, the first valid corpus batch fixes actual dimensions; query probes do not. Retrieval owns normalization and vector storage. A model alias changing silently cannot be reliably detected; record returned identity and require operator-managed revision changes for known deployment changes.

An active older space retains its non-secret endpoint/model/revision/render specification during replacement. Query dispatch must reproduce that specification and reauthorize its endpoint against current private trust; it must never embed with newly configured settings and compare against old vectors. If no longer permitted or reproducible, require a matching cached query or report semantic unavailable.

Reject an invalid batch as a whole; persist a bounded failure receipt and leave its inputs pending. Successful earlier batches remain checkpointed. Commit validated vectors and their receipt before scheduling another batch. A missing usage field or subset means unknown, not zero. Embedding usage accepts zero/null completion counts and nullable detail blocks. Bounded unknown usage keys do not reject valid output; unknown extensions and positive cache-write/creation classes keep computed cost unknown. Recognized counts still require valid types, sums and subset bounds; nonzero embedding completion is unsupported. Provider-reported cost never authorizes settlement. Token estimates never establish model limits: use verified tokenization or a proven bound, otherwise enforce bytes/items and expose uncertainty. Overlong-input failures return to deterministic segmentation; never truncate or automatically spend on a repair call.

## 5. Separate generation adapter

Use nonstreaming `responses-v1` for new generation services. Omitting `adapter` selects Responses; existing explicit `chat-completions-v1` services retain their behavior and endpoint. URLs are never rewritten or probed speculatively. Responses uses top-level `instructions`, one user `input` item, `stream:false`, `store:false`, and `max_output_tokens`; no tools, prior response IDs, or conversation state are sent. Explicit `response_mode = "json-schema"` sends `text.format`; omitted response mode remains `text-json`. [Official migration guide](https://developers.openai.com/api/docs/guides/migrate-to-responses)

Responses success requires `status:completed`, no error/incomplete condition, and exactly one completed assistant message. Join its `output_text` parts in order and validate the entire result locally. Reasoning items are ignored as output; refusals, tool items, unknown output item kinds and incomplete/failed/cancelled statuses are unsuccessful paid responses. Incomplete output allowance reports `incomplete_max_output_tokens`. Usage maps input/cached input and output/reasoning partitions without assuming missing subsets are zero.

For Responses schema mode, a bounded provider grammar preserves optional-field absence through closed object alternatives. Provider-only constraints may be broader; the original full schema remains the local acceptance rule. No null substitution, fabricated optional values or hidden text-mode fallback occurs. Both schemas are retained with the original Responses codec for offline recovery. Legacy Chat schema mode retains its existing strict subset. Unsupported/open/dynamic or excessive schemas fail before dispatch.

`chat-completions-v1` remains available for gateways without Responses. Only this adapter accepts `instruction_role` and `output_limit_field`; it sends `model`, `messages`, `stream:false`, one choice, and the configured output-token limit. [Official Chat reference](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)

```json
{
  "model": "generation-deployment",
  "messages": [
    {"role":"system","content":"Return JSON matching the supplied schema."},
    {"role":"user","content":"<bounded extraction packet and schema>"}
  ],
  "stream": false,
  "n": 1,
  "max_completion_tokens": 4096
}
```

Instruction role (`system`/`developer`) and legacy `max_tokens` are explicit tested profile capabilities; no speculative fallback requests. Omit temperature and provider-specific options by default. Never send tool definitions or execute returned tool calls. Source text stays in the data message and cannot configure dispatch.

Baseline `text-json` requires the entire returned text to parse as one JSON value. One outer ` ```json ` fence with newline delimiters may be removed; prose, extra fences and fragments are not extracted. For Chat, tested `json-schema` mode sends `response_format` with the importer schema, but refusals and truncated responses still need handling. [Structured output guidance](https://developers.openai.com/api/docs/guides/structured-outputs) Require one index-zero choice, assistant text, no refusal/tool calls, and `finish_reason:stop`; preserve other results as unsuccessful outputs. Schema repair is a new explicitly requested budgeted task. `lwiki.extraction.v1` validation and changeset staging remain retrieval's responsibility.

Output-token limits must include invisible billed tokens when the provider uses them; visible text length is insufficient. OpenAI explicitly documents this distinction. [Token accounting](https://developers.openai.com/api/docs/guides/token-counting) Embedding profiles cannot generate text merely because their URL looks API-compatible. No local model runtime is supported.

## 6. Research boundary

`research plan` and dry-run are local previews. `research run` persists a bounded `research-packet` for a host agent; `research import` validates a `research-submission` and retains its source or answer material through guarded local storage. `research resume` returns the outstanding packet or, with `--refresh`, regenerates it against current local state. The CLI does not search the web, fetch URLs, call a model, or open provider credentials for research. Offline research tasks are limited to local or already acquired content.

The host can submit bounded inline source content with a key, title, claimed origin and optional provenance, or answer claims referencing packet-local passage IDs. Origin URLs and acquisition metadata are host claims, not an observed CLI HTTP exchange. Exact source and citation bytes are checked before retention; claims remain unassessed, gaps stay explicit, and graph assertions are not autoaccepted. A follow-up answer may request another bounded collection round without dispatching it. Host-agent tool calls, network activity, tokens and spend are outside CLI observation.

Research defaults are three rounds, fifteen imported sources and 524288 total imported source bytes over the run. Each submission has a separate 64 KiB aggregate inline-content cap. These are local handoff limits, not provider budget guarantees.

## 7. Proposed limits and retry policy

All defaults are product choices, overridable within validated bounds; provider token limits have no universal default.

| Control | Default |
|---|---:|
| Connect / embedding / generation timeout | 10 / 60 / 120 seconds |
| Attempts including first / run concurrency | 3 / 2 |
| Embedding items / serialized request bytes | 32 / 256 KiB |
| Generation request / response bytes | 256 KiB / 1 MiB |
| Embedding response bytes | 8 MiB |

Provider jobs persist their dispatch limits and accounting across resume. Local research persists its own round, source, and source-byte limits; resume does not reset them. Provider rate controls use per-run requests-per-minute and token-per-minute bounds when known; provider-wide coordination is outside v1.

Retry 429/selected 5xx with full-jitter exponential backoff, starting at one second and capped at thirty. Respect valid `Retry-After`; if it exceeds the remaining deadline, pause instead of retrying early. Do not retry 400/403, schema failures, or malformed successful responses automatically. A pre-send connect failure may retry; a timeout/reset after possible send pauses with unknown outcome unless retry-uncertain was explicitly enabled. HTTP retries consume new reservations even when earlier billing is unknown. Cancellation interrupts waiting and stops dispatch; it cannot retract accepted provider work.

## 8. Job states and persistence

Run states match storage: `planned -> running -> completed|paused|failed|stopped`. `paused` records budget, deadline, interruption, or reconciliation reason; `stopped` records user cancellation. Resume revalidates source revisions, configuration fingerprints, and outstanding attempts before returning to `running`. Completed tasks are reused by input/prompt/schema/model/settings hash, not by headings. A changed input creates new work; old outputs survive.

Task attempts advance `pending -> reserved -> dispatch_intent -> received -> output_committed -> settled`. Definite pre-send failure releases its reservation. A crash after dispatch intent is conservatively `outcome_unknown`, even if nothing actually left the machine. No exactly-once claim is possible without provider cooperation; sending a local task key does not establish remote idempotency.

Logical `runs/<id>/run.md` contains scope, limits, completed task keys and checkpoint; storage schema2 keeps it under `.wiki/retained/runs/`. New production checkpoints use a compact proof binding vault/run/spec, the exact journal prefix and the historically committed run bytes; they omit per-event Markdown mirrors. Legacy event notes/readers remain supported for migration and carry storage's `run_id,sequence,event_type,occurred_at,request_id?` frontmatter and a bounded `lwiki.run-event.v1` fenced JSON payload:

```rust
struct UsageReceipt {
    attempt_id: String, task_key: Blake3Hash, capability: String,
    profile_id: String, endpoint_fingerprint: Blake3Hash,
    input_hash: Blake3Hash, requested_model: Option<String>,
    returned_model: Option<String>, provider_request_id: Option<String>,
    usage: KnownOrUnknown<TokenUsage>, billing: BillingDisposition,
    reservation: Option<Money>, computed_cost: Option<Money>,
    rate_card_id: Option<String>, outputs: Vec<RecordRef>,
    cache_outputs: Vec<VectorCacheRef>, // space/input/vector hashes; not durable knowledge
    failure_code: Option<String>,
}
```

Events also record attempts, retries, bytes, source/task coverage, and stop reasons. The operational length-framed, checksummed journal under `.wiki/state/` fsyncs reservation and dispatch transitions. Across cooperating processes, a short exclusive run-ledger lock serializes reservation/concurrency decisions; never hold it across HTTP or while acquiring the vault writer lock. Replay uses attempt/event IDs to avoid double settlement. Unfamiliar edits to run records produce conflicts.

Before `received`, fsync the bounded successful response and allowlisted transport metadata to restricted `.wiki/state/requests/<attempt>/`; this sensitive recovery spool is never diagnostic output. Exclude auth headers/helper output; reduce error bodies to safe codes. Validate/materialize it, commit the Markdown receipt/output records, then settle and advance checkpoints. Remove the spool only after verifying committed outputs. Vector bytes may commit to cache first, but membership activation still checks retrieval's snapshot/input fingerprints. These stores are not one atomic transaction: replay reconciles hashes at every boundary. Unspooled responses remain unknown after a crash. Embedding receipts survive cache loss but cannot restore missing vectors.

Research does not create provider `RunSpec` tasks or usage receipts. Its local retained packet/submission lifecycle has `collect_sources` and `answer` stages, bounded imports, current source dependencies and explicit gaps. Repeated import of the same submission reuses retained work; stale or conflicting packets do not acquire authority. Embedding and direct API extraction continue to use the ordinary job ledger and receipt accounting.

## 9. Budget admission and recovery

Every dispatch reserves a request slot, bounded bytes/tokens, concurrency slot, and conservative money allowance when available. Under the ledger lock require `settled + outstanding + proposed <= limit`, append/fsync, then authenticate and send. Retry attempts reserve separately. Request slots remain consumed after dispatch intent; only proven never-dispatched work releases them.

`Money` uses checked integer nanocurrency units and currency; rate cards carry version, validity interval, per-request fees, and conservative rates for all billable token classes. Round allowance upward. A hard dollar ceiling is available only with verified upper billable-unit bounds and complete applicable rates; otherwise reject `--max-cost` as unenforceable and offer request/byte limits with unknown/estimated spend. Never infer zero cost from missing usage or discounted caching.

Keep unknown charges reserved; no timeout refund. Explicit uncertain retry spends a new allowance. Provider-confirmed usage can settle known charges; reconciliation records amendments rather than editing historical receipts. A provider violating the configured bound invalidates that guarantee, records overspend, and stops further dispatch. External-agent usage and network activity inside a trusted credential helper are outside dispatcher billing observation; neither can be capped by it.

On cancellation, persist completed work, mark unresolved dispatches, and stop new requests. Recovery reconciles journal entries with Markdown outputs before scheduling. Markdown-only restore recovers recorded work, not missing charges: without complete operational accounting or provider reconciliation, continuing the old hard-dollar budget is prohibited. A new run must disclose previous unknown spend. No automatic synthesis or embedding follows cache loss.

## 10. Provider failure tests

| Injection | Required outcome |
|---|---|
| Cloned profile override; redirect with auth | No credentials sent; no helper launched |
| Offline/dry-run cache miss | Zero HTTP/DNS/helpers; dry-run zero writes |
| Expired/malformed/oversized helper output; 401 storm | Bounded failure/one refresh; no secret leakage |
| Reordered/missing/NaN/wrong-dimension vectors | Reorder valid data; reject invalid batch atomically |
| Truncated/refused/tool-call/invalid generation | Receipt retained; no import or hidden repair call |
| 429, long Retry-After, timeout after send | Bounded attempts/deadline; unknown charge retained |
| Two processes reserve final slot | At most one admitted; caps include retries |
| Crash before/after each journal/commit boundary | Replay without lost completed output/double settlement |
| Resume after changed source/model; deleted SQLite | Reuse only matching durable work; no implicit paid rebuild |

Run these against mocks and fault-injected storage first; add opt-in live contract tests for the actual selected endpoints before claiming interoperability.

### Probe allowance and gateway headers

Explicit generation probes request up to 256 output tokens, bounded by the configured service maximum and caller output-unit ceiling. Unproven gateway token/cost ceilings still fail admission rather than promising unsupported hard guarantees; request/byte ceilings remain available. Responses and Chat use the configured adapter for probes.

Response header names follow RFC 9110 token syntax, including underscores, while count/byte/name/value-control limits remain enforced. Invalid headers report `response_headers_invalid`; oversized bodies report `response_bound`. [RFC 9110 §5.6.2](https://www.rfc-editor.org/rfc/rfc9110.html#name-tokens)

## Cleanup additions to the job contract

`jobs status --run ID` exposes retained effective limits/deadline and reservations. `jobs amend --run ID --reason TEXT` permits monotonic amendments for planned, paused or stopped jobs, including expired planned jobs. It appends history, never resets consumed bytes/requests/units or uncertain charges. Explicit CLI overrides must equal effective retained values before any new dispatch; omitted CLI flags inherit. A concrete library runtime without an override mask must supply effective values. Amendment does not authorize an uncertain resend: `--retry-uncertain` remains explicit, and admission still enforces original exposure/concurrency/attempt/lifetime holds. Corpus freshness is checked before a new embedding send; an already paid Received response can be reconciled locally without another call.

Completed useful output can coexist with an unsettled earlier attempt; such a run remains paused and inspectable with its holds. A semantic rejection retains bounded private output (256 KiB), fixed grounding diagnostics and explicit host-import repair; it never automatically salvages items or buys repair. HTTP error-body retention is opt-in (`--retain-http-error-body`, 16 KiB). Safe metadata inspection is the default; `jobs diagnostics inspect ... --raw` exposes private text explicitly. Prune requires a settled, known-billing attempt. Normal errors omit body text and model-controlled identifiers.

During a pending storage cleanup epoch, operational mutations and paid admission refuse while read-only status remains available. Compact checkpoint migration is verified before obsolete transaction payloads can expire; complete journals, receipt authority and unknown-charge holds remain protected. Complete-vault backup includes `.wiki/state` and `.wiki/retained`; Markdown-only reconstruction cannot recreate accounting.

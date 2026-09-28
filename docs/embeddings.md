# Remote embedding API contract

Status: proposed, 2026-09-28. This document incorporates the user's requirement for remote semantic embeddings, static or dynamic credentials, a full URL, and a model. **No local models are part of the plan.**

## Compatible HTTP surface

Implement a small client for the `/v1/embeddings` request/response shape. Use the exact configured URL, including any deployment path or non-secret query parameters; do not append `/v1` or `/embeddings`. Accept a model identifier as an opaque string.

The baseline sends `model`, an array of input strings, and `encoding_format: "float"`. Send `dimensions` only when configured. Parse vectors by their response `index`, not response order, and retain model/usage fields when supplied. These fields follow the [OpenAI embeddings reference](https://developers.openai.com/api/reference/resources/embeddings/methods/create); compatible providers still need contract tests because they can support only a subset.

Validate vector count, unique indices, finite values, nonzero norms, consistent dimension, and configured dimension if present. Missing usage is recorded as unknown, never zero. Store float32 vectors and normalize for cosine search. Base64 response support can be a later capability unless a target provider needs it immediately.

## Example configuration

The following is a **user-local trusted profile**, not configuration to accept automatically from a cloned wiki. Bind endpoint, authentication, and secret headers together. Shared `WIKI.md` selects an allowed profile by name; it cannot override that profile's destination and redirect existing credentials elsewhere. A user explicitly configuring a new endpoint establishes that binding once. TOML remains appropriate for private machine configuration; it is outside the portable knowledge format.

Illustrative values are tuning defaults to validate, not provider limits:

```toml
[embeddings]
profile = "primary"
url = "https://embeddings.example.com/v1/embeddings"
model = "your-embedding-model"
# dimensions = 1024              # omit unless the provider supports it
revision = "deployment-2026-09"  # caller-managed embedding-space identity
encoding_format = "float"
metric = "cosine"
document_prefix = ""
query_prefix = ""
granularity = "auto"             # whole short notes; split long inputs as needed
# target_unit_tokens = ...       # optional quality target, not a corpus-size rule
timeout_seconds = 60
connect_timeout_seconds = 10
max_batch_items = 32
max_batch_bytes = 262144
max_concurrency = 2
max_attempts = 3
# max_input_tokens = ...         # provider/model-specific; no universal default
# max_batch_tokens = ...
# tokenizer = ...                # known tokenizer, otherwise counts are estimates

[embeddings.headers]
# "organization" = "non-secret-routing-value"

[embeddings.auth]
kind = "static"
key_env = "WIKI_EMBEDDING_KEY"
header = "Authorization"
prefix = "Bearer "
```

Authentication has two mutually exclusive modes:

| Mode | Required behavior |
|---|---|
| Static | Exactly one of `key`, `key_env`, or `key_file`; literal keys only in a user-local/private configuration |
| Command | An executable and argument array producing a credential on stdout; optional expiry for refresh |

A direct `key = "..."` is supported as requested, but examples and initialization should favor environment/file references so shared wiki configuration stays portable. Never echo credentials through `config show`, diagnostic output, trace logs, receipts, or errors. Secret headers use environment/file references too; ordinary custom headers cannot override managed authorization, content type, or content length accidentally.

## Dynamic credential protocol

Replace the auth block with:

```toml
[embeddings.auth]
kind = "command"
command = ["/absolute/path/to/get-wiki-token", "--audience", "embeddings"]
output = "json"
timeout_seconds = 10
refresh_skew_seconds = 60
header = "Authorization"
prefix = "Bearer "
```

The script returns:

```json
{"key":"<short-lived-token>","expires_at":"2026-09-28T18:00:00Z"}
```

Also support `output = "text"` for a script printing only a token, trimming the final newline. Without expiry, cache only within a bounded lifetime in that process; never assume credentials survive indefinitely. An explicit TTL can be configured. Do not persist tokens in the index.

Spawn the executable directly, without shell interpolation. Scripts needing a shell specify that executable explicitly. Windows batch files have special process-launch semantics; define supported helper types and test argument handling rather than assuming Unix behavior. Limit stdout bytes, enforce a timeout, validate the credential, suppress secret-bearing child output, and avoid concurrent refresh storms. On HTTP 401, refresh command credentials once and retry within the total attempt and spending limits; static credentials fail with an actionable error. HTTP 403 is not automatically interpreted as expiry.

Credential commands execute only when a remote operation actually needs them. Their authorization/configuration belongs to the user, never to captured sources or generated wiki content. A cloned vault should not silently execute a repository-supplied credential command or send an environment key to a repository-selected URL: resolve every auth mode through a locally trusted endpoint/profile. Ordinary reading, local indexing, and offline search never invoke it.

## Reliability and provider differences

- Support custom authorization header/prefix for gateways such as `api-key` authentication, and separately referenced secret headers if required.
- Require HTTPS for non-loopback endpoints and normal certificate verification. Provide custom CA configuration for managed environments. Do not follow redirects carrying credentials to another origin. Local HTTP is a development/test capability, not a local-model requirement.
- Respect `Retry-After`, use bounded backoff with jitter, and limit retryable network/429/5xx failures. Never retry invalid input indefinitely. An uncertain response can still have incurred a charge.
- Limit response bytes and request bytes as well as item counts. Provider token limits are profile-specific; unknown tokenization must remain explicit. Overlong inputs are deterministically segmented or reported; never silently truncated.
- Add rate controls (`requests_per_minute`, `tokens_per_minute` where measurable), cancellation, and resumable batches. Persist successful vectors/receipts before starting the next batch.
- Start with documented request fields. Provider-specific additional body options, if needed, must be namespaced/allowlisted and cannot replace `input`, `model`, or integrity fields unnoticed. Different query/document task options belong in the embedding-space identity.

## Index compatibility and cost visibility

Default to a whole short note/entity/assertion per embedding input. Split longer documents only when the formatted input exceeds the provider limit or a configured retrieval-quality target; prefer headings/paragraphs before a bounded fallback. Increasing the number of short files is not a reason to split them. Derived units stay in SQLite, with no chunk files added to the vault. Heading labels are metadata, not stable unit IDs. Long extraction windows are a separate concern from embedding segmentation. See [the Markdown format's document-first policy](wiki-format.md).

Fingerprint the sanitized endpoint/profile identity, requested model and explicit revision, dimensions, input formatting, and normalization policy. Record any returned model identity. Keep parser/segmentation generation separately: cache by vector-space fingerprint plus the actual formatted input hash, so identical inputs can reuse vectors after a parser upgrade. Attach those vectors to the new document/unit generation. A heading edit only invalidates inputs that actually include the changed heading. Credentials do not change the vector space, so key rotation does not trigger re-embedding.

Never mix incompatible spaces. New model/dimension/format settings create a candidate generation, which becomes active after coverage and evaluation checks. A provider can change a model behind an alias without changing dimension; metadata checks cannot guarantee detection. Prefer pinned deployment versions and explicit operator-managed revisions. A drift probe may help, but is not proof of model identity.

Proposed commands:

```sh
lwiki embeddings check --json         # local configuration validation only
lwiki embeddings check --probe        # explicit small remote request
lwiki embeddings sync --dry-run       # changed/missing inputs and estimate
lwiki embeddings sync --max-requests 100
lwiki search 'conceptual query' --mode semantic --json
```

`index sync` updates local lexical/graph data only. `embeddings sync` sends selected document text to the configured endpoint. Semantic search sends query text and reads local vectors; it does not upload the corpus on each query. Cache unchanged document inputs and recent query inputs per embedding space.

Report eligible, embedded, stale, and failed counts, plus request count, retries, reported token usage, estimated spend, and rate-card date. A dollar ceiling needs a valid provider rate card and bounded billable units; otherwise enforce request/byte/token limits and label dollar cost unknown. Provider receipts and actual billing remain authoritative.

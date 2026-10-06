# Provider configuration and testing

Generation uses **Responses** for new services. Keep existing embeddings and authentication settings, and change the generation service to:

```toml
[services.generate-main]
adapter = "responses-v1"
url = "https://gateway.example/v1/responses"
model = "your-generation-deployment"
revision = "your-deployment-revision"
response_mode = "json-schema"

[services.generate-main.auth]
kind = "static"
key_env = "WIKI_GENERATION_KEY"
```

Your profile's `generation` value must name this service. The private provider file also requires a vault binding; see the [complete configuration contract](technical/providers-jobs.md#2-private-profiles-and-trust). Keep this file outside the vault, with private permissions. Existing command authentication works with either generation adapter.

`adapter` defaults to `responses-v1` when omitted. Explicit adapters are clearer, especially in files containing embedding services. Set embedding services to `embeddings-v1`. Responses does not accept Chat-only `instruction_role` or `output_limit_field`; remove those when migrating. The endpoint is used exactly as configured and is never silently converted.

Responses sends `store:false`, no tools, and one user input. Schema mode uses a bounded provider grammar while keeping the complete original local validator, including extraction evidence checks. Optional fields remain optional; no fabricated dates, units or null substitutions are introduced. Unsupported or excessive schemas fail before sending. For gateways without structured output, use `response_mode = "text-json"` (the default). This mode accepts bare JSON or exactly one outer ` ```json ` fence; prose around JSON is rejected.

For gateways without Responses, retain:

```toml
adapter = "chat-completions-v1"
url = "https://gateway.example/v1/chat/completions"
instruction_role = "system"
output_limit_field = "max_completion_tokens" # or max_tokens if required
response_mode = "text-json"
```

## Set up embeddings for a new wiki

Use a private provider file outside the wiki. `--providers-config` requires an **absolute path**; a relative path is rejected. Obtain the wiki ID from `lwiki --wiki /absolute/wiki --json read --path WIKI.md` (`meta.wiki_id`). Replace all three path/ID placeholders below with your actual values:

```toml
version = 1
[profiles.openai]
embedding = "embed-small"

[services.embed-small]
adapter = "embeddings-v1"
url = "https://api.openai.com/v1/embeddings"
model = "text-embedding-3-small"
revision = "my-embedding-policy-v1"
dimensions = 1536
max_batch_items = 32
max_batch_bytes = 131072

[services.embed-small.auth]
kind = "static"
key_file = "/absolute/private/token"

[vault_bindings.personal]
root = "/absolute/wiki"
wiki_id = "paste-your-vault-id"
allowed_profiles = ["openai"]
```

The configuration and credential files must be regular private files; on Unix use owner-only permissions such as `0600`. A token file may end in one LF or CRLF. Alternatively use `key_env = "OPENAI_API_KEY"` and provide the key in that process's environment. Never put the token in captured sources, a shared vault, or command arguments. The root/ID binding authorizes this specific wiki; a copied wiki requires its own explicit binding.

With `WIKI` and `PROVIDERS` set to their absolute paths, these commands make small, explicitly bounded paid requests:

```sh
lwiki --wiki "$WIKI" --json doctor --probe --role embed \
  --providers-config "$PROVIDERS" --profile openai --max-requests 1
lwiki --wiki "$WIKI" --json embeddings sync \
  --providers-config "$PROVIDERS" --profile openai --max-requests 20 \
  --quality-target-bytes 3000 --max-input-bytes 8000
lwiki --wiki "$WIKI" search 'how can I recover from a failed deployment' --mode semantic \
  --providers-config "$PROVIDERS" --profile openai --max-requests 1
```

A successful sync reports published coverage. Normalized preparation retains acknowledgments for unchanged owners, so an unchanged repeat reports zero generated and reused inputs without rereading their text. `reused_inputs` counts distinct compatible cached inputs consulted for the invocation's selected work; total retained coverage is reported separately. Inputs generated or recovered during the same invocation are excluded from reuse counts. Semantic search embeds the query, then searches the retained corpus vectors. A cached identical query can subsequently run with `--offline`. Offline mode cannot embed a new query. Add `--dry-run` to preview an operation without sending text or opening credentials; coverage and provider authorization may remain unchecked.

Request limits include retries. Each embedding attempt conservatively reserves **8 MiB of response allowance**, even when its eventual response is smaller. A `--max-response-bytes` ceiling must cover settled usage plus outstanding and proposed reservations. Without a complete trusted rate card, monetary cost remains unknown and conservative reservations can remain after successful output. This is not a report of zero spend or a claim that no token usage was returned. Inspect `jobs status --run RUN_ID` before changing a budget. A failed probe is retained and paused; correcting configuration and rerunning the probe creates a new run, preserving earlier accounting.

The [live usability evaluation](execution/reports/UX-VALIDATION.md) records the exact tested corpus, model and limitations. Other endpoints, models and generation adapters still require their own checks.

## Manual checks

Run commands against a disposable vault with your explicit provider file and profile:

```sh
lwiki --wiki VAULT --json doctor --probe --role embed \
  --providers-config PROVIDERS --profile PROFILE --max-requests 1
lwiki --wiki VAULT --json doctor --probe --role generate \
  --providers-config PROVIDERS --profile PROFILE --max-requests 1
lwiki --wiki VAULT --json embeddings sync \
  --providers-config PROVIDERS --profile PROFILE --max-requests 5
lwiki --wiki VAULT --json search 'who is responsible for backups' --mode semantic \
  --providers-config PROVIDERS --profile PROFILE --max-requests 1
lwiki --wiki VAULT --json graph extract --executor api --source-id SOURCE_ID \
  --providers-config PROVIDERS --profile PROFILE --max-requests 2
```

These commands can make paid requests. Generation probes allow up to 256 output tokens, reduced by configured service and explicit caller ceilings. `--dry-run` and `--offline` continue to prohibit provider requests. Follow the existing staged import/resolve/review flow after extraction, then use agent research handoffs (`research run`, `research import`) with the host agent’s own tools. Research does not read provider configuration or execute requests.

An HTTP success can still contain incomplete, refused or invalid output. Errors expose a fixed `reason` in their details (sometimes nested under `cause`) and preserve the same rejection code in local attempt metadata. For example, `incomplete_max_output_tokens` means the provider did not finish within its output allowance; `response_headers_invalid` identifies malformed headers. Error bodies and credential output are never copied into diagnostics. Paid failed attempts remain accounted for, and invalid successful output is not automatically resent.

Gateway usage extensions and nullable details are accepted without inventing zero charges. Unknown token partitions, unknown extensions, or additional cache-write classes can leave cost unknown. Request and byte budgets remain useful; hard token/cost budgets require a proven adapter/model bound and may be unavailable for gateway deployments. A mock test passing does not establish live gateway compatibility or semantic ranking quality.

## Embedding passage size

`embeddings check --offline --json` reports the effective active-space `data.settings` and coverage; without a retained active cache, settings are null. A dry-run reports requested settings and unknown cache state instead. The hard default is 12,000 UTF-8 bytes per rendered input, including title/heading context. Long source text is split with complete original byte coverage; short notes stay whole.

For multi-section articles, an explicitly authorized sync with `--quality-target-bytes 3000 --max-input-bytes 12000` produces finer units, preferring heading and paragraph boundaries. This changes the corpus input policy and may require more requests. Compare cited spans and ranking on the same source set; scores across different models are not directly comparable. After the segmentation correction, run sync to publish the updated membership; unchanged complete input hashes can reuse retained vectors. Deleting `.wiki/cache` removes those vectors.

## Retained jobs and safe diagnostics

`jobs status --run RUN` shows effective cumulative limits, progress and unknown attempts. Raise selected bounds with `jobs amend --run RUN --reason REASON --max-requests N` (and other explicit limit flags); omitted bounds and consumed reservations remain. A resumed CLI command inherits retained bounds unless a flag is supplied; a differing explicit flag requires an amendment. Library runtimes with no override mask must pass the effective retained limits. Retry an uncertain send only with `--retry-uncertain`; successful output can coexist with an older unresolved billing/concurrency hold. A changed current input is refused before resend.

Generation output is limited by the service's `max_output_tokens` (default 4096) and caller bounds. Requests above a known service cap fail before packet/job creation. A provider-free dry run cannot check service configuration or credentials and reports those unchecked dimensions.

Semantic rejection retains bounded private raw output for explicit host repair: inspect safe metadata with `jobs diagnostics inspect --run RUN --attempt ATTEMPT --kind semantic-rejection`, add `--raw` only when you intend to view private text, then submit a corrected grounded response with `graph import --file FILE`. HTTP response bodies are retained only when `--retain-http-error-body` is explicitly set, capped at 16 KiB; inspect them with `--kind http-error`. Normal output uses fixed safe reasons and hashes instead of model-controlled identifiers or body text. `jobs diagnostics prune` refuses unsettled or unknown-billing attempts. These records live in private operational storage and must be included in complete-vault backups.

`doctor --probe --role generate --extraction-schema` sends the actual bounded extraction grammar with synthetic empty output. It checks that contract fixture only; a simple generation probe does not establish extraction-schema compatibility, citation quality or the cause of a previous HTTP400. Real gateway qualification remains external.

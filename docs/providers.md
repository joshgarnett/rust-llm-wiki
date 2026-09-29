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

`adapter` defaults to `responses-v1` when omitted. Explicit adapters are clearer, especially in files containing embedding or search services. Set embedding services to `embeddings-v1`. Responses does not accept Chat-only `instruction_role` or `output_limit_field`; remove those when migrating. The endpoint is used exactly as configured and is never silently converted.

Responses sends `store:false`, no tools, and one user input. Schema mode uses a bounded provider grammar while keeping the complete original local validator, including extraction evidence checks. Optional fields remain optional; no fabricated dates, units or null substitutions are introduced. Unsupported or excessive schemas fail before sending. For gateways without structured output, use `response_mode = "text-json"` (the default). This mode accepts bare JSON or exactly one outer ` ```json ` fence; prose around JSON is rejected.

For gateways without Responses, retain:

```toml
adapter = "chat-completions-v1"
url = "https://gateway.example/v1/chat/completions"
instruction_role = "system"
output_limit_field = "max_completion_tokens" # or max_tokens if required
response_mode = "text-json"
```

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

These commands can make paid requests. Generation probes allow up to 256 output tokens, reduced by configured service and explicit caller ceilings. `--dry-run` and `--offline` continue to prohibit provider requests. Follow the existing staged import/resolve/review flow after extraction, then exercise research with explicit URLs and a small request budget.

An HTTP success can still contain incomplete, refused or invalid output. Errors expose a fixed `reason` in their details (sometimes nested under `cause`) and preserve the same rejection code in local attempt metadata. For example, `incomplete_max_output_tokens` means the provider did not finish within its output allowance; `response_headers_invalid` identifies malformed headers. Error bodies and credential output are never copied into diagnostics. Paid failed attempts remain accounted for, and invalid successful output is not automatically resent.

Gateway usage extensions and nullable details are accepted without inventing zero charges. Unknown token partitions, unknown extensions, or additional cache-write classes can leave cost unknown. Request and byte budgets remain useful; hard token/cost budgets require a proven adapter/model bound and may be unavailable for gateway deployments. A mock test passing does not establish live gateway compatibility or semantic ranking quality.

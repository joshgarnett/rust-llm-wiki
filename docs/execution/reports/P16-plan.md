# P16.a — Trusted profiles and credentials interface proposal

Planning only. Root accepted P15 as local commit `44f3da7` during this handoff, after 131 integrated parents and strict lint/fmt/build/CLI-schema/seed checks; this packet still grants no source implementation or Cargo lease. Read execution README/STATE, P16 work package, providers/jobs sections 1–5, 7, 9, 10, CLI invocation/errors/offline rules, retrieval's dispatcher boundary, and current config/jobs/vault/Cargo interfaces. No source, fixture, Cargo, Git, network, environment-secret, credential-file, or helper action was performed. This file is the sole new artifact.

## Existing boundaries

`config::local::resolve` accepts explicit private JSON preferences and portable flat selectors. `Preferences.profile_permitted` checks the JSON `allowed_profiles` list; it grants no endpoint/credential authority and has no canonical-root binding. Preserve this local JSON format. Provider authority must come from a separate strict private `providers.toml`, loaded only for explicit provider operations, never by vault ancestor traversal. Root supplies/injects its platform user-config path; fixtures use explicit temporary paths. Ordinary local commands never load provider credentials.

Reuse opaque IDs, `VaultRoot::path()` (already canonical), a fresh valid Vault marker ID, `jobs::Capability`, `ExecutionPolicy`, `JobClock`, `CancellationToken`, `AttemptBound`, `RateCard`, and existing error families. P15 provides immutable run config/profile fingerprints and one-use private send authority; P16 must recompute trusted config/profile/endpoint and exact encoded wire bindings before it calls that boundary. Neither deserialized config nor a preference flag constructs authority.

## Minimal root-owned shared surface

Raw TOML DTOs are strict Deserialize-only objects; no Debug/Serialize implementation exposes literal keys, header secrets, helper argv, or parser snippets. Version 1 only, duplicate/unknown keys rejected throughout. Recommended parsing bound reuses the current 64 KiB private-config ceiling.

```text
ProviderConfigDocument {
  version: u32,
  profiles: BTreeMap<String, ProfileConfig>,
  services: BTreeMap<String, ServiceConfig>,
  vault_bindings: BTreeMap<String, VaultBindingConfig>
}
ProfileConfig { embedding: Option<String>, generation: Option<String>, search: Option<String> }
VaultBindingConfig { root: PathBuf, wiki_id: RecordId, allowed_profiles: Vec<String> }
ServiceConfig {
  adapter: AdapterKind, url: String,
  model: Option<String>, revision: Option<String>,
  max_batch_items: Option<u16>, max_batch_bytes: Option<u64>,
  dimensions: Option<Dimensions>, max_input_tokens: Option<u64>, tokenizer: Option<String>,
  instruction_role: Option<InstructionRole>, output_limit_field: Option<OutputLimitField>,
  response_mode: Option<ResponseMode>, max_output_tokens: Option<u64>,
  timeout_seconds: Option<u32>, connect_timeout_seconds: Option<u32>,
  ca_file: Option<PathBuf>, allow_loopback_http: bool,
  headers: BTreeMap<String,String>, secret_headers: BTreeMap<String,SecretReference>,
  auth: AuthConfig, rate_card: Option<RateCard>
}
AdapterKind = EmbeddingsV1 | ChatCompletionsV1 | BraveWebV1
Dimensions = Auto | Fixed(nonzero u32)
InstructionRole = System | Developer
OutputLimitField = MaxCompletionTokens | MaxTokens
ResponseMode = TextJson | JsonSchema
SecretReference = Env{name} | File{path}
AuthConfig = Static{source: LiteralSecret|Env|File,header,prefix}
           | Command{argv,output:Json|Text,timeout_seconds,ttl_seconds,refresh_skew_seconds,header,prefix}
```

The parser must accept the contract's flat service tables and static `key`/`key_env`/`key_file` fields, then enforce exactly one static source. Reject fields belonging to a different adapter; an embedding service cannot be assigned to generation. P16.c uses the explicit wire settings without speculative defaults/fallback requests. Configured tokenizer names alone are not proof of exact token bounds; trusted tested preflight still supplies `TokenBound`. Search/fetch wire implementation remains its later package; `Fetch` is not an authenticated provider capability inferred from these profiles.

Suggested functions/opaque types (root freezes visibility and exact signatures):

```rust,ignore
pub fn load(path: &Path) -> Result<ProviderConfig>; // bounded regular private file; no secret resolution
impl ProviderConfig {
    pub fn authorize(&self, fs: &VaultFs, vault_id: &RecordId,
        profile: &str, capability: Capability) -> Result<TrustedService>;
}
impl TrustedService {
    pub fn summary(&self) -> ServiceSummary; // only IDs/capability/model/fingerprints
    pub(crate) fn recheck(&self, fs: &VaultFs) -> Result<()>;
}
pub struct ServiceSummary {
    pub profile_id: String, pub service_id: String, pub capability: Capability,
    pub model: Option<String>, pub revision: Option<String>,
    pub endpoint_fingerprint: Blake3Hash, pub profile_fingerprint: Blake3Hash,
    pub config_fingerprint: Blake3Hash,
}
```

`TrustedService` has private fields/constructor and no Deserialize. It binds the exact canonical root, marker ID, profile/service/capability, current private config content guard, and complete validated non-secret service configuration. A cloned vault ID at another root fails before any helper/auth operation. Missing/changing marker, mismatched root/ID, profile denial, and cross-capability assignments fail explicitly. Relative binding roots are forbidden; resolve configured existing roots canonically and compare exactly. Recheck before credentials and immediately before transport; this is a guarded check, not a filesystem CAS claim.

Keep original full URL text alongside a parsed URL: append no API path, merge no query parameters, and do not print it in errors. Reject userinfo, fragments, non-HTTP(S), known credential query names, and explicitly disallow secret query values (their absence cannot be perfectly detected). HTTPS certificate/hostname validation remains enabled; only an explicit private test setting permits HTTP to a literal loopback IP. TLS settings cannot disable verification. CA references are bounded regular files with captured content hashes. Transport instructions are fixed no ambient proxy and no redirects, including same-origin redirects with auth. P16.b must enforce them in its client, not merely store flags.

Reject duplicate case-insensitive header names, CR/LF/control injection, and attempts to override managed auth/content/host/length headers through ordinary headers. Extra secret headers are explicit env/file references. Endpoint/space identity excludes credentials. Public config/profile fingerprints bind the normalized non-secret security/wire/rate settings and source selectors; private auth cache keys bind endpoint plus complete auth configuration/credential rotation. Do not export hashes of literal secret values. Root should freeze these encodings before P16/P17 consume them; P17's space identity separately excludes auth and includes its rendering specification.

## Credential broker and injected boundary

```rust,ignore
pub trait SecretInputs: Send + Sync {
    fn environment(&self, name: &str, max_bytes: usize) -> Result<Option<SecretBytes>>;
    fn file(&self, absolute: &Path, max_bytes: usize) -> Result<SecretBytes>;
}
pub trait HelperRunner: Send + Sync {
    fn run(&self, invocation: &HelperInvocation, limits: &HelperLimits,
        clock: &dyn JobClock, cancel: &CancellationToken) -> Result<HelperOutput>;
}
pub struct CredentialOptions {
    pub clock: Arc<dyn JobClock>, pub inputs: Arc<dyn SecretInputs>,
    pub runner: Arc<dyn HelperRunner>,
}
pub struct CredentialContext {
    pub policy: ExecutionPolicy, pub cancel: CancellationToken, pub deadline_utc_ms: i64,
}
impl CredentialBroker {
    pub fn new(options: CredentialOptions) -> Self;
    pub(crate) fn resolve(&self, service: &TrustedService, fs: &VaultFs,
        context: &CredentialContext) -> Result<CredentialLease>;
    pub(crate) fn refresh_after_401(&self, service: &TrustedService, fs: &VaultFs,
        observed_epoch: u64, context: &CredentialContext) -> Result<CredentialLease>;
}
```

`SecretBytes`, helper output, authenticated header values, and `CredentialLease` have no Debug/Serialize/Deserialize/public secret getters. Public tests inject fake secrets/runners via bounded constructors for input/output wrappers; those constructors grant no trusted-service or authenticated-dispatch authority. Production P16.b alone consumes lease headers. No static/env/file token is read during profile validation. Offline/dry-run gates precede inputs, helper launch, and cache mutation; check again after mutex waits and helper completion. Dry-run does not populate an in-memory credential cache or create directories. Static missing/empty/control-bearing tokens fail ProviderAuth safely; key files use bounded checked reads. Suggested shared ceiling: 16 KiB per token/file/output; strip one terminal LF/CRLF from text helper output and, if root agrees, key-file text. Environment/literal values never trim arbitrary whitespace.

Helpers spawn argv directly: absolute executable/interpreter, closed stdin, private-config directory cwd, discarded stderr, bounded concurrent stdout reads, cancellation/monotonic deadline, kill and wait on excess/timeout. Reject implicit Windows .bat/.cmd. Proposed argument ceiling: 64 args, 4 KiB each, 16 KiB aggregate. Configurable timeout cannot exceed the contract's 10 seconds and is further limited by remaining run deadline. JSON stdout is exactly key plus optional expires_at, strict duplicate/unknown-key rejection; text removes exactly one terminal newline. Never attach stdout, stderr, command text, secret paths/values, or provider error bodies to errors. Errors may expose a safe error code/stage and validated profile/service IDs.

Suggested TTL default 300 seconds, proposed validated upper 3600 seconds (root choice); refresh skew 60 by default and strictly less than TTL. Parse expiry with existing `time`, require expiry beyond now+skew, checked arithmetic, and cache until min(expiry-skew, acquisition+TTL). Reject clock regression; cached validity uses both monotonic elapsed and UTC. Serialize refresh per endpoint/auth cache key; waiters reuse the fresh epoch, independent processes may refresh separately. A 401 refresh for command auth uses the observed epoch so parallel 401s do not launch duplicate refreshes. P16.b limits this to once per attempt and reserves a NEW counted HTTP retry; static 401 fails and 403 never triggers refresh. Helpers are trusted user code, not sandboxed, and their own network/spend is outside dispatcher billing observation.

## P15 and dependency requests

Ordering remains trusted non-secret preflight → actual immutable wire/billable bounds → durable reservation/intent → credential resolution → final config/policy/deadline checks → consuming begin_send/SendAuthorization → one transport send. Preserve the stored AttemptRef for post-send spool recording; it never reconstitutes send authority. A trusted dispatcher may prove TransportNotEntered/CredentialsUnavailable to release not-sent allowances; errors alone do not refund. P15 now also checks after durability, and P16 must check immediately before transport. No lock spans helper execution, HTTP, or vault-writer acquisition.

Root dependency request: a strict TOML parser (`toml`) and URL parser (`url`); neither is currently in Cargo.lock. Root verifies/pins available versions and features before implementation. Existing serde/serde_json/time suffice for strict DTOs/expiry, standard mutex/process/io for the synchronous helper. Consider `zeroize` for best-effort token-memory cleanup; no claim that process/environment/compiler copies can be erased. P16.b owns the HTTP/TLS dependency selection (no native external database/runtime); OS config-directory resolution can stay root-injected and avoid another dependency in .a. No dependency, version, host, or live-provider qualification was performed in this packet.

## Planned gates, not executed

- Exact full-URL and canonical root+ID bindings; cloned root/same ID; malicious portable profile; unknown/duplicate TOML keys, wrong adapter capability, managed-header/query-secret/TLS violations. Fake secret inputs/runner assert zero calls on denied grants.
- Offline/dry-run before secret resolution with literal/env/file/command modes, cold/warm cache and expired token; zero helper/input/network callbacks and unchanged fixture tree/cache.
- Static empty/control/oversize sources; helper expiry/skew/TTL/UTC+monotonic regression, malformed/duplicate JSON, one LF/CRLF, excess output, timeout/cancel, stderr redaction, process refresh serialization and observed-epoch 401 reuse. Concrete native helper tests only after implementation authorization, using disposable executables.
- P16.b loopback mocks prove no ambient proxy, verified TLS/custom CA, no redirect auth, one accounted 401 retry and deadline/cancellation after helper; no live compatibility claim.

No gate is ready or waived by this proposal. Independent review of secret/trust boundaries remains required after implementation; Sol fallback must be disclosed if Astra cannot run.

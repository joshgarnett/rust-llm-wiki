//! Explicit private configuration; deserialized preferences never grant authority.
use crate::{
    domain::*,
    jobs::{Capability, RateCard},
    records::parse_note,
    vault::VaultFs,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(not(windows))]
use std::{fs::File, io::Read};

pub const MAX_PROVIDER_CONFIG_BYTES: usize = 65_536;
pub const MAX_SECRET_BYTES: usize = 16_384;
pub const MAX_HELPER_ARGS: usize = 64;
pub const MAX_HELPER_ARG_BYTES: usize = 4_096;
pub const MAX_HELPER_ARG_TOTAL: usize = 16_384;
pub const MAX_HELPER_SECONDS: u32 = 10;
pub const MAX_HELPER_TTL_SECONDS: u32 = 3_600;
fn config_error(message: &'static str) -> WikiError {
    WikiError::new(ErrorCode::ConfigInvalid, message)
}
fn untrusted() -> WikiError {
    WikiError::new(
        ErrorCode::ProfileUntrusted,
        "provider profile is not authorized for this vault and capability",
    )
}
fn hash<T: Serialize>(domain: &'static str, value: &T) -> Result<Blake3Hash> {
    let bytes = serde_json::to_vec(&(domain, value))
        .map_err(|_| config_error("provider fingerprint encoding failed"))?;
    Ok(Blake3Hash::digest(bytes))
}
fn default_ttl() -> u32 {
    300
}
fn default_skew() -> u32 {
    60
}
fn default_timeout() -> u32 {
    10
}
fn default_prefix() -> String {
    "Bearer ".into()
}
fn default_header() -> String {
    "Authorization".into()
}

fn default_generation_adapter() -> AdapterKind {
    AdapterKind::ResponsesV1
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) enum AdapterKind {
    #[serde(rename = "embeddings-v1")]
    EmbeddingsV1,
    #[serde(rename = "chat-completions-v1")]
    ChatCompletionsV1,
    #[serde(rename = "responses-v1")]
    ResponsesV1,
}
impl AdapterKind {
    fn capability(self) -> Capability {
        match self {
            Self::EmbeddingsV1 => Capability::Embed,
            Self::ChatCompletionsV1 | Self::ResponsesV1 => Capability::Generate,
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum Dimensions {
    Fixed(u32),
    Auto(String),
}
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum InstructionRole {
    System,
    Developer,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
pub(crate) enum OutputLimitField {
    #[serde(rename = "max_completion_tokens")]
    MaxCompletionTokens,
    #[serde(rename = "max_tokens")]
    MaxTokens,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
pub(crate) enum ResponseMode {
    #[serde(rename = "text-json")]
    TextJson,
    #[serde(rename = "json-schema")]
    JsonSchema,
}
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HelperOutputMode {
    Json,
    Text,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SecretReference {
    Env { name: String },
    File { path: PathBuf },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum RawAuth {
    Static {
        key: Option<String>,
        key_env: Option<String>,
        key_file: Option<PathBuf>,
        #[serde(default = "default_header")]
        header: String,
        #[serde(default = "default_prefix")]
        prefix: String,
    },
    Command {
        command: Vec<String>,
        output: HelperOutputMode,
        #[serde(default = "default_timeout")]
        timeout_seconds: u32,
        #[serde(default = "default_ttl")]
        ttl_seconds: u32,
        #[serde(default = "default_skew")]
        refresh_skew_seconds: u32,
        #[serde(default = "default_header")]
        header: String,
        #[serde(default = "default_prefix")]
        prefix: String,
    },
}
pub(crate) enum StaticSource {
    Literal(Vec<u8>),
    Env(String),
    File(PathBuf),
}
pub(crate) enum AuthConfig {
    Static {
        source: StaticSource,
        header: String,
        prefix: String,
    },
    Command {
        argv: Vec<String>,
        output: HelperOutputMode,
        timeout_seconds: u32,
        ttl_seconds: u32,
        refresh_skew_seconds: u32,
        header: String,
        prefix: String,
    },
}
impl AuthConfig {
    pub(crate) fn header(&self) -> &str {
        match self {
            Self::Static { header, .. } | Self::Command { header, .. } => header,
        }
    }
    fn redacted(&self) -> serde_json::Value {
        match self {
            Self::Static {
                source,
                header,
                prefix,
            } => {
                let selector = match source {
                    StaticSource::Literal(_) => serde_json::json!({"kind":"literal"}),
                    StaticSource::Env(name) => serde_json::json!({"kind":"env","name":name}),
                    StaticSource::File(path) => serde_json::json!({"kind":"file","path":path}),
                };
                let _ = prefix;
                serde_json::json!({"kind":"static","selector":selector,"header":header,"prefix":"private_prefix"})
            }
            Self::Command {
                output,
                timeout_seconds,
                ttl_seconds,
                refresh_skew_seconds,
                header,
                prefix,
                ..
            } => {
                let _ = prefix;
                serde_json::json!({"kind":"command","command":"private_argv","output":output,"timeout_seconds":timeout_seconds,"ttl_seconds":ttl_seconds,"refresh_skew_seconds":refresh_skew_seconds,"header":header,"prefix":"private_prefix"})
            }
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileConfig {
    embedding: Option<String>,
    generation: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct VaultBinding {
    root: PathBuf,
    wiki_id: RecordId,
    allowed_profiles: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawService {
    #[serde(default = "default_generation_adapter")]
    adapter: AdapterKind,
    url: String,
    model: Option<String>,
    revision: Option<String>,
    max_batch_items: Option<u16>,
    max_batch_bytes: Option<u64>,
    dimensions: Option<Dimensions>,
    max_input_tokens: Option<u64>,
    tokenizer: Option<String>,
    instruction_role: Option<InstructionRole>,
    output_limit_field: Option<OutputLimitField>,
    response_mode: Option<ResponseMode>,
    max_output_tokens: Option<u64>,
    timeout_seconds: Option<u32>,
    connect_timeout_seconds: Option<u32>,
    ca_file: Option<PathBuf>,
    #[serde(default)]
    allow_loopback_http: bool,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    secret_headers: BTreeMap<String, SecretReference>,
    auth: RawAuth,
    rate_card: Option<RateCard>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDocument {
    version: u32,
    profiles: BTreeMap<String, ProfileConfig>,
    services: BTreeMap<String, RawService>,
    vault_bindings: BTreeMap<String, VaultBinding>,
}
/// Validated non-secret adapter and security settings, visible only to providers.
pub(crate) struct Service {
    pub(crate) adapter: AdapterKind,
    pub(crate) url: String,
    pub(crate) model: Option<String>,
    pub(crate) revision: Option<String>,
    pub(crate) max_batch_items: Option<u16>,
    pub(crate) max_batch_bytes: Option<u64>,
    pub(crate) dimensions: Option<Dimensions>,
    pub(crate) max_input_tokens: Option<u64>,
    pub(crate) instruction_role: Option<InstructionRole>,
    pub(crate) output_limit_field: Option<OutputLimitField>,
    pub(crate) response_mode: Option<ResponseMode>,
    pub(crate) max_output_tokens: Option<u64>,
    pub(crate) timeout_seconds: Option<u32>,
    pub(crate) connect_timeout_seconds: Option<u32>,
    #[cfg(test)]
    pub(crate) allow_loopback_http: bool,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) secret_headers: BTreeMap<String, SecretReference>,
    pub(crate) auth: AuthConfig,
    pub(crate) rate_card: Option<RateCard>,
    pub(crate) ca_bytes: Option<Arc<[u8]>>,
    ca_guard: Option<(PathBuf, Blake3Hash)>,
    endpoint_fingerprint: Blake3Hash,
    fingerprint: Blake3Hash,
}
pub struct ProviderConfig {
    path: PathBuf,
    content_guard: Blake3Hash,
    config_fingerprint: Blake3Hash,
    profiles: BTreeMap<String, ProfileConfig>,
    services: BTreeMap<String, Arc<Service>>,
    bindings: BTreeMap<String, VaultBinding>,
}
/// Constructed only by a checked private config/root/vault/capability binding.
pub struct TrustedService {
    path: PathBuf,
    content_guard: Blake3Hash,
    root: PathBuf,
    vault_id: RecordId,
    marker_hash: Blake3Hash,
    service: Arc<Service>,
    summary: ServiceSummary,
    auth_key: Blake3Hash,
    cwd: PathBuf,
}
#[derive(Clone, Debug, Serialize)]
pub struct ServiceSummary {
    pub profile_id: String,
    pub service_id: String,
    pub capability: Capability,
    pub model: Option<String>,
    pub revision: Option<String>,
    pub endpoint_fingerprint: Blake3Hash,
    pub profile_fingerprint: Blake3Hash,
    pub config_fingerprint: Blake3Hash,
}
impl ProviderConfig {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(config_error(
                "provider configuration path must be explicit and absolute",
            ));
        }
        let bytes = checked_file(path, MAX_PROVIDER_CONFIG_BYTES, true)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| config_error("invalid provider configuration encoding"))?;
        let raw: RawDocument =
            toml::from_str(text).map_err(|_| config_error("invalid provider configuration"))?;
        if raw.version != 1
            || raw.profiles.len() > 128
            || raw.services.len() > 128
            || raw.vault_bindings.len() > 4096
        {
            return Err(config_error(
                "unsupported provider configuration version or size",
            ));
        }
        let path = std::fs::canonicalize(path)
            .map_err(|_| config_error("provider configuration unavailable"))?;
        let mut services = BTreeMap::new();
        for (name, raw_service) in raw.services {
            identifier(&name)?;
            services.insert(name, Arc::new(service(raw_service)?));
        }
        for (name, profile) in &raw.profiles {
            identifier(name)?;
            for (id, capability) in [
                (profile.embedding.as_ref(), Capability::Embed),
                (profile.generation.as_ref(), Capability::Generate),
            ] {
                if let Some(id) = id {
                    identifier(id)?;
                    if services
                        .get(id)
                        .is_none_or(|s| s.adapter.capability() != capability)
                    {
                        return Err(config_error(
                            "profile service capability differs or service is missing",
                        ));
                    }
                }
            }
        }
        for (name, binding) in &raw.vault_bindings {
            identifier(name)?;
            if !binding.root.is_absolute()
                || binding.root.to_str().is_none()
                || binding.allowed_profiles.len() > 128
            {
                return Err(config_error("invalid private vault binding"));
            }
            let mut seen = BTreeSet::new();
            for id in &binding.allowed_profiles {
                if !raw.profiles.contains_key(id) || !seen.insert(id) {
                    return Err(config_error("invalid allowed-profile binding"));
                }
            }
        }
        let normalized_services: BTreeMap<_, _> = services
            .iter()
            .map(|(id, s)| (id.clone(), s.fingerprint.clone()))
            .collect();
        let config_fingerprint = hash(
            "lwiki.provider.config.v1",
            &(
                1u32,
                &raw.profiles,
                &normalized_services,
                &raw.vault_bindings,
                (
                    MAX_PROVIDER_CONFIG_BYTES,
                    MAX_SECRET_BYTES,
                    MAX_HELPER_ARGS,
                    MAX_HELPER_ARG_BYTES,
                    MAX_HELPER_ARG_TOTAL,
                    MAX_HELPER_SECONDS,
                    MAX_HELPER_TTL_SECONDS,
                ),
            ),
        )?;
        Ok(Self {
            path,
            content_guard: Blake3Hash::digest(bytes),
            config_fingerprint,
            profiles: raw.profiles,
            services,
            bindings: raw.vault_bindings,
        })
    }
    pub fn authorize(
        &self,
        fs: &VaultFs,
        vault_id: &RecordId,
        profile: &str,
        capability: Capability,
    ) -> Result<TrustedService> {
        if Blake3Hash::digest(checked_file(&self.path, MAX_PROVIDER_CONFIG_BYTES, true)?)
            != self.content_guard
        {
            return Err(untrusted());
        }
        let marker = marker(fs, vault_id)?;
        let bound = self.bindings.values().any(|b| {
            b.wiki_id == *vault_id
                && b.allowed_profiles.iter().any(|p| p == profile)
                && std::fs::canonicalize(&b.root).ok().as_deref() == Some(fs.root().path())
        });
        if !bound {
            return Err(untrusted());
        }
        let p = self.profiles.get(profile).ok_or_else(untrusted)?;
        let service_id = match capability {
            Capability::Embed => p.embedding.as_ref(),
            Capability::Generate => p.generation.as_ref(),
            _ => None,
        }
        .ok_or_else(untrusted)?;
        let service = self.services.get(service_id).ok_or_else(untrusted)?.clone();
        check_ca(&service)?;
        let profile_fingerprint = hash(
            "lwiki.provider.profile.v1",
            &(profile, capability, service_id, &service.fingerprint),
        )?;
        let summary = ServiceSummary {
            profile_id: profile.into(),
            service_id: service_id.clone(),
            capability,
            model: service.model.clone(),
            revision: service.revision.clone(),
            endpoint_fingerprint: service.endpoint_fingerprint.clone(),
            profile_fingerprint,
            config_fingerprint: self.config_fingerprint.clone(),
        };
        let auth_key = hash(
            "lwiki.provider.private-auth.v1",
            &(
                &self.content_guard,
                service_id,
                &service.endpoint_fingerprint,
            ),
        )?;
        let cwd = self
            .path
            .parent()
            .ok_or_else(|| config_error("private config directory unavailable"))?
            .to_path_buf();
        Ok(TrustedService {
            path: self.path.clone(),
            content_guard: self.content_guard.clone(),
            root: fs.root().path().to_path_buf(),
            vault_id: vault_id.clone(),
            marker_hash: Blake3Hash::digest(marker),
            service,
            summary,
            auth_key,
            cwd,
        })
    }
}
impl TrustedService {
    pub fn summary(&self) -> ServiceSummary {
        self.summary.clone()
    }
    pub(crate) fn service(&self) -> &Service {
        &self.service
    }
    pub(crate) fn auth_key(&self) -> &Blake3Hash {
        &self.auth_key
    }
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }
    pub(crate) fn recheck(&self, fs: &VaultFs) -> Result<()> {
        if fs.root().path() != self.root
            || Blake3Hash::digest(marker(fs, &self.vault_id)?) != self.marker_hash
            || Blake3Hash::digest(checked_file(&self.path, MAX_PROVIDER_CONFIG_BYTES, true)?)
                != self.content_guard
        {
            return Err(untrusted());
        }
        check_ca(&self.service)
    }
}
fn marker(fs: &VaultFs, id: &RecordId) -> Result<Vec<u8>> {
    let bytes = crate::changes::prepare::read_bounded(
        fs,
        &VaultRelativePath::new("WIKI.md")?,
        MAX_PROVIDER_CONFIG_BYTES,
    )
    .map_err(|_| untrusted())?
    .ok_or_else(untrusted)?;
    let note = parse_note(&bytes);
    if !note
        .canonical
        .as_ref()
        .is_some_and(|r| r.kind() == RecordKind::Vault && r.id() == id)
    {
        return Err(untrusted());
    }
    Ok(bytes)
}
fn check_ca(service: &Service) -> Result<()> {
    if let Some((path, expected)) = &service.ca_guard
        && Blake3Hash::digest(checked_file(path, MAX_PROVIDER_CONFIG_BYTES, false)?) != *expected
    {
        return Err(untrusted());
    }
    Ok(())
}
fn identifier(value: &str) -> Result<()> {
    RecordId::new(value)
        .map(|_| ())
        .map_err(|_| config_error("invalid private provider identifier"))
}
pub(crate) fn checked_file(path: &Path, max: usize, private: bool) -> Result<Vec<u8>> {
    #[cfg(windows)]
    {
        use crate::vault::{acl_policy::Protection, windows_security};
        windows_security::read_protected(
            path,
            max,
            if private {
                Protection::Private
            } else {
                Protection::IntegrityProtected
            },
        )
        .map_err(|_| config_error("provider input protection, identity or byte bound failed"))
    }
    #[cfg(not(windows))]
    {
        let before = std::fs::symlink_metadata(path)
            .map_err(|_| config_error("private provider input unavailable"))?;
        if !before.is_file() || before.file_type().is_symlink() || before.len() > max as u64 {
            return Err(config_error(
                "provider input must be a bounded regular file",
            ));
        }
        protected_permissions(&before, private)?;
        let mut file =
            File::open(path).map_err(|_| config_error("private provider input unavailable"))?;
        let opened = file
            .metadata()
            .map_err(|_| config_error("private provider input unavailable"))?;
        if !opened.is_file() || opened.len() > max as u64 {
            return Err(config_error(
                "provider input must be a bounded regular file",
            ));
        }
        protected_permissions(&opened, private)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if before.dev() != opened.dev() || before.ino() != opened.ino() {
                return Err(config_error("provider input changed while opening"));
            }
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| config_error("private provider input unavailable"))?;
        if bytes.len() > max {
            return Err(config_error("provider input exceeds its byte ceiling"));
        }
        let after = std::fs::symlink_metadata(path)
            .map_err(|_| config_error("private provider input unavailable"))?;
        if !after.is_file() || after.file_type().is_symlink() || after.len() != bytes.len() as u64 {
            return Err(config_error("provider input changed while reading"));
        }
        protected_permissions(&after, private)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if opened.dev() != after.dev() || opened.ino() != after.ino() {
                return Err(config_error("provider input changed while reading"));
            }
        }
        Ok(bytes)
    }
}
#[cfg(not(windows))]
fn protected_permissions(metadata: &std::fs::Metadata, private: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & if private { 0o077 } else { 0o022 } != 0 {
            return Err(config_error(
                "provider input permissions are not private or protected",
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (metadata, private);
        return Err(config_error(
            "protected provider inputs unavailable on this platform",
        ));
    }
    Ok(())
}
fn header_name(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 128
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    {
        return Err(config_error("invalid provider header configuration"));
    }
    Ok(())
}
fn env_name(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 128
        || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || s.as_bytes()[0].is_ascii_digit()
    {
        return Err(config_error("invalid credential source configuration"));
    }
    Ok(())
}
fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(config_error(
            "provider file and executable paths must be absolute UTF-8",
        ));
    }
    Ok(())
}
fn bounded_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn validate_reference(r: &SecretReference) -> Result<()> {
    match r {
        SecretReference::Env { name } => env_name(name),
        SecretReference::File { path } => absolute(path),
    }
}
fn auth(raw: RawAuth) -> Result<AuthConfig> {
    let a = match raw {
        RawAuth::Static {
            key,
            key_env,
            key_file,
            header,
            prefix,
        } => {
            if usize::from(key.is_some())
                + usize::from(key_env.is_some())
                + usize::from(key_file.is_some())
                != 1
            {
                return Err(config_error(
                    "static auth requires exactly one credential source",
                ));
            }
            let source = if let Some(s) = key {
                if s.len() > MAX_SECRET_BYTES {
                    return Err(config_error("literal credential exceeds byte ceiling"));
                }
                StaticSource::Literal(s.into_bytes())
            } else if let Some(s) = key_env {
                env_name(&s)?;
                StaticSource::Env(s)
            } else {
                let p = key_file.ok_or_else(|| config_error("credential source missing"))?;
                absolute(&p)?;
                StaticSource::File(p)
            };
            AuthConfig::Static {
                source,
                header,
                prefix,
            }
        }
        RawAuth::Command {
            command,
            output,
            timeout_seconds,
            ttl_seconds,
            refresh_skew_seconds,
            header,
            prefix,
        } => {
            if command.is_empty()
                || command.len() > MAX_HELPER_ARGS
                || command
                    .iter()
                    .any(|s| !bounded_text(s, MAX_HELPER_ARG_BYTES))
                || command.iter().map(String::len).sum::<usize>() > MAX_HELPER_ARG_TOTAL
                || timeout_seconds == 0
                || timeout_seconds > MAX_HELPER_SECONDS
                || ttl_seconds == 0
                || ttl_seconds > MAX_HELPER_TTL_SECONDS
                || refresh_skew_seconds >= ttl_seconds
            {
                return Err(config_error(
                    "invalid bounded credential helper configuration",
                ));
            }
            absolute(Path::new(&command[0]))?;
            if command[0].to_ascii_lowercase().ends_with(".bat")
                || command[0].to_ascii_lowercase().ends_with(".cmd")
            {
                return Err(config_error(
                    "implicit batch credential helpers are unsupported",
                ));
            }
            AuthConfig::Command {
                argv: command,
                output,
                timeout_seconds,
                ttl_seconds,
                refresh_skew_seconds,
                header,
                prefix,
            }
        }
    };
    header_name(a.header())?;
    if [
        "host",
        "proxy-authorization",
        "content-type",
        "content-length",
        "connection",
        "transfer-encoding",
    ]
    .contains(&a.header().to_ascii_lowercase().as_str())
    {
        return Err(config_error(
            "credential header conflicts with managed transport headers",
        ));
    }
    let prefix = match &a {
        AuthConfig::Static { prefix, .. } | AuthConfig::Command { prefix, .. } => prefix,
    };
    if prefix.len() > 128 || prefix.chars().any(char::is_control) {
        return Err(config_error("invalid auth header prefix"));
    }
    Ok(a)
}
fn service(r: RawService) -> Result<Service> {
    if !bounded_text(&r.url, 4096) || r.url.chars().any(char::is_whitespace) || r.url.contains('\\')
    {
        return Err(config_error("invalid provider endpoint"));
    }
    let u = url::Url::parse(&r.url).map_err(|_| config_error("invalid provider endpoint"))?;
    let loopback = match u.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    let authority_has_userinfo = r.url.split_once("://").is_some_and(|(_, rest)| {
        rest.split(['/', '?', '#'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
    });
    if authority_has_userinfo
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.host_str().is_none()
        || (u.scheme() != "https" && !(u.scheme() == "http" && r.allow_loopback_http && loopback))
    {
        return Err(config_error(
            "provider endpoint violates private URL/TLS policy",
        ));
    }
    for (key, _) in u.query_pairs() {
        if [
            "key",
            "api_key",
            "apikey",
            "token",
            "access_token",
            "oauth_token",
            "api_token",
            "auth",
            "authorization",
            "password",
            "secret",
            "client_secret",
            "signature",
            "sig",
        ]
        .contains(&key.to_ascii_lowercase().replace('-', "_").as_str())
        {
            return Err(config_error("credential query parameters are forbidden"));
        }
    }
    for text in [&r.model, &r.revision, &r.tokenizer].into_iter().flatten() {
        if !bounded_text(text, 256) {
            return Err(config_error(
                "invalid provider model/revision/tokenizer setting",
            ));
        }
    }
    if r.timeout_seconds == Some(0)
        || r.timeout_seconds.is_some_and(|n| n > 900)
        || r.connect_timeout_seconds == Some(0)
        || r.connect_timeout_seconds.is_some_and(|n| n > 10)
        || r.max_batch_items == Some(0)
        || r.max_batch_items.is_some_and(|n| n > 4096)
        || r.max_batch_bytes == Some(0)
        || r.max_batch_bytes
            .is_some_and(|n| n > crate::jobs::OTHER_SPOOL_MAX_BYTES)
        || r.max_input_tokens == Some(0)
        || r.max_output_tokens == Some(0)
    {
        return Err(config_error("invalid provider request bounds"));
    }
    if let Some(d) = &r.dimensions {
        match d {
            Dimensions::Fixed(n) if *n > 0 && *n <= 65536 => {}
            Dimensions::Auto(s) if s == "auto" => {}
            _ => return Err(config_error("invalid embedding dimensions")),
        }
    }
    let embedding = r.max_batch_items.is_some()
        || r.max_batch_bytes.is_some()
        || r.dimensions.is_some()
        || r.max_input_tokens.is_some()
        || r.tokenizer.is_some();
    let generation = r.instruction_role.is_some()
        || r.output_limit_field.is_some()
        || r.response_mode.is_some()
        || r.max_output_tokens.is_some();
    if (r.adapter != AdapterKind::EmbeddingsV1 && embedding)
        || (!matches!(
            r.adapter,
            AdapterKind::ChatCompletionsV1 | AdapterKind::ResponsesV1
        ) && generation)
        || (r.adapter == AdapterKind::ResponsesV1
            && (r.instruction_role.is_some() || r.output_limit_field.is_some()))
        || r.model.is_none()
    {
        return Err(config_error(
            "adapter settings do not match the declared capability",
        ));
    }
    let a = auth(r.auth)?;
    let mut seen = BTreeSet::new();
    seen.insert(a.header().to_ascii_lowercase());
    let managed = [
        "authorization",
        "proxy-authorization",
        "content-type",
        "content-length",
        "host",
        "connection",
        "transfer-encoding",
    ];
    for (name, value) in &r.headers {
        header_name(name)?;
        if value.len() > 4096
            || value.chars().any(char::is_control)
            || managed.contains(&name.to_ascii_lowercase().as_str())
            || !seen.insert(name.to_ascii_lowercase())
        {
            return Err(config_error("duplicate or managed provider header"));
        }
    }
    for (name, source) in &r.secret_headers {
        header_name(name)?;
        validate_reference(source)?;
        if managed.contains(&name.to_ascii_lowercase().as_str())
            || !seen.insert(name.to_ascii_lowercase())
        {
            return Err(config_error("duplicate or managed provider secret header"));
        }
    }
    let (ca_bytes, ca_guard) = if let Some(path) = r.ca_file {
        absolute(&path)?;
        let b = checked_file(&path, MAX_PROVIDER_CONFIG_BYTES, false)?;
        let p = std::fs::canonicalize(path).map_err(|_| config_error("provider CA unavailable"))?;
        (
            Some(Arc::<[u8]>::from(b.clone())),
            Some((p, Blake3Hash::digest(b))),
        )
    } else {
        (None, None)
    };
    if let Some(card) = &r.rate_card
        && (card.version == 0
            || card.fingerprint
                != crate::jobs::budgets::rate_card_fingerprint(card)
                    .map_err(|_| config_error("invalid pricing configuration"))?)
    {
        return Err(config_error("invalid pricing configuration"));
    }
    let endpoint_fingerprint = hash(
        "lwiki.provider.endpoint.v1",
        &(
            &r.url,
            ca_guard.as_ref().map(|(_, h)| h),
            r.allow_loopback_http,
            true,
            "no_proxy",
            "no_redirect",
        ),
    )?;
    let projection = serde_json::json!({"adapter":r.adapter,"endpoint":endpoint_fingerprint,"model":r.model,"revision":r.revision,"max_batch_items":r.max_batch_items,"max_batch_bytes":r.max_batch_bytes,"dimensions":r.dimensions,"max_input_tokens":r.max_input_tokens,"tokenizer":r.tokenizer,"instruction_role":r.instruction_role,"output_limit_field":r.output_limit_field,"response_mode":r.response_mode,"max_output_tokens":r.max_output_tokens,"timeout_seconds":r.timeout_seconds,"connect_timeout_seconds":r.connect_timeout_seconds,"headers":r.headers,"secret_headers":r.secret_headers,"auth":a.redacted(),"rate_card":r.rate_card});
    let fingerprint = hash("lwiki.provider.service.v1", &projection)?;
    Ok(Service {
        adapter: r.adapter,
        url: r.url,
        model: r.model,
        revision: r.revision,
        max_batch_items: r.max_batch_items,
        max_batch_bytes: r.max_batch_bytes,
        dimensions: r.dimensions,
        max_input_tokens: r.max_input_tokens,
        instruction_role: r.instruction_role,
        output_limit_field: r.output_limit_field,
        response_mode: r.response_mode,
        max_output_tokens: r.max_output_tokens,
        timeout_seconds: r.timeout_seconds,
        connect_timeout_seconds: r.connect_timeout_seconds,
        #[cfg(test)]
        allow_loopback_http: r.allow_loopback_http,
        headers: r.headers,
        secret_headers: r.secret_headers,
        auth: a,
        rate_card: r.rate_card,
        ca_bytes,
        ca_guard,
        endpoint_fingerprint,
        fingerprint,
    })
}

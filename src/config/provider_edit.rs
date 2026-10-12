//! Bounded, append-only edits of one wiki's provider roles.
//!
//! Original tables and bytes remain intact. Distinct edits retain old definitions
//! and can reach the existing 128-entry or 64 KiB limits; this is not a pruning
//! mechanism. Planning resolves no credentials and runs no credential command.
use super::{
    DefaultWiki,
    provider_setup::{
        SetupAuth, append_automatic_embedding_grant, auth_table, dimensions_value, encode_document,
        insert_exact, invalid, parse_document,
    },
    providers::{AutomaticEmbeddingConsent, ProviderAutomaticEmbeddingDecision, ReasoningEffort},
};
use crate::domain::*;
use serde::Serialize;
use std::fmt;
use toml::{Table, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderRole {
    Embedding,
    Generation,
}

impl ProviderRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Generation => "generation",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Embedding => 0,
            Self::Generation => 1,
        }
    }
}

const ROLES: [ProviderRole; 2] = [ProviderRole::Embedding, ProviderRole::Generation];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ProviderAdapter {
    #[serde(rename = "embeddings-v1")]
    EmbeddingsV1,
    #[serde(rename = "responses-v1")]
    ResponsesV1,
    #[serde(rename = "chat-completions-v1")]
    ChatCompletionsV1,
}

impl ProviderAdapter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmbeddingsV1 => "embeddings-v1",
            Self::ResponsesV1 => "responses-v1",
            Self::ChatCompletionsV1 => "chat-completions-v1",
        }
    }

    fn role(self) -> ProviderRole {
        match self {
            Self::EmbeddingsV1 => ProviderRole::Embedding,
            Self::ResponsesV1 | Self::ChatCompletionsV1 => ProviderRole::Generation,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum ProviderAuthPatch {
    #[default]
    Preserve,
    Replace(SetupAuth),
    /// Copy the final proposed auth for this role, including a patch in this plan.
    CopyFrom(ProviderRole),
}

/// Omitted fields preserve the selected role's complete parsed service table.
/// A newly added role requires a model, URL and credential choice.
#[derive(Clone)]
pub struct ProviderRolePatch {
    pub role: ProviderRole,
    pub model: Option<String>,
    pub url: Option<String>,
    pub adapter: Option<ProviderAdapter>,
    pub dimensions: Option<String>,
    pub max_output_tokens: Option<u64>,
    /// None preserves; Some(None) removes; Some(Some(effort)) sets explicitly.
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
    pub auth: ProviderAuthPatch,
}

impl ProviderRolePatch {
    pub fn new(role: ProviderRole) -> Self {
        Self {
            role,
            model: None,
            url: None,
            adapter: None,
            dimensions: None,
            max_output_tokens: None,
            reasoning_effort: None,
            auth: ProviderAuthPatch::Preserve,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.url.is_none()
            && self.adapter.is_none()
            && self.dimensions.is_none()
            && self.max_output_tokens.is_none()
            && self.reasoning_effort.is_none()
            && matches!(self.auth, ProviderAuthPatch::Preserve)
    }
}

impl fmt::Debug for ProviderRolePatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRolePatch")
            .field("role", &self.role)
            .field("settings", &"private_settings")
            .field("auth", &self.auth)
            .finish()
    }
}

#[derive(Clone)]
pub struct GenerationSetup {
    pub model: String,
    pub url: String,
    pub adapter: ProviderAdapter,
    pub auth: SetupAuth,
    pub max_output_tokens: Option<u64>,
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
}

impl GenerationSetup {
    pub fn into_patch(self) -> ProviderRolePatch {
        ProviderRolePatch {
            role: ProviderRole::Generation,
            model: Some(self.model),
            url: Some(self.url),
            adapter: Some(self.adapter),
            dimensions: None,
            max_output_tokens: self.max_output_tokens,
            reasoning_effort: self.reasoning_effort,
            auth: ProviderAuthPatch::Replace(self.auth),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderGrantDecision {
    Preserve,
    /// Caller has reviewed every final role's destination, model and text scope.
    ReviewedAllRoles,
    /// Compatibility permission for embedding; generation must remain unchanged
    /// and already authorized for the selected wiki.
    EmbeddingOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderField {
    Model,
    Url,
    Adapter,
    Dimensions,
    MaxOutputTokens,
    ReasoningEffort,
    Auth,
}

impl ProviderField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Url => "url",
            Self::Adapter => "adapter",
            Self::Dimensions => "dimensions",
            Self::MaxOutputTokens => "max_output_tokens",
            Self::ReasoningEffort => "reasoning_effort",
            Self::Auth => "auth",
        }
    }
}

const FIELDS: [ProviderField; 7] = [
    ProviderField::Model,
    ProviderField::Url,
    ProviderField::Adapter,
    ProviderField::Dimensions,
    ProviderField::MaxOutputTokens,
    ProviderField::ReasoningEffort,
    ProviderField::Auth,
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderChangedField {
    pub role: ProviderRole,
    pub field: ProviderField,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderConsentReason {
    AddedRole,
    ChangedRole,
    MissingPermission,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderConsentRequirement {
    pub role: ProviderRole,
    pub reason: ProviderConsentReason,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderDestination {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderCommandSummary {
    pub output: super::provider_setup::SetupCommandOutput,
    pub ttl_seconds: u32,
    pub refresh_skew_seconds: u32,
    pub timeout_seconds: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderRoleSummary {
    pub role: ProviderRole,
    pub service: String,
    pub previous_service: Option<String>,
    pub model: String,
    pub revision: Option<String>,
    pub adapter: ProviderAdapter,
    pub dimensions: Option<String>,
    pub effective_output_token_limit: Option<u64>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub destination: ProviderDestination,
    pub auth_source: String,
    pub command_auth: Option<ProviderCommandSummary>,
    pub changed: bool,
    /// Full destination/TLS settings changed, even if the public host is equal.
    pub destination_changed: bool,
    /// Adapter, model, revision or configured dimensions changed.
    pub model_identity_changed: bool,
    /// Permission held by the role in the input profile at this exact root/ID.
    pub previous_permission: bool,
}

/// Comparison of planned settings only, not vector identity or consent authority.
/// The domain-separated fingerprint covers the full URL, CA path, loopback TLS
/// policy, adapter, model, revision and configured dimensions. It excludes auth,
/// role/profile/service IDs and CA contents; runtime must resolve full identity.
/// Endpoint paths and query values are never included in public fields or Debug.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderEmbeddingIdentity {
    pub fingerprint: Blake3Hash,
    pub service: String,
    pub model: String,
    pub revision: Option<String>,
    pub dimensions: Option<String>,
}

/// Private proposed bytes are available only through an explicit accessor. This
/// type deliberately has no Serialize implementation or revealing Debug fields.
pub struct ProviderEditPlan {
    bytes: Vec<u8>,
    profile: String,
    previous_profile: Option<String>,
    pub roles: Vec<ProviderRoleSummary>,
    pub changed_fields: Vec<ProviderChangedField>,
    pub previous_embedding_identity: Option<ProviderEmbeddingIdentity>,
    pub new_embedding_identity: Option<ProviderEmbeddingIdentity>,
    pub consent_requirements: Vec<ProviderConsentRequirement>,
    pub consent_satisfied: bool,
    /// Permission to publish these settings. Automatic embedding permission does
    /// not grant explicit provider calls or authorize generation changes.
    pub configuration_consent_satisfied: bool,
    /// Descriptor comparison only; runtime must revalidate root, marker and bytes.
    pub previous_automatic_embedding_consent: AutomaticEmbeddingConsent,
    pub automatic_embedding_consent: AutomaticEmbeddingConsent,
    pub projected_bytes: usize,
}

impl ProviderEditPlan {
    pub fn proposed_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn previous_profile(&self) -> Option<&str> {
        self.previous_profile.as_deref()
    }
}

impl fmt::Debug for ProviderEditPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderEditPlan")
            .field("bytes", &"private_configuration")
            .field("profile", &self.profile)
            .field("roles", &self.roles)
            .field("changed_fields", &self.changed_fields)
            .field("consent_requirements", &self.consent_requirements)
            .field("consent_satisfied", &self.consent_satisfied)
            .field(
                "configuration_consent_satisfied",
                &self.configuration_consent_satisfied,
            )
            .field(
                "automatic_embedding_consent",
                &self.automatic_embedding_consent,
            )
            .field("projected_bytes", &self.projected_bytes)
            .finish()
    }
}

fn entries<'a>(document: &'a Table, section: &str) -> Result<&'a Table> {
    document
        .get(section)
        .and_then(Value::as_table)
        .ok_or_else(|| invalid("provider configuration is missing a required table"))
}

fn service_table<'a>(document: &'a Table, service: &str) -> Result<&'a Table> {
    entries(document, "services")?
        .get(service)
        .and_then(Value::as_table)
        .ok_or_else(|| invalid("selected provider service is missing; check the selected profile"))
}

fn normalized(service: &Table) -> Table {
    let mut value = service.clone();
    value
        .entry("adapter")
        .or_insert_with(|| Value::String("responses-v1".into()));
    let adapter = value.get("adapter").and_then(Value::as_str);
    if adapter != Some("embeddings-v1") {
        value
            .entry("max_output_tokens")
            .or_insert(Value::Integer(4096));
    }
    if let Some(auth) = value.get_mut("auth").and_then(Value::as_table_mut) {
        auth.entry("header")
            .or_insert_with(|| Value::String("Authorization".into()));
        auth.entry("prefix")
            .or_insert_with(|| Value::String("Bearer ".into()));
        if auth.get("kind").and_then(Value::as_str) == Some("command") {
            for (name, seconds) in [
                ("ttl_seconds", 300),
                ("refresh_skew_seconds", 60),
                ("timeout_seconds", 10),
            ] {
                auth.entry(name).or_insert(Value::Integer(seconds));
            }
        }
    }
    value
}

fn digest(domain: &'static str, value: &Table) -> Result<String> {
    let encoded = toml::to_string(value)
        .map_err(|_| invalid("provider edit encoding failed; original is preserved"))?;
    let mut hash = blake3::Hasher::new();
    hash.update(domain.as_bytes());
    hash.update(&[0]);
    hash.update(encoded.as_bytes());
    Ok(hash.finalize().to_hex().to_string())
}

fn bound_to(document: &Table, wiki: &DefaultWiki, profile: &str) -> Result<bool> {
    let root = wiki
        .path
        .to_str()
        .ok_or_else(|| invalid("wiki path must be UTF-8"))?;
    Ok(entries(document, "vault_bindings")?.values().any(|value| {
        let Some(binding) = value.as_table() else {
            return false;
        };
        binding.get("root").and_then(Value::as_str) == Some(root)
            && binding.get("wiki_id").and_then(Value::as_str) == Some(wiki.wiki_id.as_str())
            && binding
                .get("allowed_profiles")
                .and_then(Value::as_array)
                .is_some_and(|profiles| profiles.iter().any(|id| id.as_str() == Some(profile)))
    }))
}

fn final_auth(
    role: ProviderRole,
    candidates: &[Option<Table>; 2],
    patches: &[Option<&ProviderRolePatch>; 2],
    visiting: &mut [bool; 2],
) -> Result<Table> {
    let index = role.index();
    if visiting[index] {
        return Err(invalid(
            "credential reuse forms a cycle; choose credentials for one role",
        ));
    }
    visiting[index] = true;
    let value = match patches[index].map(|patch| &patch.auth) {
        Some(ProviderAuthPatch::Replace(auth)) => {
            let mut replacement = auth_table(auth)?;
            // A credential source choice does not change a gateway's header
            // name or prefix. Keep manually configured transport formatting.
            if let Some(previous) = candidates[index]
                .as_ref()
                .and_then(|service| service.get("auth"))
                .and_then(Value::as_table)
            {
                for field in ["header", "prefix"] {
                    if let Some(value) = previous.get(field) {
                        replacement.insert(field.into(), value.clone());
                    }
                }
            }
            Ok(replacement)
        }
        Some(ProviderAuthPatch::CopyFrom(source)) => {
            if candidates[source.index()].is_none() {
                Err(invalid(
                    "credential reuse needs a configured source role; choose credentials or configure that role",
                ))
            } else {
                final_auth(*source, candidates, patches, visiting)
            }
        }
        Some(ProviderAuthPatch::Preserve) | None => candidates[index]
            .as_ref()
            .and_then(|service| service.get("auth"))
            .and_then(Value::as_table)
            .cloned()
            .ok_or_else(|| {
                invalid(
                    "new provider role needs credentials; choose an environment, file or command",
                )
            }),
    };
    visiting[index] = false;
    value
}

fn dimensions(service: &Table) -> Option<String> {
    match service.get("dimensions") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Integer(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn embedding_identity(service: &Table, id: &str) -> Result<ProviderEmbeddingIdentity> {
    let normal = normalized(service);
    let mut identity = Table::new();
    for field in [
        "url",
        "ca_file",
        "adapter",
        "model",
        "revision",
        "dimensions",
    ] {
        if let Some(value) = normal.get(field) {
            identity.insert(field.into(), value.clone());
        }
    }
    identity.insert(
        "allow_loopback_http".into(),
        service
            .get("allow_loopback_http")
            .cloned()
            .unwrap_or(Value::Boolean(false)),
    );
    Ok(ProviderEmbeddingIdentity {
        fingerprint: Blake3Hash::new(format!(
            "blake3:{}",
            digest("lwiki.provider.planned-embedding.v1", &identity)?
        ))?,
        service: id.into(),
        model: service["model"].as_str().unwrap_or_default().into(),
        revision: service
            .get("revision")
            .and_then(Value::as_str)
            .map(Into::into),
        dimensions: dimensions(service),
    })
}

fn role_summary(
    role: ProviderRole,
    service: &Table,
    id: &str,
    previous_service: Option<&str>,
    changed: bool,
    previous_permission: bool,
) -> Result<ProviderRoleSummary> {
    let endpoint = url::Url::parse(
        service
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .map_err(|_| invalid("provider destination is invalid; supply a valid HTTPS URL"))?;
    let normal = normalized(service);
    let auth = normal
        .get("auth")
        .and_then(Value::as_table)
        .ok_or_else(|| invalid("provider credentials are missing"))?;
    let command = auth.get("kind").and_then(Value::as_str) == Some("command");
    let command_auth = command.then(|| ProviderCommandSummary {
        output: if auth.get("output").and_then(Value::as_str) == Some("json") {
            super::provider_setup::SetupCommandOutput::Json
        } else {
            super::provider_setup::SetupCommandOutput::Text
        },
        ttl_seconds: auth["ttl_seconds"].as_integer().unwrap_or_default() as u32,
        refresh_skew_seconds: auth["refresh_skew_seconds"]
            .as_integer()
            .unwrap_or_default() as u32,
        timeout_seconds: auth["timeout_seconds"].as_integer().unwrap_or_default() as u32,
    });
    let auth_source = if command {
        "command"
    } else if auth.contains_key("key_env") {
        "environment"
    } else if auth.contains_key("key_file") {
        "private_file"
    } else {
        "private_literal"
    };
    let adapter = match normal.get("adapter").and_then(Value::as_str) {
        Some("embeddings-v1") => ProviderAdapter::EmbeddingsV1,
        Some("chat-completions-v1") => ProviderAdapter::ChatCompletionsV1,
        _ => ProviderAdapter::ResponsesV1,
    };
    Ok(ProviderRoleSummary {
        role,
        service: id.into(),
        previous_service: previous_service.map(Into::into),
        model: service["model"].as_str().unwrap_or_default().into(),
        revision: service
            .get("revision")
            .and_then(Value::as_str)
            .map(Into::into),
        adapter,
        dimensions: dimensions(service),
        effective_output_token_limit: (role == ProviderRole::Generation)
            .then(|| normal["max_output_tokens"].as_integer().unwrap_or_default() as u64),
        reasoning_effort: service
            .get("reasoning_effort")
            .cloned()
            .map(|value| {
                value
                    .try_into()
                    .map_err(|_| invalid("invalid reasoning effort"))
            })
            .transpose()?,
        destination: ProviderDestination {
            scheme: endpoint.scheme().into(),
            host: endpoint.host_str().unwrap_or_default().into(),
            port: endpoint.port_or_known_default(),
        },
        auth_source: auth_source.into(),
        command_auth,
        changed,
        destination_changed: false,
        model_identity_changed: false,
        previous_permission,
    })
}

fn identity_fields_changed(original: Option<&Table>, candidate: &Table, fields: &[&str]) -> bool {
    original.is_none()
        || fields
            .iter()
            .any(|field| original.and_then(|service| service.get(*field)) != candidate.get(*field))
}

/// Plan a scoped provider edit from the already inspected private bytes.
/// `wiki.path` is the caller's canonical root. The planner compares bindings at
/// that exact root and ID and does not obtain authority from caller assertions.
/// It neither publishes bytes nor changes preferences. A caller must require
/// `configuration_consent_satisfied` before publishing or selecting a profile when this plan has
/// consent requirements. This matters even when a historical target profile has
/// an old retained binding: append-only planning does not revoke that binding.
pub fn plan_provider_edit(
    existing: Option<&[u8]>,
    wiki: &DefaultWiki,
    effective_profile: Option<&str>,
    patches: &[ProviderRolePatch],
    grant: ProviderGrantDecision,
) -> Result<ProviderEditPlan> {
    plan_provider_edit_with_automatic(
        existing,
        wiki,
        effective_profile,
        patches,
        grant,
        ProviderAutomaticEmbeddingDecision::Preserve,
    )
}

/// Plan automatic text permission independently of explicit role permission.
/// Preserve carries matching original consent across identity-preserving role
/// copies, including auth rotation. Reviewed permits the fixed embedding scope
/// only and cannot satisfy consent_requirements for generation or explicit work.
pub fn plan_provider_edit_with_automatic(
    existing: Option<&[u8]>,
    wiki: &DefaultWiki,
    effective_profile: Option<&str>,
    patches: &[ProviderRolePatch],
    grant: ProviderGrantDecision,
    automatic: ProviderAutomaticEmbeddingDecision,
) -> Result<ProviderEditPlan> {
    if !wiki.path.is_absolute() || wiki.path.to_str().is_none() {
        return Err(invalid("select a wiki with an absolute UTF-8 folder path"));
    }
    let mut document = parse_document(existing)?;
    let mut selected_patches: [Option<&ProviderRolePatch>; 2] = [None, None];
    for patch in patches {
        let slot = &mut selected_patches[patch.role.index()];
        if slot.is_some() {
            return Err(invalid("supply at most one patch for each provider role"));
        }
        if patch
            .adapter
            .is_some_and(|adapter| adapter.role() != patch.role)
            || (patch.role == ProviderRole::Embedding
                && (patch.max_output_tokens.is_some() || patch.reasoning_effort.is_some()))
            || (patch.role == ProviderRole::Generation && patch.dimensions.is_some())
        {
            return Err(invalid(
                "provider setting belongs to the other role; check the selected role",
            ));
        }
        *slot = Some(patch);
    }
    let original_profile = if let Some(profile) = effective_profile {
        Some(
            entries(&document, "profiles")?
                .get(profile)
                .and_then(Value::as_table)
                .ok_or_else(|| {
                    invalid("selected provider profile is missing; choose an existing profile")
                })?
                .clone(),
        )
    } else {
        None
    };
    let prior_bound = match effective_profile {
        Some(profile) => bound_to(&document, wiki, profile)?,
        None => false,
    };
    let mut previous_ids: [Option<String>; 2] = [None, None];
    let mut originals: [Option<Table>; 2] = [None, None];
    let mut candidates: [Option<Table>; 2] = [None, None];
    for role in ROLES {
        let index = role.index();
        if let Some(id) = original_profile
            .as_ref()
            .and_then(|profile| profile.get(role.as_str()))
            .and_then(Value::as_str)
        {
            previous_ids[index] = Some(id.into());
            originals[index] = Some(service_table(&document, id)?.clone());
        }
        candidates[index] = originals[index].clone();
        if let Some(patch) = selected_patches[index] {
            let service = candidates[index].get_or_insert_with(|| {
                Table::from_iter([(
                    "adapter".into(),
                    Value::String(
                        match role {
                            ProviderRole::Embedding => "embeddings-v1",
                            ProviderRole::Generation => "responses-v1",
                        }
                        .into(),
                    ),
                )])
            });
            for (name, value) in [("model", &patch.model), ("url", &patch.url)] {
                if let Some(value) = value {
                    service.insert(name.into(), Value::String(value.clone()));
                }
            }
            if let Some(adapter) = patch.adapter {
                service.insert("adapter".into(), Value::String(adapter.as_str().into()));
            }
            if let Some(dimensions) = &patch.dimensions {
                service.insert("dimensions".into(), dimensions_value(dimensions)?);
            }
            if let Some(effort) = patch.reasoning_effort {
                if let Some(effort) = effort {
                    service.insert(
                        "reasoning_effort".into(),
                        Value::String(effort.as_str().into()),
                    );
                } else {
                    service.remove("reasoning_effort");
                }
            }
            if let Some(limit) = patch.max_output_tokens {
                let limit = i64::try_from(limit).map_err(|_| {
                    invalid("output token limit is too large; use a positive supported limit")
                })?;
                service.insert("max_output_tokens".into(), Value::Integer(limit));
            }
        }
    }
    // Resolve reuse only after both roles' ordinary fields have been collected.
    let mut auths: [Option<Table>; 2] = [None, None];
    for role in ROLES {
        let index = role.index();
        if candidates[index].is_some() {
            auths[index] = Some(final_auth(
                role,
                &candidates,
                &selected_patches,
                &mut [false; 2],
            )?);
        }
    }
    for role in ROLES {
        let index = role.index();
        if let Some(service) = &mut candidates[index] {
            for field in ["model", "url"] {
                if !service.contains_key(field) {
                    return Err(invalid(
                        "new provider role needs a model and HTTPS URL; supply both settings",
                    ));
                }
            }
            service.insert(
                "auth".into(),
                Value::Table(auths[index].take().expect("resolved auth")),
            );
        }
    }
    if candidates.iter().all(Option::is_none) {
        return Err(invalid(
            "choose an embedding or generation service before granting provider permission",
        ));
    }
    let mut changed = [false; 2];
    let mut changed_fields = Vec::new();
    let mut consent_requirements = Vec::new();
    for role in ROLES {
        let index = role.index();
        let Some(candidate) = &candidates[index] else {
            continue;
        };
        let normal = normalized(candidate);
        let original = originals[index].as_ref().map(normalized);
        changed[index] = original.as_ref() != Some(&normal);
        for field in FIELDS {
            if original
                .as_ref()
                .and_then(|service| service.get(field.as_str()))
                != normal.get(field.as_str())
            {
                changed_fields.push(ProviderChangedField { role, field });
            }
        }
        let reason = if originals[index].is_none() {
            Some(ProviderConsentReason::AddedRole)
        } else if changed[index] {
            Some(ProviderConsentReason::ChangedRole)
        } else if !prior_bound {
            Some(ProviderConsentReason::MissingPermission)
        } else {
            None
        };
        if let Some(reason) = reason {
            consent_requirements.push(ProviderConsentRequirement { role, reason });
        }
    }
    if grant == ProviderGrantDecision::EmbeddingOnly {
        if candidates[0].is_none() {
            return Err(invalid(
                "embedding permission requires an embedding service; use --grant-provider for generation",
            ));
        }
        if candidates[1].is_some() && (changed[1] || !prior_bound) {
            return Err(invalid(
                "generation needs reviewed permission; use --grant-provider to approve every configured role",
            ));
        }
    }
    let consent_satisfied =
        consent_requirements.is_empty() || grant != ProviderGrantDecision::Preserve;
    let mut additions = Table::new();
    let mut new_profile = Table::new();
    let mut final_ids: [Option<String>; 2] = [None, None];
    for role in ROLES {
        let index = role.index();
        let Some(candidate) = &candidates[index] else {
            continue;
        };
        let normal = normalized(candidate);
        let id = if !changed[index] {
            previous_ids[index]
                .clone()
                .expect("unchanged existing service")
        } else if let Some((id, _)) = entries(&document, "services")?.iter().find(|(_, value)| {
            value
                .as_table()
                .is_some_and(|service| normalized(service) == normal)
        }) {
            id.clone()
        } else {
            let hash = digest("lwiki.provider.edit-service.v1", &normal)?;
            let id = format!("lwiki-{}-{}", role.as_str(), &hash[..24]);
            insert_exact(
                &mut document,
                &mut additions,
                "services",
                &id,
                Value::Table(candidate.clone()),
            )?;
            id
        };
        new_profile.insert(role.as_str().into(), Value::String(id.clone()));
        final_ids[index] = Some(id);
    }
    let profile = if original_profile.as_ref() == Some(&new_profile) {
        effective_profile
            .expect("original selected profile")
            .to_string()
    } else if let Some((id, _)) = entries(&document, "profiles")?
        .iter()
        .find(|(_, value)| value.as_table() == Some(&new_profile))
    {
        id.clone()
    } else {
        let hash = digest("lwiki.provider.edit-profile.v1", &new_profile)?;
        let id = format!("lwiki-{}", &hash[..24]);
        insert_exact(
            &mut document,
            &mut additions,
            "profiles",
            &id,
            Value::Table(new_profile),
        )?;
        id
    };
    // No original profile/binding is edited. Preserve cannot silently expand
    // either another wiki's authority or this wiki's authority to a changed role.
    if consent_satisfied && !bound_to(&document, wiki, &profile)? {
        let binding = Table::from_iter([
            (
                "root".into(),
                Value::String(wiki.path.to_str().expect("checked root").into()),
            ),
            ("wiki_id".into(), Value::String(wiki.wiki_id.to_string())),
            (
                "allowed_profiles".into(),
                Value::Array(vec![Value::String(profile.clone())]),
            ),
        ]);
        let hash = digest("lwiki.provider.edit-binding.v1", &binding)?;
        let name = format!("lwiki-vault-{}", &hash[..24]);
        insert_exact(
            &mut document,
            &mut additions,
            "vault_bindings",
            &name,
            Value::Table(binding),
        )?;
    }
    if entries(&document, "services")?.len() > 128 || entries(&document, "profiles")?.len() > 128 {
        return Err(invalid(
            "provider edits reached the 128 service/profile limit; original is preserved; use a reviewed smaller provider configuration",
        ));
    }
    let (previous_automatic_embedding_consent, automatic_embedding_consent) =
        append_automatic_embedding_grant(
            existing,
            &mut document,
            &mut additions,
            wiki,
            (effective_profile, &profile),
            automatic,
        )?;
    let bytes = encode_document(existing, &document, &additions)?;
    let configuration_consent_satisfied = consent_satisfied
        || (automatic_embedding_consent == AutomaticEmbeddingConsent::Matching
            && consent_requirements.iter().all(|requirement| {
                requirement.role == ProviderRole::Embedding
                    || requirement.reason == ProviderConsentReason::MissingPermission
            }));
    let mut roles = Vec::new();
    for role in ROLES {
        let index = role.index();
        if let Some(id) = &final_ids[index] {
            let mut summary = role_summary(
                role,
                service_table(&document, id)?,
                id,
                previous_ids[index].as_deref(),
                changed[index],
                originals[index].is_some() && prior_bound,
            )?;
            let final_settings = normalized(service_table(&document, id)?);
            let original_settings = originals[index].as_ref().map(normalized);
            summary.destination_changed = identity_fields_changed(
                original_settings.as_ref(),
                &final_settings,
                &["url", "ca_file", "allow_loopback_http"],
            );
            summary.model_identity_changed = identity_fields_changed(
                original_settings.as_ref(),
                &final_settings,
                &["adapter", "model", "revision", "dimensions"],
            );
            roles.push(summary);
        }
    }
    let previous_embedding_identity = match (&originals[0], &previous_ids[0]) {
        (Some(service), Some(id)) => Some(embedding_identity(service, id)?),
        _ => None,
    };
    let new_embedding_identity = match &final_ids[0] {
        Some(id) => Some(embedding_identity(service_table(&document, id)?, id)?),
        None => None,
    };
    Ok(ProviderEditPlan {
        projected_bytes: bytes.len(),
        bytes,
        profile,
        previous_profile: effective_profile.map(Into::into),
        roles,
        changed_fields,
        previous_embedding_identity,
        new_embedding_identity,
        consent_requirements,
        consent_satisfied,
        configuration_consent_satisfied,
        previous_automatic_embedding_consent,
        automatic_embedding_consent,
    })
}

#[cfg(test)]
#[path = "provider_edit_tests.rs"]
mod tests;

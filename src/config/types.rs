//! Preferences cannot grant provider endpoint or credential authority.
use crate::domain::RecordId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
pub const MAX_LOCAL_CONFIG_BYTES: usize = 65_536;
pub const LOCAL_CONFIG_SCHEMA_VERSION: &str = "3";
#[derive(Debug, Clone, Default)]
pub struct PreferenceOptions {
    pub offline: bool,
    pub read_max_bytes: Option<usize>,
    pub lock_timeout_ms: Option<u64>,
    pub profile: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Preferences {
    pub offline: bool,
    pub read_max_bytes: usize,
    pub lock_timeout_ms: u64,
    pub profile: Option<String>,
    pub profile_permitted: bool,
    pub warnings: Vec<String>,
    pub origins: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalPreferences {
    pub offline: Option<bool>,
    pub read_max_bytes: Option<usize>,
    pub lock_timeout_ms: Option<u64>,
    pub profile: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalConfigDocument {
    pub schema_version: String,
    #[serde(default)]
    pub defaults: LocalPreferences,
    #[serde(default)]
    pub vaults: BTreeMap<RecordId, LocalPreferences>,
    #[serde(default)]
    pub allowed_profiles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_wiki: Option<DefaultWiki>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers_config: Option<PathBuf>,
    #[serde(default)]
    pub embeddings: EmbeddingDefaults,
    #[serde(default)]
    pub remote: RemoteDefaults,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmbeddingDefaults {
    pub document_prefix: Option<String>,
    pub query_prefix: Option<String>,
    pub max_input_bytes: Option<usize>,
    pub quality_target_bytes: Option<usize>,
}

/// New-job defaults only. These values never amend a retained job's limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteDefaults {
    pub max_requests: Option<u64>,
    pub concurrency: Option<u32>,
    pub attempts_per_task: Option<u32>,
    pub deadline_ms: Option<u64>,
    pub max_request_bytes: Option<u64>,
    pub max_response_bytes: Option<u64>,
    pub max_input_units: Option<u64>,
    pub max_output_units: Option<u64>,
    pub max_cost: Option<String>,
    pub currency: Option<String>,
    pub requests_per_minute: Option<u32>,
    pub tokens_per_minute: Option<u64>,
}

/// A single fallback location, pinned to the identity observed when selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultWiki {
    pub path: PathBuf,
    pub wiki_id: RecordId,
}

impl Default for LocalConfigDocument {
    fn default() -> Self {
        Self {
            schema_version: LOCAL_CONFIG_SCHEMA_VERSION.into(),
            defaults: LocalPreferences::default(),
            vaults: BTreeMap::new(),
            allowed_profiles: Vec::new(),
            default_wiki: None,
            providers_config: None,
            embeddings: EmbeddingDefaults::default(),
            remote: RemoteDefaults::default(),
        }
    }
}

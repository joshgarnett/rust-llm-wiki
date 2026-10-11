//! Preferences cannot grant provider endpoint or credential authority.
use crate::domain::RecordId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
pub const MAX_LOCAL_CONFIG_BYTES: usize = 65_536;
pub const LOCAL_CONFIG_SCHEMA_VERSION: &str = "2";
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
        }
    }
}

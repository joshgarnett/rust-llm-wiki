//! One bounded user-preferences snapshot; it never grants provider authority.
use super::types::*;
use crate::{domain::*, records::parse_note, vault::VaultFs};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};
const MAX_CONFIG_BYTES: usize = super::types::MAX_LOCAL_CONFIG_BYTES;
fn config_error(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ConfigInvalid, message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserConfigFormat {
    Json,
    Toml,
}
fn format_for_path(path: &Path) -> UserConfigFormat {
    // Explicit historical preferences paths without an extension still mean JSON.
    if path.extension() == Some(OsStr::new("toml")) {
        UserConfigFormat::Toml
    } else {
        UserConfigFormat::Json
    }
}
fn parse_document(path: &Path, bytes: &[u8]) -> Result<LocalConfigDocument> {
    let format = format_for_path(path);
    // Parse failures never repeat user input, which could contain an accidental key.
    let (value, fields, scoped_provider_present): (LocalConfigDocument, Vec<String>, bool) =
        match format {
            UserConfigFormat::Json => {
                let raw: serde_json::Value =
                    crate::changes::prepare::strict_json(bytes).map_err(|_| {
                        super::local_io::error(
                            path,
                            "parse configuration",
                            "invalid JSON document or duplicate key",
                        )
                    })?;
                let fields = raw
                    .as_object()
                    .map(|map| map.keys().cloned().collect())
                    .unwrap_or_default();
                if raw
                    .get("defaults")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|defaults| defaults.contains_key("providers_config"))
                {
                    return Err(super::local_io::error(
                        path,
                        "validate configuration",
                        "providers_config is allowed only at the top level or in vaults entries",
                    ));
                }
                let scoped_provider_present = raw
                    .get("vaults")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|vaults| {
                        vaults.values().any(|preferences| {
                            preferences.as_object().is_some_and(|preferences| {
                                preferences.contains_key("providers_config")
                            })
                        })
                    });
                let value = serde_json::from_value(raw).map_err(|_| {
                    super::local_io::error(
                        path,
                        "parse configuration",
                        "invalid JSON configuration fields",
                    )
                })?;
                (value, fields, scoped_provider_present)
            }
            UserConfigFormat::Toml => {
                let text = std::str::from_utf8(bytes).map_err(|_| {
                    super::local_io::error(path, "parse configuration", "TOML must be UTF-8")
                })?;
                let raw: toml::Value = toml::from_str(text).map_err(|_| {
                    super::local_io::error(
                        path,
                        "parse configuration",
                        "invalid TOML document or duplicate key",
                    )
                })?;
                let fields = raw
                    .as_table()
                    .map(|map| map.keys().cloned().collect())
                    .unwrap_or_default();
                if raw
                    .get("defaults")
                    .and_then(toml::Value::as_table)
                    .is_some_and(|defaults| defaults.contains_key("providers_config"))
                {
                    return Err(super::local_io::error(
                        path,
                        "validate configuration",
                        "providers_config is allowed only at the top level or in vaults entries",
                    ));
                }
                let scoped_provider_present = raw
                    .get("vaults")
                    .and_then(toml::Value::as_table)
                    .is_some_and(|vaults| {
                        vaults.values().any(|preferences| {
                            preferences.as_table().is_some_and(|preferences| {
                                preferences.contains_key("providers_config")
                            })
                        })
                    });
                let value = raw.try_into().map_err(|_| {
                    super::local_io::error(
                        path,
                        "parse configuration",
                        "invalid TOML configuration fields",
                    )
                })?;
                (value, fields, scoped_provider_present)
            }
        };
    if !["1", "2", LOCAL_CONFIG_SCHEMA_VERSION].contains(&value.schema_version.as_str())
        || (format == UserConfigFormat::Toml && value.schema_version != LOCAL_CONFIG_SCHEMA_VERSION)
    {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "unsupported schema version for this format; original is preserved",
        ));
    }
    if value.schema_version == "1" && fields.iter().any(|field| field == "default_wiki") {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "schema version 1 cannot contain default_wiki; migration requires version 2",
        ));
    }
    if value.schema_version != LOCAL_CONFIG_SCHEMA_VERSION
        && (scoped_provider_present
            || fields.iter().any(|field| {
                ["providers_config", "embeddings", "remote"].contains(&field.as_str())
            }))
    {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "provider, embedding and remote defaults require schema version 3",
        ));
    }
    validate_document(path, &value)?;
    Ok(value)
}
fn validate_document(path: &Path, value: &LocalConfigDocument) -> Result<()> {
    if value.defaults.providers_config.is_some() {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "providers_config is allowed only at the top level or in vaults entries",
        ));
    }
    if value.schema_version != LOCAL_CONFIG_SCHEMA_VERSION
        && value
            .vaults
            .values()
            .any(|preferences| preferences.providers_config.is_some())
    {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "scoped provider selections require schema version 3",
        ));
    }
    if let Some(default) = &value.default_wiki {
        validate_default(path, default)?;
    }
    if let Some(providers) = &value.providers_config {
        validate_absolute_path(path, providers, "providers_config")?;
    }
    for preferences in std::iter::once(&value.defaults).chain(value.vaults.values()) {
        if let Some(providers) = &preferences.providers_config {
            validate_absolute_path(path, providers, "vaults.providers_config")?;
        }
        if preferences
            .read_max_bytes
            .is_some_and(|v| v == 0 || v > crate::app::offline::MAX_INPUT_BYTES)
            || preferences.lock_timeout_ms.is_some_and(|v| v > 30_000)
        {
            return Err(super::local_io::error(
                path,
                "validate configuration",
                "read byte ceiling must be 1..=16 MiB and writer timeout at most 30000 ms",
            ));
        }
        if preferences
            .profile
            .as_ref()
            .is_some_and(|v| !valid_profile(v))
        {
            return Err(super::local_io::error(
                path,
                "validate configuration",
                "profile name must contain 1..=128 bytes without control characters",
            ));
        }
    }
    if value.allowed_profiles.iter().any(|v| !valid_profile(v)) {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "allowed profile names must contain 1..=128 bytes without control characters",
        ));
    }
    let embeddings = &value.embeddings;
    let input_limit = embeddings.max_input_bytes.unwrap_or(12_000);
    if input_limit == 0
        || input_limit > 128 * 1024
        || embeddings
            .quality_target_bytes
            .is_some_and(|n| n == 0 || n > input_limit)
        || embeddings
            .document_prefix
            .as_ref()
            .is_some_and(|s| s.len() > 128 * 1024)
        || embeddings
            .query_prefix
            .as_ref()
            .is_some_and(|s| s.len() > 128 * 1024)
    {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "embedding byte limits or prefixes are invalid",
        ));
    }
    let remote = &value.remote;
    if remote.max_requests == Some(0)
        || remote.concurrency == Some(0)
        || remote.attempts_per_task == Some(0)
        || remote.deadline_ms == Some(0)
        || remote.deadline_ms.is_some_and(|v| v > 86_400_000)
        || remote.max_request_bytes == Some(0)
        || remote.max_response_bytes == Some(0)
        || remote.requests_per_minute == Some(0)
        || remote.tokens_per_minute == Some(0)
    {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "remote request, concurrency, attempt, byte and rate ceilings must be positive; deadline must be 1..=86400000 ms",
        ));
    }
    if let Some(currency) = &remote.currency {
        crate::jobs::Currency::new(currency.clone()).map_err(|_| {
            super::local_io::error(
                path,
                "validate configuration",
                "currency must be three uppercase ASCII letters",
            )
        })?;
    }
    if let Some(cost) = &remote.max_cost {
        use crate::jobs::MoneyParsing;
        let currency =
            crate::jobs::Currency::new(remote.currency.clone().unwrap_or_else(|| "USD".into()))
                .map_err(|_| {
                    super::local_io::error(
                        path,
                        "validate configuration",
                        "invalid remote cost currency",
                    )
                })?;
        crate::jobs::Money::parse_decimal(currency, cost)
            .map_err(|_| super::local_io::error(path, "validate configuration", "max_cost must be a bounded nonnegative fixed decimal with at most nine fractional digits"))?;
    }
    Ok(())
}
fn valid_profile(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn validate_absolute_path(config_path: &Path, path: &Path, field: &str) -> Result<()> {
    if !path.is_absolute()
        || path.to_str().is_none()
        || path.as_os_str().len() > 4096
        || path.components().count() > 64
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(super::local_io::error(
            config_path,
            "validate configuration",
            format!(
                "{field} must be an absolute UTF-8 path bounded to 4096 bytes/64 components without . or .."
            ),
        ));
    }
    Ok(())
}
fn validate_default(config_path: &Path, default: &DefaultWiki) -> Result<()> {
    validate_absolute_path(config_path, &default.path, "default_wiki.path")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserConfigOrigin {
    Ambient,
    Explicit,
    Disabled,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserConfigSummary {
    pub path: Option<PathBuf>,
    pub requested_path: Option<PathBuf>,
    pub origin: UserConfigOrigin,
    pub present: bool,
    pub schema_version: Option<String>,
    pub format: Option<UserConfigFormat>,
    pub migration_required: bool,
    pub shadowed_legacy_path: Option<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LoadedUserConfig {
    summary: UserConfigSummary,
    document: LocalConfigDocument,
    bytes: Option<Vec<u8>>,
    parent_snapshot: super::local_io::ParentSnapshot,
}
impl LoadedUserConfig {
    pub fn summary(&self) -> &UserConfigSummary {
        &self.summary
    }
    pub fn document(&self) -> &LocalConfigDocument {
        &self.document
    }
    pub fn default_wiki(&self) -> Option<&DefaultWiki> {
        self.document.default_wiki.as_ref()
    }
    pub fn path(&self) -> Option<&Path> {
        self.summary.path.as_deref()
    }
    pub fn toml_migration_target(&self) -> Option<PathBuf> {
        self.path()
            .filter(|path| format_for_path(path) == UserConfigFormat::Json)
            .map(|path| path.with_extension("toml"))
    }
}

fn ambient_path(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf> {
    let base = if let Some(xdg) = xdg {
        let path = Path::new(xdg);
        if !path.is_absolute() {
            let mut error = config_error(format!(
                "XDG_CONFIG_HOME must be absolute, found {}",
                path.display()
            ));
            error.hint = Some("Set XDG_CONFIG_HOME to an absolute path, use --preferences FILE, or use --no-user-config".into());
            return Err(error);
        }
        path.to_path_buf()
    } else {
        let home = home.filter(|home| !home.is_empty()).ok_or_else(|| {
            config_error("HOME is unavailable; use --preferences FILE or --no-user-config")
        })?;
        let home = Path::new(home);
        if !home.is_absolute() {
            return Err(config_error(
                "HOME must be absolute; use --preferences FILE or --no-user-config",
            ));
        }
        home.join(".config")
    };
    Ok(base.join("lwiki/config.toml"))
}

pub fn load_user_config(explicit: Option<&Path>, no_user_config: bool) -> Result<LoadedUserConfig> {
    load_selected(explicit, no_user_config, false)
}
/// Setup may create its explicitly named destination; normal reads never do this.
pub fn load_user_config_for_setup(
    explicit: Option<&Path>,
    no_user_config: bool,
) -> Result<LoadedUserConfig> {
    load_selected(explicit, no_user_config, true)
}
fn load_selected(
    explicit: Option<&Path>,
    no_user_config: bool,
    allow_missing_explicit: bool,
) -> Result<LoadedUserConfig> {
    if let Some(explicit) = explicit {
        let path = if explicit.is_absolute() {
            explicit.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| config_error(format!("resolve explicit preferences path: {e}")))?
                .join(explicit)
        };
        return load_path_inner(
            Some(&path),
            UserConfigOrigin::Explicit,
            allow_missing_explicit,
        );
    }
    if no_user_config {
        return load_path(None, UserConfigOrigin::Disabled);
    }
    let path = ambient_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;
    load_ambient(&path)
}
fn load_ambient(toml_path: &Path) -> Result<LoadedUserConfig> {
    let mut loaded = load_path(Some(toml_path), UserConfigOrigin::Ambient)?;
    let legacy = toml_path.with_file_name("config.json");
    if !loaded.summary.present {
        let json = load_path(Some(&legacy), UserConfigOrigin::Ambient)?;
        return if json.summary.present {
            Ok(json)
        } else {
            Ok(loaded)
        };
    }
    match std::fs::symlink_metadata(&legacy) {
        Ok(_) => {
            loaded.summary.shadowed_legacy_path = Some(legacy.clone());
            loaded.summary.warnings.push(format!(
                "Using {}. Your older {} is unchanged and is no longer used.",
                toml_path.display(),
                legacy.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(super::local_io::error(
                &legacy,
                "inspect shadowed configuration",
                error,
            ));
        }
    }
    Ok(loaded)
}
fn load_path(requested: Option<&Path>, origin: UserConfigOrigin) -> Result<LoadedUserConfig> {
    load_path_inner(requested, origin, false)
}
fn load_path_inner(
    requested: Option<&Path>,
    origin: UserConfigOrigin,
    allow_missing_explicit: bool,
) -> Result<LoadedUserConfig> {
    let path = requested.map(super::local_io::effective_path).transpose()?;
    let parent_snapshot = path
        .as_deref()
        .map(super::local_io::ParentSnapshot::capture)
        .transpose()?
        .unwrap_or_default();
    let bytes = path.as_deref().map(read_path).transpose()?.flatten();
    if origin == UserConfigOrigin::Explicit && bytes.is_none() && !allow_missing_explicit {
        return Err(super::local_io::error(
            path.as_deref().expect("explicit path"),
            "read explicit preferences",
            "file does not exist",
        ));
    }
    let document = match (&path, &bytes) {
        (Some(path), Some(bytes)) => parse_document(path, bytes)?,
        _ => LocalConfigDocument::default(),
    };
    if let Some(path) = &path {
        parent_snapshot.verify(path)?;
    }
    Ok(LoadedUserConfig {
        summary: UserConfigSummary {
            format: path.as_deref().map(format_for_path),
            requested_path: requested.map(Path::to_path_buf),
            path,
            origin,
            present: bytes.is_some(),
            schema_version: bytes.as_ref().map(|_| document.schema_version.clone()),
            migration_required: bytes.is_some()
                && document.schema_version != LOCAL_CONFIG_SCHEMA_VERSION,
            shadowed_legacy_path: None,
            warnings: vec![],
        },
        document,
        bytes,
        parent_snapshot,
    })
}
fn read_path(path: &Path) -> Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    {
        super::local_io::Directory::open(path, false)?
            .map(|directory| {
                directory.read(
                    path.file_name().expect("validated file name"),
                    MAX_CONFIG_BYTES,
                )
            })
            .transpose()
            .map(Option::flatten)
    }
    #[cfg(not(unix))]
    {
        Err(super::local_io::unsupported(path))
    }
}

pub fn require_write_target(config: &LoadedUserConfig) -> Result<&Path> {
    config.path().ok_or_else(|| config_error("default changes require user configuration; omit --no-user-config or provide --preferences FILE"))
}
/// Compare private selections with the same canonical-parent rules used by guarded snapshots.
pub fn normalize_private_path(path: &Path) -> Result<PathBuf> {
    super::local_io::effective_path(path)
}
#[derive(Debug, Clone, Serialize)]
pub struct DocumentUpdateReport {
    pub configuration_path: PathBuf,
    pub previous_configuration_path: PathBuf,
    pub changed: bool,
    pub applied: bool,
    pub durable: bool,
    pub migration_from: Option<String>,
    pub preserved_original_path: Option<PathBuf>,
    pub failure: Option<WikiError>,
}
fn backup_path(path: &Path, version: &str) -> PathBuf {
    let mut name = path.file_name().expect("validated file name").to_owned();
    name.push(format!(".v{version}.bak"));
    path.with_file_name(name)
}
fn serialize_document(path: &Path, document: &LocalConfigDocument) -> Result<Vec<u8>> {
    validate_document(path, document)?;
    let mut bytes = match format_for_path(path) {
        UserConfigFormat::Toml => toml::to_string_pretty(document)
            .map(String::into_bytes)
            .map_err(|_| {
                super::local_io::error(
                    path,
                    "serialize configuration",
                    "document cannot be represented as TOML",
                )
            })?,
        UserConfigFormat::Json => {
            let mut value = serde_json::to_value(document)
                .map_err(|_| config_error("cannot serialize configuration"))?;
            if document.schema_version != LOCAL_CONFIG_SCHEMA_VERSION {
                for field in ["providers_config", "embeddings", "remote"] {
                    value
                        .as_object_mut()
                        .expect("document object")
                        .remove(field);
                }
            }
            serde_json::to_vec_pretty(&value)
                .map_err(|_| config_error("cannot serialize configuration"))?
        }
    };
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(super::local_io::error(
            path,
            "write configuration",
            "proposed configuration exceeds 64 KiB",
        ));
    }
    parse_document(path, &bytes)?;
    Ok(bytes)
}
/// Publish a complete validated document using this invocation's loaded snapshot.
/// A distinct target is explicit JSON-to-TOML migration, never ambient read repair.
/// Post-publication failures are returned in the report so completed writes remain visible.
pub fn update_document(
    config: &LoadedUserConfig,
    mut proposed: LocalConfigDocument,
    target: Option<&Path>,
    dry_run: bool,
    timeout_ms: u64,
) -> Result<DocumentUpdateReport> {
    let source = require_write_target(config)?;
    if timeout_ms > 30_000 {
        return Err(config_error(
            "configuration writer timeout exceeds 30000 ms",
        ));
    }
    let destination = target
        .map(super::local_io::effective_path)
        .transpose()?
        .unwrap_or_else(|| source.to_path_buf());
    let migrating_path = destination != source;
    if migrating_path
        && (format_for_path(source) != UserConfigFormat::Json
            || format_for_path(&destination) != UserConfigFormat::Toml)
    {
        return Err(config_error(
            "a separate configuration destination is supported only for explicit JSON-to-TOML migration",
        ));
    }
    if migrating_path || format_for_path(&destination) == UserConfigFormat::Toml {
        proposed.schema_version = LOCAL_CONFIG_SCHEMA_VERSION.into();
    }
    if proposed.schema_version != "2" && proposed.schema_version != LOCAL_CONFIG_SCHEMA_VERSION {
        return Err(config_error(
            "configuration updates require schema version 2 or 3",
        ));
    }
    // Legacy JSON setters cannot silently discard newly configured schema-3 fields.
    if proposed.schema_version == "2"
        && (proposed.providers_config.is_some()
            || proposed
                .vaults
                .values()
                .any(|preferences| preferences.providers_config.is_some())
            || serde_json::to_value(&proposed.embeddings).ok()
                != serde_json::to_value(EmbeddingDefaults::default()).ok()
            || serde_json::to_value(&proposed.remote).ok()
                != serde_json::to_value(RemoteDefaults::default()).ok())
    {
        return Err(config_error("extended defaults require schema version 3"));
    }
    let bytes = serialize_document(&destination, &proposed)?;
    if let Some(default) = &proposed.default_wiki
        && !dry_run
        && crate::vault::discovery::default_from_path(&default.path)? != *default
    {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "proposed default wiki identity or canonical path changed",
        ));
    }
    let migration = config.bytes.is_some()
        && (migrating_path || proposed.schema_version != config.document.schema_version);
    let backup = migration.then(|| backup_path(source, &config.document.schema_version));
    let same_document =
        serde_json::to_value(&proposed).ok() == serde_json::to_value(&config.document).ok();
    let mut report = DocumentUpdateReport {
        configuration_path: destination.clone(),
        previous_configuration_path: source.to_path_buf(),
        changed: config.bytes.is_none() || migrating_path || migration || !same_document,
        applied: false,
        durable: false,
        migration_from: migration.then(|| config.document.schema_version.clone()),
        preserved_original_path: backup.clone(),
        failure: None,
    };
    config.parent_snapshot.verify(source)?;
    if read_path(source)? != config.bytes {
        return Err(conflict(source));
    }
    let destination_snapshot = super::local_io::ParentSnapshot::capture(&destination)?;
    let previous_destination = if migrating_path {
        read_path(&destination)?
    } else {
        config.bytes.clone()
    };
    if migrating_path
        && previous_destination
            .as_ref()
            .is_some_and(|existing| existing != &bytes)
    {
        return Err(conflict(&destination));
    }
    if let Some(backup) = &backup
        && read_path(backup)?
            .as_ref()
            .is_some_and(|existing| Some(existing.as_slice()) != config.bytes.as_deref())
    {
        return Err(conflict(backup));
    }
    #[cfg(unix)]
    {
        for path in [&destination, source] {
            if let Some(directory) = super::local_io::Directory::open(path, false)? {
                directory.verify_writable()?;
                directory.verify_owned_target()?;
            }
        }
    }
    if dry_run {
        return Ok(report);
    }
    #[cfg(unix)]
    {
        let source_directory =
            super::local_io::Directory::open(source, true)?.expect("created source parent");
        let source_guard = source_directory.lock(timeout_ms)?;
        source_directory.verify_owned_target()?;
        let destination_directory = if migrating_path {
            Some(
                super::local_io::Directory::open(&destination, true)?
                    .expect("created destination parent"),
            )
        } else {
            None
        };
        let directory = destination_directory.as_ref().unwrap_or(&source_directory);
        // Source then destination is fixed for migration; destination is never another JSON source.
        let destination_guard = if migrating_path {
            Some(directory.lock(timeout_ms)?)
        } else {
            None
        };
        let check = || -> Result<()> {
            config.parent_snapshot.verify(source)?;
            destination_snapshot.verify(&destination)?;
            source_directory.verify_lock(&source_guard.file, &source_guard.name)?;
            if let Some(guard) = &destination_guard {
                directory.verify_lock(&guard.file, &guard.name)?;
            }
            if source_directory.read(source.file_name().expect("name"), MAX_CONFIG_BYTES)?
                != config.bytes
            {
                return Err(conflict(source));
            }
            let expected = if migrating_path {
                &previous_destination
            } else {
                &config.bytes
            };
            if directory.read(destination.file_name().expect("name"), MAX_CONFIG_BYTES)?
                != *expected
            {
                return Err(conflict(&destination));
            }
            directory.verify_owned_target()?;
            Ok(())
        };
        check()?;
        if !report.changed {
            let result = directory
                .sync_existing(
                    destination.file_name().expect("name"),
                    MAX_CONFIG_BYTES,
                    config
                        .bytes
                        .as_deref()
                        .expect("unchanged existing configuration"),
                    false,
                )
                .and_then(|_| check());
            report.durable = result.is_ok();
            report.failure = result.err();
            return Ok(report);
        }
        let mut stage = directory.stage(&bytes)?;
        #[cfg(test)]
        test_checkpoint("staged", source)?;
        if migration {
            source_directory.preserve(
                config.bytes.as_deref().expect("migration bytes"),
                MAX_CONFIG_BYTES,
                &format!(".v{}.bak", config.document.schema_version),
            )?;
        }
        #[cfg(test)]
        test_checkpoint("preserved", source)?;
        check()?;
        if let Some(default) = &proposed.default_wiki
            && crate::vault::discovery::default_from_path(&default.path)? != *default
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "proposed default wiki changed during configuration update",
            ));
        }
        if migrating_path && previous_destination.as_deref() == Some(bytes.as_slice()) {
            // An exact previous publication is a retry; sync it before reporting durability.
            report.applied = true;
            let result = directory
                .sync_existing(
                    destination.file_name().expect("name"),
                    MAX_CONFIG_BYTES,
                    &bytes,
                    false,
                )
                .and_then(|_| check());
            report.durable = result.is_ok();
            report.failure = result.err();
            return Ok(report);
        }
        let mut publication = super::local_io::PublicationState::default();
        let result =
            directory.publish_tracked(&mut stage, previous_destination.is_some(), &mut publication);
        report.applied = publication.applied;
        report.durable = publication.durable;
        if let Err(error) = result {
            if !publication.applied {
                return Err(error);
            }
            report.failure = Some(error);
        }
        #[cfg(test)]
        if report.failure.is_none()
            && let Err(error) = test_checkpoint("published", source)
        {
            report.failure = Some(error);
        }
        Ok(report)
    }
    #[cfg(not(unix))]
    {
        Err(super::local_io::unsupported(&destination))
    }
}
fn conflict(path: &Path) -> WikiError {
    WikiError::new(
        ErrorCode::ContentConflict,
        format!(
            "configuration changed or destination conflicts with preview: {}",
            path.display()
        ),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct DefaultUpdateReport {
    pub configuration_path: PathBuf,
    pub previous: Option<DefaultWiki>,
    pub proposed: Option<DefaultWiki>,
    pub changed: bool,
    pub applied: bool,
    pub migration_from: Option<String>,
    pub preserved_v1_path: Option<PathBuf>,
}
/// The historical JSON default setter keeps v2 documents readable by v2 clients.
pub fn update_default(
    config: &LoadedUserConfig,
    proposed: Option<DefaultWiki>,
    dry_run: bool,
    timeout_ms: u64,
) -> Result<DefaultUpdateReport> {
    let path = require_write_target(config)?;
    let mut document = config.document.clone();
    if format_for_path(path) == UserConfigFormat::Json
        && (!config.summary.present || ["1", "2"].contains(&document.schema_version.as_str()))
    {
        document.schema_version = "2".into();
    } else {
        document.schema_version = LOCAL_CONFIG_SCHEMA_VERSION.into();
    }
    document.default_wiki = proposed.clone();
    let result = update_document(config, document, None, dry_run, timeout_ms)?;
    if let Some(mut failure) = result.failure {
        if result.applied {
            failure.hint = Some(format!(
                "Configuration was published at {}; reload it before retrying. Durability confirmed: {}",
                result.configuration_path.display(),
                result.durable
            ));
        }
        return Err(failure);
    }
    Ok(DefaultUpdateReport {
        configuration_path: result.configuration_path,
        previous: config.document.default_wiki.clone(),
        proposed,
        changed: result.changed,
        applied: result.applied,
        migration_from: result.migration_from,
        preserved_v1_path: result.preserved_original_path,
    })
}

#[cfg(test)]
type CheckpointHook = Box<dyn FnMut(&str, &Path) -> Result<()>>;
#[cfg(test)]
std::thread_local! { static CHECKPOINT_HOOK: std::cell::RefCell<Option<CheckpointHook>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn test_checkpoint(stage: &str, path: &Path) -> Result<()> {
    CHECKPOINT_HOOK.with(|hook| {
        hook.borrow_mut()
            .as_mut()
            .map_or(Ok(()), |hook| hook(stage, path))
    })
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;

fn apply(target: &mut Preferences, prefs: &LocalPreferences, origin: &str) {
    if let Some(v) = prefs.offline {
        target.offline = v;
        target.origins.insert("offline".into(), origin.into());
    }
    if let Some(v) = prefs.read_max_bytes {
        target.read_max_bytes = v;
        target
            .origins
            .insert("read_max_bytes".into(), origin.into());
    }
    if let Some(v) = prefs.lock_timeout_ms {
        target.lock_timeout_ms = v;
        target
            .origins
            .insert("lock_timeout_ms".into(), origin.into());
    }
    if let Some(v) = &prefs.profile {
        target.profile = Some(v.clone());
        target.origins.insert("profile".into(), origin.into());
    }
}
pub fn resolve(
    fs: &VaultFs,
    vault_id: &RecordId,
    options: &PreferenceOptions,
    user_local_path: Option<&Path>,
) -> Result<Preferences> {
    let loaded = match user_local_path {
        Some(path) => {
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|e| config_error(e.to_string()))?
                    .join(path)
            };
            load_path(Some(&path), UserConfigOrigin::Explicit)?
        }
        None => load_path(None, UserConfigOrigin::Disabled)?,
    };
    resolve_loaded(fs, vault_id, options, &loaded)
}

pub fn resolve_loaded(
    fs: &VaultFs,
    vault_id: &RecordId,
    options: &PreferenceOptions,
    loaded: &LoadedUserConfig,
) -> Result<Preferences> {
    let marker = crate::changes::prepare::read_bounded(
        fs,
        &VaultRelativePath::new("WIKI.md")?,
        crate::app::MAX_INPUT_BYTES,
    )?
    .ok_or_else(|| config_error("missing vault marker"))?;
    let note = parse_note(&marker);
    let record = note
        .canonical
        .as_ref()
        .filter(|r| r.kind() == RecordKind::Vault && r.id() == vault_id)
        .ok_or_else(|| config_error("configuration vault binding differs"))?;
    // Portable ordinary scalar keys remain flat Properties; never grant endpoint,
    // credential, executable, TLS or network permission through a vault note.
    for key in record.fields().keys().filter(|k| k.starts_with("lwiki_")) {
        if ![
            "lwiki_read_max_bytes",
            "lwiki_lock_timeout_ms",
            "lwiki_offline",
        ]
        .contains(&key.as_str())
        {
            return Err(config_error(format!(
                "unsupported portable preference: {key}"
            )));
        }
    }
    let portable = LocalPreferences {
        offline: record
            .field("lwiki_offline")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| config_error("lwiki_offline must be boolean"))
            })
            .transpose()?,
        read_max_bytes: record
            .field("lwiki_read_max_bytes")
            .map(|v| {
                v.as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| config_error("lwiki_read_max_bytes must be integer"))
            })
            .transpose()?,
        lock_timeout_ms: record
            .field("lwiki_lock_timeout_ms")
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| config_error("lwiki_lock_timeout_ms must be integer"))
            })
            .transpose()?,
        profile: record.string("wiki_profile").map(str::to_owned),
        providers_config: None,
    };
    let local = loaded.summary.present.then_some(&loaded.document);
    let mut result = Preferences {
        offline: false,
        read_max_bytes: crate::app::offline::DEFAULT_READ_BYTES,
        lock_timeout_ms: 5000,
        profile: None,
        profile_permitted: false,
        warnings: loaded.summary.warnings.clone(),
        origins: [
            "offline",
            "read_max_bytes",
            "lock_timeout_ms",
            "profile",
            "profile_permitted",
        ]
        .into_iter()
        .map(|key| (key.into(), "built_in".into()))
        .collect::<BTreeMap<_, _>>(),
    };
    apply(&mut result, &portable, "portable_wiki");
    if let Some(config) = &local {
        apply(&mut result, &config.defaults, "user_defaults");
        if let Some(prefs) = config.vaults.get(vault_id) {
            apply(&mut result, prefs, "user_vault");
        }
    }
    if let Some(v) = options.read_max_bytes {
        result.read_max_bytes = v;
        result
            .origins
            .insert("read_max_bytes".into(), "command_line".into());
    }
    if let Some(v) = options.lock_timeout_ms {
        result.lock_timeout_ms = v;
        result
            .origins
            .insert("lock_timeout_ms".into(), "command_line".into());
    }
    if let Some(v) = &options.profile {
        result.profile = Some(v.clone());
        result
            .origins
            .insert("profile".into(), "command_line".into());
    }
    result.offline |= options.offline;
    if options.offline {
        result
            .origins
            .insert("offline".into(), "command_line".into());
    }
    if result.read_max_bytes == 0 || result.read_max_bytes > crate::app::offline::MAX_INPUT_BYTES {
        return Err(config_error("read byte ceiling must be 1..=16 MiB"));
    }
    if result.lock_timeout_ms > 30_000 {
        return Err(config_error("writer lock timeout exceeds 30 seconds"));
    }
    if let Some(profile) = &result.profile {
        result.profile_permitted = local
            .as_ref()
            .is_some_and(|c| c.allowed_profiles.contains(profile));
        if local.is_some() {
            result
                .origins
                .insert("profile_permitted".into(), "user_config".into());
        }
        // This preference controls explicit provider work. Automatic embedding
        // consent is checked separately against the selected wiki and endpoint.
        // Absence here is not itself an error or a reason to warn on local reads.
    }
    Ok(result)
}

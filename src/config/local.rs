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
fn parse_document(path: &Path, bytes: &[u8]) -> Result<LocalConfigDocument> {
    let value: LocalConfigDocument = crate::changes::prepare::strict_json(bytes)
        .map_err(|e| super::local_io::error(path, "parse configuration", e.message))?;
    if !["1", LOCAL_CONFIG_SCHEMA_VERSION].contains(&value.schema_version.as_str()) {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "unsupported schema version; original is preserved",
        ));
    }
    if value.schema_version == "1" {
        let raw: serde_json::Value = crate::changes::prepare::strict_json(bytes)
            .map_err(|e| super::local_io::error(path, "parse configuration", e.message))?;
        if raw.get("default_wiki").is_some() {
            return Err(super::local_io::error(
                path,
                "validate configuration",
                "schema version 1 cannot contain default_wiki; migration requires version 2",
            ));
        }
    }
    if let Some(default) = &value.default_wiki {
        validate_default(path, default)?;
    }
    for preferences in std::iter::once(&value.defaults).chain(value.vaults.values()) {
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
            .is_some_and(|v| v.is_empty() || v.len() > 128)
        {
            return Err(super::local_io::error(
                path,
                "validate configuration",
                "profile name must contain 1..=128 bytes",
            ));
        }
    }
    if value
        .allowed_profiles
        .iter()
        .any(|v| v.is_empty() || v.len() > 128)
    {
        return Err(super::local_io::error(
            path,
            "validate configuration",
            "allowed profile names must contain 1..=128 bytes",
        ));
    }
    Ok(value)
}
fn validate_default(config_path: &Path, default: &DefaultWiki) -> Result<()> {
    if !default.path.is_absolute()
        || default.path.to_str().is_none()
        || default.path.as_os_str().len() > 4096
        || default.path.components().count() > 64
        || default
            .path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(super::local_io::error(
            config_path,
            "validate default wiki",
            "default_wiki.path must be a canonical absolute UTF-8 path bounded to 4096 bytes/64 components",
        ));
    }
    Ok(())
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
    pub migration_required: bool,
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
    Ok(base.join("lwiki/config.json"))
}

pub fn load_user_config(explicit: Option<&Path>, no_user_config: bool) -> Result<LoadedUserConfig> {
    let (path, origin) = if let Some(explicit) = explicit {
        let path = if explicit.is_absolute() {
            explicit.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| config_error(format!("resolve explicit preferences path: {e}")))?
                .join(explicit)
        };
        (Some(path), UserConfigOrigin::Explicit)
    } else if no_user_config {
        (None, UserConfigOrigin::Disabled)
    } else {
        (
            Some(ambient_path(
                std::env::var_os("XDG_CONFIG_HOME").as_deref(),
                std::env::var_os("HOME").as_deref(),
            )?),
            UserConfigOrigin::Ambient,
        )
    };
    load_path(path.as_deref(), origin)
}

fn load_path(requested: Option<&Path>, origin: UserConfigOrigin) -> Result<LoadedUserConfig> {
    let path = requested.map(super::local_io::effective_path).transpose()?;
    let parent_snapshot = path
        .as_deref()
        .map(super::local_io::ParentSnapshot::capture)
        .transpose()?
        .unwrap_or_default();
    let bytes = if let Some(path) = &path {
        #[cfg(unix)]
        {
            super::local_io::Directory::open(path, false)?
                .map(|directory| {
                    directory.read(
                        path.file_name().expect("validated file name"),
                        MAX_CONFIG_BYTES,
                    )
                })
                .transpose()?
                .flatten()
        }
        #[cfg(not(unix))]
        {
            return Err(super::local_io::unsupported(path));
        }
    } else {
        None
    };
    if origin == UserConfigOrigin::Explicit && bytes.is_none() {
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
            path,
            requested_path: requested.map(Path::to_path_buf),
            origin,
            present: bytes.is_some(),
            schema_version: bytes.as_ref().map(|_| document.schema_version.clone()),
            migration_required: bytes.is_some() && document.schema_version == "1",
        },
        document,
        bytes,
        parent_snapshot,
    })
}

pub fn require_write_target(config: &LoadedUserConfig) -> Result<&Path> {
    config.path().ok_or_else(|| config_error("default changes require user configuration; omit --no-user-config or provide --preferences FILE"))
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

/// Apply only to the original bytes loaded for this invocation; never reload policy.
pub fn update_default(
    config: &LoadedUserConfig,
    proposed: Option<DefaultWiki>,
    dry_run: bool,
    timeout_ms: u64,
) -> Result<DefaultUpdateReport> {
    let path = require_write_target(config)?;
    if timeout_ms > 30_000 {
        return Err(config_error(
            "configuration writer timeout exceeds 30000 ms",
        ));
    }
    if let Some(proposed) = &proposed {
        validate_default(path, proposed)?;
        // Init previews may name an uncreated wiki; actual writes must reidentify it.
        if !dry_run && crate::vault::discovery::default_from_path(&proposed.path)? != *proposed {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "proposed default wiki identity or canonical path changed",
            ));
        }
    }
    let migration = config.summary.migration_required;
    let changed = config.document.default_wiki != proposed || migration;
    let backup = migration.then(|| {
        let mut name = path.file_name().expect("validated file name").to_owned();
        name.push(".v1.bak");
        path.with_file_name(name)
    });
    let mut report = DefaultUpdateReport {
        configuration_path: path.to_path_buf(),
        previous: config.document.default_wiki.clone(),
        proposed: proposed.clone(),
        changed,
        applied: false,
        migration_from: migration.then(|| "1".into()),
        preserved_v1_path: backup,
    };
    let mut document = config.document.clone();
    document.schema_version = LOCAL_CONFIG_SCHEMA_VERSION.into();
    document.default_wiki = proposed;
    let mut bytes =
        serde_json::to_vec_pretty(&document).map_err(|e| config_error(e.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(super::local_io::error(
            path,
            "write configuration",
            "proposed JSON exceeds 64 KiB",
        ));
    }
    // The exact candidate is validated on a copy before any existing bytes change.
    let validated = parse_document(path, &bytes)?;
    if validated.schema_version != LOCAL_CONFIG_SCHEMA_VERSION {
        return Err(config_error("configuration migration validation failed"));
    }
    if dry_run {
        return Ok(report);
    }
    #[cfg(unix)]
    {
        config.parent_snapshot.verify(path)?;
        if !changed {
            let current = super::local_io::Directory::open(path, false)?
                .map(|directory| {
                    directory.read(
                        path.file_name().expect("validated file name"),
                        MAX_CONFIG_BYTES,
                    )
                })
                .transpose()?
                .flatten();
            if current != config.bytes {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    format!("configuration changed since preview: {}", path.display()),
                ));
            }
            return Ok(report);
        }
        let directory = super::local_io::Directory::open(path, true)?.expect("created parent");
        let guard = directory.lock(timeout_ms)?;
        let check = || -> Result<()> {
            config.parent_snapshot.verify(path)?;
            directory.verify_lock(&guard.file, &guard.name)?;
            if directory.read(
                path.file_name().expect("validated file name"),
                MAX_CONFIG_BYTES,
            )? != config.bytes
            {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    format!("configuration changed since preview: {}", path.display()),
                ));
            }
            Ok(())
        };
        check()?;
        let mut stage = directory.stage(&bytes)?;
        #[cfg(test)]
        test_checkpoint("staged", path)?;
        if migration {
            directory.preserve_v1(config.bytes.as_deref().expect("v1 bytes"), MAX_CONFIG_BYTES)?;
        }
        #[cfg(test)]
        test_checkpoint("preserved", path)?;
        check()?;
        if let Some(proposed) = &report.proposed {
            if crate::vault::discovery::default_from_path(&proposed.path)? != *proposed {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "proposed default wiki changed during configuration update",
                ));
            }
        }
        directory.publish(&mut stage)?;
        #[cfg(test)]
        test_checkpoint("published", path)?;
        report.applied = true;
        Ok(report)
    }
    #[cfg(not(unix))]
    {
        Err(super::local_io::unsupported(path))
    }
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
    };
    let local = loaded.summary.present.then_some(&loaded.document);
    let mut result = Preferences {
        offline: false,
        read_max_bytes: crate::app::offline::DEFAULT_READ_BYTES,
        lock_timeout_ms: 5000,
        profile: None,
        profile_permitted: false,
        warnings: vec![],
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
        if !result.profile_permitted {
            result.warnings.push(format!("selected profile {profile:?} is not listed in local preferences; provider authorization is checked separately before remote work"));
        }
    }
    Ok(result)
}

//! Explicit private JSON preferences; no ambient credential or environment reads.
use super::types::*;
use crate::{domain::*, records::parse_note, vault::VaultFs};
use std::{fs::File, io::Read, path::Path};
const MAX_CONFIG_BYTES: usize = super::types::MAX_LOCAL_CONFIG_BYTES;
fn config_error(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ConfigInvalid, message)
}
fn read_local(path: &Path) -> Result<LocalConfigDocument> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| config_error(format!("inspect local configuration: {e}")))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_CONFIG_BYTES as u64
    {
        return Err(config_error(
            "local configuration must be bounded regular JSON file",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| config_error(e.to_string()))?
        .take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| config_error(e.to_string()))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(config_error("local configuration exceeds 64 KiB"));
    }
    let value: LocalConfigDocument =
        crate::changes::prepare::strict_json(&bytes).map_err(|e| config_error(e.message))?;
    if value.schema_version != "1" {
        return Err(config_error("unsupported local configuration schema"));
    }
    Ok(value)
}
fn apply(target: &mut Preferences, prefs: &LocalPreferences) {
    if let Some(v) = prefs.offline {
        target.offline = v;
    }
    if let Some(v) = prefs.read_max_bytes {
        target.read_max_bytes = v;
    }
    if let Some(v) = prefs.lock_timeout_ms {
        target.lock_timeout_ms = v;
    }
    if let Some(v) = &prefs.profile {
        target.profile = Some(v.clone());
    }
}
pub fn resolve(
    fs: &VaultFs,
    vault_id: &RecordId,
    options: &PreferenceOptions,
    user_local_path: Option<&Path>,
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
    let local = user_local_path.map(read_local).transpose()?;
    let mut result = Preferences {
        offline: false,
        read_max_bytes: crate::app::offline::DEFAULT_READ_BYTES,
        lock_timeout_ms: 5000,
        profile: None,
        profile_permitted: false,
        warnings: vec![],
    };
    apply(&mut result, &portable);
    if let Some(config) = &local {
        apply(&mut result, &config.defaults);
        if let Some(prefs) = config.vaults.get(vault_id) {
            apply(&mut result, prefs);
        }
    }
    if let Some(v) = options.read_max_bytes {
        result.read_max_bytes = v;
    }
    if let Some(v) = options.lock_timeout_ms {
        result.lock_timeout_ms = v;
    }
    if let Some(v) = &options.profile {
        result.profile = Some(v.clone());
    }
    result.offline |= options.offline;
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
        if !result.profile_permitted {
            result.warnings.push(format!("selected profile {profile:?} is not listed in local preferences; provider authorization is checked separately before remote work"));
        }
    }
    Ok(result)
}

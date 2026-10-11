//! Narrow setup edits using the existing provider schema and authority validator.
use super::providers::{MAX_PROVIDER_CONFIG_BYTES, ProviderConfig};
use crate::{config::DefaultWiki, domain::*};
use std::path::PathBuf;
use toml::{Table, Value};

#[derive(Clone, Debug)]
pub enum SetupAuth {
    Environment(String),
    PrivateFile(PathBuf),
}

#[derive(Clone, Debug)]
pub struct EmbeddingSetup {
    pub model: String,
    pub url: String,
    pub dimensions: Option<String>,
    pub auth: SetupAuth,
    pub grant: bool,
}

pub struct ProviderSetupDocument {
    pub bytes: Vec<u8>,
    pub profile: String,
    pub service: String,
}

fn invalid(message: &'static str) -> WikiError {
    WikiError::new(ErrorCode::ConfigInvalid, message)
}

fn table<'a>(value: &'a mut Table, name: &str) -> Result<&'a mut Table> {
    value
        .get_mut(name)
        .and_then(Value::as_table_mut)
        .ok_or_else(|| {
            invalid(
                "provider configuration must contain profiles, services and vault_bindings tables",
            )
        })
}

fn insert_exact(
    document: &mut Table,
    additions: &mut Table,
    section: &str,
    name: &str,
    candidate: Value,
) -> Result<()> {
    let entries = table(document, section)?;
    if let Some(existing) = entries.get(name) {
        if *existing != candidate {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "setup provider entry already exists with different settings; existing configuration is preserved",
            ));
        }
    } else {
        entries.insert(name.into(), candidate.clone());
        additions
            .entry(section)
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .expect("setup additions table")
            .insert(name.into(), candidate);
    }
    Ok(())
}

/// Preserve unrelated entries and grants. Deterministic names make replay idempotent.
/// No auth source is read or executed here, including referenced private files.
pub fn build_embedding_document(
    existing: Option<&[u8]>,
    setup: &EmbeddingSetup,
    wiki: &DefaultWiki,
) -> Result<ProviderSetupDocument> {
    let mut document = if let Some(bytes) = existing {
        ProviderConfig::validate_setup_bytes(bytes)?;
        let text = std::str::from_utf8(bytes)
            .map_err(|_| invalid("invalid provider configuration encoding"))?;
        toml::from_str::<Table>(text)
            .map_err(|_| invalid("invalid provider configuration; original is preserved"))?
    } else {
        Table::from_iter([
            ("version".into(), Value::Integer(1)),
            ("profiles".into(), Value::Table(Table::new())),
            ("services".into(), Value::Table(Table::new())),
            ("vault_bindings".into(), Value::Table(Table::new())),
        ])
    };
    let mut additions = Table::new();
    let mut auth = Table::from_iter([("kind".into(), Value::String("static".into()))]);
    match &setup.auth {
        SetupAuth::Environment(name) => {
            auth.insert("key_env".into(), Value::String(name.clone()));
        }
        SetupAuth::PrivateFile(path) => {
            let path = path
                .to_str()
                .ok_or_else(|| invalid("credential file path must be UTF-8"))?;
            auth.insert("key_file".into(), Value::String(path.into()));
        }
    }
    let mut service_value = Table::from_iter([
        ("adapter".into(), Value::String("embeddings-v1".into())),
        ("url".into(), Value::String(setup.url.clone())),
        ("model".into(), Value::String(setup.model.clone())),
        ("auth".into(), Value::Table(auth)),
    ]);
    if let Some(dimensions) = &setup.dimensions {
        let value = if dimensions == "auto" {
            Value::String("auto".into())
        } else {
            let dimensions = dimensions
                .parse::<u32>()
                .ok()
                .filter(|dimensions| (1..=65_536).contains(dimensions))
                .ok_or_else(|| {
                    invalid("embedding dimensions must be auto or an integer from 1 to 65536")
                })?;
            Value::Integer(i64::from(dimensions))
        };
        service_value.insert("dimensions".into(), value);
    }
    let value = Value::Table(service_value);
    let encoded = toml::to_string(&value).map_err(|_| invalid("provider setup encoding failed"))?;
    let fingerprint = blake3::hash(encoded.as_bytes()).to_hex().to_string();
    let service = format!("lwiki-embedding-{}", &fingerprint[..24]);
    let profile = format!("lwiki-{}", &fingerprint[..24]);
    insert_exact(&mut document, &mut additions, "services", &service, value)?;
    insert_exact(
        &mut document,
        &mut additions,
        "profiles",
        &profile,
        Value::Table(Table::from_iter([(
            "embedding".into(),
            Value::String(service.clone()),
        )])),
    )?;
    if setup.grant {
        let bindings = table(&mut document, "vault_bindings")?;
        // Append a binding instead of rewriting an existing user's grants.
        let matching = bindings.values().any(|value| {
            let Some(binding) = value.as_table() else {
                return false;
            };
            binding.get("root").and_then(Value::as_str) == wiki.path.to_str()
                && binding.get("wiki_id").and_then(Value::as_str) == Some(wiki.wiki_id.as_str())
                && binding
                    .get("allowed_profiles")
                    .and_then(Value::as_array)
                    .is_some_and(|profiles| {
                        profiles
                            .iter()
                            .any(|value| value.as_str() == Some(&profile))
                    })
        });
        if !matching {
            let hash = blake3::hash(format!("{}\n{profile}", wiki.wiki_id).as_bytes())
                .to_hex()
                .to_string();
            let name = format!("lwiki-vault-{}", &hash[..24]);
            let path = wiki
                .path
                .to_str()
                .ok_or_else(|| invalid("wiki path must be UTF-8"))?;
            insert_exact(
                &mut document,
                &mut additions,
                "vault_bindings",
                &name,
                Value::Table(Table::from_iter([
                    ("root".into(), Value::String(path.into())),
                    ("wiki_id".into(), Value::String(wiki.wiki_id.to_string())),
                    (
                        "allowed_profiles".into(),
                        Value::Array(vec![Value::String(profile.clone())]),
                    ),
                ])),
            )?;
        }
    }
    let bytes = if let Some(existing) = existing {
        if additions.is_empty() {
            existing.to_vec()
        } else {
            let appended = toml::to_string(&additions)
                .map_err(|_| invalid("provider setup encoding failed"))?;
            let mut bytes = existing.to_vec();
            if !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            bytes.push(b'\n');
            // Parents have no scalar entries. Omit optional empty parent headers
            // so an existing explicit table is never redeclared.
            for line in appended.lines() {
                if ["[profiles]", "[services]", "[vault_bindings]"].contains(&line) {
                    continue;
                }
                bytes.extend_from_slice(line.as_bytes());
                bytes.push(b'\n');
            }
            bytes
        }
    } else {
        toml::to_string(&document)
            .map_err(|_| invalid("provider setup encoding failed"))?
            .into_bytes()
    };
    if bytes.len() > MAX_PROVIDER_CONFIG_BYTES {
        return Err(invalid(
            "proposed provider configuration exceeds 64 KiB; original is preserved",
        ));
    }
    ProviderConfig::validate_setup_bytes(&bytes)?;
    Ok(ProviderSetupDocument {
        bytes,
        profile,
        service,
    })
}

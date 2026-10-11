//! Narrow logical-to-physical mappings. Original citations and manifest bytes do not change.
use crate::{
    domain::*,
    records::parse_note,
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use serde::{Deserialize, Serialize};
pub(crate) const ACTIVE: &str = ".wiki/state/storage/layout.json";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Layout {
    pub version: u32,
    pub vault_id: RecordId,
    pub migration_id: RecordId,
    pub migration_hash: Blake3Hash,
}
pub(crate) fn raw_read(
    root: &VaultRoot,
    path: &VaultRelativePath,
    max: usize,
) -> Result<Option<Vec<u8>>> {
    crate::vault::fs::read_regular_bounded(root, path, true, max, "storage-read")
}
pub(crate) type RawReader<'a> =
    dyn FnMut(&VaultRelativePath, usize) -> Result<Option<Vec<u8>>> + 'a;

// Enforce the validator's own per-read ceiling even for supplied readers.
pub(crate) fn read_with_reader(
    reader: &mut RawReader<'_>,
    path: &VaultRelativePath,
    max: usize,
) -> Result<Option<Vec<u8>>> {
    let bytes = reader(path, max)?;
    if bytes.as_ref().is_some_and(|bytes| bytes.len() > max) {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "storage read exceeds bound",
        ));
    }
    Ok(bytes)
}

pub(crate) fn vault(root: &VaultRoot) -> Result<(RecordId, u32)> {
    vault_with_reader(&mut |path, max| raw_read(root, path, max))
}

pub(crate) fn vault_with_reader(reader: &mut RawReader<'_>) -> Result<(RecordId, u32)> {
    let bytes = read_with_reader(reader, &VaultRelativePath::new("WIKI.md")?, 1024 * 1024)?
        .ok_or_else(|| WikiError::invalid("storage vault marker missing"))?;
    let record = parse_note(&bytes)
        .canonical
        .ok_or_else(|| WikiError::invalid("storage vault marker invalid"))?;
    if record.kind() != RecordKind::Vault {
        return Err(WikiError::invalid("storage root is not a vault"));
    }
    Ok((
        record.id().clone(),
        if record.string("wiki_schema") == Some("2") {
            2
        } else {
            1
        },
    ))
}
/// A validated mapping bound to one canonical vault root. Freshness of the raw
/// observations belongs to the caller; mapping never performs additional reads.
#[derive(Debug)]
pub(crate) struct ValidatedLayout {
    root: VaultRoot,
    retained: bool,
}
impl ValidatedLayout {
    pub(crate) fn capture_with_reader(
        root: &VaultRoot,
        reader: &mut RawReader<'_>,
    ) -> Result<Self> {
        Self::capture_with_reader_budgeted(root, reader, &mut || Ok(()))
    }
    pub(crate) fn capture_with_reader_budgeted(
        root: &VaultRoot,
        reader: &mut RawReader<'_>,
        on_step: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Self> {
        let retained = active_with_reader(root, reader, on_step, false, None)?;
        Ok(Self {
            root: root.clone(),
            retained,
        })
    }
    pub(crate) fn capture_fs_with_reader(fs: &VaultFs, reader: &mut RawReader<'_>) -> Result<Self> {
        let retained = active_with_reader(fs.root(), reader, &mut || Ok(()), false, Some(fs))?;
        Ok(Self {
            root: fs.root().clone(),
            retained,
        })
    }
    fn require_root(&self, root: &VaultRoot) -> Result<()> {
        if &self.root != root {
            return Err(WikiError::invalid(
                "validated layout belongs to a different vault root",
            ));
        }
        Ok(())
    }
    pub(crate) fn map(
        &self,
        root: &VaultRoot,
        logical: &VaultRelativePath,
    ) -> Result<VaultRelativePath> {
        self.require_root(root)?;
        Ok(if self.retained {
            managed_path(logical).unwrap_or_else(|| logical.clone())
        } else {
            logical.clone()
        })
    }
    pub(crate) fn retained(&self, root: &VaultRoot) -> Result<bool> {
        self.require_root(root)?;
        Ok(self.retained)
    }
}

/// Complete activation observation carrying an owned execution I/O policy.
/// Root-only callers keep the ordinary resolver and its established cost.
pub(crate) fn active_fs(fs: &VaultFs) -> Result<bool> {
    if let Some(result) = super::maintenance_activation::worker_active_fs(fs) {
        return result;
    }
    let result = active_with_reader(
        fs.root(),
        &mut |path, max| fs.read_regular_bounded(path, true, max, "storage-read"),
        &mut || Ok(()),
        true,
        Some(fs),
    );
    if result.is_err() {
        super::cleanup::invalidate_import_epoch(fs.root());
    }
    result
}

pub fn active(root: &VaultRoot) -> Result<bool> {
    if let Some(result) = super::maintenance_activation::worker_active(root) {
        return result;
    }
    let result = active_with_reader(
        root,
        &mut |path, max| raw_read(root, path, max),
        &mut || Ok(()),
        true,
        None,
    );
    if result.is_err() {
        super::cleanup::invalidate_import_epoch(root);
    }
    result
}

fn active_with_reader(
    root: &VaultRoot,
    reader: &mut RawReader<'_>,
    on_step: &mut dyn FnMut() -> Result<()>,
    native_reader: bool,
    scoped_fs: Option<&VaultFs>,
) -> Result<bool> {
    if super::maintenance_activation::worker_context_active() {
        return Err(WikiError::new(
            ErrorCode::Internal,
            "worker cannot decode unproved activation authority",
        ));
    }
    let Some(bytes) = read_with_reader(reader, &VaultRelativePath::new(ACTIVE)?, 4096)? else {
        if read_with_reader(reader, &VaultRelativePath::new("WIKI.md")?, 1024 * 1024)?.is_none() {
            #[cfg(unix)]
            if let Some(fs) = scoped_fs.filter(|fs| fs.uses_descriptor_relative()) {
                return empty_initialization_fs(fs, on_step).map(|()| false);
            }
            // Initialization validates/creates empty managed directories before
            // publishing its first marker. Missing markers never waive retained
            // history: only an empty authority namespace (plus writer lock) fits.
            for name in [".wiki/retained", ".wiki/state/storage"] {
                if root
                    .resolve_raw_budgeted(&VaultRelativePath::new(name)?, on_step)?
                    .exists()
                {
                    return Err(WikiError::new(
                        ErrorCode::RecoveryRequired,
                        "missing vault marker with retained storage authority",
                    ));
                }
            }
            for name in ["changes", "runs", ".wiki/state"] {
                let directory =
                    root.resolve_raw_budgeted(&VaultRelativePath::new(name)?, on_step)?;
                on_step()?;
                match std::fs::read_dir(directory) {
                    Ok(entries) => {
                        for entry in entries {
                            on_step()?;
                            let entry = entry
                                .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
                            let writer_lock = name == ".wiki/state"
                                && entry.file_name() == "writer.lock"
                                && entry
                                    .file_type()
                                    .map_err(|e| {
                                        WikiError::new(ErrorCode::Internal, e.to_string())
                                    })?
                                    .is_file();
                            if !writer_lock {
                                return Err(WikiError::new(
                                    ErrorCode::RecoveryRequired,
                                    "missing vault marker with retained operational history",
                                ));
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(WikiError::new(ErrorCode::Internal, e.to_string())),
                }
            }
            return Ok(false);
        }
        if vault_with_reader(reader)?.1 == 2
            && !super::cleanup::pending_activation_matches_with_reader(reader)?
        {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "vault format 2 requires retained storage activation; restore the complete vault backup including .wiki",
            ));
        }
        return Ok(false);
    };
    let layout: Layout = decode(&bytes)?;
    let (id, schema) = vault_with_reader(reader)?;
    if layout.version != 2 || layout.vault_id != id || schema != 2 {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "storage activation differs from vault format",
        ));
    }
    if let Some(fs) = scoped_fs.filter(|fs| native_reader && fs.uses_descriptor_relative()) {
        super::cleanup::verify_activation_fs(fs, &layout)?;
    } else if native_reader {
        super::cleanup::verify_activation(root, &layout)?;
    } else {
        super::cleanup::verify_activation_with_reader(&layout, reader)?;
    }
    Ok(true)
}
#[cfg(unix)]
fn empty_initialization_fs(fs: &VaultFs, on_step: &mut dyn FnMut() -> Result<()>) -> Result<()> {
    use crate::vault::paths::descriptor;
    for name in [".wiki/retained", ".wiki/state/storage"] {
        let relative = VaultRelativePath::new(name)?;
        let route = fs.bind_raw_budgeted(&relative, on_step)?;
        match route.metadata() {
            Ok(_) => {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "missing vault marker with retained storage authority",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(WikiError::new(ErrorCode::Internal, e.to_string())),
        }
    }
    for name in ["changes", "runs", ".wiki/state"] {
        let relative = VaultRelativePath::new(name)?;
        let route = fs.bind_raw_budgeted(&relative, on_step)?;
        on_step()?;
        match route.metadata() {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(WikiError::new(ErrorCode::Internal, e.to_string())),
        }
        let directory = match descriptor::open_at(
            route
                .parent()
                .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
            route.leaf(),
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        ) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(WikiError::new(ErrorCode::Internal, e.to_string())),
        };
        route.require_directory_identity(&directory)?;
        descriptor::directory_entries(&directory, &mut |leaf, directory| {
            on_step()?;
            if name == ".wiki/state"
                && leaf == "writer.lock"
                && descriptor::entry_is_regular(directory, leaf)?
            {
                return Ok(());
            }
            Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "missing vault marker with retained operational history",
            ))
        })?;
        let fresh = fs.bind_raw_budgeted(&relative, on_step)?;
        route.same_parent(&fresh)?;
        fresh.require_directory_identity(&directory)?;
    }
    Ok(())
}
pub fn managed_path(path: &VaultRelativePath) -> Option<VaultRelativePath> {
    let s = path.as_str();
    let internal = if s == "changes" || s.starts_with("changes/") {
        format!(".wiki/retained/{s}")
    } else if s == "knowledge/extractions/packets"
        || s.starts_with("knowledge/extractions/packets/")
    {
        s.replacen("knowledge/extractions/packets", ".wiki/retained/packets", 1)
    } else {
        let p: Vec<_> = s.split('/').collect();
        if p.len() >= 3
            && p[0] == "runs"
            && (matches!(p[2], "events" | "checkpoints")
                || p.len() == 3 && matches!(p[2], "run.md" | "research.md")
                || p[2] == "outputs" && p.len() >= 4 && !p[3].starts_with("report_"))
        {
            format!(".wiki/retained/{s}")
        } else {
            return None;
        }
    };
    VaultRelativePath::new(internal).ok()
}
pub fn physical_relative(root: &VaultRoot, path: &VaultRelativePath) -> Result<VaultRelativePath> {
    crate::maintenance_diagnostic::observe(
        crate::maintenance_diagnostic::Phase::RouteConstruction,
        || {
            if let Some(mapped) = managed_path(path)
                && active(root)?
            {
                return Ok(mapped);
            }
            Ok(path.clone())
        },
    )
}
pub(crate) fn physical_relative_fs(
    fs: &VaultFs,
    path: &VaultRelativePath,
) -> Result<VaultRelativePath> {
    crate::maintenance_diagnostic::observe(
        crate::maintenance_diagnostic::Phase::RouteConstruction,
        || {
            if let Some(mapped) = managed_path(path)
                && active_fs(fs)?
            {
                return Ok(mapped);
            }
            Ok(path.clone())
        },
    )
}
pub(crate) fn logical_path(path: &VaultRelativePath) -> Option<VaultRelativePath> {
    let s = path.as_str();
    if (s == ".wiki/retained/packets" || s.starts_with(".wiki/retained/packets/"))
        && let Some(tail) = s.strip_prefix(".wiki/retained/packets")
    {
        return VaultRelativePath::new(format!("knowledge/extractions/packets{tail}")).ok();
    }
    let logical = VaultRelativePath::new(s.strip_prefix(".wiki/retained/")?).ok()?;
    managed_path(&logical)
        .filter(|mapped| mapped == path)
        .map(|_| logical)
}
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let payload = serde_json::to_value(value).map_err(|e| WikiError::invalid(e.to_string()))?;
    let bytes = serde_json::to_vec(&payload).map_err(|e| WikiError::invalid(e.to_string()))?;
    serde_json::to_vec(&serde_json::json!({"payload":payload,"checksum":Blake3Hash::digest(bytes)}))
        .map_err(|e| WikiError::invalid(e.to_string()))
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Envelope {
        payload: serde_json::Value,
        checksum: Blake3Hash,
    }
    let env: Envelope = crate::changes::prepare::strict_json(bytes)?;
    if Blake3Hash::digest(
        serde_json::to_vec(&env.payload).map_err(|e| WikiError::invalid(e.to_string()))?,
    ) != env.checksum
    {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "storage receipt checksum differs",
        ));
    }
    serde_json::from_value(env.payload).map_err(|e| WikiError::invalid(e.to_string()))
}
pub(crate) fn put(
    fs: &VaultFs,
    writer: &WriterPermit,
    path: &VaultRelativePath,
    bytes: &[u8],
) -> Result<()> {
    if let Some(existing) = fs.read_regular_bounded(path, true, bytes.len(), "storage-read")? {
        if existing != bytes {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "immutable storage bytes changed",
            ));
        }
        return crate::changes::journal::require_sync(fs.sync_target(path, writer)?);
    }
    if let Some((parent, _)) = path.as_str().rsplit_once('/') {
        crate::changes::journal::require_sync(
            fs.ensure_directory(&VaultRelativePath::new(parent)?, writer)?,
        )?;
    }
    let staged = fs.stage(path, bytes, writer)?;
    crate::changes::journal::require_sync(fs.replace(staged, &ExpectedState::Absent, writer)?)
}
pub(crate) fn object_path(hash: &Blake3Hash) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!(".wiki/retained/objects/blake3/{}", hash.hex()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PayloadMap {
    pub version: u32,
    pub change_id: RecordId,
    pub manifest_hash: Blake3Hash,
    pub entries: Vec<crate::changes::PayloadRef>,
}
pub(crate) fn payload_map_path(id: &RecordId) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!(".wiki/retained/payloads/{id}.json"))
}
/// Exact legacy payload references may use copied immutable objects after
/// activation. The mapping is bound to the original unmodified manifest.
pub(crate) fn legacy_payload(
    fs: &VaultFs,
    path: &VaultRelativePath,
    maximum: usize,
) -> Result<Option<Vec<u8>>> {
    legacy_payload_lookup(fs, path, maximum, false)
}
pub(crate) fn optional_legacy_payload(
    fs: &VaultFs,
    path: &VaultRelativePath,
    maximum: usize,
) -> Result<Option<Vec<u8>>> {
    legacy_payload_lookup(fs, path, maximum, true)
}
fn legacy_payload_lookup(
    fs: &VaultFs,
    path: &VaultRelativePath,
    maximum: usize,
    allow_missing: bool,
) -> Result<Option<Vec<u8>>> {
    let parts: Vec<_> = path.as_str().split('/').collect();
    if parts.len() < 4
        || parts[0] != "changes"
        || !matches!(parts[2], "before" | "proposed" | "assets")
        || !active_fs(fs)?
    {
        return Ok(None);
    }
    let id = RecordId::new(parts[1])?;
    let Some(bytes) = fs.read_regular_bounded(
        &payload_map_path(&id)?,
        true,
        crate::changes::prepare::MAX_MANIFEST_BYTES,
        "storage-read",
    )?
    else {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "retained legacy payload map missing",
        ));
    };
    let map: PayloadMap = decode(&bytes)?;
    let (manifest, hash) =
        crate::changes::ChangeEngine::new(fs.clone())?.load_manifest_structure(&id)?;
    if map.version != 1 || map.change_id != id || map.manifest_hash != hash {
        return Err(WikiError::invalid("legacy payload map binding differs"));
    }
    let Some(reference) = map.entries.iter().find(|entry| &entry.path == path) else {
        return Ok(None);
    };
    if !manifest
        .operations
        .iter()
        .flat_map(|op| [&op.before_payload, &op.after_payload])
        .flatten()
        .any(|r| r == reference)
    {
        return Err(WikiError::invalid(
            "legacy object not bound to original payload",
        ));
    }
    let Some(bytes) = fs.read_regular_bounded(
        &object_path(&reference.hash)?,
        true,
        maximum,
        "storage-read",
    )?
    else {
        return if allow_missing {
            Ok(None)
        } else {
            Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "retained undo payload is unavailable after cleanup; committed history remains inspectable",
            ))
        };
    };
    if bytes.len() as u64 != reference.byte_len || Blake3Hash::digest(&bytes) != reference.hash {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "retained shared object integrity failure",
        ));
    }
    Ok(Some(bytes))
}

#[cfg(test)]
#[path = "layout_binding_tests.rs"]
mod layout_binding_tests;

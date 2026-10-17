//! Narrow logical-to-physical mappings. Original citations and manifest bytes do not change.
use crate::{
    domain::*,
    records::parse_note,
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum LayoutKind {
    Legacy = 1,
    Retained = 2,
    ReadableSources = 3,
}
impl LayoutKind {
    pub(crate) fn from_version(version: u32) -> Result<Self> {
        match version {
            1 => Ok(Self::Legacy),
            2 => Ok(Self::Retained),
            3 => Ok(Self::ReadableSources),
            _ => Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "unsupported storage layout version",
            )),
        }
    }
}
/// Select membership only from the exact base marker in a caller-authenticated
/// validation input. Proposed markers and display frontmatter grant no exclusion.
/// Physical discovery separately proves the corresponding activation receipt.
pub(crate) fn input_layout_kind(input: &crate::changes::ValidationInput) -> Option<LayoutKind> {
    let mut markers = input
        .documents
        .iter()
        .filter(|document| document.path.as_str() == "WIKI.md");
    let marker = markers.next()?;
    if markers.next().is_some() {
        return None;
    }
    marker_layout_kind(&input.vault_id, marker)
}
pub(crate) fn marker_layout_kind(
    vault_id: &RecordId,
    marker: &crate::changes::ScanDocument,
) -> Option<LayoutKind> {
    if marker.path.as_str() != "WIKI.md" || Blake3Hash::digest(&marker.bytes) != marker.hash {
        return None;
    }
    let record = parse_note(&marker.bytes).canonical?;
    if record.kind() != RecordKind::Vault || record.id() != vault_id {
        return None;
    }
    match record.string("wiki_schema") {
        Some("1") => Some(LayoutKind::Legacy),
        Some("2") => Some(LayoutKind::Retained),
        Some("3") => Some(LayoutKind::ReadableSources),
        _ => None,
    }
}
pub(crate) fn presentation_path_excluded(
    kind: Option<LayoutKind>,
    path: &VaultRelativePath,
) -> bool {
    kind == Some(LayoutKind::ReadableSources)
        && path
            .as_str()
            .starts_with(crate::domain::source_presentation::PRESENTATION_PREFIX)
}
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
        match record.string("wiki_schema") {
            Some("1") => 1,
            Some("2") => 2,
            Some("3") => 3,
            _ => return Err(WikiError::invalid("unsupported vault schema")),
        },
    ))
}
/// A validated mapping bound to one canonical vault root. Freshness of the raw
/// observations belongs to the caller; mapping never performs additional reads.
#[derive(Debug)]
pub(crate) struct ValidatedLayout {
    root: VaultRoot,
    kind: LayoutKind,
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
        let kind = active_with_reader(root, reader, on_step, false)?;
        Ok(Self {
            root: root.clone(),
            kind,
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
        Ok(managed_path_for(self.kind, logical).unwrap_or_else(|| logical.clone()))
    }
    pub(crate) fn retained(&self, root: &VaultRoot) -> Result<bool> {
        self.require_root(root)?;
        Ok(self.kind != LayoutKind::Legacy)
    }
    pub(crate) fn kind(&self, root: &VaultRoot) -> Result<LayoutKind> {
        self.require_root(root)?;
        Ok(self.kind)
    }
}

pub fn active(root: &VaultRoot) -> Result<bool> {
    Ok(layout_kind(root)? != LayoutKind::Legacy)
}

pub fn layout_kind(root: &VaultRoot) -> Result<LayoutKind> {
    if let Some(result) = super::maintenance_activation::worker_kind(root) {
        return result;
    }
    let result = active_with_reader(
        root,
        &mut |path, max| raw_read(root, path, max),
        &mut || Ok(()),
        true,
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
) -> Result<LayoutKind> {
    if super::maintenance_activation::worker_context_active() {
        return Err(WikiError::new(
            ErrorCode::Internal,
            "worker cannot decode unproved activation authority",
        ));
    }
    let Some(bytes) = read_with_reader(reader, &VaultRelativePath::new(ACTIVE)?, 4096)? else {
        if read_with_reader(reader, &VaultRelativePath::new("WIKI.md")?, 1024 * 1024)?.is_none() {
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
            return Ok(LayoutKind::Legacy);
        }
        if vault_with_reader(reader)?.1 >= 2
            && !super::cleanup::pending_activation_matches_with_reader(reader)?
            && !super::source_layout::pending_activation_matches_with_reader(reader)?
        {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "vault format 2 requires retained storage activation; restore the complete vault backup including .wiki",
            ));
        }
        return Ok(LayoutKind::Legacy);
    };
    let layout: Layout = decode(&bytes)?;
    let (id, schema) = vault_with_reader(reader)?;
    let pending_upgrade = layout.version == 2
        && schema == 3
        && super::source_layout::pending_activation_matches_with_reader(reader)?;
    if !matches!(layout.version, 2 | 3)
        || layout.vault_id != id
        || schema != layout.version && !pending_upgrade
    {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "storage activation differs from vault format",
        ));
    }
    if layout.version == 3 {
        super::source_layout::verify_activation_with_reader(&layout, reader)?;
    } else if native_reader {
        super::cleanup::verify_activation(root, &layout)?;
    } else {
        super::cleanup::verify_activation_with_reader(&layout, reader)?;
    }
    LayoutKind::from_version(layout.version)
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
            if (managed_path(path).is_some() || source_managed_path(path).is_some())
                && let Some(mapped) = managed_path_for(layout_kind(root)?, path)
            {
                return Ok(mapped);
            }
            Ok(path.clone())
        },
    )
}
/// Source mappings are fixed grammar, never caller-supplied path aliases.
pub(crate) fn source_managed_path(path: &VaultRelativePath) -> Option<VaultRelativePath> {
    let p: Vec<_> = path.as_str().split('/').collect();
    if p.first() != Some(&"sources") || p.get(1) == Some(&"views") {
        return None;
    }
    let valid = match p.as_slice() {
        ["sources"] => false,
        ["sources", source] => RecordId::new(*source).is_ok(),
        ["sources", source, "source.md" | "revisions"] => RecordId::new(*source).is_ok(),
        ["sources", source, "revisions", revision] => {
            RecordId::new(*source).is_ok() && RecordId::new(*revision).is_ok()
        }
        ["sources", source, "revisions", revision, file] => {
            RecordId::new(*source).is_ok()
                && RecordId::new(*revision).is_ok()
                && (*file == "revision.md"
                    || *file == "content.md"
                    || file.starts_with("original.") && file.len() > 9)
        }
        _ => false,
    };
    valid
        .then(|| VaultRelativePath::new(format!(".wiki/retained/{}", path.as_str())).ok())
        .flatten()
}
pub fn managed_path_for(kind: LayoutKind, path: &VaultRelativePath) -> Option<VaultRelativePath> {
    match kind {
        LayoutKind::Legacy => None,
        LayoutKind::Retained => managed_path(path),
        LayoutKind::ReadableSources => source_managed_path(path).or_else(|| managed_path(path)),
    }
}
pub(crate) fn logical_path_for(
    kind: LayoutKind,
    path: &VaultRelativePath,
) -> Option<VaultRelativePath> {
    if kind == LayoutKind::ReadableSources
        && let Some(tail) = path.as_str().strip_prefix(".wiki/retained/")
    {
        let logical = VaultRelativePath::new(tail).ok()?;
        if managed_path_for(kind, &logical).as_ref() == Some(path) {
            return Some(logical);
        }
    }
    logical_path(path)
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
    if let Some(existing) = raw_read(fs.root(), path, bytes.len())? {
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
        || !active(fs.root())?
    {
        return Ok(None);
    }
    let id = RecordId::new(parts[1])?;
    let Some(bytes) = raw_read(
        fs.root(),
        &payload_map_path(&id)?,
        crate::changes::prepare::MAX_MANIFEST_BYTES,
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
    let Some(bytes) = raw_read(fs.root(), &object_path(&reference.hash)?, maximum)? else {
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

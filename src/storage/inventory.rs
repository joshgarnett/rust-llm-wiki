use super::{layout, types::*};
use crate::{domain::*, vault::VaultFs};
use std::{collections::BTreeSet, io::Read};
fn class(path: &str) -> String {
    if path.starts_with("sources/") || path.starts_with("knowledge/evidence/") {
        "evidence_provenance"
    } else if path.starts_with(".wiki/cache/") {
        "rebuildable_cache"
    } else if path.starts_with(".wiki/state/requests/")
        || path.starts_with(".wiki/state/diagnostics/")
    {
        "protected_response_or_diagnostic"
    } else if path.starts_with("changes/")
        || path.starts_with(".wiki/retained/")
        || path.starts_with(".wiki/state/")
    {
        "recovery_accounting"
    } else if path.starts_with("runs/") || path.starts_with("knowledge/extractions/") {
        "active_work_or_report"
    } else if path.ends_with(".md") {
        "readable_knowledge"
    } else {
        "unclassified"
    }
    .into()
}
pub(crate) fn validate(options: &StorageOptions) -> Result<()> {
    if options.max_files == 0
        || options.max_files > 1_000_000
        || options.max_bytes == 0
        || options.max_bytes > 16 * 1024 * 1024 * 1024
        || options.retain_undo_changes > 100_000
    {
        return Err(WikiError::new(ErrorCode::Usage, "storage bounds invalid"));
    }
    Ok(())
}
pub fn inventory(fs: &VaultFs, options: &StorageOptions) -> Result<StorageInventory> {
    validate(options)?;
    let (vault_id, _) = layout::vault(fs.root())?;
    let mut result = StorageInventory {
        vault_id,
        layout_version: if layout::active(fs.root())? { 2 } else { 1 },
        complete: true,
        totals: StorageTotals::default(),
        files: vec![],
        warnings: vec![],
    };
    let mut pending = vec![String::new()];
    let mut unique = BTreeSet::new();
    let mut visited = 0usize;
    while let Some(prefix) = pending.pop() {
        let directory = if prefix.is_empty() {
            fs.root().path().to_path_buf()
        } else {
            fs.root().resolve_raw(&VaultRelativePath::new(&prefix)?)?
        };
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(directory)
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?
        {
            visited += 1;
            if visited > options.max_files.saturating_mul(4).saturating_add(1024) {
                result.complete = false;
                result.warnings.push(
                    "storage directory entry bound reached; totals cover inspected files only"
                        .into(),
                );
                return Ok(result);
            }
            entries.push(entry.map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?);
        }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| WikiError::invalid("storage filename UTF-8 unavailable"))?;
            if name == ".git" {
                if !result.warnings.iter().any(|w| {
                    w == "Version-control .git entries are outside the vault storage inventory."
                }) {
                    result.warnings.push(
                        "Version-control .git entries are outside the vault storage inventory."
                            .into(),
                    );
                }
                continue;
            }
            let path = VaultRelativePath::new(if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            })?;
            let meta = entry
                .file_type()
                .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
            if meta.is_symlink() {
                result.complete = false;
                result.warnings.push(format!("protected symlink: {path}"));
                continue;
            }
            if meta.is_dir() {
                if entry.path().join("WIKI.md").is_file() {
                    result.complete = false;
                    result
                        .warnings
                        .push(format!("protected nested vault: {path}"));
                    continue;
                }
                pending.push(path.as_str().into());
                continue;
            }
            if !meta.is_file() {
                result.complete = false;
                result
                    .warnings
                    .push(format!("protected nonregular entry: {path}"));
                continue;
            }
            let length = entry
                .metadata()
                .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?
                .len();
            if result.files.len() >= options.max_files
                || result.totals.logical_bytes.saturating_add(length) > options.max_bytes
            {
                result.complete = false;
                result.warnings.push(
                    "storage inventory bound reached; totals cover inspected files only".into(),
                );
                return Ok(result);
            }
            let mut file = std::fs::File::open(fs.root().resolve_raw(&path)?)
                .map_err(|e| WikiError::new(ErrorCode::FreshnessConflict, e.to_string()))?;
            if !file
                .metadata()
                .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?
                .is_file()
            {
                return Err(WikiError::invalid("inventory file is no longer regular"));
            }
            let mut hasher = blake3::Hasher::new();
            let mut buffer = [0u8; 65536];
            let mut observed = 0u64;
            loop {
                let n = file
                    .read(&mut buffer)
                    .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
                if n == 0 {
                    break;
                }
                observed += n as u64;
                if observed > length {
                    return Err(WikiError::new(
                        ErrorCode::FreshnessConflict,
                        "inventory file changed length",
                    ));
                }
                hasher.update(&buffer[..n]);
            }
            if observed != length {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "inventory file changed length",
                ));
            }
            let hash = Blake3Hash::new(format!("blake3:{}", hasher.finalize().to_hex()))?;
            result.totals.files += 1;
            result.totals.logical_bytes += length;
            if unique.insert((hash.clone(), length)) {
                result.totals.unique_content_bytes += length;
            }
            result.totals.duplicate_bytes =
                result.totals.logical_bytes - result.totals.unique_content_bytes;
            result.files.push(StorageFile {
                class: class(path.as_str()),
                path,
                hash,
                bytes: length,
            });
        }
    }
    result.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}

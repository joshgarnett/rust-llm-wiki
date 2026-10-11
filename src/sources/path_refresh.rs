//! Explicit input capture and thin path selection for the existing refresh API.
use super::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore, types::*};
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::parse_note,
    vault::{VaultRoot, fs::read_regular_bounded},
};
use serde::Serialize;
use std::path::Path;

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;

/// A path selected with the same uniqueness rules as ID-based refresh.
#[derive(Debug, Clone, Serialize)]
pub struct SourcePathSelection {
    pub path: VaultRelativePath,
    pub source_id: RecordId,
    pub hash: Blake3Hash,
    pub title: String,
}

impl SourceStore {
    /// Legacy selection uses the existing complete, bounded canonical view.
    pub fn select_refresh_path(&self, path: &VaultRelativePath) -> Result<SourcePathSelection> {
        let view = super::SourceView::from_fs_bounded(&self.fs, 64 * 1024 * 1024, 4096)?;
        let note = view.notes.get(path).ok_or_else(|| missing(path))?;
        let record = note
            .canonical
            .as_ref()
            .filter(|record| record.kind() == RecordKind::Source)
            .ok_or_else(|| invalid_source(path))?;
        let (resolved, unique) = view.resolve(record.id(), RecordKind::Source, None)?;
        if resolved != path || unique.source_hash != note.source_hash {
            return Err(invalid_source(path));
        }
        let selected = SourcePathSelection {
            path: path.clone(),
            source_id: record.id().clone(),
            hash: note.source_hash.clone(),
            title: record.title().to_owned(),
        };
        self.recheck_selection(&selected)?;
        Ok(selected)
    }

    /// Indexed facts discover the ID; unique lookup and exact current bytes
    /// authenticate the selected author note. No global freshness is asserted.
    pub(crate) fn select_refresh_path_indexed(
        &self,
        lookup: &dyn SourceRefreshLookup,
        path: &VaultRelativePath,
    ) -> Result<SourcePathSelection> {
        let unavailable = || {
            let mut error = WikiError::new(
                ErrorCode::SourceIntegrity,
                format!("published index cannot select a valid, unique managed Source at {path}"),
            );
            error.hint = Some("Inspect the path and run doctor --diagnostics or check for invalid or duplicate identity claims. Restore valid unique metadata and run index sync before retrying.".into());
            error
        };
        let found = lookup.record_at_path(path)?.ok_or_else(unavailable)?;
        if found.path != *path || found.record.kind() != RecordKind::Source {
            return Err(invalid_source(path));
        }
        let unique = lookup
            .unique_record(found.record.id())?
            .ok_or_else(unavailable)?;
        if unique.path != *path || unique.hash != found.hash || unique.record != found.record {
            return Err(invalid_source(path));
        }
        let selected = SourcePathSelection {
            path: path.clone(),
            source_id: found.record.id().clone(),
            hash: found.hash,
            title: found.record.title().to_owned(),
        };
        self.recheck_selection(&selected)?;
        Ok(selected)
    }

    fn recheck_selection(&self, selected: &SourcePathSelection) -> Result<()> {
        let bytes = read_regular_bounded(
            self.fs.root(),
            &selected.path,
            false,
            MAX_INPUT_BYTES,
            "source-path-selection",
        )?
        .ok_or_else(|| missing(&selected.path))?;
        let note = parse_note(&bytes);
        if note.source_hash != selected.hash
            || !note.canonical.is_some_and(|record| {
                record.kind() == RecordKind::Source && record.id() == &selected.source_id
            })
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                format!("selected Source bytes changed: {}", selected.path),
            ));
        }
        Ok(())
    }
}

/// Read only the explicitly supplied regular input, never a remembered origin.
/// The whole-file expected hash applies to exact bytes before extraction.
pub fn guarded_capture_file(
    path: &Path,
    title: Option<&str>,
    media_type: Option<String>,
    expected: &Blake3Hash,
) -> Result<CaptureRequest> {
    if path == Path::new("-") {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "guarded refresh input must name a regular file; standard input is unsupported",
        ));
    }
    let origin = path
        .to_str()
        .ok_or_else(|| WikiError::invalid("source input path must be UTF-8"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| WikiError::invalid("source input must name a UTF-8 regular file"))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let root = VaultRoot::for_initialization(parent.unwrap_or_else(|| Path::new(".")))?;
    let relative = VaultRelativePath::new(name)?;
    let original = read_regular_bounded(
        &root,
        &relative,
        true,
        MAX_INPUT_BYTES,
        "guarded-refresh-input",
    )?
    .ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecordNotFound,
            format!("source input is absent: {}", path.display()),
        )
    })?;
    let actual = Blake3Hash::digest(&original);
    if actual != *expected {
        let mut error = WikiError::new(
            ErrorCode::ContentConflict,
            format!(
                "source input hash differs from reviewed bytes: {}",
                path.display()
            ),
        );
        error.details = serde_json::json!({"path":origin,"expected":expected,"actual":actual});
        return Err(error);
    }
    let extraction = if super::local_text::local_text_candidate(path)
        && std::str::from_utf8(&original).is_ok()
    {
        ExtractionInput::Utf8Preserve
    } else {
        ExtractionInput::Unsupported {
            extractor: "unsupported-local-format-v1".into(),
            fingerprint: Blake3Hash::digest(b"unsupported-local-format-v1"),
        }
    };
    Ok(CaptureRequest {
        title: title.unwrap_or(name).to_owned(),
        origin_kind: SourceOrigin::LocalFile,
        origin: origin.to_owned(),
        original,
        extraction,
        media_type,
    })
}

fn missing(path: &VaultRelativePath) -> WikiError {
    WikiError::new(
        ErrorCode::RecordNotFound,
        format!("path must select an existing managed Source author note: {path}"),
    )
}
fn invalid_source(path: &VaultRelativePath) -> WikiError {
    WikiError::new(
        ErrorCode::SourceIntegrity,
        format!("path must select one valid, unique Source identity: {path}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn capture_preserves_unknown_frontmatter_bom_crlf_and_rejects_stale_input() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("café.md");
        let bytes = "\u{feff}---\r\nunknown: yes # author comment\r\n---\r\n東京\r\n".as_bytes();
        fs::write(&path, bytes).unwrap();
        let request = guarded_capture_file(&path, None, None, &Blake3Hash::digest(bytes)).unwrap();
        assert_eq!(request.original, bytes);
        assert!(matches!(request.extraction, ExtractionInput::Utf8Preserve));
        assert_eq!(request.title, "café.md");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            guarded_capture_file(&path, None, None, &Blake3Hash::digest(b"stale"))
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
    }

    #[test]
    fn input_is_explicit_bounded_and_arbitrary_bytes_remain_exact() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        fs::write(&path, [0xff, 0xfe, 0]).unwrap();
        let request =
            guarded_capture_file(&path, None, None, &Blake3Hash::digest([0xff, 0xfe, 0])).unwrap();
        assert_eq!(request.original, [0xff, 0xfe, 0]);
        assert!(matches!(
            request.extraction,
            ExtractionInput::Unsupported { .. }
        ));
        assert_eq!(
            guarded_capture_file(Path::new("-"), None, None, &Blake3Hash::digest(b""))
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_INPUT_BYTES as u64 + 1).unwrap();
        assert_eq!(
            guarded_capture_file(&path, None, None, &Blake3Hash::digest(b""))
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
    }

    #[cfg(unix)]
    #[test]
    fn guarded_input_refuses_final_symlink_and_directory() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original.md");
        let link = temp.path().join("linked.md");
        fs::write(&original, b"input").unwrap();
        std::os::unix::fs::symlink(&original, &link).unwrap();
        assert!(guarded_capture_file(&link, None, None, &Blake3Hash::digest(b"input")).is_err());
        assert!(guarded_capture_file(temp.path(), None, None, &Blake3Hash::digest(b"")).is_err());
    }
}

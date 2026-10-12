//! Bounded, deterministic local-file selection. Observation grants no publication authority.
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordKind, Result, VaultRelativePath, WikiError},
    records::parse_note,
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{VaultRoot, fs::read_regular_bounded},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
pub(crate) struct CaptureInputLimits {
    pub max_files: usize,
    pub max_entries: usize,
    pub max_depth: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_output_bytes: usize,
}
impl Default for CaptureInputLimits {
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_entries: 100_000,
            max_depth: 32,
            max_file_bytes: 16 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024,
            max_output_bytes: 16 * 1024 * 1024,
        }
    }
}

pub(crate) struct ObservedCaptureInput {
    pub input_path: PathBuf,
    pub origin: PathBuf,
    pub hash: Blake3Hash,
    pub request: CaptureRequest,
}
pub(crate) struct CaptureInputIssue {
    pub input_path: PathBuf,
    pub skipped: bool,
    pub reason: String,
    pub error: Option<WikiError>,
}
pub(crate) struct CaptureInputs {
    pub files: Vec<ObservedCaptureInput>,
    pub issues: Vec<CaptureInputIssue>,
    pub directories: Vec<PathBuf>,
}

fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn input_error(path: &Path, action: &str, error: impl std::fmt::Display) -> WikiError {
    let mut error = WikiError::new(
        ErrorCode::Usage,
        format!("{action} {}: {error}", path.display()),
    );
    error.hint = Some(
        "Check that this path names a readable regular file or folder, then repeat source capture."
            .into(),
    );
    error.details = serde_json::json!({"path":path,"action":action});
    error
}
fn text_default(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "md" | "markdown" | "txt"))
}
fn hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with('.'))
}

/// Resolve lexical aliases without using an inode as the original's identity.
pub(crate) fn normalized_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| input_error(path, "resolve input", e))?
            .join(path)
    };
    // Resolve the parent once; the final component must be inspected separately
    // so an explicitly named symbolic link never becomes an admitted file.
    let mut lexical = PathBuf::new();
    for part in absolute.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            part => lexical.push(part.as_os_str()),
        }
    }
    let Some(name) = lexical.file_name() else {
        return std::fs::canonicalize(&lexical).map_err(|e| input_error(path, "resolve input", e));
    };
    let parent = lexical
        .parent()
        .ok_or_else(|| input_error(path, "resolve input", "missing parent"))?;
    let parent =
        std::fs::canonicalize(parent).map_err(|e| input_error(path, "resolve input parent", e))?;
    let resolved = parent.join(name);
    resolved
        .to_str()
        .ok_or_else(|| input_error(path, "resolve input", "path must be UTF-8"))?;
    Ok(resolved)
}

pub(crate) fn read_input(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let parent = path
        .parent()
        .ok_or_else(|| input_error(path, "read input", "missing parent"))?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| input_error(path, "read input", "filename must be UTF-8"))?;
    let root = VaultRoot::for_initialization(parent)
        .map_err(|e| input_error(path, "open input folder", e.message))?;
    read_regular_bounded(
        &root,
        &VaultRelativePath::new(name)?,
        true,
        max_bytes,
        "capture input",
    )
    .map_err(|e| input_error(path, "read input", e.message))?
    .ok_or_else(|| input_error(path, "read input", "file is missing"))
}

impl ObservedCaptureInput {
    pub(crate) fn recheck(&self) -> Result<()> {
        let bytes = read_input(&self.origin, self.request.original.len())?;
        if Blake3Hash::digest(&bytes) != self.hash {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                format!(
                    "capture input changed after observation: {}; repeat source capture",
                    self.input_path.display()
                ),
            ));
        }
        Ok(())
    }
}

struct Selection<'a> {
    wiki: &'a Path,
    limits: CaptureInputLimits,
    selected: BTreeMap<PathBuf, PathBuf>,
    issues: Vec<CaptureInputIssue>,
    directories: BTreeSet<PathBuf>,
    visited: BTreeSet<PathBuf>,
    entries: usize,
    output_bytes: usize,
}
impl Selection<'_> {
    fn tick(&mut self, display: &Path) -> Result<()> {
        self.entries += 1;
        self.output_bytes = self
            .output_bytes
            .checked_add(display.as_os_str().len() + 512)
            .ok_or_else(|| budget("capture result size overflow"))?;
        if self.entries > self.limits.max_entries
            || self.output_bytes > self.limits.max_output_bytes
        {
            return Err(budget(
                "capture selection exceeds its entry or output allowance",
            ));
        }
        Ok(())
    }
    fn issue(&mut self, display: PathBuf, skipped: bool, reason: &str, error: Option<WikiError>) {
        self.issues.push(CaptureInputIssue {
            input_path: display,
            skipped,
            reason: reason.into(),
            error,
        });
    }
    fn visit(
        &mut self,
        path: PathBuf,
        display: PathBuf,
        explicit: bool,
        depth: usize,
    ) -> Result<()> {
        self.tick(&display)?;
        if depth > self.limits.max_depth {
            return Err(budget("capture directory depth exceeds 32 levels"));
        }
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                self.issue(
                    display.clone(),
                    false,
                    "unavailable",
                    Some(input_error(&display, "inspect input", e)),
                );
                return Ok(());
            }
        };
        if meta.file_type().is_symlink() {
            self.issue(
                display.clone(),
                !explicit,
                "symlink",
                explicit.then(|| {
                    input_error(
                        &display,
                        "capture input",
                        "symbolic links are not followed; select the original path",
                    )
                }),
            );
            return Ok(());
        }
        if hidden(&path) && (meta.is_dir() || !explicit) {
            self.issue(display, true, "hidden", None);
            return Ok(());
        }
        if let Ok(relative) = path.strip_prefix(self.wiki)
            && relative.components().next().is_some_and(|part| {
                let name = part.as_os_str().to_string_lossy().to_ascii_lowercase();
                matches!(
                    name.as_str(),
                    ".wiki" | ".git" | "sources" | "pages" | "knowledge" | "changes" | "wiki.md"
                )
            })
        {
            self.issue(display, true, "managed_wiki", None);
            return Ok(());
        }
        if !self.visited.insert(path.clone()) {
            return Ok(());
        }
        if meta.is_dir() {
            if path != self.wiki && path.join("WIKI.md").is_file() {
                let marker = read_input(&path.join("WIKI.md"), 1024 * 1024);
                if marker.ok().is_some_and(|bytes| {
                    parse_note(&bytes)
                        .canonical
                        .is_some_and(|r| r.kind() == RecordKind::Vault)
                }) {
                    self.issue(display, true, "managed_wiki", None);
                    return Ok(());
                }
            }
            if explicit {
                self.directories.insert(path.clone());
            }
            let children = match std::fs::read_dir(&path) {
                Ok(children) => children,
                Err(e) => {
                    self.issue(
                        display.clone(),
                        false,
                        "unreadable_folder",
                        Some(input_error(&display, "read folder", e)),
                    );
                    return Ok(());
                }
            };
            let mut children_sorted = Vec::new();
            for child in children {
                let child = child.map_err(|e| input_error(&display, "read folder member", e))?;
                if children_sorted.len() + self.entries >= self.limits.max_entries {
                    return Err(budget("capture directory entries exceed their allowance"));
                }
                children_sorted.push(child.file_name());
            }
            children_sorted.sort();
            for name in children_sorted {
                self.visit(path.join(&name), display.join(&name), false, depth + 1)?;
            }
        } else if !meta.is_file() {
            self.issue(display, true, "not_regular", None);
        } else if !explicit && !text_default(&path) {
            self.issue(display, true, "file_type", None);
        } else {
            if self.selected.len() >= self.limits.max_files {
                return Err(budget("capture selects more than 10000 files"));
            }
            self.selected.insert(path, display);
        }
        Ok(())
    }
}

pub(crate) fn observe_inputs(
    paths: &[PathBuf],
    files_from: Option<&Path>,
    wiki: &Path,
    limits: CaptureInputLimits,
) -> Result<CaptureInputs> {
    let mut inputs = paths.to_vec();
    if let Some(list) = files_from {
        let list = normalized_path(list)?;
        let bytes = read_input(&list, 1024 * 1024)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| input_error(&list, "read path list", "list must be UTF-8"))?;
        for line in text.lines().filter(|line| !line.is_empty()) {
            if inputs.len() >= limits.max_files {
                return Err(budget("capture path list exceeds file allowance"));
            }
            let path = PathBuf::from(line);
            inputs.push(if path.is_absolute() {
                path
            } else {
                list.parent().expect("list parent").join(path)
            });
        }
    }
    if inputs.is_empty() {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "source capture needs a file, folder, or --files-from path list",
        ));
    }
    if inputs.len() > limits.max_files {
        return Err(budget("capture arguments exceed file allowance"));
    }
    let mut selection = Selection {
        wiki,
        limits,
        selected: BTreeMap::new(),
        issues: vec![],
        directories: BTreeSet::new(),
        visited: BTreeSet::new(),
        entries: 0,
        output_bytes: 0,
    };
    // Explicit members precede directory traversal, so a selected non-default
    // format is retained even when an overlapping folder would exclude it.
    inputs.sort_by_key(|path| std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()));
    for display in inputs {
        match normalized_path(&display) {
            Ok(path) => selection.visit(path, display, true, 0)?,
            Err(e) => {
                selection.tick(&display)?;
                selection.issue(display, false, "unavailable", Some(e));
            }
        }
    }
    let mut total = 0usize;
    // All global input limits are checked before any canonical publication.
    for path in selection.selected.keys() {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let len = usize::try_from(meta.len()).map_err(|_| budget("capture input size overflow"))?;
        if len > limits.max_file_bytes {
            return Err(budget("capture input exceeds 16 MiB; select smaller notes"));
        }
        total = total
            .checked_add(len)
            .filter(|&v| v <= limits.max_total_bytes)
            .ok_or_else(|| budget("capture inputs exceed 256 MiB; select fewer notes"))?;
    }
    let mut files = Vec::new();
    total = 0;
    for (path, display) in selection.selected {
        let bytes = match read_input(&path, limits.max_file_bytes) {
            Ok(bytes) => bytes,
            Err(error) => {
                selection.issues.push(CaptureInputIssue {
                    input_path: display,
                    skipped: false,
                    reason: "unreadable".into(),
                    error: Some(error),
                });
                continue;
            }
        };
        total = total
            .checked_add(bytes.len())
            .filter(|&v| v <= limits.max_total_bytes)
            .ok_or_else(|| budget("observed capture inputs exceed 256 MiB"))?;
        if parse_note(&bytes).canonical.is_some() {
            selection.issues.push(CaptureInputIssue {
                input_path: display,
                skipped: true,
                reason: "managed_note".into(),
                error: None,
            });
            continue;
        }
        let extraction = if super::local_text::local_text_candidate(&path)
            && std::str::from_utf8(&bytes).is_ok()
        {
            ExtractionInput::Utf8Preserve
        } else {
            ExtractionInput::Unsupported {
                extractor: "unsupported-local-format-v1".into(),
                fingerprint: Blake3Hash::digest(b"unsupported-local-format-v1"),
            }
        };
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Captured source")
            .to_owned();
        files.push(ObservedCaptureInput {
            input_path: display,
            origin: path.clone(),
            hash: Blake3Hash::digest(&bytes),
            request: CaptureRequest {
                title,
                origin_kind: SourceOrigin::LocalFile,
                origin: path.to_str().expect("normalized UTF-8 path").into(),
                original: bytes,
                extraction,
                media_type: None,
            },
        });
    }
    Ok(CaptureInputs {
        files,
        issues: selection.issues,
        directories: selection.directories.into_iter().collect(),
    })
}

#[cfg(test)]
#[path = "capture_inputs_tests.rs"]
mod tests;

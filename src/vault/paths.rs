//! Canonical vault binding, portable paths, and deterministic envelope discovery.
use crate::domain::{ErrorCode, Result, VaultRelativePath, WikiError};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use unicase::UniCase;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRoot {
    path: PathBuf,
}

pub(crate) fn io_error(action: &str, error: std::io::Error) -> WikiError {
    WikiError::new(ErrorCode::Internal, format!("{action}: {error}"))
}
fn root_error(action: &str, error: std::io::Error) -> WikiError {
    if error.kind() == std::io::ErrorKind::NotFound {
        WikiError::new(
            ErrorCode::VaultNotFound,
            format!("{action}: vault path not found"),
        )
    } else {
        io_error(action, error)
    }
}
fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| WikiError::invalid("non-UTF-8 filesystem path is unsupported"))
}
fn exact_marker(path: &Path) -> Result<bool> {
    for entry in fs::read_dir(path).map_err(|e| io_error("read vault directory", e))? {
        let entry = entry.map_err(|e| io_error("read vault entry", e))?;
        if entry.file_name() == "WIKI.md" {
            let kind = entry
                .file_type()
                .map_err(|e| io_error("inspect WIKI.md", e))?;
            return Ok(kind.is_file() && !kind.is_symlink());
        }
    }
    Ok(false)
}
impl VaultRoot {
    /// Reject a symlink supplied as the root itself; canonicalize ancestor aliases once.
    pub fn for_initialization(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        utf8(path)?;
        let meta = fs::symlink_metadata(path).map_err(|e| root_error("inspect vault root", e))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(WikiError::invalid(
                "vault root must be a directory, not a symlink",
            ));
        }
        let path = fs::canonicalize(path).map_err(|e| root_error("canonicalize vault root", e))?;
        utf8(&path)?;
        Ok(Self { path })
    }
    pub fn explicit(path: impl AsRef<Path>) -> Result<Self> {
        let root = Self::for_initialization(path)?;
        if !exact_marker(&root.path)? {
            return Err(WikiError::new(
                ErrorCode::VaultNotFound,
                "vault requires exact-case regular WIKI.md",
            ));
        }
        Ok(root)
    }
    pub fn discover(start: impl AsRef<Path>) -> Result<Self> {
        utf8(start.as_ref())?;
        let mut path =
            fs::canonicalize(start).map_err(|e| root_error("resolve discovery start", e))?;
        if !path.is_dir() {
            path.pop();
        }
        loop {
            if exact_marker(&path)? {
                return Self::explicit(path);
            }
            if !path.pop() {
                return Err(WikiError::new(
                    ErrorCode::VaultNotFound,
                    "no ancestor has exact-case WIKI.md",
                ));
            }
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Reject existing symlink components, including the final destination.
    pub fn resolve(&self, relative: &VaultRelativePath) -> Result<PathBuf> {
        let mut path = self.path.clone();
        for component in relative.as_str().split('/') {
            path.push(component);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(WikiError::invalid("managed path has a symlink component"));
                }
                Ok(meta) => {
                    let canonical = fs::canonicalize(&path)
                        .map_err(|e| io_error("check destination containment", e))?;
                    if !canonical.starts_with(&self.path) {
                        return Err(WikiError::invalid("managed path escapes canonical vault"));
                    }
                    if meta.is_dir() && exact_marker(&path)? {
                        return Err(WikiError::invalid(
                            "managed path crosses a nested vault boundary",
                        ));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_error("inspect managed path", e)),
            }
        }
        Ok(path)
    }
    /// Compare every path prefix against plans and existing siblings, using full Unicode folding.
    pub fn validate_portable_paths(&self, paths: &[VaultRelativePath]) -> Result<()> {
        let mut planned = BTreeMap::<String, String>::new();
        let mut targets = std::collections::BTreeSet::new();
        for relative in paths {
            if !targets.insert(relative.as_str()) {
                return Err(WikiError::invalid("duplicate planned target"));
            }
            self.resolve(relative)?;
            let mut parent = self.path.clone();
            let mut prefix = String::new();
            for component in relative.as_str().split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);
                let folded = UniCase::unicode(&prefix).to_folded_case();
                if let Some(other) = planned.insert(folded, prefix.clone())
                    && other != prefix
                {
                    return Err(WikiError::invalid("case-folded planned path collision"));
                }
                if parent.is_dir() {
                    let folded = UniCase::unicode(component).to_folded_case();
                    for entry in
                        fs::read_dir(&parent).map_err(|e| io_error("check portable siblings", e))?
                    {
                        let name = entry
                            .map_err(|e| io_error("read portable sibling", e))?
                            .file_name();
                        let name = name.to_str().ok_or_else(|| {
                            WikiError::invalid("non-UTF-8 filesystem path is unsupported")
                        })?;
                        if name != component && UniCase::unicode(name).to_folded_case() == folded {
                            return Err(WikiError::invalid("case-folded existing path collision"));
                        }
                    }
                }
                parent.push(component);
            }
        }
        Ok(())
    }
    /// Canonical Markdown envelopes only; source payloads are read via their revision owner.
    pub fn scan_markdown(&self) -> Result<Vec<VaultRelativePath>> {
        let mut out = Vec::new();
        Self::scan_dir(&self.path, "", &mut out)?;
        out.sort_by(|a, b| a.as_str().as_bytes().cmp(b.as_str().as_bytes()));
        Ok(out)
    }
    fn scan_dir(directory: &Path, prefix: &str, out: &mut Vec<VaultRelativePath>) -> Result<()> {
        for entry in fs::read_dir(directory).map_err(|e| io_error("scan directory", e))? {
            let entry = entry.map_err(|e| io_error("scan entry", e))?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| WikiError::invalid("non-UTF-8 filesystem path is unsupported"))?;
            let kind = entry
                .file_type()
                .map_err(|e| io_error("scan file type", e))?;
            if kind.is_symlink()
                || matches!(name, ".wiki" | ".git")
                || (prefix.is_empty() && name == "changes")
            {
                continue;
            }
            let relative = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let parts: Vec<_> = relative.split('/').collect();
            let revision_payload = parts.len() >= 5
                && UniCase::unicode(parts[0]).to_folded_case() == "sources"
                && UniCase::unicode(parts[2]).to_folded_case() == "revisions";
            if revision_payload && !(parts.len() == 5 && parts[4] == "revision.md") {
                continue;
            }
            if kind.is_dir() {
                // An exact regular marker establishes a separate canonical vault.
                if exact_marker(&entry.path())? {
                    continue;
                }
                Self::scan_dir(&entry.path(), &relative, out)?;
            } else if kind.is_file() && name.ends_with(".md") && name != "index.md" {
                out.push(VaultRelativePath::new(relative)?);
            }
        }
        Ok(())
    }
}

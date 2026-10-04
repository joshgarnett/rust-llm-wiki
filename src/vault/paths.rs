//! Canonical vault binding, portable paths, and deterministic envelope discovery.
use crate::domain::{ErrorCode, Result, VaultRelativePath, WikiError};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use unicase::UniCase;

/// Aggregate diagnostics only; no path policy or filesystem operations change.
#[cfg(test)]
pub(crate) mod profile {
    use std::{cell::RefCell, collections::BTreeMap, path::Path, time::Instant};

    #[derive(Debug, Default, Clone, serde::Serialize)]
    pub(crate) struct Enumeration {
        pub opens: u64,
        pub entries: u64,
        pub folds: u64,
        pub elapsed_ns: u128,
    }
    #[derive(Debug, Default, Clone, serde::Serialize)]
    pub(crate) struct PathProfile {
        pub portable_calls: u64,
        pub portable_elapsed_ns: u128,
        pub enumerations: BTreeMap<String, Enumeration>,
    }
    std::thread_local! {
        static ACTIVE: RefCell<Option<PathProfile>> = const { RefCell::new(None) };
    }
    pub(crate) fn begin() {
        ACTIVE.with(|value| {
            assert!(value.borrow().is_none(), "path profile already active");
            *value.borrow_mut() = Some(PathProfile::default());
        });
    }
    pub(crate) fn finish() -> PathProfile {
        ACTIVE.with(|value| value.borrow_mut().take().expect("path profile inactive"))
    }
    #[derive(Clone, Copy)]
    pub(crate) enum Kind {
        Physical,
        Logical,
        Marker,
        Immutable,
    }
    pub(crate) struct EnumerationGuard {
        start: Option<Instant>,
        key: String,
        counts: Enumeration,
    }
    pub(crate) fn enumeration(path: &Path, root: &Path, kind: Kind) -> EnumerationGuard {
        let enabled = ACTIVE.with(|value| value.borrow().is_some());
        let group = match path.strip_prefix(root).ok().and_then(Path::to_str) {
            Some("sources") => "sources_root",
            Some(value) if value.starts_with("sources/") && value.ends_with("/revisions") => {
                "selected_revisions"
            }
            Some(value) if value.starts_with("sources/") => "selected_source_tree",
            Some(value)
                if value.starts_with("changes")
                    || value.starts_with(".wiki/state")
                    || value.starts_with(".wiki/retained/changes") =>
            {
                "changes_state"
            }
            Some("") => "vault_root",
            _ => "other",
        };
        let kind = match kind {
            Kind::Physical => "physical",
            Kind::Logical => "logical",
            Kind::Marker => "marker",
            Kind::Immutable => "immutable",
        };
        EnumerationGuard {
            start: enabled.then(Instant::now),
            key: if enabled {
                format!("{kind}:{group}")
            } else {
                String::new()
            },
            counts: Enumeration {
                opens: u64::from(enabled),
                ..Default::default()
            },
        }
    }
    impl EnumerationGuard {
        pub(crate) fn entry(&mut self) {
            self.counts.entries += u64::from(self.start.is_some());
        }
        pub(crate) fn fold(&mut self) {
            self.counts.folds += u64::from(self.start.is_some());
        }
    }
    impl Drop for EnumerationGuard {
        fn drop(&mut self) {
            if let Some(start) = self.start {
                self.counts.elapsed_ns = start.elapsed().as_nanos();
                ACTIVE.with(|value| {
                    if let Some(profile) = value.borrow_mut().as_mut() {
                        let total = profile.enumerations.entry(self.key.clone()).or_default();
                        total.opens += self.counts.opens;
                        total.entries += self.counts.entries;
                        total.folds += self.counts.folds;
                        total.elapsed_ns += self.counts.elapsed_ns;
                    }
                });
            }
        }
    }
    pub(crate) struct PortableGuard(Option<Instant>);
    pub(crate) fn portable() -> PortableGuard {
        PortableGuard(
            ACTIVE
                .with(|value| value.borrow().is_some())
                .then(Instant::now),
        )
    }
    impl Drop for PortableGuard {
        fn drop(&mut self) {
            if let Some(start) = self.0 {
                ACTIVE.with(|value| {
                    if let Some(profile) = value.borrow_mut().as_mut() {
                        profile.portable_calls += 1;
                        profile.portable_elapsed_ns += start.elapsed().as_nanos();
                    }
                });
            }
        }
    }
}

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
    exact_marker_budgeted(path, &mut || Ok(()))
}
fn exact_marker_budgeted(path: &Path, on_entry: &mut dyn FnMut() -> Result<()>) -> Result<bool> {
    // Most managed directories are not nested vaults. A fresh negative probe
    // avoids enumerating every sibling again for each checked file path.
    // A positive probe still needs exact-name enumeration on case-insensitive
    // filesystems, where looking up WIKI.md can find a differently cased name.
    on_entry()?;
    match fs::symlink_metadata(path.join("WIKI.md")) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => return Ok(false),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error("inspect vault marker", error)),
    }
    on_entry()?;
    #[cfg(test)]
    let mut profile = profile::enumeration(path, Path::new(""), profile::Kind::Marker);
    for entry in fs::read_dir(path).map_err(|e| io_error("read vault directory", e))? {
        #[cfg(test)]
        profile.entry();
        on_entry()?;
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
        self.resolve_budgeted(relative, &mut || Ok(()))
    }
    /// Checked path resolution meters component inspection and nested-marker scans.
    pub fn resolve_budgeted(
        &self,
        relative: &VaultRelativePath,
        on_entry: &mut dyn FnMut() -> Result<()>,
    ) -> Result<PathBuf> {
        let physical = crate::storage::layout::physical_relative(self, relative)?;
        self.resolve_raw_budgeted(&physical, on_entry)
    }
    /// Containment-checked physical paths for migration, never logical aliases.
    pub(crate) fn resolve_raw(&self, relative: &VaultRelativePath) -> Result<PathBuf> {
        self.resolve_raw_budgeted(relative, &mut || Ok(()))
    }
    pub(crate) fn resolve_raw_budgeted(
        &self,
        relative: &VaultRelativePath,
        on_entry: &mut dyn FnMut() -> Result<()>,
    ) -> Result<PathBuf> {
        let mut path = self.path.clone();
        for component in relative.as_str().split('/') {
            on_entry()?;
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
                    if meta.is_dir() && exact_marker_budgeted(&path, on_entry)? {
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
        self.validate_refresh_paths(paths, None)
    }
    pub(crate) fn validate_refresh_paths(
        &self,
        paths: &[VaultRelativePath],
        scope: Option<&crate::catalog::source_refresh::PublishedRefreshPaths>,
    ) -> Result<()> {
        if let Some(scope) = scope {
            scope.require_root(self)?;
        }
        #[cfg(test)]
        let _profile = profile::portable();
        let mut planned = BTreeMap::<String, String>::new();
        let mut targets = std::collections::BTreeSet::new();
        let retained = crate::storage::layout::active(self)?;
        for relative in paths {
            if !targets.insert(relative.as_str()) {
                return Err(WikiError::invalid("duplicate planned target"));
            }
            // Relocated paths retain the generic policy. Sources keep their
            // physical path under both storage layouts; logical alias guards
            // still run, with the same frozen published-prefix exemption.
            let scope = scope.filter(|scope| scope.allows(relative));
            if retained {
                self.validate_retained_logical_siblings(relative, scope)?;
            }
            self.resolve(relative)?;
            let physical = crate::storage::layout::physical_relative(self, relative)?;
            let mut parent = self.path.clone();
            let mut prefix = String::new();
            for component in physical.as_str().split('/') {
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
                if !scope.is_some_and(|scope| scope.published(&prefix)) && parent.is_dir() {
                    let folded = UniCase::unicode(component).to_folded_case();
                    #[cfg(test)]
                    let mut profile =
                        profile::enumeration(&parent, &self.path, profile::Kind::Physical);
                    for entry in
                        fs::read_dir(&parent).map_err(|e| io_error("check portable siblings", e))?
                    {
                        #[cfg(test)]
                        profile.entry();
                        let name = entry
                            .map_err(|e| io_error("read portable sibling", e))?
                            .file_name();
                        let name = name.to_str().ok_or_else(|| {
                            WikiError::invalid("non-UTF-8 filesystem path is unsupported")
                        })?;
                        if name != component {
                            #[cfg(test)]
                            profile.fold();
                            if UniCase::unicode(name).to_folded_case() == folded {
                                return Err(WikiError::invalid(
                                    "case-folded existing path collision",
                                ));
                            }
                        }
                    }
                }
                parent.push(component);
            }
        }
        Ok(())
    }
    /// Relocation must not erase portable logical siblings. Reserve only fixed
    /// namespace components; run IDs retain their exact spelling and identity.
    fn validate_retained_logical_siblings(
        &self,
        relative: &VaultRelativePath,
        scope: Option<&crate::catalog::source_refresh::PublishedRefreshPaths>,
    ) -> Result<()> {
        let mut parent = String::new();
        for component in relative.as_str().split('/') {
            let reserved: &[&str] = match parent.as_str() {
                "" => &["changes", "knowledge", "runs"],
                "knowledge" => &["extractions"],
                "knowledge/extractions" => &["packets"],
                p if p.starts_with("runs/") && p.split('/').count() == 2 => {
                    &["events", "checkpoints", "outputs", "run.md", "research.md"]
                }
                _ => &[],
            };
            let folded = UniCase::unicode(component).to_folded_case();
            if reserved
                .iter()
                .any(|name| *name != component && *name == folded)
            {
                return Err(WikiError::invalid("case-folded managed namespace alias"));
            }
            let prefix = if parent.is_empty() {
                component.to_owned()
            } else {
                format!("{parent}/{component}")
            };
            if scope.is_some_and(|scope| scope.published(&prefix)) {
                parent = prefix;
                continue;
            }
            let visible = if parent.is_empty() {
                self.path.clone()
            } else {
                self.resolve_raw(&VaultRelativePath::new(&parent)?)?
            };
            // Runs and outputs are split directories: readable reports can stay
            // visible while machine records and their parent IDs live internally.
            let internal = if parent == "runs" || parent.starts_with("runs/") {
                Some(VaultRelativePath::new(format!(".wiki/retained/{parent}"))?)
            } else if parent.is_empty() {
                None
            } else {
                crate::storage::layout::managed_path(&VaultRelativePath::new(&parent)?)
            };
            let internal = internal.map(|path| self.resolve_raw(&path)).transpose()?;
            for directory in std::iter::once(visible).chain(internal) {
                #[cfg(test)]
                let mut profile =
                    profile::enumeration(&directory, &self.path, profile::Kind::Logical);
                let entries = match fs::read_dir(directory) {
                    Ok(entries) => entries,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(io_error("check logical siblings", e)),
                };
                for entry in entries {
                    #[cfg(test)]
                    profile.entry();
                    let name = entry
                        .map_err(|e| io_error("read logical sibling", e))?
                        .file_name();
                    let name = name.to_str().ok_or_else(|| {
                        WikiError::invalid("non-UTF-8 filesystem path is unsupported")
                    })?;
                    if name != component {
                        #[cfg(test)]
                        profile.fold();
                        if UniCase::unicode(name).to_folded_case() == folded {
                            return Err(WikiError::invalid("case-folded logical path collision"));
                        }
                    }
                }
            }
            if !parent.is_empty() {
                parent.push('/');
            }
            parent.push_str(component);
        }
        Ok(())
    }
    /// Canonical Markdown envelopes only; source payloads are read via their revision owner.
    pub fn scan_markdown(&self) -> Result<Vec<VaultRelativePath>> {
        self.scan_markdown_budgeted(&mut || Ok(()))
    }
    /// Preserve canonical discovery policy while metering directory/entry work.
    /// The callback runs before each directory open and before inspecting an entry.
    pub fn scan_markdown_budgeted(
        &self,
        on_entry: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<VaultRelativePath>> {
        self.scan_markdown_limited(usize::MAX, on_entry)
    }
    /// Reject the next canonical file before retaining its path.
    pub fn scan_markdown_limited(
        &self,
        max_files: usize,
        on_entry: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<VaultRelativePath>> {
        self.scan_markdown_admitted(max_files, on_entry, &mut |_| Ok(()))
    }
    /// Maintenance can account for path bytes before the scan retains them.
    pub(crate) fn scan_markdown_admitted(
        &self,
        max_files: usize,
        on_entry: &mut dyn FnMut() -> Result<()>,
        on_path: &mut dyn FnMut(&VaultRelativePath) -> Result<()>,
    ) -> Result<Vec<VaultRelativePath>> {
        let retained = crate::storage::layout::active(self)?;
        self.scan_markdown_layout(max_files, on_entry, on_path, retained)
    }
    /// Reuse an operation's validated layout; retain all physical path checks.
    pub(crate) fn scan_markdown_with_layout(
        &self,
        max_files: usize,
        on_entry: &mut dyn FnMut() -> Result<()>,
        on_path: &mut dyn FnMut(&VaultRelativePath) -> Result<()>,
        layout: &crate::storage::layout::ValidatedLayout,
    ) -> Result<Vec<VaultRelativePath>> {
        self.scan_markdown_layout(max_files, on_entry, on_path, layout.retained(self)?)
    }
    fn scan_markdown_layout(
        &self,
        max_files: usize,
        on_entry: &mut dyn FnMut() -> Result<()>,
        on_path: &mut dyn FnMut(&VaultRelativePath) -> Result<()>,
        retained: bool,
    ) -> Result<Vec<VaultRelativePath>> {
        let mut out = Vec::new();
        Self::scan_dir(&self.path, "", &mut out, max_files, on_entry, on_path)?;
        if retained {
            // Old physical copies may survive interrupted unlink. Only the active
            // logical namespace supplies canonical identity after activation.
            out.retain(|path| crate::storage::layout::managed_path(path).is_none());
            for (physical, logical) in [
                (".wiki/retained/packets", "knowledge/extractions/packets"),
                (".wiki/retained/runs", "runs"),
            ] {
                let directory =
                    self.resolve_raw_budgeted(&VaultRelativePath::new(physical)?, on_entry)?;
                if directory.is_dir() {
                    Self::scan_dir(&directory, logical, &mut out, max_files, on_entry, on_path)?;
                }
            }
        }
        out.sort_by(|a, b| a.as_str().as_bytes().cmp(b.as_str().as_bytes()));
        Ok(out)
    }
    fn scan_dir(
        directory: &Path,
        prefix: &str,
        out: &mut Vec<VaultRelativePath>,
        max_files: usize,
        on_entry: &mut dyn FnMut() -> Result<()>,
        on_path: &mut dyn FnMut(&VaultRelativePath) -> Result<()>,
    ) -> Result<()> {
        on_entry()?;
        for entry in fs::read_dir(directory).map_err(|e| io_error("scan directory", e))? {
            on_entry()?;
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
                if exact_marker_budgeted(&entry.path(), on_entry)? {
                    continue;
                }
                Self::scan_dir(&entry.path(), &relative, out, max_files, on_entry, on_path)?;
            } else if kind.is_file() && name.ends_with(".md") && name != "index.md" {
                if out.len() >= max_files {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "canonical scan file ceiling",
                    ));
                }
                let path = VaultRelativePath::new(relative)?;
                on_path(&path)?;
                out.push(path);
            }
        }
        Ok(())
    }
}

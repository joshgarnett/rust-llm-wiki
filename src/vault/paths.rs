//! Canonical vault binding, portable paths, and deterministic envelope discovery.
use crate::domain::{ErrorCode, Result, VaultRelativePath, WikiError};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use unicase::UniCase;

/// All 39 ASCII decimal digits have exactly one inverse spelling under the
/// pinned Unicode fold implementation. This removes only alias enumeration;
/// normal component resolution, type, symlink and nested-vault checks still run.
fn numeric_source_component(component: &str) -> bool {
    component.len() == 39 && component.bytes().all(|byte| byte.is_ascii_digit())
}

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
                if value == ".wiki/retained/objects/blake3"
                    || value.starts_with(".wiki/retained/objects/blake3/") =>
            {
                "retained_objects"
            }
            Some(value)
                if value == "changes"
                    || value.starts_with("changes/")
                    || value == ".wiki/retained/changes"
                    || value.starts_with(".wiki/retained/changes/") =>
            {
                "retained_changes"
            }
            Some(value)
                if value == ".wiki/state/changes" || value.starts_with(".wiki/state/changes/") =>
            {
                "change_journals"
            }
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

fn canonical_containment(root: &Path, path: &Path) -> Result<()> {
    let canonical =
        fs::canonicalize(path).map_err(|e| io_error("check destination containment", e))?;
    if !canonical.starts_with(root) {
        return Err(WikiError::invalid("managed path escapes canonical vault"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn nofollow_containment_handle(path: &Path, original: &fs::Metadata) -> Option<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    // Do not open observed special files. A regular file/directory can become
    // a FIFO before open, so explicitly bound that lookup with O_NONBLOCK.
    // O_EVTONLY asks for metadata authority; File owns and closes the handle.
    if !original.is_file() && !original.is_dir() {
        return None;
    }
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_EVTONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW_ANY | libc::O_NONBLOCK)
        .open(path)
        .ok()
}

#[cfg(target_os = "macos")]
fn nofollow_contained(root: &Path, path: &Path, original: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    // VaultRoot binds an absolute canonical root once. The caller appends only
    // validated VaultRelativePath components (no empty, dot or parent parts).
    // Do not infer containment for arbitrary aliases supplied to this helper.
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    if !root.is_absolute()
        || relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return false;
    }
    let Some(handle) = nofollow_containment_handle(path, original) else {
        return false;
    };
    let Ok(opened) = handle.metadata() else {
        return false;
    };
    // The kernel lookup followed no symlink anywhere, and still names the
    // object observed by this prefix's lstat. Later path-use races retain the
    // existing contract; neither this check nor realpath locks ancestry.
    let type_mask = libc::S_IFMT as u32;
    opened.dev() == original.dev()
        && opened.ino() == original.ino()
        && opened.mode() & type_mask == original.mode() & type_mask
}

fn destination_containment(root: &Path, path: &Path, original: &fs::Metadata) -> Result<()> {
    #[cfg(target_os = "macos")]
    if nofollow_contained(root, path, original) {
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    let _ = original;
    // Unsupported flags, lookup/fstat errors, identity changes and special
    // types all retain the original check, error and inspection placement.
    // The fast helper's File has already dropped before this fallback.
    canonical_containment(root, path)
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
                    destination_containment(&self.path, &path, &meta)?;
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
                let singleton_source = scope.is_some_and(|scope| scope.singleton_source(&prefix))
                    && parent == self.path.join("sources")
                    && numeric_source_component(component);
                if !singleton_source
                    && !scope.is_some_and(|scope| scope.published(&prefix))
                    && parent.is_dir()
                {
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
            if scope.is_some_and(|scope| scope.published(&prefix))
                || scope.is_some_and(|scope| scope.singleton_source(&prefix))
                    && parent == "sources"
                    && numeric_source_component(component)
            {
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
        #[cfg(test)]
        crate::catalog::query_diagnostics::access("canonical_scan");
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

#[cfg(all(test, target_os = "macos"))]
mod containment_tests {
    use super::*;
    use std::os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::symlink},
    };

    fn fixture() -> (tempfile::TempDir, VaultRoot, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("root")).unwrap();
        fs::create_dir(temp.path().join("outside")).unwrap();
        let root = VaultRoot::for_initialization(temp.path().join("root")).unwrap();
        let outside = fs::canonicalize(temp.path().join("outside")).unwrap();
        (temp, root, outside)
    }

    fn parity(root: &VaultRoot, path: &Path, before: &fs::Metadata) -> Result<()> {
        let old = canonical_containment(root.path(), path);
        let new = destination_containment(root.path(), path, before);
        assert_eq!(
            old.as_ref().err().map(|e| (&e.code, &e.message)),
            new.as_ref().err().map(|e| (&e.code, &e.message))
        );
        new
    }

    #[test]
    fn matched_leaf_and_ancestor_replacement_refuse_before_marker_probe() {
        for ancestor in [false, true] {
            let (_temp, root, outside) = fixture();
            let parent = root.path().join("a");
            let leaf = if ancestor {
                parent.join("b")
            } else {
                parent.clone()
            };
            fs::create_dir_all(&leaf).unwrap();
            // Freeze the same observation boundary as the production loop:
            // lstat has succeeded; mutation precedes the guard's lookup.
            let before = fs::symlink_metadata(&leaf).unwrap();
            if ancestor {
                fs::rename(&parent, outside.join("moved")).unwrap();
                symlink(outside.join("moved"), &parent).unwrap();
                fs::write(outside.join("moved/b/WIKI.md"), b"outside").unwrap();
            } else {
                fs::remove_dir(&leaf).unwrap();
                symlink(&outside, &leaf).unwrap();
                fs::write(outside.join("WIKI.md"), b"outside").unwrap();
            }
            assert!(!nofollow_contained(root.path(), &leaf, &before));
            let mut marker_calls = 0;
            let error = parity(&root, &leaf, &before)
                .and_then(|()| {
                    exact_marker_budgeted(&leaf, &mut || {
                        marker_calls += 1;
                        Ok(())
                    })
                })
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::RecordInvalid);
            assert!(error.message.contains("escapes canonical vault"));
            assert_eq!(marker_calls, 0);
        }
    }

    #[test]
    fn introduced_inside_symlink_identity_change_and_disappearance_use_legacy() {
        let (_temp, root, _outside) = fixture();
        let path = root.path().join("prefix");
        let target = root.path().join("target");
        fs::create_dir(&path).unwrap();
        fs::create_dir(&target).unwrap();
        let before = fs::symlink_metadata(&path).unwrap();
        fs::remove_dir(&path).unwrap();
        symlink(&target, &path).unwrap();
        assert!(!nofollow_contained(root.path(), &path, &before));
        parity(&root, &path, &before).unwrap();
        fs::remove_file(&path).unwrap();
        let error = parity(&root, &path, &before).unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(error.message.starts_with("check destination containment:"));

        fs::write(&path, b"first").unwrap();
        let before = fs::symlink_metadata(&path).unwrap();
        // Keep the old object alive, preventing accidental inode reuse.
        fs::rename(&path, root.path().join("old-object")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(!nofollow_contained(root.path(), &path, &before));
        parity(&root, &path, &before).unwrap();
    }

    #[test]
    fn stale_regular_or_directory_metadata_cannot_block_on_fifo_replacement() {
        for directory in [false, true] {
            let (_temp, root, _outside) = fixture();
            let path = root.path().join("prefix");
            if directory {
                fs::create_dir(&path).unwrap();
            } else {
                fs::write(&path, b"original").unwrap();
            }
            let before = fs::symlink_metadata(&path).unwrap();
            // Hold the observed object at another name to prevent inode reuse.
            fs::rename(&path, root.path().join("held-original")).unwrap();
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: a NUL-terminated owned fixture path and valid mode bits.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let handle = nofollow_containment_handle(&path, &before).unwrap();
            // SAFETY: borrow a live File-owned descriptor, with no pointer arg.
            let flags = unsafe { libc::fcntl(handle.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0 && flags & libc::O_NONBLOCK != 0);
            drop(handle);
            assert!(!nofollow_contained(root.path(), &path, &before));
            parity(&root, &path, &before).unwrap();
        }
    }

    #[test]
    fn replaced_stored_canonical_root_refuses_before_marker_probe() {
        use std::os::unix::fs::MetadataExt;
        let (_temp, root, outside) = fixture();
        let path = root.path().join("prefix");
        fs::create_dir(&path).unwrap();
        let before = fs::symlink_metadata(&path).unwrap();
        let moved = outside.join("moved-root");
        fs::rename(root.path(), &moved).unwrap();
        symlink(&moved, root.path()).unwrap();
        fs::write(moved.join("prefix/WIKI.md"), b"outside").unwrap();
        // The leaf still has identical identity: ancestor no-follow, rather
        // than leaf fstat identity alone, must reject this root transition.
        let named = fs::symlink_metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (named.dev(), named.ino()));
        assert!(!nofollow_contained(root.path(), &path, &before));
        let mut marker_calls = 0;
        let error = parity(&root, &path, &before)
            .and_then(|()| {
                exact_marker_budgeted(&path, &mut || {
                    marker_calls += 1;
                    Ok(())
                })
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RecordInvalid);
        assert!(error.message.contains("escapes canonical vault"));
        assert_eq!(marker_calls, 0);
    }

    #[test]
    fn metadata_handles_are_cloexec_owned_and_special_types_skip_open() {
        let (_temp, root, _outside) = fixture();
        let file = root.path().join("file");
        fs::write(&file, b"data").unwrap();
        fs::hard_link(&file, root.path().join("hard-alias")).unwrap();
        let meta = fs::symlink_metadata(&file).unwrap();
        let handle = nofollow_containment_handle(&file, &meta).unwrap();
        // SAFETY: this borrows a live File-owned descriptor and passes no pointer.
        let flags = unsafe { libc::fcntl(handle.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
        // SAFETY: the same live owned descriptor, with no pointer argument.
        let status = unsafe { libc::fcntl(handle.as_raw_fd(), libc::F_GETFL) };
        assert!(status >= 0 && status & libc::O_NONBLOCK != 0);
        drop(handle); // File Drop closes ownership; no racy fd-number reuse oracle.
        assert!(nofollow_contained(root.path(), &file, &meta));

        let socket = root.path().join("socket");
        let _socket = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let meta = fs::symlink_metadata(&socket).unwrap();
        assert!(nofollow_containment_handle(&socket, &meta).is_none());
        parity(&root, &socket, &meta).unwrap();
    }

    #[test]
    fn firmlink_namespace_uses_legacy_oracle_and_noncanonical_alias_fallback() {
        let (_temp, root, _outside) = fixture();
        let path = root.path().join("file");
        fs::write(&path, b"own fixture").unwrap();
        let physical = Path::new("/System/Volumes/Data").join(path.strip_prefix("/").unwrap());
        let meta = fs::symlink_metadata(&physical).unwrap();
        assert!(!nofollow_contained(root.path(), &physical, &meta));
        // Some macOS namespaces retain the physical volume prefix. Neither
        // ALLOW nor REFUSE is hardcoded: preserve the actual legacy oracle.
        let _ = parity(&root, &physical, &meta);
    }
}

#[cfg(test)]
mod numeric_source_tests {
    use super::*;

    #[test]
    fn pinned_unicode_fold_has_no_other_inverse_for_decimal_names() {
        // Verify the actual compiled dependency, including multi-scalar folds.
        // No scalar may disappear or emit an ASCII digit except that digit.
        // Therefore concatenation cannot create another decimal spelling.
        for scalar in 0..=0x10ffff {
            let Some(ch) = char::from_u32(scalar) else {
                continue;
            };
            let input = ch.to_string();
            let folded = UniCase::unicode(input.as_str()).to_folded_case();
            assert!(!folded.is_empty(), "empty fold for U+{scalar:04X}");
            if folded.bytes().any(|byte| byte.is_ascii_digit()) {
                assert!(
                    ch.is_ascii_digit(),
                    "digit inverse U+{scalar:04X}: {folded:?}"
                );
                assert_eq!(folded, input);
            }
        }
        for _ in 0..32 {
            let id = crate::domain::RecordId::generate(crate::domain::RecordKind::Source).unwrap();
            assert!(numeric_source_component(id.as_str()));
            assert_eq!(UniCase::unicode(id.as_str()).to_folded_case(), id.as_str());
        }
        assert!(!numeric_source_component(
            "source_0000000000000000000000000000000000000"
        ));
        assert!(!numeric_source_component(
            "00000000000000000000000000000000000000"
        ));
    }
}

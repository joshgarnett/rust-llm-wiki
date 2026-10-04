//! Read-only canonical and projected source views. Payloads never become records.
use super::types::*;
use crate::{
    changes::{ReadDependency, ValidationInput},
    domain::{
        Blake3Hash, CanonicalRecord, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
    },
    records::{LinkResolution, ParsedNote, parse_note},
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};
use std::{fs::File, io::Read};

fn bounded_source_read(
    fs: &VaultFs,
    path: &VaultRelativePath,
    limit: usize,
) -> Result<Option<Vec<u8>>> {
    let path = fs.root().resolve(path)?;
    let io_error = |e: std::io::Error| {
        WikiError::new(ErrorCode::Internal, format!("bounded source read: {e}"))
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(e)),
    };
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() {
        return Err(integrity("source read target is not a regular file"));
    }
    if meta.len() > limit as u64 {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "source file exceeds read ceiling before allocation",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > limit {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "source file exceeds read ceiling",
        ));
    }
    Ok(Some(bytes))
}

pub(crate) fn integrity(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::SourceIntegrity, message)
}
pub(crate) fn record_bytes(record: CanonicalRecord, body: &[u8]) -> Result<Vec<u8>> {
    let mut bytes = b"---\n".to_vec();
    for (key, value) in record.fields() {
        bytes.extend_from_slice(key.as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend(serde_json::to_vec(value).map_err(|e| WikiError::invalid(e.to_string()))?);
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(b"---\n");
    bytes.extend_from_slice(body);
    Ok(bytes)
}
pub(crate) fn common(
    id: &RecordId,
    kind: RecordKind,
    title: &str,
) -> BTreeMap<String, serde_json::Value> {
    BTreeMap::from([
        ("wiki_schema".into(), "1".into()),
        ("wiki_id".into(), id.as_str().into()),
        ("wiki_kind".into(), kind.as_str().into()),
        ("title".into(), title.into()),
    ])
}
pub(crate) fn timestamp() -> Result<String> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| WikiError::invalid(e.to_string()))
}
pub(crate) fn canonical_path(path: &VaultRelativePath) -> bool {
    let p: Vec<_> = path.as_str().split('/').collect();
    if p.first()
        .is_some_and(|s| matches!(*s, ".wiki" | ".git" | "changes"))
    {
        return false;
    }
    let revision = p.len() >= 5
        && unicase::UniCase::unicode(p[0]).to_folded_case() == "sources"
        && unicase::UniCase::unicode(p[2]).to_folded_case() == "revisions";
    (!revision || (p.len() == 5 && p[4] == "revision.md"))
        && path.as_str().ends_with(".md")
        && p.last() != Some(&"index.md")
}
impl SourceStore {
    pub fn new(fs: VaultFs) -> Self {
        Self { fs }
    }
    pub fn fs(&self) -> &VaultFs {
        &self.fs
    }
    pub fn view(&self) -> Result<SourceView<'_>> {
        SourceView::from_fs(&self.fs)
    }
    pub fn view_with_overlay(&self, input: &ValidationInput) -> Result<SourceView<'_>> {
        SourceView::from_input(&self.fs, input)
    }
}
impl<'a> SourceView<'a> {
    /// Explicit maintenance shares the captured canonical notes and observes
    /// named assets through one caller-owned consistency boundary.
    pub(crate) fn from_shared_maintenance(
        fs: &'a VaultFs,
        notes: super::SourceNotes,
        reader: &'a dyn SourceInputReads,
    ) -> Self {
        Self {
            fs,
            notes,
            overlay: BTreeMap::new(),
            closed: false,
            observed_reads: Some(reader),
        }
    }
    pub(crate) fn require_consistent(&self) -> Result<()> {
        self.observed_reads
            .map_or(Ok(()), SourceInputReads::require_clean)
    }
    /// Bounded canonical input for packet/import work; source assets are read
    /// separately through revision_content_bounded with their complete hashes.
    pub fn from_fs_bounded(fs: &'a VaultFs, max_bytes: usize, max_files: usize) -> Result<Self> {
        if max_bytes == 0 || max_bytes > 64 * 1024 * 1024 || max_files == 0 || max_files > 4096 {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "source view bounds exceed their ceiling",
            ));
        }
        let mut entries = 0usize;
        let paths = fs.root().scan_markdown_limited(max_files, &mut || {
            if entries >= 65536 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "source view enumeration limit exceeded",
                ));
            }
            entries += 1;
            Ok(())
        })?;
        let mut notes = BTreeMap::new();
        let mut remaining = max_bytes;
        for (files, path) in paths.into_iter().filter(canonical_path).enumerate() {
            if files >= max_files {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "source view file limit exceeded",
                ));
            }
            if let Some(bytes) = bounded_source_read(fs, &path, remaining)? {
                remaining -= bytes.len();
                notes.insert(path, parse_note(&bytes));
            }
        }
        Ok(Self {
            fs,
            notes: notes.into(),
            overlay: BTreeMap::new(),
            closed: false,
            observed_reads: None,
        })
    }
    pub fn from_fs(fs: &'a VaultFs) -> Result<Self> {
        let mut notes = BTreeMap::new();
        for path in fs
            .root()
            .scan_markdown()?
            .into_iter()
            .filter(canonical_path)
        {
            if let Some(before) = fs.read_before(&path)? {
                notes.insert(path, parse_note(&before.bytes));
            }
        }
        Ok(SourceView {
            fs,
            notes: notes.into(),
            overlay: BTreeMap::new(),
            closed: false,
            observed_reads: None,
        })
    }
    pub fn from_input(fs: &'a VaultFs, input: &ValidationInput) -> Result<Self> {
        let mut notes = BTreeMap::new();
        let mut overlay = BTreeMap::new();
        let mut seen_documents = BTreeSet::new();
        for document in &input.documents {
            if !seen_documents.insert(&document.path) {
                return Err(WikiError::invalid("duplicate scan path"));
            }
            if Blake3Hash::digest(&document.bytes) != document.hash {
                return Err(integrity("scan document hash mismatch"));
            }
            if canonical_path(&document.path)
                && notes
                    .insert(document.path.clone(), parse_note(&document.bytes))
                    .is_some()
            {
                return Err(WikiError::invalid("duplicate scan path"));
            }
            if !canonical_path(&document.path) {
                overlay.insert(document.path.clone(), Some(document.bytes.clone()));
            }
        }
        let mut seen_overlay = BTreeSet::new();
        for target in &input.overlay {
            if !seen_overlay.insert(&target.path) {
                return Err(WikiError::invalid("duplicate overlay path"));
            }
            overlay.insert(target.path.clone(), target.bytes.clone());
            if canonical_path(&target.path) {
                match &target.bytes {
                    Some(bytes) => {
                        notes.insert(target.path.clone(), parse_note(bytes));
                    }
                    None => {
                        notes.remove(&target.path);
                    }
                }
            }
        }
        Ok(SourceView {
            fs,
            notes: notes.into(),
            overlay,
            closed: false,
            observed_reads: None,
        })
    }
    /// Source verification over a complete, caller-metered captured input only.
    pub fn from_closed_input(fs: &'a VaultFs, input: &ValidationInput) -> Result<Self> {
        let mut view = Self::from_input(fs, input)?;
        view.closed = true;
        Ok(view)
    }
    pub(crate) fn resolve(
        &self,
        id: &RecordId,
        kind: RecordKind,
        companion: Option<&str>,
    ) -> Result<(&VaultRelativePath, &ParsedNote)> {
        if self.notes.ambiguous(id) {
            return Err(WikiError::new(
                ErrorCode::ReferenceAmbiguous,
                format!("duplicate ID {id}, including malformed records"),
            ));
        }
        match self.notes.resolve_typed(id, kind, companion) {
            LinkResolution::Resolved { path, .. } => self
                .notes
                .get_key_value(&path)
                .ok_or_else(|| integrity("resolved note disappeared")),
            LinkResolution::Ambiguous { .. } => Err(WikiError::new(
                ErrorCode::ReferenceAmbiguous,
                format!("duplicate ID {id}"),
            )),
            LinkResolution::Missing => Err(WikiError::new(
                ErrorCode::RecordNotFound,
                format!("missing {kind} {id}"),
            )),
            _ => Err(integrity(format!(
                "{kind} {id} kind or companion path disagrees"
            ))),
        }
    }
    pub(crate) fn read(
        &self,
        path: &VaultRelativePath,
        dependencies: &mut BTreeMap<VaultRelativePath, ExpectedState>,
    ) -> Result<Vec<u8>> {
        self.read_limited(path, dependencies, None)
    }
    pub(crate) fn read_bounded(
        &self,
        path: &VaultRelativePath,
        dependencies: &mut BTreeMap<VaultRelativePath, ExpectedState>,
        limit: usize,
    ) -> Result<Vec<u8>> {
        if limit == 0 || limit > 64 * 1024 * 1024 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "invalid source asset read ceiling",
            ));
        }
        self.read_limited(path, dependencies, Some(limit))
    }
    fn read_limited(
        &self,
        path: &VaultRelativePath,
        dependencies: &mut BTreeMap<VaultRelativePath, ExpectedState>,
        limit: Option<usize>,
    ) -> Result<Vec<u8>> {
        // Captured source payloads share the engine's 64 MiB per-file ceiling.
        // Even ordinary integrity checks must enforce it before allocation.
        let limit = limit.unwrap_or(64 * 1024 * 1024);
        let bytes = if let Some(reader) = self.observed_reads {
            reader.read_observed(path, limit)?
        } else if let Some(value) = self.overlay.get(path) {
            if value.as_ref().is_some_and(|b| b.len() > limit) {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "source asset exceeds read ceiling",
                ));
            }
            value.clone().ok_or_else(|| {
                integrity(format!(
                    "{} payload {path}",
                    if self.closed { "missing" } else { "deleted" }
                ))
            })?
        } else if let Some(note) = self.notes.get(path) {
            if note.raw.len() > limit {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "source asset exceeds read ceiling",
                ));
            }
            note.raw.clone()
        } else if self.closed {
            return Err(integrity(format!(
                "payload absent from closed proof input: {path}"
            )));
        } else {
            bounded_source_read(self.fs, path, limit)?
                .ok_or_else(|| integrity(format!("missing payload {path}")))?
        };
        dependencies.insert(
            path.clone(),
            ExpectedState::Hash(Blake3Hash::digest(&bytes)),
        );
        Ok(bytes)
    }
    /// Closed proof callers supply explicit absence as well as all present assets.
    pub(crate) fn expected_state(&self, path: &VaultRelativePath) -> Result<ExpectedState> {
        if let Some(reader) = self.observed_reads {
            reader.state_observed(path)
        } else if let Some(bytes) = self.overlay.get(path) {
            if bytes
                .as_ref()
                .is_some_and(|bytes| bytes.len() > 64 * 1024 * 1024)
            {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "source asset exceeds read ceiling",
                ));
            }
            Ok(bytes.as_ref().map_or(ExpectedState::Absent, |bytes| {
                ExpectedState::Hash(Blake3Hash::digest(bytes))
            }))
        } else if self.closed {
            self.notes
                .get(path)
                .map(|note| ExpectedState::Hash(note.source_hash.clone()))
                .ok_or_else(|| integrity(format!("asset absent from closed proof input: {path}")))
        } else {
            Ok(bounded_source_read(self.fs, path, 64 * 1024 * 1024)?
                .map_or(ExpectedState::Absent, |bytes| {
                    ExpectedState::Hash(Blake3Hash::digest(bytes))
                }))
        }
    }
    pub(crate) fn note_dependency(
        path: &VaultRelativePath,
        note: &ParsedNote,
        dependencies: &mut BTreeMap<VaultRelativePath, ExpectedState>,
    ) {
        dependencies.insert(path.clone(), ExpectedState::Hash(note.source_hash.clone()));
    }
    pub(crate) fn revision_content(
        &self,
        source_id: &RecordId,
        revision_id: &RecordId,
        dependencies: &mut BTreeMap<VaultRelativePath, ExpectedState>,
    ) -> Result<Vec<u8>> {
        self.revision_content_inner(source_id, revision_id, dependencies, None)
    }
    pub(crate) fn revision_content_bounded(
        &self,
        source_id: &RecordId,
        revision_id: &RecordId,
        dependencies: &mut BTreeMap<VaultRelativePath, ExpectedState>,
        max_original_bytes: usize,
        max_content_bytes: usize,
    ) -> Result<Vec<u8>> {
        if max_original_bytes == 0
            || max_original_bytes > 64 * 1024 * 1024
            || max_content_bytes == 0
            || max_content_bytes > 64 * 1024 * 1024
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "source payload bounds exceed their ceiling",
            ));
        }
        self.revision_content_inner(
            source_id,
            revision_id,
            dependencies,
            Some((max_original_bytes, max_content_bytes)),
        )
    }
    fn revision_content_inner(
        &self,
        source_id: &RecordId,
        revision_id: &RecordId,
        dependencies: &mut BTreeMap<VaultRelativePath, ExpectedState>,
        limits: Option<(usize, usize)>,
    ) -> Result<Vec<u8>> {
        let (sp, sn) = self.resolve(source_id, RecordKind::Source, None)?;
        let source = sn.canonical.as_ref().expect("resolved canonical");
        Self::note_dependency(sp, sn, dependencies);
        let revisions = source
            .field("wiki_revisions")
            .and_then(serde_json::Value::as_array)
            .expect("validated list");
        let mut ids = BTreeSet::new();
        for value in revisions {
            if !ids.insert(value.as_str().expect("validated ID")) {
                return Err(integrity("source retains duplicate revision IDs"));
            }
        }
        if !ids.contains(revision_id.as_str())
            || !ids.contains(
                source
                    .string("wiki_current_revision")
                    .expect("validated head"),
            )
        {
            return Err(integrity("source revision is not retained in its manifest"));
        }
        let head_id = RecordId::new(
            source
                .string("wiki_current_revision")
                .expect("validated head"),
        )?;
        let (head_path, head_note) = self.resolve(
            &head_id,
            RecordKind::Revision,
            source.string("wiki_revision"),
        )?;
        if head_note
            .canonical
            .as_ref()
            .expect("resolved head")
            .string("wiki_source_id")
            != Some(source_id.as_str())
        {
            return Err(integrity("source head belongs to another source"));
        }
        Self::note_dependency(head_path, head_note, dependencies);
        let (rp, rn) = self.resolve(revision_id, RecordKind::Revision, None)?;
        let revision = rn.canonical.as_ref().expect("resolved canonical");
        Self::note_dependency(rp, rn, dependencies);
        if revision.string("wiki_source_id") != Some(source_id.as_str()) {
            return Err(integrity("revision belongs to another source"));
        }
        self.resolve(
            source_id,
            RecordKind::Source,
            revision.string("wiki_source"),
        )?;
        if source.string("wiki_current_revision") == Some(revision_id.as_str()) {
            self.resolve(
                revision_id,
                RecordKind::Revision,
                source.string("wiki_revision"),
            )?;
        }
        let parent = rp
            .as_str()
            .rsplit_once('/')
            .map(|v| v.0)
            .ok_or_else(|| integrity("revision has no directory"))?;
        let original_path = VaultRelativePath::new(format!(
            "{parent}/{}",
            revision
                .string("wiki_original_path")
                .expect("validated payload")
        ))?;
        let original = self.read_limited(&original_path, dependencies, limits.map(|n| n.0))?;
        if Blake3Hash::digest(&original).as_str()
            != revision
                .string("wiki_original_hash")
                .expect("validated hash")
        {
            return Err(integrity("complete original hash mismatch"));
        }
        if revision.string("wiki_extraction_status") != Some("complete") {
            return Err(integrity("revision has no supported extracted text"));
        }
        let content_path = VaultRelativePath::new(format!(
            "{parent}/{}",
            revision
                .string("wiki_content_path")
                .expect("complete content")
        ))?;
        let content = self.read_limited(&content_path, dependencies, limits.map(|n| n.1))?;
        if Blake3Hash::digest(&content).as_str()
            != revision.string("wiki_content_hash").expect("complete hash")
        {
            return Err(integrity("complete content hash mismatch"));
        }
        std::str::from_utf8(&content).map_err(|_| integrity("content snapshot is not UTF-8"))?;
        Ok(content)
    }
}
pub(crate) fn dependencies(
    values: BTreeMap<VaultRelativePath, ExpectedState>,
) -> Vec<ReadDependency> {
    values
        .into_iter()
        .map(|(path, expected)| ReadDependency { path, expected })
        .collect()
}

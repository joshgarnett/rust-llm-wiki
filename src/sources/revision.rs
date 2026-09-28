//! Read-only canonical and projected source views. Payloads never become records.
use super::types::*;
use crate::{
    changes::{ReadDependency, ValidationInput},
    domain::{
        Blake3Hash, CanonicalRecord, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
    },
    records::{LinkResolution, ParsedNote, RegistryEntry, parse_note, resolve_typed},
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

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
    pub fn from_fs(fs: &'a VaultFs) -> Result<Self> {
        let mut notes = BTreeMap::new();
        for path in fs.root().scan_markdown()? {
            if let Some(before) = fs.read_before(&path)? {
                notes.insert(path, parse_note(&before.bytes));
            }
        }
        Ok(SourceView {
            fs,
            notes,
            overlay: BTreeMap::new(),
            closed: false,
        })
    }
    pub fn from_input(fs: &'a VaultFs, input: &ValidationInput) -> Result<Self> {
        let mut notes = BTreeMap::new();
        for document in &input.documents {
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
        }
        let mut overlay = BTreeMap::new();
        for target in &input.overlay {
            if overlay
                .insert(target.path.clone(), target.bytes.clone())
                .is_some()
            {
                return Err(WikiError::invalid("duplicate overlay path"));
            }
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
            notes,
            overlay,
            closed: false,
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
        if self
            .notes
            .values()
            .filter(|note| {
                note.fields
                    .as_ref()
                    .and_then(|f| f.get("wiki_id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(id.as_str())
            })
            .count()
            > 1
        {
            return Err(WikiError::new(
                ErrorCode::ReferenceAmbiguous,
                format!("duplicate ID {id}, including malformed records"),
            ));
        }
        let registry: Vec<_> = self
            .notes
            .iter()
            .filter_map(|(path, note)| {
                note.canonical.as_ref().map(|record| RegistryEntry {
                    id: record.id().clone(),
                    kind: record.kind(),
                    path: path.clone(),
                    aliases: vec![],
                })
            })
            .collect();
        match resolve_typed(&registry, id, kind, companion) {
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
        let bytes = if let Some(value) = self.overlay.get(path) {
            value.clone().ok_or_else(|| {
                integrity(format!(
                    "{} payload {path}",
                    if self.closed { "missing" } else { "deleted" }
                ))
            })?
        } else if let Some(note) = self.notes.get(path) {
            note.raw.clone()
        } else if self.closed {
            return Err(integrity(format!(
                "payload absent from closed proof input: {path}"
            )));
        } else {
            self.fs
                .read_before(path)?
                .ok_or_else(|| integrity(format!("missing payload {path}")))?
                .bytes
        };
        dependencies.insert(
            path.clone(),
            ExpectedState::Hash(Blake3Hash::digest(&bytes)),
        );
        Ok(bytes)
    }
    /// Closed proof callers supply explicit absence as well as all present assets.
    pub(crate) fn expected_state(&self, path: &VaultRelativePath) -> Result<ExpectedState> {
        if let Some(bytes) = self.overlay.get(path) {
            Ok(bytes.as_ref().map_or(ExpectedState::Absent, |bytes| {
                ExpectedState::Hash(Blake3Hash::digest(bytes))
            }))
        } else if self.closed {
            self.notes
                .get(path)
                .map(|note| ExpectedState::Hash(note.source_hash.clone()))
                .ok_or_else(|| integrity(format!("asset absent from closed proof input: {path}")))
        } else {
            Ok(self
                .fs
                .read_before(path)?
                .map_or(ExpectedState::Absent, |before| {
                    ExpectedState::Hash(before.hash)
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
        let original = self.read(&original_path, dependencies)?;
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
        let content = self.read(&content_path, dependencies)?;
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

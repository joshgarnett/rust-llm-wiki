//! Local operations share the real changeset engine; dry-run uses pure plans only.
use crate::{
    changes::*,
    domain::*,
    records::{ParsedNote, RegistryEntry, edit_note, parse_note, resolve_untyped},
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

pub use super::types::{DEFAULT_READ_BYTES, MAX_INPUT_BYTES};

enum PageWriteInput {
    Ordinary(ChangeDraft),
    Inverse(crate::changes::rollback::ValidatedPageInverse),
}

impl PageWriteInput {
    fn draft(&self) -> &ChangeDraft {
        match self {
            Self::Ordinary(draft) => draft,
            Self::Inverse(inverse) => inverse.draft(),
        }
    }
}

fn usage(message: &str) -> WikiError {
    WikiError::new(ErrorCode::Usage, message)
}
/// Resolve a read only after selecting its body; explicit endpoints stay strict.
pub(crate) fn requested_read_range(
    body: &str,
    range: Option<ByteSpan>,
    from: Option<u64>,
) -> Result<ByteSpan> {
    if range.is_some() && from.is_some() {
        return Err(usage("read-from cannot also specify an exact byte range"));
    }
    let body_end =
        u64::try_from(body.len()).map_err(|_| usage("body length exceeds byte range limit"))?;
    let requested = match (range, from) {
        (Some(range), None) => range,
        (None, Some(start)) => {
            if start > body_end {
                return Err(usage(
                    "byte range must use valid UTF-8 boundaries within body",
                ));
            }
            ByteSpan::new(start, body_end)?
        }
        (None, None) => ByteSpan::new(0, body_end)?,
        (Some(_), Some(_)) => unreachable!("conflicting read bounds rejected above"),
    };
    let start = usize::try_from(requested.start())
        .map_err(|_| usage("byte range exceeds platform limit"))?;
    let end =
        usize::try_from(requested.end()).map_err(|_| usage("byte range exceeds platform limit"))?;
    if end > body.len() || !body.is_char_boundary(start) || !body.is_char_boundary(end) {
        return Err(usage(
            "byte range must use valid UTF-8 boundaries within body",
        ));
    }
    Ok(requested)
}
pub(crate) fn bounded_utf8_end(
    text: &str,
    start: usize,
    wanted_end: usize,
    max_bytes: usize,
) -> Result<usize> {
    let mut end = wanted_end.min(start.saturating_add(max_bytes));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == start && wanted_end > start {
        let minimum = text[start..wanted_end]
            .chars()
            .next()
            .expect("nonempty UTF-8 range")
            .len_utf8();
        let mut error = usage(&format!(
            "read byte allowance is too small for the next UTF-8 character; use --max-bytes {minimum} or larger"
        ));
        error.details = serde_json::json!({"minimum_max_bytes": minimum, "start": start});
        return Err(error);
    }
    Ok(end)
}
pub(crate) fn page_envelope_error(note: &ParsedNote) -> WikiError {
    let required = [
        "wiki_schema",
        "wiki_id",
        "wiki_kind",
        "title",
        "wiki_status",
    ];
    let missing: Vec<_> = required
        .into_iter()
        .filter(|field| {
            note.fields
                .as_ref()
                .is_none_or(|fields| !fields.contains_key(*field))
        })
        .collect();
    let template = "---\nwiki_schema: \"1\"\nwiki_id: Page.Example\nwiki_kind: page\ntitle: Example\nwiki_status: draft\n---\n# Example\n\nWrite cited knowledge here.\n";
    let mut error = WikiError::invalid(
        "page put requires a valid page envelope; inspect `lwiki schema page` (also `schema record`) and add wiki_schema, wiki_id, wiki_kind: page, title, and wiki_status",
    );
    error.details = serde_json::json!({"missing_fields":missing,"validation":note.diagnostics,"next_action":"lwiki schema page","template":template});
    error
}
fn bounded(bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "input exceeds 16 MiB ceiling",
        ));
    }
    Ok(())
}
fn empty_draft(title: String, operations: Vec<ExpectedWrite>) -> ChangeDraft {
    ChangeDraft {
        title,
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations,
    }
}
fn read_bytes(fs: &VaultFs, path: &VaultRelativePath) -> Result<Vec<u8>> {
    crate::changes::prepare::read_bounded(fs, path, MAX_INPUT_BYTES)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecordNotFound,
            format!("record not found: {path}"),
        )
    })
}
fn rewrite_links(
    body: &str,
    registry: &[RegistryEntry],
    target: &RecordId,
    to: &VaultRelativePath,
) -> Result<Vec<u8>> {
    crate::records::link_rewrite::rewrite_links_selected(
        body,
        &mut |destination| Ok(resolve_untyped(registry, destination)),
        target,
        to,
    )
}
fn rewrite_companions(
    note: &ParsedNote,
    registry: &[RegistryEntry],
    target: &RecordId,
    to: &VaultRelativePath,
) -> Result<BTreeMap<String, serde_json::Value>> {
    crate::records::link_rewrite::rewrite_companions_selected(
        note,
        &mut |destination| Ok(resolve_untyped(registry, destination)),
        target,
        to,
    )
}

use super::types::*;
use crate::{
    catalog::{Catalog, CatalogGraphValidator, CatalogProjection, scan},
    sources::{CaptureRequest, SourcePlan, SourceStore},
    vault::WriterPermit,
};
use std::time::Duration;
impl OfflineApp {
    pub fn new(fs: VaultFs, options: OperationOptions) -> Result<Self> {
        if options.lock_timeout_ms > 30_000 {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "writer lock timeout exceeds 30 seconds",
            ));
        }
        let engine = ChangeEngine::new(fs.clone())?;
        Ok(Self {
            fs,
            vault_id: engine.vault_id().clone(),
            options,
        })
    }
    pub fn fs(&self) -> &VaultFs {
        &self.fs
    }
    pub fn vault_id(&self) -> &RecordId {
        &self.vault_id
    }
    pub fn options(&self) -> &OperationOptions {
        &self.options
    }
    pub(super) fn engine(&self) -> Result<ChangeEngine> {
        ChangeEngine::new(self.fs.clone())
    }
    pub(super) fn catalog(&self) -> Catalog {
        Catalog::with_options(
            self.fs.clone(),
            self.vault_id.clone(),
            crate::catalog::CatalogOptions {
                busy_timeout_ms: self.options.lock_timeout_ms,
                ..Default::default()
            },
        )
    }
    pub(super) fn writer(&self) -> Result<WriterPermit> {
        WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(self.options.lock_timeout_ms),
        )
    }
    fn current_projection(&self) -> Result<CatalogProjection> {
        self.catalog().guard_current(None)?;
        scan::scan(&self.fs, &self.vault_id)
    }
    fn resolve_path(
        &self,
        selector: &RecordSelector,
        p: &CatalogProjection,
    ) -> Result<VaultRelativePath> {
        match selector {
            RecordSelector::Path(path) => {
                self.fs.root().resolve(path)?;
                Ok(path.clone())
            }
            RecordSelector::Id(id) => {
                if p.diagnostics.iter().any(|d| {
                    d.record_id.as_ref() == Some(id) && d.code == ErrorCode::ReferenceAmbiguous
                }) {
                    return Err(WikiError::new(
                        ErrorCode::ReferenceAmbiguous,
                        format!("duplicate canonical ID: {id}"),
                    ));
                }
                if let Some(row) = p.records.get(id) {
                    return Ok(row.path.clone());
                }
                let mut candidates = BTreeSet::new();
                for document in p.documents.iter().filter(|d| {
                    d.owner_revision.is_none() && crate::sources::revision::canonical_path(&d.path)
                }) {
                    let note = parse_note(&read_bytes(&self.fs, &document.path)?);
                    if note.status == crate::records::ParseStatus::UnsupportedSchema
                        && note
                            .fields
                            .as_ref()
                            .and_then(|f| f.get("wiki_id"))
                            .and_then(serde_json::Value::as_str)
                            == Some(id.as_str())
                    {
                        candidates.insert(document.path.clone());
                    }
                }
                if candidates.len() > 1 {
                    return Err(WikiError::new(
                        ErrorCode::ReferenceAmbiguous,
                        format!("duplicate canonical ID: {id}"),
                    ));
                }
                candidates.into_iter().next().ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::RecordNotFound,
                        format!("record ID not found: {id}"),
                    )
                })
            }
        }
    }
    pub fn read(&self, request: ReadRequest) -> Result<ReadOutcome> {
        self.read_selected(request, None)
    }
    /// Read forward from a body byte offset to its EOF under the same byte cap.
    /// The request must not also contain an exact range.
    pub fn read_from(&self, request: ReadRequest, start: u64) -> Result<ReadOutcome> {
        if request.range.is_some() {
            return Err(usage("read-from cannot also specify an exact byte range"));
        }
        self.read_selected(request, Some(start))
    }
    fn read_selected(&self, request: ReadRequest, from: Option<u64>) -> Result<ReadOutcome> {
        if request.max_bytes == 0 || request.max_bytes > MAX_INPUT_BYTES {
            return Err(usage("read limit must be 1..=16 MiB"));
        }
        let p = self.current_projection()?;
        let path = self.resolve_path(&request.selector, &p)?;
        let document = p.documents.iter().find(|d| d.path == path).ok_or_else(|| {
            WikiError::new(
                ErrorCode::RecordNotFound,
                "path must name a visible canonical note or captured content passage",
            )
        })?;
        let bytes = read_bytes(&self.fs, &path)?;
        if Blake3Hash::digest(&bytes) != document.hash {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "record bytes changed after canonical scan",
            ));
        }
        let note = parse_note(&bytes);
        let canonical = crate::sources::revision::canonical_path(&path);
        let body = if canonical {
            note.body()
        } else {
            bytes.as_slice()
        };
        let text = std::str::from_utf8(body)
            .map_err(|_| WikiError::invalid("requested body is not UTF-8"))?;
        let requested = requested_read_range(text, request.range, from)?;
        let start = usize::try_from(requested.start())
            .map_err(|_| usage("byte range exceeds platform limit"))?;
        let wanted_end = usize::try_from(requested.end())
            .map_err(|_| usage("byte range exceeds platform limit"))?;
        let end = bounded_utf8_end(text, start, wanted_end, request.max_bytes)?;
        let record = if canonical {
            p.records
                .values()
                .find(|row| row.path == path)
                .map(|row| row.record.clone())
        } else {
            None
        };
        Ok(ReadOutcome {
            continuation: if end < wanted_end {
                Some(ByteSpan::new(end as u64, wanted_end as u64)?)
            } else {
                None
            },
            path: path.clone(),
            hash: Blake3Hash::digest(&bytes),
            record,
            metadata: if canonical { note.fields.clone() } else { None },
            body: text[start..end].to_owned(),
            range: ByteSpan::new(start as u64, end as u64)?,
            truncated: end < wanted_end,
            source_citation: None,
            diagnostics: p
                .diagnostics
                .into_iter()
                .filter(|d| d.path == path)
                .collect(),
        })
    }
    pub(crate) fn execute_draft(
        &self,
        draft: ChangeDraft,
        force_stage: bool,
    ) -> Result<MutationOutcome> {
        self.catalog().guard_current(None)?;
        let engine = self.engine()?;
        let plan = engine.plan(&draft)?;
        engine.validate_draft_graph(&draft, &CatalogGraphValidator)?;
        let summary = summarize(&draft.title, &plan.read_preconditions, &plan.operations);
        let allocated_ids = draft.allocated_ids.clone();
        if self.options.dry_run || plan.operations.is_empty() {
            return Ok(MutationOutcome {
                source_capture: None,
                plan: summary,
                allocated_ids,
                change: None,
                status: None,
                snapshot: None,
                reused: plan.operations.is_empty(),
            });
        }
        let w = self.writer()?;
        let retained = engine.prepare(&w, draft)?;
        if force_stage || self.options.stage_only {
            return Ok(MutationOutcome {
                source_capture: None,
                plan: summary,
                allocated_ids,
                change: Some(retained.prepared),
                status: Some(retained.status),
                snapshot: None,
                reused: false,
            });
        }
        let applied = engine
            .apply(
                &w,
                &retained.prepared,
                &CatalogGraphValidator,
                &self.catalog(),
            )
            .map_err(|error| retained_error(error, &retained.prepared))?;
        Ok(MutationOutcome {
            source_capture: None,
            plan: summary,
            allocated_ids,
            change: Some(applied.change),
            status: Some(applied.status),
            snapshot: applied.snapshot,
            reused: false,
        })
    }
    pub(crate) fn plan_page_write(
        &self,
        path: VaultRelativePath,
        bytes: Vec<u8>,
        if_match: Option<Blake3Hash>,
    ) -> Result<ExpectedWrite> {
        bounded(&bytes)?;
        if !crate::sources::revision::canonical_path(&path) {
            return Err(WikiError::invalid(
                "page target must be a canonical .md record path; index.md is reserved for generated navigation (use pages/overview.md for an authored landing page)",
            ));
        }
        let new = parse_note(&bytes);
        let record = new
            .canonical
            .as_ref()
            .filter(|r| r.kind() == RecordKind::Page)
            .ok_or_else(|| page_envelope_error(&new))?;
        let before = crate::changes::prepare::read_bounded(&self.fs, &path, MAX_INPUT_BYTES)?;
        let expected = match (before, if_match) {
            (Some(old), Some(hash)) => {
                if Blake3Hash::digest(&old) != hash {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "page author hash differs from current bytes",
                    ));
                }
                let old_note = parse_note(&old);
                let old_record = old_note
                    .canonical
                    .as_ref()
                    .filter(|r| r.kind() == RecordKind::Page && r.id() == record.id())
                    .ok_or_else(|| {
                        WikiError::invalid(
                            "replacement must preserve existing page identity and kind",
                        )
                    })?;
                let _ = old_record;
                ExpectedState::Hash(hash)
            }
            (Some(_), None) => {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "existing page replacement requires if-match hash",
                ));
            }
            (None, Some(_)) => {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "if-match replacement target is absent",
                ));
            }
            (None, None) => ExpectedState::Absent,
        };
        Ok(ExpectedWrite {
            target: path,
            expected,
            proposed: Some(bytes),
            apply_after: vec![],
        })
    }
    pub fn page_put(
        &self,
        path: VaultRelativePath,
        bytes: Vec<u8>,
        if_match: Option<Blake3Hash>,
    ) -> Result<MutationOutcome> {
        let operation = self.plan_page_write(path, bytes, if_match)?;
        self.execute_page_draft(empty_draft("Put page".into(), vec![operation]))
    }
    pub(crate) fn default_page_path(&self, id: &RecordId) -> Result<VaultRelativePath> {
        use crate::catalog::query_types::QueryCatalog;
        let catalog = self.catalog();
        let existing = if catalog.operation_state()?.is_some() {
            catalog.guard_query()?;
            catalog
                .query_snapshot(crate::catalog::query_types::QueryReadLimits::default())?
                .record(id)?
                .map(|row| row.path)
        } else {
            crate::catalog::scan::scan(&self.fs, &self.vault_id)?
                .records
                .get(id)
                .map(|row| row.path.clone())
        };
        existing
            .map(Ok)
            .unwrap_or_else(|| VaultRelativePath::new(format!("pages/{id}.md")))
    }
    /// Page writes use selected admission on a normalized publication. Other
    /// mutation kinds retain their own validation and authority boundaries.
    pub(crate) fn execute_page_draft(&self, draft: ChangeDraft) -> Result<MutationOutcome> {
        self.execute_page_input(PageWriteInput::Ordinary(draft), false)
    }

    fn execute_page_input(
        &self,
        input: PageWriteInput,
        force_stage: bool,
    ) -> Result<MutationOutcome> {
        use crate::catalog::{
            query_types::QueryReadLimits,
            source_projection::RefreshProjectionLimits,
            source_refresh::IndexedRefreshSession,
            write_projection::{project_page_inverse, project_pages},
        };
        let catalog = self.catalog();
        if catalog.operation_state()?.is_none() {
            return match input {
                PageWriteInput::Ordinary(draft) => self.execute_draft(draft, force_stage),
                PageWriteInput::Inverse(_) => Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "normalized Page inverse lost its catalog authority",
                )),
            };
        }
        let draft = input.draft();
        let mut outcome = MutationOutcome {
            source_capture: None,
            plan: summarize(&draft.title, &draft.read_preconditions, &draft.operations),
            allocated_ids: draft.allocated_ids.clone(),
            change: None,
            status: None,
            snapshot: None,
            reused: false,
        };
        // WAL readers can change shared-memory coordination even when opened
        // read-only. A preview checks explicit file guards but leaves indexed
        // admission unknown instead of opening SQLite or claiming a sealed plan.
        if self.options.dry_run {
            return Ok(outcome);
        }
        catalog.guard_query()?;
        let reader = catalog.query_snapshot(QueryReadLimits::default())?;
        let limits = RefreshProjectionLimits::default();
        let projected = match input {
            PageWriteInput::Ordinary(draft) => project_pages(&self.fs, &reader, draft, &limits)?,
            PageWriteInput::Inverse(inverse) => {
                project_page_inverse(&self.fs, &reader, inverse, &limits)?
            }
        };
        let Some(projected) = projected else {
            outcome.plan.operations.clear();
            outcome.reused = true;
            return Ok(outcome);
        };
        // The owned projection pins its base; publication acquires and checks
        // its own reader. Release this planner before the WAL checkpoint.
        drop(reader);
        let draft = projected.draft();
        outcome.plan = summarize(&draft.title, &draft.read_preconditions, &draft.operations);
        let writer = self.writer()?;
        catalog.guard_current(None)?;
        let mut session = IndexedRefreshSession::prepare_write(&catalog, &writer, projected)?;
        let change = session.proof().change.clone();
        outcome.change = Some(change.clone());
        if force_stage || self.options.stage_only {
            outcome.status = Some(ChangeStatus::Prepared);
            return Ok(outcome);
        }
        let report = self
            .engine()?
            .apply_indexed_refresh(&writer, &mut session)
            .map_err(|error| retained_error(error, &change))?;
        outcome.status = Some(report.status);
        outcome.snapshot = report.snapshot;
        Ok(outcome)
    }
    pub fn page_rename(
        &self,
        id: RecordId,
        to: VaultRelativePath,
        hash: Blake3Hash,
    ) -> Result<MutationOutcome> {
        if self.catalog().operation_state()?.is_some() {
            return self.page_rename_indexed(id, to, hash);
        }
        if !crate::sources::revision::canonical_path(&to) {
            return Err(WikiError::invalid(
                "rename destination must be canonical Markdown path",
            ));
        }
        let p = self.current_projection()?;
        let from = self.resolve_path(&RecordSelector::Id(id.clone()), &p)?;
        let row = p.records.get(&id).ok_or_else(|| {
            WikiError::invalid("rename requires a uniquely adopted supported record")
        })?;
        if !matches!(
            row.record.kind(),
            RecordKind::Page
                | RecordKind::Entity
                | RecordKind::Assertion
                | RecordKind::Evidence
                | RecordKind::Extraction
                | RecordKind::Decision
        ) {
            return Err(WikiError::invalid(
                "record ownership or immutability prevents page rename",
            ));
        }
        let original = read_bytes(&self.fs, &from)?;
        if Blake3Hash::digest(&original) != hash {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "rename source hash differs",
            ));
        }
        if from == to {
            return self.execute_draft(empty_draft("Rename unchanged path".into(), vec![]), false);
        }
        self.fs
            .root()
            .validate_portable_paths(&[from.clone(), to.clone()])?;
        if self.fs.read_before(&to)?.is_some() {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "rename destination exists",
            ));
        }
        let registry: Vec<_> = p
            .records
            .values()
            .map(|r| RegistryEntry {
                id: r.record.id().clone(),
                kind: r.record.kind(),
                path: r.path.clone(),
                aliases: r
                    .record
                    .field("aliases")
                    .and_then(|v| v.as_array())
                    .map(|v| {
                        v.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        let mut operations = Vec::new();
        let mut destination_bytes = original.clone();
        for doc in p.documents.iter().filter(|d| d.owner_revision.is_none()) {
            let bytes = read_bytes(&self.fs, &doc.path)?;
            let note = parse_note(&bytes);
            let Ok(body) = std::str::from_utf8(note.body()) else {
                continue;
            };
            let changed_body = rewrite_links(body, &registry, &id, &to)?;
            let fields = rewrite_companions(&note, &registry, &id, &to)?;
            if changed_body == note.body() && fields.is_empty() {
                continue;
            }
            let changed = if note.canonical.is_some() {
                edit_note(
                    &note,
                    &fields,
                    if changed_body != note.body() {
                        Some(changed_body.as_slice())
                    } else {
                        None
                    },
                    &note.source_hash,
                )?
            } else if fields.is_empty() {
                let mut out = note.raw[..note.raw.len() - note.body().len()].to_vec();
                out.extend(changed_body);
                out
            } else {
                return Err(WikiError::invalid("cannot safely edit incoming companion"));
            };
            if doc.path == from {
                destination_bytes = changed;
            } else {
                operations.push(ExpectedWrite {
                    target: doc.path.clone(),
                    expected: ExpectedState::Hash(note.source_hash),
                    proposed: Some(changed),
                    apply_after: vec![to.clone()],
                });
            }
        }
        destination_bytes =
            super::page_citations::rebase_source_citation_links(&destination_bytes, &from, &to)?;
        operations.push(ExpectedWrite {
            target: to.clone(),
            expected: ExpectedState::Absent,
            proposed: Some(destination_bytes),
            apply_after: vec![],
        });
        operations.push(ExpectedWrite {
            target: from.clone(),
            expected: ExpectedState::Hash(hash),
            proposed: None,
            apply_after: vec![to.clone()],
        });
        let mut draft = empty_draft(format!("Rename {id} to {to}"), operations);
        let writes: BTreeSet<_> = draft.operations.iter().map(|o| o.target.clone()).collect();
        draft.read_preconditions = p
            .dependencies
            .into_iter()
            .filter(|d| !writes.contains(&d.path))
            .collect();
        self.execute_draft(draft, false)
    }
    fn source_plan(&self, plan: SourcePlan) -> Result<MutationOutcome> {
        let mut draft = plan
            .draft
            .unwrap_or_else(|| empty_draft("Unchanged source".into(), vec![]));
        if draft.operations.is_empty() {
            draft.read_preconditions = plan.dependencies;
        }
        let mut outcome = self.execute_draft(draft, false)?;
        outcome
            .allocated_ids
            .entry("source".into())
            .or_insert(plan.source_id);
        outcome
            .allocated_ids
            .entry("revision".into())
            .or_insert(plan.revision_id);
        outcome.reused |= plan.reused;
        outcome.source_capture = plan.capture_state;
        Ok(outcome)
    }
    pub fn source_add(&self, request: CaptureRequest) -> Result<MutationOutcome> {
        capture_bounds(&request)?;
        let catalog = self.catalog();
        if catalog.operation_state()?.is_some() {
            use crate::{
                catalog::{
                    capture_projection::project_capture, query_types::QueryReadLimits,
                    source_projection::RefreshProjectionLimits,
                },
                sources::SourceRefreshLookup,
            };
            let store = SourceStore::new(self.fs.clone());
            // Preview allocates a proposed pair, but does not reserve it or open
            // SQLite. Applying the request performs fresh allocation/admission.
            if self.options.dry_run {
                let plan = store.plan_capture(request)?;
                let draft = plan.draft.as_ref().expect("capture draft");
                return Ok(MutationOutcome {
                    source_capture: plan.capture_state,
                    plan: summarize(&draft.title, &draft.read_preconditions, &draft.operations),
                    allocated_ids: draft.allocated_ids.clone(),
                    change: None,
                    status: None,
                    snapshot: None,
                    reused: false,
                });
            }
            catalog.guard_query()?;
            let reader = catalog.query_snapshot(QueryReadLimits::default())?;
            reader.require_policy_layout()?;
            for _ in 0..16 {
                let plan = store.plan_capture(request.clone())?;
                let root = VaultRelativePath::new(format!("sources/{}", plan.source_id))?;
                let physical = self.fs.root().resolve(&root)?;
                // Do not adopt even an empty, externally occupied directory.
                match std::fs::symlink_metadata(physical) {
                    Ok(_) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(WikiError::new(
                            ErrorCode::Internal,
                            format!("inspect source allocation: {error}"),
                        ));
                    }
                }
                let source_path = VaultRelativePath::new(format!("{root}/source.md"))?;
                let revision_path = VaultRelativePath::new(format!(
                    "{root}/revisions/{}/revision.md",
                    plan.revision_id
                ))?;
                if reader.revision_identity_is_reserved(&plan.source_id, &source_path)?
                    || reader.revision_identity_is_reserved(&plan.revision_id, &revision_path)?
                {
                    continue;
                }
                let capture = plan.capture_state;
                let projected =
                    project_capture(&self.fs, &reader, plan, &RefreshProjectionLimits::default())?;
                drop(reader);
                return self.publish_source_write(&catalog, projected, capture);
            }
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "could not reserve a fresh source and revision identity after 16 attempts",
            ));
        }
        self.source_plan(SourceStore::new(self.fs.clone()).plan_capture(request)?)
    }
    pub(super) fn publish_source_write(
        &self,
        catalog: &Catalog,
        projected: crate::catalog::write_projection::ProjectedWrite,
        capture: Option<crate::sources::SourceCaptureState>,
    ) -> Result<MutationOutcome> {
        let draft = projected.draft();
        let mut outcome = MutationOutcome {
            source_capture: capture,
            plan: summarize(&draft.title, &draft.read_preconditions, &draft.operations),
            allocated_ids: draft.allocated_ids.clone(),
            change: None,
            status: None,
            snapshot: None,
            reused: false,
        };
        let writer = self.writer()?;
        catalog.guard_current(None)?;
        let mut session = crate::catalog::source_refresh::IndexedRefreshSession::prepare_write(
            catalog, &writer, projected,
        )?;
        let change = session.proof().change.clone();
        outcome.change = Some(change.clone());
        if self.options.stage_only {
            outcome.status = Some(ChangeStatus::Prepared);
            return Ok(outcome);
        }
        let report = self
            .engine()?
            .apply_indexed_refresh(&writer, &mut session)
            .map_err(|error| retained_error(error, &change))?;
        outcome.status = Some(report.status);
        outcome.snapshot = report.snapshot;
        Ok(outcome)
    }
    pub fn source_refresh(&self, id: RecordId, request: CaptureRequest) -> Result<MutationOutcome> {
        self.source_refresh_with_title(id, request, None)
    }
    pub fn source_refresh_with_title(
        &self,
        id: RecordId,
        request: CaptureRequest,
        title: Option<&str>,
    ) -> Result<MutationOutcome> {
        capture_bounds(&request)?;
        let catalog = self.catalog();
        if catalog.operation_state()?.is_some() {
            if self.options.dry_run {
                // Selected target/head/reuse require indexed facts. Leave them
                // unresolved in a pure preview, with no shared-memory changes.
                let _ = SourceStore::new(self.fs.clone()).plan_capture(request)?;
                return Ok(MutationOutcome {
                    source_capture: None,
                    plan: summarize("Refresh source (target unresolved in preview)", &[], &[]),
                    allocated_ids: BTreeMap::from([("source".into(), id)]),
                    change: None,
                    status: None,
                    snapshot: None,
                    reused: false,
                });
            }
            return self.indexed_source_refresh(&catalog, id, request, title);
        }
        let p = self.current_projection()?;
        self.resolve_path(&RecordSelector::Id(id.clone()), &p)?;
        self.source_plan(
            SourceStore::new(self.fs.clone()).plan_refresh_with_title(&id, request, title)?,
        )
    }
    fn indexed_source_refresh(
        &self,
        catalog: &Catalog,
        id: RecordId,
        request: CaptureRequest,
        title: Option<&str>,
    ) -> Result<MutationOutcome> {
        use crate::catalog::{
            query_types::QueryReadLimits,
            source_projection::{RefreshProjectionLimits, project_refresh},
            source_refresh::IndexedRefreshSession,
        };
        #[cfg(test)]
        let profile_acquire = super::refresh_path_profile::phase("acquire");
        catalog.guard_query()?;
        let reader = catalog.query_snapshot(QueryReadLimits::default())?;
        reader.require_fact_layout()?;
        #[cfg(test)]
        drop(profile_acquire);
        #[cfg(test)]
        let profile_plan = super::refresh_path_profile::phase("plan");
        let plan = SourceStore::new(self.fs.clone()).plan_refresh_indexed(
            &reader,
            &id,
            request,
            title,
            &crate::sources::SourceRefreshLimits::default(),
        )?;
        #[cfg(test)]
        drop(profile_plan);
        let mut outcome = MutationOutcome {
            source_capture: plan.plan.capture_state.clone(),
            plan: summarize("Unchanged source", &plan.plan.dependencies, &[]),
            allocated_ids: BTreeMap::from([
                ("source".into(), plan.plan.source_id.clone()),
                ("revision".into(), plan.plan.revision_id.clone()),
            ]),
            change: None,
            status: None,
            snapshot: None,
            reused: plan.plan.reused,
        };
        #[cfg(test)]
        let profile_project = super::refresh_path_profile::phase("project");
        let Some(projected) =
            project_refresh(&self.fs, &reader, plan, &RefreshProjectionLimits::default())?
        else {
            return Ok(outcome);
        };
        #[cfg(test)]
        drop(profile_project);
        drop(reader);
        let draft = projected.draft();
        outcome.plan = summarize(&draft.title, &draft.read_preconditions, &draft.operations);
        if self.options.dry_run {
            return Ok(outcome);
        }
        #[cfg(test)]
        let profile_prepare = super::refresh_path_profile::phase("prepare");
        let writer = self.writer()?;
        catalog.guard_current(None)?;
        // The sealed projection retains its pinned base. Preparation refuses a
        // concurrent publication rather than replanning an already reviewed draft.
        let mut session = IndexedRefreshSession::prepare_projected(catalog, &writer, projected)?;
        #[cfg(test)]
        drop(profile_prepare);
        let change = session.proof().change.clone();
        outcome.change = Some(change.clone());
        if self.options.stage_only {
            outcome.status = Some(ChangeStatus::Prepared);
            return Ok(outcome);
        }
        #[cfg(test)]
        let profile_apply = super::refresh_path_profile::phase("apply_including_publish");
        let report = self
            .engine()?
            .apply_indexed_refresh(&writer, &mut session)
            .map_err(|error| retained_error(error, &change))?;
        #[cfg(test)]
        drop(profile_apply);
        outcome.status = Some(report.status);
        outcome.snapshot = report.snapshot;
        Ok(outcome)
    }
    pub fn source_withdraw(&self, id: RecordId, reason: &str) -> Result<MutationOutcome> {
        if reason.trim().is_empty() || reason.len() > 4096 {
            return Err(WikiError::invalid(
                "withdrawal requires a nonempty reason of at most 4096 bytes",
            ));
        }
        let catalog = self.catalog();
        if catalog.operation_state()?.is_some() {
            if self.options.dry_run {
                return Ok(MutationOutcome {
                    source_capture: None,
                    plan: summarize("Withdraw source (target unresolved in preview)", &[], &[]),
                    allocated_ids: BTreeMap::from([("source".into(), id)]),
                    change: None,
                    status: None,
                    snapshot: None,
                    reused: false,
                });
            }
            catalog.guard_query()?;
            let reader =
                catalog.query_snapshot(crate::catalog::query_types::QueryReadLimits::default())?;
            let projected = crate::catalog::withdraw_projection::project_withdraw(
                &self.fs,
                &reader,
                &id,
                reason,
                &crate::catalog::source_projection::RefreshProjectionLimits::default(),
            )?;
            drop(reader);
            return match projected {
                Some(projected) => self.publish_source_write(&catalog, projected, None),
                None => Ok(MutationOutcome {
                    source_capture: None,
                    plan: summarize("Unchanged withdrawn source", &[], &[]),
                    allocated_ids: BTreeMap::from([("source".into(), id)]),
                    change: None,
                    status: None,
                    snapshot: None,
                    reused: true,
                }),
            };
        }
        let p = self.current_projection()?;
        self.resolve_path(&RecordSelector::Id(id.clone()), &p)?;
        self.source_plan(SourceStore::new(self.fs.clone()).plan_withdraw(&id, reason)?)
    }
    pub fn evidence_revalidate(
        &self,
        id: RecordId,
        revision: RecordId,
        hash: Blake3Hash,
    ) -> Result<MutationOutcome> {
        let p = self.current_projection()?;
        self.resolve_path(&RecordSelector::Id(id.clone()), &p)?;
        self.resolve_path(&RecordSelector::Id(revision.clone()), &p)?;
        let plan = SourceStore::new(self.fs.clone()).plan_revalidate(&id, &revision, &hash)?;
        self.execute_draft(plan.draft, true)
    }
    pub fn index_sync(&self, rebuild: bool) -> Result<IndexOutcome> {
        if self.catalog().operation_state()?.is_some() {
            return self.index_normalized(rebuild);
        }
        if self.options.dry_run {
            self.current_projection()?;
            return Ok(IndexOutcome {
                report: None,
                dry_run: true,
                cache_state_unknown: true,
                maintenance: None,
            });
        }
        let w = self.writer()?;
        let c = self.catalog();
        let report = if rebuild { c.rebuild(&w)? } else { c.sync(&w)? };
        Ok(IndexOutcome {
            report: Some(report),
            dry_run: false,
            cache_state_unknown: false,
            maintenance: None,
        })
    }
    pub fn index_rebuild_normalized(&self) -> Result<IndexOutcome> {
        self.index_normalized(true)
    }
    fn index_normalized(&self, rebuild: bool) -> Result<IndexOutcome> {
        let catalog = self.catalog();
        // Dry-run describes maintenance without acquiring a writer, creating
        // SQLite files or scanning the full corpus.
        catalog.operation_state()?;
        if self.options.dry_run {
            return Ok(IndexOutcome {
                report: None,
                dry_run: true,
                cache_state_unknown: true,
                maintenance: Some(
                    serde_json::json!({"layout":"normalized","canonical_scan_performed":false}),
                ),
            });
        }
        let writer = self.writer()?;
        let result = if rebuild {
            catalog.rebuild_normalized(&writer)?
        } else {
            catalog.sync_normalized(&writer)?
        };
        Ok(IndexOutcome {
            maintenance: Some(serde_json::json!({
                "layout":"normalized", "resumed":result.resumed,
                "input":result.input, "build":result.build,
                "page_sync": result.page_sync,
                "retirement_deferred":result.retirement_deferred,
                "cleanup_errors":result.cleanup_errors,
                "abandoned_rebuild_candidates": result.abandoned_rebuild_candidates,
            })),
            report: Some(result.report),
            dry_run: false,
            cache_state_unknown: false,
        })
    }
    pub fn check(&self) -> Result<CheckOutcome> {
        if self.options.dry_run {
            return Ok(CheckOutcome {
                diagnostics: Vec::new(),
                error_count: 0,
                canonical_check_performed: false,
                cache_integrity_check_performed: false,
                cache_matches_canonical: None,
                complete: false,
                checked_snapshot: None,
                audit: None,
            });
        }
        let catalog = self.catalog();
        if catalog.operation_state()?.is_some() {
            let writer = self.writer()?;
            let result = catalog.check_normalized(&writer)?;
            return Ok(CheckOutcome {
                error_count: result.diagnostics.len(),
                diagnostics: result.diagnostics,
                canonical_check_performed: true,
                cache_integrity_check_performed: true,
                cache_matches_canonical: Some(true),
                complete: true,
                checked_snapshot: Some(result.snapshot),
                audit: Some(serde_json::json!({
                    "layout":"normalized", "input":result.input, "work":result.work,
                    "search_index":result.search_index, "scratch_bytes":result.scratch_bytes,
                    "scratch_cleaned":true, "revision_owner_history_checked":true,
                    "unused_retained_payloads_checked":false,
                })),
            });
        }
        let p = scan::scan(&self.fs, &self.vault_id)?;
        let error_count = p.diagnostics.len();
        Ok(CheckOutcome {
            diagnostics: p.diagnostics,
            error_count,
            canonical_check_performed: true,
            cache_integrity_check_performed: false,
            cache_matches_canonical: None,
            complete: true,
            checked_snapshot: None,
            audit: None,
        })
    }
    pub fn doctor(&self) -> Result<DoctorOutcome> {
        let metadata = if self.options.dry_run {
            crate::catalog::DoctorCacheMetadata::default()
        } else {
            self.catalog().doctor_cache_metadata()
        };
        Ok(DoctorOutcome {
            check: None,
            canonical_check_performed: false,
            canonical_freshness: "unknown".into(),
            history_check_performed: false,
            cache_integrity_check_performed: false,
            cache_layout: metadata.layout,
            cache_state: metadata.state,
            cache_error: metadata.error,
            cache_header_snapshot: metadata.header_snapshot,
            header_check_performed: metadata.header_check_performed,
            parser_compatible: metadata.parser_compatible,
            cache_note: metadata.note,
            operation_state: metadata.operation_state,
            active_change: metadata.active_change,
            unresolved_changes: Vec::new(),
            incomplete_preparations: Vec::new(),
            provider_probe_performed: false,
        })
    }
    /// Review authenticated metadata without inspecting retained or target bodies.
    pub fn changes_summary(&self, id: RecordId) -> Result<ChangeSummary> {
        let i = self.engine()?.inspect_metadata(&id)?;
        let manifest = i.manifest;
        let operations = manifest
            .operations
            .into_iter()
            .enumerate()
            .map(|(operation, metadata)| ChangeSummaryOperation {
                operation,
                kind: match (&metadata.before, &metadata.after) {
                    (ExpectedState::Absent, _) => "create",
                    (_, ExpectedState::Absent) => "delete",
                    _ => "update",
                },
                metadata,
            })
            .collect::<Vec<_>>();
        Ok(ChangeSummary {
            inspection: "metadata_summary",
            prepared: i.prepared,
            vault_id: manifest.vault_id,
            manifest_version: manifest.version,
            status: i.status,
            note_status: i.note_status,
            title: manifest.title,
            created_at: manifest.created_at,
            origin: manifest.origin,
            inverse_of: manifest.inverse_of,
            allocated_ids: manifest.allocated_ids,
            operation_count: operations.len(),
            operations,
            read_preconditions: manifest.read_preconditions,
            checks: ChangeSummaryChecks {
                manifest_binding: "verified",
                status: "verified",
                note_status: "diagnostic",
                payload_availability: "not_checked",
                payload_integrity: "not_checked",
                current_target_freshness: "not_checked",
            },
        })
    }
    pub fn changes_show(&self, id: RecordId) -> Result<ChangeDetails> {
        let engine = self.engine()?;
        let i = engine.inspect_history(&id)?;
        let unavailable_payloads = engine.missing_retained_payloads(&id)?;
        let mut payloads = Vec::new();
        let mut omitted_payloads = Vec::new();
        let mut cumulative = 0u64;
        for (index, op) in i.manifest.operations.iter().enumerate() {
            let length = op
                .before_payload
                .as_ref()
                .map_or(0, |p| p.byte_len)
                .checked_add(op.after_payload.as_ref().map_or(0, |p| p.byte_len))
                .ok_or_else(|| WikiError::invalid("retained payload length overflow"))?;
            let unavailable = [op.before_payload.as_ref(), op.after_payload.as_ref()]
                .into_iter()
                .flatten()
                .any(|payload| unavailable_payloads.contains(&payload.path));
            if unavailable || cumulative.saturating_add(length) > MAX_INPUT_BYTES as u64 {
                omitted_payloads.push(index);
                continue;
            }
            payloads.push(verified_change_payload(&engine, &i, index)?);
            cumulative += length;
        }
        Ok(ChangeDetails {
            prepared: i.prepared,
            manifest: i.manifest,
            status: i.status,
            note_status: i.note_status,
            frames: i.journal.frames,
            observations: i.observations,
            payloads,
            omitted_payloads,
            unavailable_payloads,
        })
    }
    pub fn changes_payload(&self, id: RecordId, operation: usize) -> Result<ChangePayload> {
        let engine = self.engine()?;
        let i = engine.inspect_history(&id)?;
        verified_change_payload(&engine, &i, operation)
    }

    fn retained_draft(&self, engine: &ChangeEngine, i: &ChangeInspection) -> Result<ChangeDraft> {
        let mut operations = Vec::new();
        for (index, op) in i.manifest.operations.iter().enumerate() {
            operations.push(ExpectedWrite {
                target: op.target.clone(),
                expected: op.before.clone(),
                proposed: engine.verify_payload(
                    &i.prepared.change_id,
                    index,
                    "proposed",
                    &op.target,
                    &op.after,
                    &op.after_payload,
                )?,
                apply_after: op
                    .apply_after
                    .iter()
                    .map(|j| i.manifest.operations[*j].target.clone())
                    .collect(),
            });
        }
        Ok(ChangeDraft {
            title: i.manifest.title.clone(),
            origin: i.manifest.origin.clone(),
            inverse_of: i.manifest.inverse_of.clone(),
            allocated_ids: i.manifest.allocated_ids.clone(),
            read_preconditions: i.manifest.read_preconditions.clone(),
            operations,
        })
    }
    pub fn changes_apply(&self, id: RecordId) -> Result<MutationOutcome> {
        let engine = self.engine()?;
        let catalog = self.catalog();
        let authority = catalog.operation_state()?;
        let (manifest, manifest_hash) = engine.load_manifest_structure(&id)?;
        let change = PreparedChange {
            change_id: id,
            manifest_hash,
        };
        let proof = engine.indexed_replay_proof(&manifest, &change, authority.as_ref())?;
        let mut outcome = MutationOutcome {
            source_capture: None,
            plan: summarize_manifest(&manifest),
            allocated_ids: manifest.allocated_ids.clone(),
            change: Some(change.clone()),
            status: None,
            snapshot: None,
            reused: false,
        };
        if let Some(proof) = proof {
            use crate::catalog::source_refresh::IndexedRefreshSession;
            if self.options.dry_run {
                if let Some(report) = engine.indexed_refresh_terminal_outcome(&change)? {
                    outcome.status = Some(report.status);
                    outcome.snapshot = report.snapshot;
                    outcome.reused = true;
                } else {
                    IndexedRefreshSession::check_preview(&catalog, &proof)?;
                    outcome.status = Some(
                        journal::load_journal(&self.fs, &manifest, &change.manifest_hash)?
                            .status
                            .unwrap_or(ChangeStatus::Prepared),
                    );
                }
                return Ok(outcome);
            }
            let writer = self.writer()?;
            let report =
                if let Some(report) = engine.indexed_refresh_terminal_report(&writer, &change)? {
                    outcome.reused = true;
                    report
                } else {
                    let mut session = IndexedRefreshSession::resume(&catalog, &writer, proof)
                        .map_err(|error| retained_error(error, &change))?;
                    engine
                        .apply_indexed_refresh(&writer, &mut session)
                        .map_err(|error| retained_error(error, &change))?
                };
            outcome.status = Some(report.status);
            outcome.snapshot = report.snapshot;
            return Ok(outcome);
        }
        // A retained terminal result is historical evidence, even after its
        // payloads expire. Report it without reopening obsolete canonical state.
        if let Some(report) =
            crate::changes::outcome::terminal_report(&self.fs, &manifest, &change.manifest_hash)?
        {
            if !self.options.dry_run {
                let writer = self.writer()?;
                crate::changes::outcome::sync_receipt(&self.fs, &writer, &change.change_id)?;
                if engine
                    .indexed_replay_proof(&manifest, &change, catalog.operation_state()?.as_ref())?
                    .is_some()
                    || crate::changes::outcome::terminal_report(
                        &self.fs,
                        &manifest,
                        &change.manifest_hash,
                    )?
                    .as_ref()
                        != Some(&report)
                {
                    return Err(WikiError::new(
                        ErrorCode::RecoveryRequired,
                        "terminal change outcome changed during retry",
                    ));
                }
            }
            outcome.status = Some(report.status);
            outcome.snapshot = report.snapshot;
            outcome.reused = true;
            return Ok(outcome);
        }
        if authority.is_some() {
            use crate::catalog::source_refresh::IndexedRefreshSession;
            if self.options.dry_run {
                IndexedRefreshSession::preview_legacy_page(&catalog, &change)?;
                outcome.status = Some(ChangeStatus::Prepared);
            } else {
                let writer = self.writer()?;
                let mut session =
                    IndexedRefreshSession::adopt_legacy_page(&catalog, &writer, &change)
                        .map_err(|error| retained_error(error, &change))?;
                let report = engine
                    .apply_indexed_refresh(&writer, &mut session)
                    .map_err(|error| retained_error(error, &change))?;
                outcome.status = Some(report.status);
                outcome.snapshot = report.snapshot;
            }
            return Ok(outcome);
        }
        // Never invoke the whole-vault legacy validator behind an activated
        // normalized catalog, including in dry-run.
        catalog.require_legacy_catalog()?;
        let inspection = engine.inspect(&change.change_id)?;
        if self.options.dry_run {
            if inspection
                .observations
                .iter()
                .any(|o| o.observed != o.before && o.observed != o.after)
            {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "change targets contain unfamiliar edits",
                ));
            }
            engine.validate_prepared_graph(&change, &CatalogGraphValidator)?;
            outcome.status = Some(inspection.status);
            return Ok(outcome);
        }
        let writer = self.writer()?;
        let report = engine
            .apply(&writer, &change, &CatalogGraphValidator, &catalog)
            .map_err(|error| retained_error(error, &change))?;
        outcome.status = Some(report.status);
        outcome.snapshot = report.snapshot;
        outcome.reused = matches!(
            inspection.status,
            ChangeStatus::Committed | ChangeStatus::Aborted
        );
        Ok(outcome)
    }
    pub fn changes_abort(&self, id: RecordId) -> Result<MutationOutcome> {
        let engine = self.engine()?;
        let i = engine.inspect(&id)?;
        let draft = self.retained_draft(&engine, &i)?;
        let summary = summarize(&draft.title, &draft.read_preconditions, &draft.operations);
        let allocated_ids = draft.allocated_ids.clone();
        if self.options.dry_run {
            if !matches!(i.status, ChangeStatus::Prepared | ChangeStatus::Aborted) {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "applying change cannot be aborted",
                ));
            }
            return Ok(MutationOutcome {
                source_capture: None,
                plan: summary,
                allocated_ids,
                change: Some(i.prepared),
                status: Some(i.status),
                snapshot: None,
                reused: i.status == ChangeStatus::Aborted,
            });
        }
        let w = self.writer()?;
        let report = engine.abort(&w, &i.prepared)?;
        Ok(MutationOutcome {
            source_capture: None,
            plan: summary,
            allocated_ids,
            change: Some(report.change),
            status: Some(report.status),
            snapshot: report.snapshot,
            reused: i.status == ChangeStatus::Aborted,
        })
    }
    pub fn changes_rollback(&self, id: RecordId) -> Result<MutationOutcome> {
        let engine = self.engine()?;
        let i = engine.inspect(&id)?;
        if self.catalog().operation_state()?.is_some() {
            return self.execute_page_input(
                PageWriteInput::Inverse(engine.page_inverse_plan(&i.prepared)?),
                true,
            );
        }
        self.execute_draft(engine.inverse_plan(&i.prepared)?.draft, true)
    }
    fn pending(&self, engine: &ChangeEngine) -> Result<Vec<RecordId>> {
        let mut pending = Vec::new();
        for id in engine.change_ids()? {
            let i = engine.inspect_history(&id)?;
            if matches!(
                i.status,
                ChangeStatus::Applying
                    | ChangeStatus::FilesApplied
                    | ChangeStatus::Indexed
                    | ChangeStatus::Conflict
            ) || i.journal.status.is_none()
                && i.observations.iter().any(|o| o.observed != o.before)
            {
                pending.push(id);
            }
        }
        Ok(pending)
    }
    pub fn recover(&self) -> Result<RecoverOutcome> {
        let engine = self.engine()?;
        let catalog = self.catalog();
        let authority = catalog.operation_state()?;
        let mut pending = self.pending(&engine)?;
        if let Some(active) = authority.as_ref().and_then(|state| state.active()) {
            if !pending.contains(&active.change.change_id) {
                pending.push(active.change.change_id.clone());
                pending.sort();
            }
        }
        if self.options.dry_run {
            return Ok(RecoverOutcome {
                report: None,
                pending,
                dry_run: true,
            });
        }
        let w = self.writer()?;
        let report = if catalog.operation_state()?.is_some() {
            engine.recover_indexed(&w, &catalog)?
        } else {
            engine.recover(&w, &CatalogGraphValidator, &catalog)?
        };
        Ok(RecoverOutcome {
            report: Some(report),
            pending,
            dry_run: false,
        })
    }
    pub fn migrate(
        &self,
        selector: RecordSelector,
        hash: Blake3Hash,
        target_schema: &str,
    ) -> Result<MutationOutcome> {
        let p = self.current_projection()?;
        let path = self.resolve_path(&selector, &p)?;
        let raw = read_bytes(&self.fs, &path)?;
        let note = parse_note(&raw);
        if target_schema == "2"
            && note
                .canonical
                .as_ref()
                .is_some_and(|record| record.kind() == RecordKind::Vault)
        {
            let mut error = WikiError::new(
                ErrorCode::Usage,
                "vault schema 2 requires coordinated storage migration; use storage plan and storage cleanup",
            );
            error.details = serde_json::json!({"reason":"storage_migration_required","next_action":"lwiki storage plan"});
            return Err(error);
        }
        let proposed = crate::records::edit::migrate_schema(&note, target_schema, &hash)?;
        self.execute_draft(
            empty_draft(
                format!("Migrate {path} to schema {target_schema}"),
                vec![ExpectedWrite {
                    target: path,
                    expected: ExpectedState::Hash(hash),
                    proposed: Some(proposed),
                    apply_after: vec![],
                }],
            ),
            true,
        )
    }
}
fn summarize_manifest(manifest: &ChangeManifest) -> PlanSummary {
    PlanSummary {
        title: manifest.title.clone(),
        read_preconditions: manifest.read_preconditions.clone(),
        operations: manifest
            .operations
            .iter()
            .map(|op| PlannedOperation {
                path: op.target.clone(),
                before: op.before.clone(),
                after: op.after.clone(),
                byte_len: op
                    .after_payload
                    .as_ref()
                    .map_or(0, |payload| payload.byte_len),
                apply_after: op
                    .apply_after
                    .iter()
                    .map(|index| manifest.operations[*index].target.clone())
                    .collect(),
            })
            .collect(),
    }
}
pub(super) fn summarize(
    title: &str,
    dependencies: &[ReadDependency],
    operations: &[ExpectedWrite],
) -> PlanSummary {
    PlanSummary {
        title: title.into(),
        read_preconditions: dependencies.to_vec(),
        operations: operations
            .iter()
            .map(|op| PlannedOperation {
                path: op.target.clone(),
                before: op.expected.clone(),
                after: op.proposed.as_ref().map_or(ExpectedState::Absent, |b| {
                    ExpectedState::Hash(Blake3Hash::digest(b))
                }),
                byte_len: op.proposed.as_ref().map_or(0, |b| b.len() as u64),
                apply_after: op.apply_after.clone(),
            })
            .collect(),
    }
}

/// Bootstrap is the sole pre-engine mutation. The guarded marker commits last.
pub fn init(path: &std::path::Path, title: &str, options: OperationOptions) -> Result<InitOutcome> {
    use crate::vault::{DurableIo, NativeIo, VaultRoot};
    let io = NativeIo;
    if options.lock_timeout_ms > 30_000 {
        return Err(WikiError::new(
            ErrorCode::ConfigInvalid,
            "writer lock timeout exceeds 30 seconds",
        ));
    }
    if title.trim().is_empty() || title.len() > 65_536 {
        return Err(usage("init title must be nonblank and at most 64 KiB"));
    }
    let exists = match std::fs::symlink_metadata(path) {
        Ok(m) => {
            if m.file_type().is_symlink() || !m.is_dir() {
                return Err(WikiError::invalid(
                    "initialization target must be regular directory",
                ));
            }
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(WikiError::new(ErrorCode::Internal, e.to_string())),
    };
    let (parent, final_name) = if exists {
        (None, None)
    } else {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let parent = VaultRoot::for_initialization(parent)?;
        let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
            WikiError::invalid("new vault directory must have UTF-8 final component")
        })?;
        let relative = VaultRelativePath::new(name)?;
        parent.validate_portable_paths(std::slice::from_ref(&relative))?;
        (Some(parent), Some(relative))
    };
    let resolved = if let (Some(parent), Some(name)) = (&parent, &final_name) {
        parent.resolve(name)?
    } else {
        VaultRoot::for_initialization(path)?.path().to_path_buf()
    };
    if std::fs::symlink_metadata(resolved.join("WIKI.md")).is_ok() {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "initialization refuses existing WIKI.md",
        ));
    }
    let directories = vec![
        "pages",
        "knowledge/entities",
        "knowledge/assertions",
        "knowledge/evidence",
        "knowledge/decisions",
        "sources",
        "changes",
        ".wiki/cache",
    ];
    let id = RecordId::generate(RecordKind::Vault)?;
    if options.dry_run {
        return Ok(InitOutcome {
            id,
            path: resolved
                .to_str()
                .ok_or_else(|| usage("vault path is not UTF-8"))?
                .to_owned(),
            created: false,
            planned_directories: directories.into_iter().map(str::to_owned).collect(),
        });
    }
    if !exists {
        io.create_directory(&resolved).map_err(|e| {
            WikiError::new(ErrorCode::Internal, format!("create vault directory: {e}"))
        })?;
        io.sync_directory(parent.as_ref().expect("new parent checked").path())
            .map_err(|e| {
                WikiError::new(ErrorCode::Internal, format!("sync new vault parent: {e}"))
            })?;
    }
    let root = VaultRoot::for_initialization(&resolved)?;
    let fs = VaultFs::new(root.clone());
    let mut planned: Vec<_> = directories
        .iter()
        .map(|d| VaultRelativePath::new(*d))
        .collect::<Result<_>>()?;
    planned.extend([
        VaultRelativePath::new("WIKI.md")?,
        VaultRelativePath::new(".wiki/.gitignore")?,
    ]);
    root.validate_portable_paths(&planned)?;
    let w = WriterPermit::acquire(&root, Duration::from_millis(options.lock_timeout_ms))?;
    if fs
        .read_before(&VaultRelativePath::new("WIKI.md")?)?
        .is_some()
    {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "initialization marker appeared",
        ));
    }
    for directory in &directories {
        fs.ensure_directory(&VaultRelativePath::new(*directory)?, &w)?;
    }
    let ignore = VaultRelativePath::new(".wiki/.gitignore")?;
    let before = crate::changes::prepare::read_bounded(&fs, &ignore, MAX_INPUT_BYTES)?;
    let expected = before.as_ref().map_or(ExpectedState::Absent, |b| {
        ExpectedState::Hash(Blake3Hash::digest(b))
    });
    let mut bytes = before.unwrap_or_default();
    if !bytes
        .split(|b| *b == b'\n')
        .any(|line| line == b"cache/" || line == b"/cache/")
    {
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(b"cache/\n");
        let staged = fs.stage(&ignore, &bytes, &w)?;
        fs.replace(staged, &expected, &w)?;
    }
    let marker = format!(
        "---\nwiki_schema: \"1\"\nwiki_id: {}\nwiki_kind: vault\ntitle: {}\n---\n",
        serde_json::to_string(id.as_str()).map_err(|e| WikiError::invalid(e.to_string()))?,
        serde_json::to_string(title).map_err(|e| WikiError::invalid(e.to_string()))?
    );
    let target = VaultRelativePath::new("WIKI.md")?;
    let staged = fs.stage(&target, marker.as_bytes(), &w)?;
    fs.replace(staged, &ExpectedState::Absent, &w)?;
    Ok(InitOutcome {
        id,
        path: root
            .path()
            .to_str()
            .ok_or_else(|| usage("vault path is not UTF-8"))?
            .to_owned(),
        created: true,
        planned_directories: directories.into_iter().map(str::to_owned).collect(),
    })
}

fn capture_bounds(request: &CaptureRequest) -> Result<()> {
    bounded(&request.original)?;
    if let crate::sources::ExtractionInput::Supplied { content, .. } = &request.extraction {
        bounded(content)?;
    }
    Ok(())
}

pub(super) fn retained_error(mut error: WikiError, change: &PreparedChange) -> WikiError {
    let reference =
        serde_json::json!({"change_id": change.change_id, "manifest_hash": change.manifest_hash});
    if let Some(details) = error.details.as_object_mut() {
        details.insert("change".into(), reference);
    } else {
        error.details = serde_json::json!({"change": reference, "cause_details": error.details});
    }
    error
}

fn verified_change_payload(
    engine: &ChangeEngine,
    i: &ChangeInspection,
    index: usize,
) -> Result<ChangePayload> {
    let op = i
        .manifest
        .operations
        .get(index)
        .ok_or_else(|| usage("change operation index is out of range"))?;
    Ok(ChangePayload {
        operation: index,
        target: op.target.clone(),
        before: engine.verify_payload(
            &i.prepared.change_id,
            index,
            "before",
            &op.target,
            &op.before,
            &op.before_payload,
        )?,
        proposed: engine.verify_payload(
            &i.prepared.change_id,
            index,
            "proposed",
            &op.target,
            &op.after,
            &op.after_payload,
        )?,
    })
}

//! Local operations share the real changeset engine; dry-run uses pure plans only.
use crate::{
    changes::*,
    domain::*,
    records::{
        LinkResolution, LinkSyntax, ParsedNote, RegistryEntry, edit_note, extract_links,
        parse_note, resolve_untyped,
    },
    vault::{ExpectedState, VaultFs},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

pub use super::types::{DEFAULT_READ_BYTES, MAX_INPUT_BYTES};
fn usage(message: &str) -> WikiError {
    WikiError::new(ErrorCode::Usage, message)
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
fn splice(raw: &[u8], mut edits: Vec<(Range<usize>, Vec<u8>)>) -> Result<Vec<u8>> {
    edits.sort_by_key(|(range, _)| range.start);
    edits.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let mut out = Vec::new();
    let mut cursor = 0;
    for (range, value) in edits {
        if range.start < cursor || range.end > raw.len() {
            return Err(WikiError::invalid(
                "overlapping or invalid link edit ranges",
            ));
        }
        out.extend_from_slice(&raw[cursor..range.start]);
        out.extend(value);
        cursor = range.end;
    }
    out.extend_from_slice(&raw[cursor..]);
    Ok(out)
}
fn destination_range(raw: &str, start: usize) -> Result<Range<usize>> {
    let bytes = raw.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'<') {
        let begin = i + 1;
        let end = raw[begin..]
            .find('>')
            .map(|n| begin + n)
            .ok_or_else(|| WikiError::invalid("cannot safely locate Markdown destination"))?;
        return Ok(begin..end);
    }
    let begin = i;
    let mut depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i = i.saturating_add(2);
                continue;
            }
            b'(' => depth += 1,
            b')' if depth == 0 => break,
            b')' => depth -= 1,
            b if b.is_ascii_whitespace() && depth == 0 => break,
            _ => {}
        }
        i += 1;
    }
    if begin == i {
        return Err(WikiError::invalid(
            "cannot safely locate Markdown destination",
        ));
    }
    Ok(begin..i)
}
/// Only parser-resolved links change; original labels, fragments and titles survive.
fn rewrite_links(
    body: &str,
    registry: &[RegistryEntry],
    target: &RecordId,
    to: &VaultRelativePath,
) -> Result<Vec<u8>> {
    let mut edits = Vec::new();
    let mut reference_destinations = BTreeSet::new();
    let original_links = extract_links(body);
    let mut expected = Vec::new();
    for (index, link) in original_links.iter().enumerate() {
        let LinkResolution::Resolved { id, fragment, .. } =
            resolve_untyped(registry, &link.destination)
        else {
            continue;
        };
        if &id != target {
            continue;
        }
        let new_dest = fragment.map_or_else(|| to.as_str().to_owned(), |f| format!("{to}#{f}"));
        expected.push((index, new_dest.clone()));
        let raw = &body[link.range.clone()];
        let local = match link.syntax {
            LinkSyntax::Wiki => {
                let end = raw[2..raw.len() - 2]
                    .find('|')
                    .map_or(raw.len() - 2, |n| n + 2);
                Some(2..end)
            }
            LinkSyntax::Markdown => {
                if let Some(at) = raw.find("](") {
                    Some(destination_range(raw, at + 2)?)
                } else {
                    reference_destinations.insert((link.destination.clone(), new_dest.clone()));
                    None
                }
            }
        };
        if let Some(range) = local {
            if raw[range.clone()] != link.destination {
                return Err(WikiError::invalid(
                    "cannot safely preserve escaped or complex Markdown destination",
                ));
            }
            edits.push((
                link.range.start + range.start..link.range.start + range.end,
                new_dest.into_bytes(),
            ));
        }
    }
    let parser = pulldown_cmark::Parser::new(body);
    for (_, definition) in parser.reference_definitions().iter() {
        if let Some((_, new_dest)) = reference_destinations
            .iter()
            .find(|(dest, _)| dest == definition.dest.as_ref())
        {
            let raw = &body[definition.span.clone()];
            let at = raw
                .find("]:")
                .ok_or_else(|| WikiError::invalid("cannot safely locate reference destination"))?;
            let range = destination_range(raw, at + 2)?;
            if &raw[range.clone()] != definition.dest.as_ref() {
                return Err(WikiError::invalid(
                    "cannot safely preserve escaped reference destination",
                ));
            }
            edits.push((
                definition.span.start + range.start..definition.span.start + range.end,
                new_dest.as_bytes().to_vec(),
            ));
        }
    }
    let changed = splice(body.as_bytes(), edits)?;
    let text = std::str::from_utf8(&changed)
        .map_err(|_| WikiError::invalid("link edit produced non-UTF-8"))?;
    let after = extract_links(text);
    if after.len() != original_links.len()
        || expected
            .iter()
            .any(|(index, destination)| after[*index].destination != *destination)
    {
        return Err(WikiError::invalid(
            "link edit cannot preserve parsed destinations safely",
        ));
    }
    Ok(changed)
}
fn rewrite_companions(
    note: &ParsedNote,
    registry: &[RegistryEntry],
    target: &RecordId,
    to: &VaultRelativePath,
) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut changes = BTreeMap::new();
    let Some(record) = &note.canonical else {
        return Ok(changes);
    };
    for (key, value) in record.fields() {
        if !key.starts_with("wiki_") {
            continue;
        }
        let Some(value) = value
            .as_str()
            .filter(|v| v.starts_with("[[") && v.ends_with("]]"))
        else {
            continue;
        };
        let id_field = if key == "wiki_revision" && record.kind() == RecordKind::Source {
            "wiki_current_revision".to_owned()
        } else if key == "wiki_revision" {
            "wiki_source_revision".to_owned()
        } else {
            format!("{key}_id")
        };
        if record.string(&id_field) != Some(target.as_str()) {
            continue;
        }
        match resolve_untyped(registry, value) {
            LinkResolution::Resolved { id, .. } if &id != target => {
                return Err(WikiError::invalid(
                    "incoming companion resolves to another identity",
                ));
            }
            LinkResolution::Ambiguous { .. } => {
                return Err(WikiError::new(
                    ErrorCode::ReferenceAmbiguous,
                    "incoming companion path is ambiguous",
                ));
            }
            LinkResolution::External => {
                return Err(WikiError::invalid("incoming companion is external"));
            }
            _ => {}
        }
        let interior = &value[2..value.len() - 2];
        let (destination, label) = interior
            .split_once('|')
            .map_or((interior, None), |(path, label)| (path, Some(label)));
        let fragment = destination.split_once('#').map(|(_, fragment)| fragment);
        let mut replacement = format!("[[{to}");
        if let Some(fragment) = fragment {
            replacement.push('#');
            replacement.push_str(fragment);
        }
        if let Some(label) = label {
            replacement.push('|');
            replacement.push_str(label);
        }
        replacement.push_str("]]");
        changes.insert(key.clone(), replacement.into());
    }
    Ok(changes)
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
    fn engine(&self) -> Result<ChangeEngine> {
        ChangeEngine::new(self.fs.clone())
    }
    fn catalog(&self) -> Catalog {
        Catalog::new(self.fs.clone(), self.vault_id.clone())
    }
    fn writer(&self) -> Result<WriterPermit> {
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
        let requested = request
            .range
            .unwrap_or(ByteSpan::new(0, body.len() as u64)?);
        let start = usize::try_from(requested.start())
            .map_err(|_| usage("byte range exceeds platform limit"))?;
        let wanted_end = usize::try_from(requested.end())
            .map_err(|_| usage("byte range exceeds platform limit"))?;
        if wanted_end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(wanted_end)
        {
            return Err(usage(
                "byte range must use valid UTF-8 boundaries within body",
            ));
        }
        let mut end = wanted_end.min(start.saturating_add(request.max_bytes));
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let record = if canonical {
            p.records
                .values()
                .find(|row| row.path == path)
                .map(|row| row.record.clone())
        } else {
            None
        };
        Ok(ReadOutcome {
            path: path.clone(),
            hash: Blake3Hash::digest(&bytes),
            record,
            metadata: if canonical { note.fields.clone() } else { None },
            body: text[start..end].to_owned(),
            range: ByteSpan::new(start as u64, end as u64)?,
            truncated: end < wanted_end,
            diagnostics: p
                .diagnostics
                .into_iter()
                .filter(|d| d.path == path)
                .collect(),
        })
    }
    fn input(&self, draft: &ChangeDraft) -> Result<ValidationInput> {
        let mut input = scan::scan_input(&self.fs, &self.vault_id)?;
        input.overlay = draft
            .operations
            .iter()
            .map(|op| ProposedTarget {
                path: op.target.clone(),
                bytes: op.proposed.clone(),
            })
            .collect();
        Ok(input)
    }
    fn execute_draft(&self, draft: ChangeDraft, force_stage: bool) -> Result<MutationOutcome> {
        self.catalog().guard_current(None)?;
        let engine = self.engine()?;
        let plan = engine.plan(&draft)?;
        CatalogGraphValidator.validate(&self.fs, &self.input(&draft)?)?;
        let summary = summarize(&draft.title, &plan.read_preconditions, &plan.operations);
        let allocated_ids = draft.allocated_ids.clone();
        if self.options.dry_run || plan.operations.is_empty() {
            return Ok(MutationOutcome {
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
            plan: summary,
            allocated_ids,
            change: Some(applied.change),
            status: Some(applied.status),
            snapshot: applied.snapshot,
            reused: false,
        })
    }
    pub fn page_put(
        &self,
        path: VaultRelativePath,
        bytes: Vec<u8>,
        if_match: Option<Blake3Hash>,
    ) -> Result<MutationOutcome> {
        bounded(&bytes)?;
        if !crate::sources::revision::canonical_path(&path) {
            return Err(WikiError::invalid(
                "page target must be canonical Markdown path",
            ));
        }
        let new = parse_note(&bytes);
        let record = new
            .canonical
            .as_ref()
            .filter(|r| r.kind() == RecordKind::Page)
            .ok_or_else(|| WikiError::invalid("page put requires a valid page envelope"))?;
        let before = crate::changes::prepare::read_bounded(&self.fs, &path, MAX_INPUT_BYTES)?;
        let expected = match (before, if_match) {
            (Some(old), Some(hash)) => {
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
        self.execute_draft(
            empty_draft(
                format!("Put {}", record.title()),
                vec![ExpectedWrite {
                    target: path,
                    expected,
                    proposed: Some(bytes),
                    apply_after: vec![],
                }],
            ),
            false,
        )
    }
    pub fn page_rename(
        &self,
        id: RecordId,
        to: VaultRelativePath,
        hash: Blake3Hash,
    ) -> Result<MutationOutcome> {
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
        Ok(outcome)
    }
    pub fn source_add(&self, request: CaptureRequest) -> Result<MutationOutcome> {
        capture_bounds(&request)?;
        self.source_plan(SourceStore::new(self.fs.clone()).plan_capture(request)?)
    }
    pub fn source_refresh(&self, id: RecordId, request: CaptureRequest) -> Result<MutationOutcome> {
        capture_bounds(&request)?;
        let p = self.current_projection()?;
        self.resolve_path(&RecordSelector::Id(id.clone()), &p)?;
        self.source_plan(SourceStore::new(self.fs.clone()).plan_refresh(&id, request)?)
    }
    pub fn source_withdraw(&self, id: RecordId, reason: &str) -> Result<MutationOutcome> {
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
        if self.options.dry_run {
            self.current_projection()?;
            return Ok(IndexOutcome {
                report: None,
                dry_run: true,
                cache_state_unknown: true,
            });
        }
        let w = self.writer()?;
        let c = self.catalog();
        let report = if rebuild { c.rebuild(&w)? } else { c.sync(&w)? };
        Ok(IndexOutcome {
            report: Some(report),
            dry_run: false,
            cache_state_unknown: false,
        })
    }
    pub fn check(&self) -> Result<CheckOutcome> {
        let p = scan::scan(&self.fs, &self.vault_id)?;
        let error_count = p.diagnostics.len();
        Ok(CheckOutcome {
            diagnostics: p.diagnostics,
            error_count,
        })
    }
    pub fn doctor(&self) -> Result<DoctorOutcome> {
        let check = self.check()?;
        let engine = self.engine()?;
        let pending = self.pending(&engine)?;
        let incomplete = engine.incomplete_preparations()?;
        let (mut cache_state, mut cache_error) = ("unknown".to_owned(), None);
        if !self.options.dry_run {
            if !self.catalog().cache_path()?.exists() {
                cache_state = "absent".into();
            } else {
                match self.catalog().check_available() {
                    Ok(()) => cache_state = "ready".into(),
                    Err(e) => {
                        cache_state = "unavailable".into();
                        cache_error = Some(e);
                    }
                }
            }
        }
        Ok(DoctorOutcome {
            check,
            cache_state,
            cache_error,
            unresolved_changes: pending,
            incomplete_preparations: incomplete,
            provider_probe_performed: false,
        })
    }
    pub fn changes_show(&self, id: RecordId) -> Result<ChangeDetails> {
        let engine = self.engine()?;
        let i = engine.inspect(&id)?;
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
            if cumulative.saturating_add(length) > MAX_INPUT_BYTES as u64 {
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
        })
    }
    pub fn changes_payload(&self, id: RecordId, operation: usize) -> Result<ChangePayload> {
        let engine = self.engine()?;
        let i = engine.inspect(&id)?;
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
        let i = engine.inspect(&id)?;
        let draft = self.retained_draft(&engine, &i)?;
        let summary = summarize(&draft.title, &draft.read_preconditions, &draft.operations);
        let allocated_ids = draft.allocated_ids.clone();
        if self.options.dry_run {
            if i.observations
                .iter()
                .any(|o| o.observed != o.before && o.observed != o.after)
            {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "change targets contain unfamiliar edits",
                ));
            }
            CatalogGraphValidator.validate(&self.fs, &self.input(&draft)?)?;
            return Ok(MutationOutcome {
                plan: summary,
                allocated_ids,
                change: Some(i.prepared),
                status: Some(i.status),
                snapshot: None,
                reused: false,
            });
        }
        let w = self.writer()?;
        let report = engine
            .apply(&w, &i.prepared, &CatalogGraphValidator, &self.catalog())
            .map_err(|error| retained_error(error, &i.prepared))?;
        Ok(MutationOutcome {
            plan: summary,
            allocated_ids,
            change: Some(report.change),
            status: Some(report.status),
            snapshot: report.snapshot,
            reused: matches!(i.status, ChangeStatus::Committed | ChangeStatus::Aborted),
        })
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
        self.execute_draft(engine.inverse_plan(&i.prepared)?.draft, true)
    }
    fn pending(&self, engine: &ChangeEngine) -> Result<Vec<RecordId>> {
        let mut pending = Vec::new();
        for id in engine.change_ids()? {
            let i = engine.inspect(&id)?;
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
        let pending = self.pending(&engine)?;
        if self.options.dry_run {
            return Ok(RecoverOutcome {
                report: None,
                pending,
                dry_run: true,
            });
        }
        let w = self.writer()?;
        let report = engine.recover(&w, &CatalogGraphValidator, &self.catalog())?;
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
fn summarize(
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

fn retained_error(mut error: WikiError, change: &PreparedChange) -> WikiError {
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

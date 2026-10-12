//! An invocation owns one origin lookup and publishes bounded groups through the ordinary engines.
use super::{OfflineApp, ReadSourceCitation};
use crate::{
    catalog::{
        capture_projection::project_capture_batch,
        query_types::QueryReadLimits,
        source_projection::{RefreshProjectionLimits, project_refresh_batch},
        source_refresh::IndexedRefreshSession,
    },
    changes::{ChangeDraft, ChangeStatus, ReadDependency},
    domain::{
        Blake3Hash, ByteSpan, CitationRef, Eligibility, ErrorCode, RecordId, RecordKind, Result,
        SourceSpanRef, VaultRelativePath, WikiError,
    },
    sources::{
        CitationScope, SourceCaptureState, SourceInputReads, SourcePlan, SourceRefreshLimits,
        SourceStore, SourceView,
        capture_inputs::{CaptureInputLimits, ObservedCaptureInput, observe_inputs},
    },
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde::Serialize;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const GROUP_BYTES: usize = 4 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    static FAIL_AFTER_GROUP: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

struct CaptureGroupOutcome {
    plans: Vec<SourcePlan>,
    committed: bool,
    error: Option<WikiError>,
}

#[derive(Debug, Clone)]
pub struct SourceCaptureRequest {
    pub paths: Vec<PathBuf>,
    pub files_from: Option<PathBuf>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCaptureAction {
    Captured,
    Refreshed,
    Unchanged,
    Skipped,
    Failed,
    NotAttempted,
    Planned,
}
#[derive(Debug, Clone, Serialize)]
pub struct SourceCaptureItem {
    pub input: PathBuf,
    pub title: String,
    pub action: SourceCaptureAction,
    pub source_id: Option<RecordId>,
    pub revision_id: Option<RecordId>,
    pub capture_state: Option<SourceCaptureState>,
    pub managed_path: Option<VaultRelativePath>,
    pub citation: Option<ReadSourceCitation>,
    pub reason: Option<String>,
    pub error: Option<WikiError>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct SourceCaptureTotals {
    pub captured: usize,
    pub refreshed: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub failed: usize,
    pub not_attempted: usize,
    pub planned: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct SourceCaptureOutcome {
    pub items: Vec<SourceCaptureItem>,
    pub totals: SourceCaptureTotals,
    pub groups_committed: usize,
    /// Canonical author paths for one invocation-level after-write preparation.
    pub changed_owners: Vec<VaultRelativePath>,
    pub changed_owner_ids: Vec<RecordId>,
    pub partial: bool,
    pub dry_run: bool,
}

#[derive(Clone)]
struct BoundSource {
    id: RecordId,
    path: VaultRelativePath,
    hash: Blake3Hash,
    revision: RecordId,
    title: String,
    withdrawn: bool,
}

/// Shared legacy asset observations bound total history reads before allocating
/// another file, rather than granting each member an independent byte allowance.
struct GroupReads<'a> {
    fs: &'a VaultFs,
    remaining: Cell<usize>,
    count: Cell<usize>,
    started: Instant,
    states: RefCell<BTreeMap<VaultRelativePath, ExpectedState>>,
}
impl<'a> GroupReads<'a> {
    fn new(fs: &'a VaultFs) -> Self {
        Self {
            fs,
            remaining: Cell::new(256 * 1024 * 1024),
            count: Cell::new(0),
            started: Instant::now(),
            states: RefCell::new(BTreeMap::new()),
        }
    }
    fn read(&self, path: &VaultRelativePath, limit: usize) -> Result<Option<Vec<u8>>> {
        if self.started.elapsed() > Duration::from_secs(30)
            || self.count.get() >= 8192
            || self.remaining.get() == 0
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "capture group asset observations exceed their byte, file, or time allowance",
            ));
        }
        self.count.set(self.count.get() + 1);
        let bytes = crate::vault::fs::read_regular_bounded(
            self.fs.root(),
            path,
            false,
            limit.min(self.remaining.get()),
            "capture group history",
        )?;
        self.remaining
            .set(self.remaining.get() - bytes.as_ref().map_or(0, Vec::len));
        let expected = bytes.as_ref().map_or(ExpectedState::Absent, |bytes| {
            ExpectedState::Hash(Blake3Hash::digest(bytes))
        });
        if self
            .states
            .borrow_mut()
            .insert(path.clone(), expected.clone())
            .is_some_and(|before| before != expected)
        {
            return Err(conflict("capture group history changed during planning"));
        }
        Ok(bytes)
    }
}
impl SourceInputReads for GroupReads<'_> {
    fn read_observed(&self, path: &VaultRelativePath, max_bytes: usize) -> Result<Vec<u8>> {
        self.read(path, max_bytes)?.ok_or_else(|| {
            WikiError::new(
                ErrorCode::SourceIntegrity,
                format!("captured history is missing: {path}"),
            )
        })
    }
    fn state_observed(&self, path: &VaultRelativePath) -> Result<ExpectedState> {
        self.read(path, 64 * 1024 * 1024)?;
        Ok(self.states.borrow()[path].clone())
    }
    fn require_clean(&self) -> Result<()> {
        let paths = self.states.borrow().keys().cloned().collect::<Vec<_>>();
        for path in paths {
            self.read(&path, 64 * 1024 * 1024)?;
        }
        Ok(())
    }
}
fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn source_view<'a>(app: &'a OfflineApp) -> Result<SourceView<'a>> {
    SourceView::from_fs_bounded(&app.fs, 64 * 1024 * 1024, 4096)
}

/// Only canonical absolute origins participate. A relative origin is never resolved using today's cwd.
fn origin_lookup(view: &SourceView<'_>) -> Result<BTreeMap<PathBuf, BoundSource>> {
    let mut origins = BTreeMap::new();
    for (path, note) in &view.notes {
        let Some(record) = &note.canonical else {
            if note.fields.as_ref().is_some_and(|fields| {
                fields.get("wiki_kind").and_then(serde_json::Value::as_str) == Some("source")
                    && fields
                        .get("wiki_origin_kind")
                        .and_then(serde_json::Value::as_str)
                        == Some("local-file")
            }) {
                return Err(WikiError::new(
                    ErrorCode::SourceIntegrity,
                    format!(
                        "cannot identify captured originals because this Source is invalid: {path}"
                    ),
                ));
            }
            continue;
        };
        if record.kind() != RecordKind::Source
            || record.string("wiki_origin_kind") != Some("local-file")
        {
            continue;
        }
        let origin = PathBuf::from(
            record
                .string("wiki_origin")
                .expect("canonical source origin"),
        );
        if !origin.is_absolute() {
            continue;
        }
        if origin.components().collect::<PathBuf>().as_os_str() != origin.as_os_str()
            || origin.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err(conflict(format!(
                "captured original has an ambiguous absolute path: {path}"
            )));
        }
        view.resolve(record.id(), RecordKind::Source, None)?;
        let revision = RecordId::new(
            record
                .string("wiki_current_revision")
                .expect("canonical source head"),
        )?;
        let (_, head) = view.resolve(
            &revision,
            RecordKind::Revision,
            record.string("wiki_revision"),
        )?;
        if head
            .canonical
            .as_ref()
            .expect("resolved head")
            .string("wiki_source_id")
            != Some(record.id().as_str())
        {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "captured original's head belongs to another Source",
            ));
        }
        // Existing absolute origins may use a host ancestor alias (for example
        // /tmp on macOS). Resolve their parent when available, without following
        // or identifying the final original by its inode or content bytes.
        let origin = crate::sources::capture_inputs::normalized_path(&origin).unwrap_or(origin);
        let bound = BoundSource {
            id: record.id().clone(),
            path: path.clone(),
            hash: note.source_hash.clone(),
            revision,
            title: record.title().to_owned(),
            withdrawn: record.string("wiki_status") == Some("withdrawn"),
        };
        if origins.insert(origin.clone(), bound).is_some() {
            return Err(conflict(format!(
                "more than one Source is bound to {}; preserve both captures and resolve their origins before repeating source capture",
                origin.display()
            )));
        }
    }
    Ok(origins)
}

fn relative_origin_conflict(
    view: &SourceView<'_>,
    input: &ObservedCaptureInput,
) -> Option<WikiError> {
    // Matching suffixes cannot establish identity, but must not silently create
    // another capture when an older ambiguous binding might be this original.
    for note in view.notes.values() {
        let Some(source) = &note.canonical else {
            continue;
        };
        if source.kind() != RecordKind::Source
            || source.string("wiki_origin_kind") != Some("local-file")
        {
            continue;
        }
        let origin = Path::new(source.string("wiki_origin")?);
        if !origin.is_absolute() && input.origin.ends_with(origin) {
            let mut error = conflict(format!(
                "{} may already be captured as {} with a relative original path; that old path cannot be safely resolved from this working folder",
                input.input_path.display(),
                source.id()
            ));
            error.hint = Some("Keep the existing capture. Use source refresh with its Source ID for this note until its original path can be explicitly rebound.".into());
            return Some(error);
        }
    }
    None
}

fn combine_drafts(plans: &[SourcePlan]) -> Result<Option<ChangeDraft>> {
    let mut combined = ChangeDraft {
        title: format!("Capture or refresh {} notes", plans.len()),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        operations: vec![],
        read_preconditions: vec![],
    };
    let mut guards = BTreeMap::new();
    let mut targets = BTreeSet::new();
    for (ordinal, plan) in plans.iter().enumerate() {
        for dependency in &plan.dependencies {
            if guards
                .insert(dependency.path.clone(), dependency.expected.clone())
                .is_some_and(|old| old != dependency.expected)
            {
                return Err(conflict("capture group read guards disagree"));
            }
        }
        let Some(draft) = &plan.draft else {
            continue;
        };
        if draft.origin.is_some() || draft.inverse_of.is_some() {
            return Err(conflict(
                "capture group contains foreign publication authority",
            ));
        }
        for (key, id) in &draft.allocated_ids {
            combined
                .allocated_ids
                .insert(format!("{key}_{ordinal}"), id.clone());
        }
        for operation in &draft.operations {
            if !targets.insert(operation.target.clone()) {
                return Err(conflict("capture group writes overlap"));
            }
            if guards
                .get(&operation.target)
                .is_some_and(|guard| guard != &operation.expected)
            {
                return Err(conflict("capture group read/write guards disagree"));
            }
            combined.operations.push(operation.clone());
        }
        for dependency in &draft.read_preconditions {
            if guards
                .insert(dependency.path.clone(), dependency.expected.clone())
                .is_some_and(|old| old != dependency.expected)
            {
                return Err(conflict("capture group draft guards disagree"));
            }
        }
    }
    combined.read_preconditions = guards
        .into_iter()
        .map(|(path, expected)| ReadDependency { path, expected })
        .collect();
    Ok((!combined.operations.is_empty()).then_some(combined))
}

impl SourceCaptureOutcome {
    fn recount(&mut self) {
        self.totals = SourceCaptureTotals::default();
        for item in &self.items {
            let count = match item.action {
                SourceCaptureAction::Captured => &mut self.totals.captured,
                SourceCaptureAction::Refreshed => &mut self.totals.refreshed,
                SourceCaptureAction::Unchanged => &mut self.totals.unchanged,
                SourceCaptureAction::Skipped => &mut self.totals.skipped,
                SourceCaptureAction::Failed => &mut self.totals.failed,
                SourceCaptureAction::NotAttempted => &mut self.totals.not_attempted,
                SourceCaptureAction::Planned => &mut self.totals.planned,
            };
            *count += 1;
            if item.error.is_some() && item.action != SourceCaptureAction::Failed {
                self.totals.failed += 1;
            }
        }
        self.partial = self.totals.failed > 0 || self.totals.not_attempted > 0;
        self.changed_owners.sort();
        self.changed_owners.dedup();
        self.changed_owner_ids.sort();
        self.changed_owner_ids.dedup();
    }
}

impl OfflineApp {
    pub fn capture_sources(&self, request: SourceCaptureRequest) -> Result<SourceCaptureOutcome> {
        if self.options.stage_only {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "source capture publishes bounded groups; --stage is unavailable",
            ));
        }
        let inputs = observe_inputs(
            &request.paths,
            request.files_from.as_deref(),
            self.fs.root().path(),
            CaptureInputLimits::default(),
        )?;
        let mut outcome = SourceCaptureOutcome {
            items: inputs
                .files
                .iter()
                .map(|input| SourceCaptureItem {
                    input: input.input_path.clone(),
                    title: input.request.title.clone(),
                    action: SourceCaptureAction::NotAttempted,
                    source_id: None,
                    revision_id: None,
                    capture_state: None,
                    managed_path: None,
                    citation: None,
                    reason: None,
                    error: None,
                })
                .collect(),
            totals: SourceCaptureTotals::default(),
            groups_committed: 0,
            changed_owners: vec![],
            changed_owner_ids: vec![],
            partial: false,
            dry_run: self.options.dry_run,
        };
        for issue in inputs.issues {
            outcome.items.push(SourceCaptureItem {
                input: issue.input_path,
                title: String::new(),
                action: if issue.skipped {
                    SourceCaptureAction::Skipped
                } else {
                    SourceCaptureAction::Failed
                },
                source_id: None,
                revision_id: None,
                capture_state: None,
                managed_path: None,
                citation: None,
                reason: Some(issue.reason),
                error: issue.error,
            });
        }
        // Dry-run observes inputs, but never reserves IDs, writes a lock/cache,
        // opens a credential helper or implies committed citations.
        if self.options.dry_run {
            for item in outcome.items.iter_mut().take(inputs.files.len()) {
                item.action = SourceCaptureAction::Planned;
            }
            outcome.recount();
            return Ok(outcome);
        }
        let writer = self.writer()?;
        let catalog = self.catalog();
        catalog.guard_current(None)?;
        let normalized = catalog.operation_state()?.is_some();
        let view = source_view(self)?;
        let origins = origin_lookup(&view)?;
        let mut selected = BTreeSet::new();
        let mut fresh = Vec::new();
        let mut refresh = Vec::new();
        for (index, input) in inputs.files.iter().enumerate() {
            selected.insert(input.origin.clone());
            if let Some(bound) = origins.get(&input.origin) {
                let item = &mut outcome.items[index];
                item.title = bound.title.clone();
                item.source_id = Some(bound.id.clone());
                item.revision_id = Some(bound.revision.clone());
                item.managed_path = Some(bound.path.clone());
                if bound.withdrawn {
                    item.action = SourceCaptureAction::Failed;
                    item.error = Some(conflict(format!(
                        "{} is bound to withdrawn Source {}; keep its history and use a different original or explicitly restore its lifecycle",
                        input.input_path.display(),
                        bound.id
                    )));
                } else {
                    refresh.push(index);
                }
            } else if let Some(error) = relative_origin_conflict(&view, input) {
                outcome.items[index].action = SourceCaptureAction::Failed;
                outcome.items[index].error = Some(error);
            } else {
                fresh.push(index);
            }
        }
        for item in outcome.items.iter_mut().skip(inputs.files.len()) {
            if let Ok(origin) = crate::sources::capture_inputs::normalized_path(&item.input) {
                selected.insert(origin.clone());
                if let Some(bound) = origins.get(&origin) {
                    item.title = bound.title.clone();
                    item.source_id = Some(bound.id.clone());
                    item.revision_id = Some(bound.revision.clone());
                    item.managed_path = Some(bound.path.clone());
                }
            }
        }
        // Account for remembered originals only under explicitly selected folders.
        for (origin, bound) in &origins {
            if !selected.contains(origin)
                && inputs
                    .directories
                    .iter()
                    .any(|directory| origin.starts_with(directory))
                && std::fs::symlink_metadata(origin).is_err()
            {
                outcome.items.push(SourceCaptureItem {
                    input: origin.clone(),
                    title: bound.title.clone(),
                    action: SourceCaptureAction::Failed,
                    source_id: Some(bound.id.clone()),
                    revision_id: Some(bound.revision.clone()),
                    capture_state: None,
                    managed_path: Some(bound.path.clone()),
                    citation: None,
                    reason: Some("missing_original".into()),
                    error: Some(conflict(format!(
                        "original is missing or unreadable: {}; the last capture is retained",
                        origin.display()
                    ))),
                });
            }
        }
        drop(view);
        let mut stopped = false;
        for (members, is_refresh, max_members) in [(fresh, false, 8usize), (refresh, true, 16usize)]
        {
            let mut cursor = 0;
            while cursor < members.len() && !stopped {
                let start = cursor;
                let mut bytes = 0usize;
                while cursor < members.len() && cursor - start < max_members {
                    let len = inputs.files[members[cursor]].request.original.len();
                    if cursor > start && bytes.saturating_add(len) > GROUP_BYTES {
                        break;
                    }
                    bytes += len;
                    cursor += 1;
                }
                let mut group = Vec::new();
                for index in &members[start..cursor] {
                    match inputs.files[*index].recheck() {
                        Ok(()) => group.push(*index),
                        Err(error) => {
                            outcome.items[*index].action = SourceCaptureAction::Failed;
                            outcome.items[*index].error = Some(error);
                        }
                    }
                }
                if group.is_empty() {
                    continue;
                }
                let result = self.capture_group(
                    &writer,
                    &inputs.files,
                    &origins,
                    &group,
                    is_refresh,
                    normalized,
                );
                match result {
                    Ok(result) => {
                        if result.committed {
                            outcome.groups_committed += 1;
                        }
                        if result.error.is_some() {
                            stopped = true;
                        }
                        for (index, plan) in group.iter().zip(result.plans) {
                            let item = &mut outcome.items[*index];
                            item.source_id = Some(plan.source_id.clone());
                            item.revision_id = Some(plan.revision_id);
                            item.capture_state = plan.capture_state;
                            item.error = result.error.clone();
                            item.managed_path = Some(if is_refresh {
                                origins[&inputs.files[*index].origin].path.clone()
                            } else {
                                VaultRelativePath::new(format!(
                                    "sources/{}/source.md",
                                    plan.source_id
                                ))?
                            });
                            item.action = if plan.draft.is_none() {
                                SourceCaptureAction::Unchanged
                            } else if is_refresh {
                                SourceCaptureAction::Refreshed
                            } else {
                                SourceCaptureAction::Captured
                            };
                            if plan.draft.is_some()
                                && plan.capture_state == Some(SourceCaptureState::Complete)
                            {
                                outcome
                                    .changed_owners
                                    .push(item.managed_path.clone().expect("source path"));
                                outcome.changed_owner_ids.push(plan.source_id);
                            }
                        }
                    }
                    Err(error) => {
                        for index in group {
                            outcome.items[index].action = SourceCaptureAction::Failed;
                            outcome.items[index].error = Some(error.clone());
                        }
                        stopped = true;
                    }
                }
            }
        }
        // One final complete canonical view authenticates every returned whole
        // citation, then its union of selected dependencies is reread once.
        if let Err(error) = self.authenticate_capture_outcomes(&mut outcome) {
            for item in outcome.items.iter_mut().filter(|item| {
                matches!(
                    item.action,
                    SourceCaptureAction::Captured
                        | SourceCaptureAction::Refreshed
                        | SourceCaptureAction::Unchanged
                )
            }) {
                item.citation = None;
                item.error = Some(error.clone());
            }
            outcome.partial = true;
        }
        let citation_failure = outcome.items.iter().any(|item| item.error.is_some());
        outcome.recount();
        outcome.partial |= citation_failure;
        Ok(outcome)
    }

    fn capture_group(
        &self,
        writer: &WriterPermit,
        inputs: &[ObservedCaptureInput],
        origins: &BTreeMap<PathBuf, BoundSource>,
        group: &[usize],
        refresh: bool,
        normalized: bool,
    ) -> Result<CaptureGroupOutcome> {
        let store = SourceStore::new(self.fs.clone());
        let catalog = self.catalog();
        catalog.guard_current(None)?;
        if normalized {
            let reader = catalog.query_snapshot(QueryReadLimits::default())?;
            reader.require_policy_layout()?;
            let (plans, projected) = if refresh {
                let mut indexed = Vec::new();
                let mut remaining = 256usize * 1024 * 1024;
                for index in group {
                    let input = &inputs[*index];
                    let bound = &origins[&input.origin];
                    let exhausted = || {
                        WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "capture refresh group exceeds its cumulative planning allowance",
                        )
                    };
                    let reserve = input
                        .request
                        .original
                        .len()
                        .checked_mul(2)
                        .and_then(|v| v.checked_add(64 * 1024))
                        .ok_or_else(exhausted)?;
                    let allowance = remaining.checked_sub(reserve).ok_or_else(exhausted)? / 16;
                    if allowance == 0 {
                        return Err(exhausted());
                    }
                    let limits = SourceRefreshLimits {
                        max_canonical_bytes: allowance,
                        ..Default::default()
                    };
                    let plan = store.plan_refresh_indexed(
                        &reader,
                        &bound.id,
                        input.request.clone(),
                        None,
                        &limits,
                    )?;
                    let captured = plan.captured.iter().try_fold(0usize, |n, doc| {
                        n.checked_add(doc.bytes.len()).ok_or_else(exhausted)
                    })?;
                    let proposed = plan.plan.draft.as_ref().map_or(Ok(0usize), |draft| {
                        draft.operations.iter().try_fold(0usize, |n, operation| {
                            n.checked_add(operation.proposed.as_ref().map_or(0, Vec::len))
                                .ok_or_else(exhausted)
                        })
                    })?;
                    remaining = remaining
                        .checked_sub(
                            captured
                                .checked_mul(2)
                                .and_then(|n| n.checked_add(proposed))
                                .ok_or_else(exhausted)?,
                        )
                        .ok_or_else(exhausted)?;
                    if plan.previous_revision != bound.revision
                        || !plan
                            .captured
                            .iter()
                            .any(|doc| doc.path == bound.path && doc.hash == bound.hash)
                    {
                        return Err(conflict(
                            "captured Source or its head changed since original lookup",
                        ));
                    }
                    indexed.push(plan);
                }
                let plans = indexed.iter().map(|p| p.plan.clone()).collect();
                let projected = project_refresh_batch(
                    &self.fs,
                    &reader,
                    indexed,
                    &RefreshProjectionLimits::default(),
                )?;
                (plans, projected)
            } else {
                let plans = group
                    .iter()
                    .map(|index| store.plan_capture(inputs[*index].request.clone()))
                    .collect::<Result<Vec<_>>>()?;
                let projected = project_capture_batch(
                    &self.fs,
                    &reader,
                    plans.clone(),
                    &RefreshProjectionLimits::default(),
                )?;
                (plans, Some(projected))
            };
            for index in group {
                inputs[*index].recheck()?;
            }
            let Some(projected) = projected else {
                return Ok(CaptureGroupOutcome {
                    plans,
                    committed: false,
                    error: None,
                });
            };
            drop(reader);
            let mut session = IndexedRefreshSession::prepare_write(&catalog, writer, projected)?;
            let change = session.proof().change.clone();
            let result = self
                .engine()?
                .apply_indexed_refresh(writer, &mut session)
                .map_err(|error| super::offline::retained_error(error, &change))
                .and_then(|report| {
                    if report.status == ChangeStatus::Committed {
                        Ok(())
                    } else {
                        Err(conflict("capture group did not commit"))
                    }
                });
            let error = self.capture_publication_result(&change.change_id, result)?;
            Ok(CaptureGroupOutcome {
                plans,
                committed: true,
                error,
            })
        } else {
            let base = source_view(self)?;
            let reads = GroupReads::new(&self.fs);
            let view = SourceView::from_shared_maintenance(&self.fs, base.notes.shared(), &reads);
            let plans = group
                .iter()
                .map(|index| {
                    let input = &inputs[*index];
                    if refresh {
                        let bound = &origins[&input.origin];
                        let (_, note) = view.resolve(&bound.id, RecordKind::Source, None)?;
                        if note.source_hash != bound.hash {
                            return Err(conflict("captured Source changed since original lookup"));
                        }
                        store.plan_refresh_in_view(&view, &bound.id, input.request.clone(), None)
                    } else {
                        store.plan_capture(input.request.clone())
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            view.require_consistent()?;
            for index in group {
                inputs[*index].recheck()?;
            }
            let Some(draft) = combine_drafts(&plans)? else {
                return Ok(CaptureGroupOutcome {
                    plans,
                    committed: false,
                    error: None,
                });
            };
            let report = match self.execute_draft_with_writer(draft, false, writer) {
                Ok(report) => report,
                Err(error) => {
                    let id = error
                        .details
                        .get("change")
                        .and_then(|change| change.get("change_id"))
                        .and_then(serde_json::Value::as_str)
                        .map(RecordId::new)
                        .transpose()?;
                    let Some(id) = id else {
                        return Err(error);
                    };
                    let error = self.capture_publication_result(&id, Err(error))?;
                    return Ok(CaptureGroupOutcome {
                        plans,
                        committed: true,
                        error,
                    });
                }
            };
            if report.status != Some(ChangeStatus::Committed) {
                return Err(conflict("capture group did not commit"));
            }
            let error = self.capture_publication_result(
                &report.change.expect("committed Change").change_id,
                Ok(()),
            )?;
            Ok(CaptureGroupOutcome {
                plans,
                committed: true,
                error,
            })
        }
    }

    /// A returned-error acknowledgement cannot erase a durably committed group.
    /// Keep its real IDs/owners and stop later groups; ordinary retry observes it.
    fn capture_publication_result(
        &self,
        change_id: &RecordId,
        result: Result<()>,
    ) -> Result<Option<WikiError>> {
        #[cfg(test)]
        let result = result.and_then(|()| {
            FAIL_AFTER_GROUP.with(|fault| match fault.get() {
                Some(0) => {
                    fault.set(None);
                    Err(conflict(
                        "injected returned error after committed capture group",
                    ))
                }
                Some(left) => {
                    fault.set(Some(left - 1));
                    Ok(())
                }
                None => Ok(()),
            })
        });
        match result {
            Ok(()) => Ok(None),
            Err(error) => {
                if self.engine()?.inspect(change_id)?.status == ChangeStatus::Committed {
                    Ok(Some(error))
                } else {
                    Err(error)
                }
            }
        }
    }

    fn authenticate_capture_outcomes(&self, outcome: &mut SourceCaptureOutcome) -> Result<()> {
        let view = source_view(self)?;
        let mut dependencies: BTreeMap<_, _> = view
            .notes
            .iter()
            .map(|(path, note)| (path.clone(), ExpectedState::Hash(note.source_hash.clone())))
            .collect();
        for item in outcome.items.iter_mut().filter(|item| {
            matches!(
                item.action,
                SourceCaptureAction::Captured
                    | SourceCaptureAction::Refreshed
                    | SourceCaptureAction::Unchanged
            )
        }) {
            if item.capture_state != Some(SourceCaptureState::Complete) {
                continue;
            }
            let source = item.source_id.as_ref().expect("completed source");
            let revision = item.revision_id.as_ref().expect("completed revision");
            let content = view.revision_content_bounded(
                source,
                revision,
                &mut dependencies,
                16 * 1024 * 1024,
                16 * 1024 * 1024,
            )?;
            if content.is_empty() {
                continue;
            }
            if matches!(
                item.action,
                SourceCaptureAction::Captured | SourceCaptureAction::Refreshed
            ) {
                let (path, note) = view.resolve(revision, RecordKind::Revision, None)?;
                let parent = path
                    .as_str()
                    .rsplit_once('/')
                    .ok_or_else(|| conflict("captured revision has no parent"))?
                    .0;
                let relative = note
                    .canonical
                    .as_ref()
                    .expect("resolved revision")
                    .string("wiki_content_path")
                    .expect("complete content path");
                outcome
                    .changed_owners
                    .push(VaultRelativePath::new(format!("{parent}/{relative}"))?);
            }
            let reference = CitationRef::Source(SourceSpanRef {
                source_id: source.clone(),
                source_revision: revision.clone(),
                span: ByteSpan::new(0, content.len() as u64)?,
                quote_hash: Blake3Hash::digest(&content),
            });
            let verified = view.verify(&reference, CitationScope::Current)?;
            for dependency in verified.dependencies {
                if dependencies
                    .insert(dependency.path, dependency.expected.clone())
                    .is_some_and(|old| old != dependency.expected)
                {
                    return Err(conflict("capture citation dependencies disagree"));
                }
            }
            item.citation = Some(ReadSourceCitation {
                citation: verified.citation,
                eligibility: Eligibility::Current,
            });
        }
        for (path, expected) in dependencies {
            let bytes = crate::vault::fs::read_regular_bounded(
                self.fs.root(),
                &path,
                false,
                64 * 1024 * 1024,
                "capture citation recheck",
            )?;
            let actual = bytes.as_ref().map_or(ExpectedState::Absent, |bytes| {
                ExpectedState::Hash(Blake3Hash::digest(bytes))
            });
            if actual != expected {
                return Err(conflict(
                    "capture citation dependencies changed before output",
                ));
            }
        }
        let mut entries = 0usize;
        let membership = self
            .fs
            .root()
            .scan_markdown_limited(4096, &mut || {
                entries += 1;
                if entries > 65536 {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "capture citation membership exceeds its allowance",
                    ));
                }
                Ok(())
            })?
            .into_iter()
            .filter(crate::sources::revision::canonical_path)
            .collect::<BTreeSet<_>>();
        if membership != view.notes.keys().cloned().collect() {
            return Err(conflict(
                "canonical identity membership changed before capture output",
            ));
        }
        self.catalog().guard_current(None)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "source_capture_tests.rs"]
mod tests;

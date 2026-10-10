//! Caller-owned guarded requests; one retained Change publishes the final overlay.
use super::source_refresh_batch_types::*;
use super::{
    OfflineApp, SourceRefreshBatchOutcome, SourceRefreshBatchRequest, SourceRefreshedItem,
};
use crate::{
    catalog::{
        query_types::QueryReadLimits,
        source_projection::{RefreshProjectionLimits, project_refresh_batch},
        source_refresh::IndexedRefreshSession,
    },
    changes::{ChangeStatus, prepare::strict_json},
    domain::{Blake3Hash, ErrorCode, Result, WikiError},
    records::parse_note,
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceRefreshLimits, SourceStore},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

fn usage(message: &str) -> WikiError {
    WikiError::new(ErrorCode::Usage, message)
}
fn budget() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "source refresh batch exceeds its cumulative byte or time allowance",
    )
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}

fn external_input(path: &Path, max: usize) -> Result<(Vec<u8>, PathBuf)> {
    if path == Path::new("-") {
        return Err(usage(
            "source refresh batch requires regular files; standard input is unavailable",
        ));
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| usage(&format!("source refresh batch input: {error}")))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(usage(
            "source refresh batch input must be a regular non-symlink file",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|error| usage(&format!("source refresh batch input: {error}")))?;
    if !file
        .metadata()
        .map_err(|error| usage(&error.to_string()))?
        .is_file()
    {
        return Err(usage("source refresh batch input changed file type"));
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| usage(&error.to_string()))?;
    if bytes.len() > max {
        return Err(budget());
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| usage(&error.to_string()))?;
    Ok((bytes, canonical))
}

impl OfflineApp {
    /// Relative input paths resolve from this explicit request's directory.
    pub fn source_refresh_batch_file(&self, file: &Path) -> Result<SourceRefreshBatchOutcome> {
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::UpdateInputPlanning,
            || {
                let (bytes, canonical) =
                    external_input(file, MAX_SOURCE_REFRESH_BATCH_REQUEST_BYTES)?;
                let request: SourceRefreshBatchRequest = strict_json(&bytes)?;
                self.source_refresh_batch(request, canonical.parent().expect("request file parent"))
            },
        )
    }

    /// Stage freezes captured bytes; applying the retained Change never rereads these external files.
    pub fn source_refresh_batch(
        &self,
        request: SourceRefreshBatchRequest,
        input_dir: &Path,
    ) -> Result<SourceRefreshBatchOutcome> {
        self.source_refresh_batch_bounded(request, input_dir, RefreshProjectionLimits::default())
    }

    pub(super) fn source_refresh_batch_bounded(
        &self,
        request: SourceRefreshBatchRequest,
        input_dir: &Path,
        ceiling: RefreshProjectionLimits,
    ) -> Result<SourceRefreshBatchOutcome> {
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::UpdateInputPlanning,
            || {
                if request.items.is_empty() || request.items.len() > MAX_SOURCE_REFRESH_BATCH_ITEMS
                {
                    return Err(usage(
                        "source refresh batch requires 1–16 distinct existing Sources",
                    ));
                }
                let mut ids = BTreeSet::new();
                let mut paths = BTreeSet::new();
                let mut inputs = Vec::new();
                let mut total = 0usize;
                for (ordinal, item) in request.items.into_iter().enumerate() {
                    if item.file == Path::new("-") {
                        return Err(usage(
                            "source refresh batch member cannot use the standard-input alias",
                        ));
                    }
                    if !ids.insert(item.source_id.clone())
                        || item.source_id == item.expected_revision
                    {
                        return Err(usage(
                            "source refresh batch has duplicate or overlapping identities",
                        ));
                    }
                    if item
                        .title
                        .as_ref()
                        .is_some_and(|title| title.trim().is_empty() || title.len() > 4096)
                        || item
                            .media_type
                            .as_ref()
                            .is_some_and(|media| media.len() > 512)
                    {
                        return Err(usage(
                            "source refresh batch title or media type exceeds its bound",
                        ));
                    }
                    let path = if item.file.is_absolute() {
                        item.file.clone()
                    } else {
                        input_dir.join(&item.file)
                    };
                    let (original, canonical) = external_input(
                        &path,
                        MAX_SOURCE_REFRESH_BATCH_INPUT_BYTES.saturating_sub(total),
                    )?;
                    if !paths.insert(canonical.clone()) {
                        return Err(usage(
                            "source refresh batch input paths alias another member",
                        ));
                    }
                    if Blake3Hash::digest(&original) != item.input_hash {
                        return Err(conflict(
                            "source refresh batch input bytes differ from input_hash",
                        ));
                    }
                    total = total.checked_add(original.len()).ok_or_else(budget)?;
                    let extraction = if crate::sources::local_text::local_text_candidate(&path)
                        && std::str::from_utf8(&original).is_ok()
                    {
                        ExtractionInput::Utf8Preserve
                    } else {
                        ExtractionInput::Unsupported {
                            extractor: "unsupported-local-format-v1".into(),
                            fingerprint: Blake3Hash::digest(b"unsupported-local-format-v1"),
                        }
                    };
                    let capture = CaptureRequest {
                        title: item.title.clone().unwrap_or_else(|| {
                            path.file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("Captured source")
                                .to_owned()
                        }),
                        origin_kind: SourceOrigin::LocalFile,
                        origin: path
                            .to_str()
                            .ok_or_else(|| usage("source refresh batch path must be UTF-8"))?
                            .to_owned(),
                        original,
                        extraction,
                        media_type: item.media_type.clone(),
                    };
                    inputs.push((ordinal, item, capture));
                }
                let mut outcome = SourceRefreshBatchOutcome {
                    items: inputs
                        .iter()
                        .map(|(ordinal, item, _)| SourceRefreshedItem {
                            ordinal: *ordinal,
                            source_id: item.source_id.clone(),
                            previous_revision_id: None,
                            revision_id: None,
                            reused: None,
                            no_op: None,
                            capture_state: None,
                        })
                        .collect(),
                    plan: super::offline::summarize(
                        "Refresh Sources (selected checks unresolved in preview)",
                        &[],
                        &[],
                    ),
                    change: None,
                    status: None,
                    snapshot: None,
                    dry_run: self.options.dry_run,
                };
                if self.options.dry_run {
                    return Ok(outcome);
                }
                let catalog = self.catalog();
                if catalog.operation_state()?.is_none() {
                    return Err(WikiError::new(
                        ErrorCode::CapabilityUnavailable,
                        "source refresh batch requires the normalized catalog; run index rebuild --normalized first",
                    ));
                }
                catalog.guard_query()?;
                let reader = catalog.query_snapshot(QueryReadLimits::default())?;
                reader.require_fact_layout()?;
                let started = Instant::now();
                let mut remaining = ceiling
                    .max_canonical_bytes
                    .checked_sub(total.checked_mul(2).ok_or_else(budget)?)
                    .ok_or_else(budget)?;
                inputs.sort_by(|left, right| left.1.source_id.cmp(&right.1.source_id));
                let store = SourceStore::new(self.fs.clone());
                let mut plans = Vec::new();
                let mut guards = BTreeMap::new();
                for (ordinal, item, capture) in inputs {
                    if remaining == 0 || started.elapsed() >= ceiling.max_elapsed {
                        return Err(budget());
                    }
                    // Reserve generated payloads and conservatively bounded Source/title
                    // reserialization before the scalar planner allocates its proposal.
                    // JSON escaping grows a string by at most six times; the existing
                    // Source and revision title together fit below this sixteenfold
                    // canonical-read allowance. This bulk route can refuse large selected
                    // records earlier than scalar refresh, which keeps its own limits.
                    let reserve = capture
                        .original
                        .len()
                        .checked_mul(2)
                        .and_then(|bytes| bytes.checked_add(64 * 1024))
                        .ok_or_else(budget)?;
                    let planning_reads = remaining.checked_sub(reserve).ok_or_else(budget)? / 16;
                    if planning_reads == 0 {
                        return Err(budget());
                    }
                    let limits = SourceRefreshLimits {
                        max_canonical_bytes: planning_reads,
                        ..Default::default()
                    };
                    let plan = store.plan_refresh_indexed(
                        &reader,
                        &item.source_id,
                        capture,
                        item.title.as_deref(),
                        &limits,
                    )?;
                    let source_path = format!("sources/{}/source.md", item.source_id);
                    let document = plan
                        .captured
                        .iter()
                        .find(|document| document.path.as_str() == source_path)
                        .ok_or_else(|| {
                            conflict("source refresh batch lacks an authenticated Source envelope")
                        })?;
                    let note = parse_note(&document.bytes);
                    let source = note.canonical.ok_or_else(|| {
                        conflict("source refresh batch Source envelope is invalid")
                    })?;
                    if document.hash != item.if_match
                        || plan.previous_revision != item.expected_revision
                    {
                        return Err(conflict(
                            "source refresh batch Source hash or current head differs from its guard",
                        ));
                    }
                    if source.string("wiki_status") == Some("withdrawn") {
                        return Err(conflict(
                            "source refresh batch cannot reactivate a withdrawn Source",
                        ));
                    }
                    let captured_bytes = plan.captured.iter().try_fold(0usize, |total, doc| {
                        total.checked_add(doc.bytes.len()).ok_or_else(budget)
                    })?;
                    let proposed_bytes = plan.plan.draft.as_ref().map_or(Ok(0usize), |draft| {
                        draft.operations.iter().try_fold(0usize, |total, op| {
                            total
                                .checked_add(op.proposed.as_ref().map_or(0, Vec::len))
                                .ok_or_else(budget)
                        })
                    })?;
                    remaining = remaining
                        .checked_sub(
                            captured_bytes
                                .checked_mul(2)
                                .ok_or_else(budget)?
                                .checked_add(proposed_bytes)
                                .ok_or_else(budget)?,
                        )
                        .ok_or_else(budget)?;
                    for dependency in &plan.plan.dependencies {
                        if guards
                            .insert(dependency.path.clone(), dependency.expected.clone())
                            .is_some_and(|before| before != dependency.expected)
                        {
                            return Err(conflict(
                                "source refresh batch selected read guards disagree",
                            ));
                        }
                    }
                    outcome.items[ordinal] = SourceRefreshedItem {
                        ordinal,
                        source_id: plan.plan.source_id.clone(),
                        previous_revision_id: Some(plan.previous_revision.clone()),
                        revision_id: Some(plan.plan.revision_id.clone()),
                        reused: Some(plan.plan.reused),
                        no_op: Some(plan.plan.draft.is_none()),
                        capture_state: plan.plan.capture_state,
                    };
                    plans.push(plan);
                }
                let elapsed = started.elapsed();
                if elapsed >= ceiling.max_elapsed || remaining == 0 {
                    return Err(budget());
                }
                let limits = RefreshProjectionLimits {
                    max_canonical_bytes: remaining,
                    max_elapsed: ceiling.max_elapsed - elapsed,
                    ..ceiling
                };
                let Some(projected) = crate::maintenance_diagnostic::observe(
                    crate::maintenance_diagnostic::Phase::UpdateProjection,
                    || project_refresh_batch(&self.fs, &reader, plans, &limits),
                )?
                else {
                    if started.elapsed() >= ceiling.max_elapsed {
                        return Err(budget());
                    }
                    let dependencies = guards
                        .into_iter()
                        .map(|(path, expected)| crate::changes::ReadDependency { path, expected })
                        .collect::<Vec<_>>();
                    outcome.plan =
                        super::offline::summarize("Unchanged Sources", &dependencies, &[]);
                    return Ok(outcome);
                };
                outcome.plan = super::offline::summarize(
                    &projected.draft().title,
                    &projected.draft().read_preconditions,
                    &projected.draft().operations,
                );
                if started.elapsed() >= ceiling.max_elapsed {
                    return Err(budget());
                }
                drop(reader);
                let writer = self.writer()?;
                catalog.guard_current(None)?;
                if started.elapsed() >= ceiling.max_elapsed {
                    return Err(budget());
                }
                let mut session =
                    IndexedRefreshSession::prepare_write(&catalog, &writer, projected)?;
                let change = session.proof().change.clone();
                outcome.change = Some(change.clone());
                if self.options.stage_only {
                    outcome.status = Some(ChangeStatus::Prepared);
                    return Ok(outcome);
                }
                let report = self
                    .engine()?
                    .apply_indexed_refresh(&writer, &mut session)
                    .map_err(|error| super::offline::retained_error(error, &change))?;
                outcome.status = Some(report.status);
                outcome.snapshot = report.snapshot;
                Ok(outcome)
            },
        )
    }
}

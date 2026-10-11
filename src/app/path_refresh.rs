//! Path-selected Source refresh over the existing selected planner/publication.
use super::{MutationOutcome, OfflineApp};
use crate::{
    changes::{ChangeDraft, ChangeStatus},
    domain::{ErrorCode, Result, VaultRelativePath, WikiError},
    sources::{CaptureRequest, SourcePlan, SourceStore, path_refresh::SourcePathSelection},
    vault::ExpectedState,
};
use std::collections::BTreeMap;

#[derive(Debug, serde::Serialize)]
pub(crate) struct SourceRefreshSelectionScope {
    pub schema_version: &'static str,
    pub selection: &'static str,
    pub index_generation: Option<u64>,
    pub global_membership_verified: bool,
    pub selected_dependencies_verified: bool,
}

impl OfflineApp {
    /// The explicit input is already captured/guarded; a remembered origin is
    /// never followed. The named Source path remains part of the retained proof.
    pub fn source_refresh_path_with_title(
        &self,
        path: VaultRelativePath,
        request: CaptureRequest,
        title: Option<&str>,
    ) -> Result<MutationOutcome> {
        self.source_refresh_path_with_scope(path, request, title)
            .map(|(outcome, _)| outcome)
    }

    pub(crate) fn source_refresh_path_with_scope(
        &self,
        path: VaultRelativePath,
        request: CaptureRequest,
        title: Option<&str>,
    ) -> Result<(MutationOutcome, SourceRefreshSelectionScope)> {
        if request.original.len() > super::MAX_INPUT_BYTES {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "Source input exceeds 16 MiB",
            ));
        }
        let catalog = self.catalog();
        let store = SourceStore::new(self.fs.clone());
        if catalog.operation_state()?.is_none() {
            let selected = store.select_refresh_path(&path)?;
            let plan = store.plan_refresh_with_title(&selected.source_id, request, title)?;
            require_selected_plan(&selected, &plan)?;
            let mut draft = plan.draft.unwrap_or_else(|| ChangeDraft {
                title: "Unchanged source".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: Default::default(),
                read_preconditions: plan.dependencies.clone(),
                operations: vec![],
            });
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
            return Ok((
                outcome,
                SourceRefreshSelectionScope {
                    schema_version: "lwiki.source-refresh-selection.v1",
                    selection: "bounded_canonical_scan",
                    index_generation: None,
                    global_membership_verified: true,
                    selected_dependencies_verified: true,
                },
            ));
        }
        if self.options.dry_run {
            // Opening a WAL reader can write coordination state. Preserve the
            // established preview boundary and report unresolved path admission.
            let _ = store.plan_capture(request)?;
            return Ok((
                MutationOutcome {
                    source_capture: None,
                    plan: super::offline::summarize(
                        "Refresh Source path (target unresolved in preview)",
                        &[],
                        &[],
                    ),
                    allocated_ids: BTreeMap::new(),
                    change: None,
                    status: None,
                    snapshot: None,
                    reused: false,
                },
                SourceRefreshSelectionScope {
                    schema_version: "lwiki.source-refresh-selection.v1",
                    selection: "unresolved_preview",
                    index_generation: None,
                    global_membership_verified: false,
                    selected_dependencies_verified: false,
                },
            ));
        }
        use crate::catalog::{
            query_types::QueryReadLimits,
            source_projection::{RefreshProjectionLimits, project_refresh},
            source_refresh::IndexedRefreshSession,
        };
        catalog.guard_query()?;
        let reader = catalog.query_snapshot(QueryReadLimits::default())?;
        reader.require_fact_layout()?;
        let scope = SourceRefreshSelectionScope {
            schema_version: "lwiki.source-refresh-selection.v1",
            selection: "published_index_and_selected_dependencies",
            index_generation: Some(
                crate::catalog::query_types::QueryCatalog::snapshot(&reader).generation,
            ),
            global_membership_verified: false,
            selected_dependencies_verified: true,
        };
        let selected = store.select_refresh_path_indexed(&reader, &path)?;
        let plan = store.plan_refresh_indexed(
            &reader,
            &selected.source_id,
            request,
            title,
            &crate::sources::SourceRefreshLimits::default(),
        )?;
        require_selected_plan(&selected, &plan.plan)?;
        let mut outcome = MutationOutcome {
            source_capture: plan.plan.capture_state,
            plan: super::offline::summarize("Unchanged source", &plan.plan.dependencies, &[]),
            allocated_ids: BTreeMap::from([
                ("source".into(), plan.plan.source_id.clone()),
                ("revision".into(), plan.plan.revision_id.clone()),
            ]),
            change: None,
            status: None,
            snapshot: None,
            reused: plan.plan.reused,
        };
        let Some(projected) =
            project_refresh(&self.fs, &reader, plan, &RefreshProjectionLimits::default())?
        else {
            return Ok((outcome, scope));
        };
        drop(reader);
        let draft = projected.draft();
        outcome.plan =
            super::offline::summarize(&draft.title, &draft.read_preconditions, &draft.operations);
        let writer = self.writer()?;
        catalog.guard_current(None)?;
        let mut session = IndexedRefreshSession::prepare_projected(&catalog, &writer, projected)?;
        let change = session.proof().change.clone();
        outcome.change = Some(change.clone());
        if self.options.stage_only {
            outcome.status = Some(ChangeStatus::Prepared);
            return Ok((outcome, scope));
        }
        let report = self
            .engine()?
            .apply_indexed_refresh(&writer, &mut session)
            .map_err(|error| super::offline::retained_error(error, &change))?;
        outcome.status = Some(report.status);
        outcome.snapshot = report.snapshot;
        Ok((outcome, scope))
    }
}

fn require_selected_plan(selected: &SourcePathSelection, plan: &SourcePlan) -> Result<()> {
    if plan.source_id != selected.source_id
        || !plan.dependencies.iter().any(|dependency| {
            dependency.path == selected.path
                && dependency.expected == ExpectedState::Hash(selected.hash.clone())
        })
        || plan.draft.as_ref().is_some_and(|draft| {
            draft.operations.iter().any(|operation| {
                operation.target == selected.path
                    && operation.expected != ExpectedState::Hash(selected.hash.clone())
            })
        })
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "Source path selection changed before refresh planning",
        ));
    }
    Ok(())
}

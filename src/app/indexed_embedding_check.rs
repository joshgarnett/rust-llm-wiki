//! Read-only complete coverage over bounded canonical scopes and one vector view.
use super::*;
use crate::catalog::query_types::{QueryCatalog, QueryReadLimits};
use crate::retrieval::vectors::VectorCoverageCheck;
use std::time::Instant;

const MAX_CHECK_OWNERS: usize = 4096;

impl OfflineApp {
    pub(super) fn embeddings_coverage_indexed(
        &self,
        candidate: Option<SpaceSpec>,
    ) -> Result<(
        Option<SpaceSpec>,
        Option<Blake3Hash>,
        Option<Blake3Hash>,
        Coverage,
    )> {
        self.embeddings_coverage_indexed_observed(candidate, |_| Ok(()))
    }
    fn embeddings_coverage_indexed_observed(
        &self,
        candidate: Option<SpaceSpec>,
        mut completed_scope: impl FnMut(usize) -> Result<()>,
    ) -> Result<(
        Option<SpaceSpec>,
        Option<Blake3Hash>,
        Option<Blake3Hash>,
        Coverage,
    )> {
        let deadline = Instant::now() + Duration::from_secs(120);
        let initial_phase = Self::embedding_phase()?;
        let mut vectors = match VectorCoverageCheck::open(&self.fs, &initial_phase) {
            Ok(store) => Some(store),
            Err(error) if error.code == ErrorCode::OfflineUnavailable => None,
            Err(error) => return Err(error),
        };
        let active = vectors.as_ref().and_then(|store| store.active()).cloned();
        let active_id = active.as_ref().map(|state| state.id.clone());
        let spec = candidate.or_else(|| active.map(|state| state.spec));
        let Some(selected_spec) = &spec else {
            return Ok((None, None, active_id, Coverage::default()));
        };
        let space = selected_spec.id()?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let mut snapshot = None;
        let mut after = None;
        let mut visited = 0usize;
        let mut coverage = Coverage::default();
        loop {
            if Instant::now() >= deadline {
                return Err(incomplete(
                    visited,
                    coverage.eligible_units,
                    "command deadline",
                ));
            }
            let phase = Self::embedding_phase()?;
            let mut limits = QueryReadLimits::default();
            limits.max_elapsed_ms = limits.max_elapsed_ms.min(phase.remaining_ms()?);
            let reader = catalog.cached_query_snapshot(limits)?;
            if snapshot
                .as_ref()
                .is_some_and(|old| old != reader.snapshot())
            {
                return Err(fail(
                    ErrorCode::FreshnessConflict,
                    "catalog publication changed during embedding coverage check",
                ));
            }
            snapshot = Some(reader.snapshot().clone());
            // At the owner ceiling, one bounded discovery row distinguishes an
            // exactly complete audit from an unvisited suffix. Never report it
            // as missing vectors or a successful full audit.
            let limit = if visited == MAX_CHECK_OWNERS {
                1
            } else {
                indexed_embedding_inputs::PROOF_SCOPE_OWNERS
            };
            let paths = reader.embedding_document_paths(after.as_ref(), limit)?;
            reader.verify_operations(&catalog)?;
            phase.remaining_ms()?;
            if paths.is_empty() {
                break;
            }
            if visited == MAX_CHECK_OWNERS {
                return Err(incomplete(
                    visited,
                    coverage.eligible_units,
                    "owner ceiling",
                ));
            }
            let mut inputs = indexed_embedding_inputs::materialize(
                &catalog,
                &selected_spec.settings,
                Some(&paths),
                &Self::embedding_phase_proof_budget(&phase)?,
            )?;
            if &inputs.snapshot != reader.snapshot() {
                return Err(fail(
                    ErrorCode::FreshnessConflict,
                    "catalog publication changed within embedding coverage scope",
                ));
            }
            let checked = match &mut vectors {
                Some(store) => store.coverage(&space, &inputs.units, &phase)?,
                None => Coverage {
                    eligible_units: inputs.units.len(),
                    missing_units: inputs.units.len(),
                    ..Default::default()
                },
            };
            inputs.recheck(&catalog)?;
            reader.verify_operations(&catalog)?;
            phase.remaining_ms()?;
            coverage.eligible_units += checked.eligible_units;
            coverage.available_units += checked.available_units;
            coverage.missing_units += checked.missing_units;
            coverage.corrupt_units += checked.corrupt_units;
            coverage.pending_units += checked.pending_units;
            visited += paths.len();
            after = paths.last().cloned();
            completed_scope(visited)?;
            if paths.len() < limit {
                break;
            }
        }
        let final_phase = Self::embedding_phase()?;
        let mut limits = QueryReadLimits::default();
        limits.max_elapsed_ms = limits.max_elapsed_ms.min(final_phase.remaining_ms()?);
        let final_reader = catalog.cached_query_snapshot(limits)?;
        if snapshot.as_ref() != Some(final_reader.snapshot()) {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "catalog publication changed before embedding coverage emission",
            ));
        }
        final_reader.verify_operations(&catalog)?;
        final_phase.remaining_ms()?;
        if Instant::now() >= deadline {
            return Err(incomplete(
                visited,
                coverage.eligible_units,
                "command deadline",
            ));
        }
        Ok((spec, Some(space), active_id, coverage))
    }
}

fn incomplete(owners: usize, units: usize, reason: &str) -> WikiError {
    let mut error = fail(
        ErrorCode::BudgetExceeded,
        "embedding coverage check is incomplete; full coverage was not established",
    );
    error.details = serde_json::json!({"reason":reason, "checked_owners":owners,
        "checked_units":units, "remaining_owners":"unavailable", "complete":false,
        "max_owners":MAX_CHECK_OWNERS, "max_elapsed_ms":120000});
    error
}

#[cfg(test)]
#[path = "../../tests/fixtures/p17/common.rs"]
mod check_common;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_counts_scopes_and_refuses_intermediate_or_final_publication_drift() {
        for drift_at in [None, Some(16), Some(17)] {
            let fixture = check_common::Fixture::new();
            for index in 0..17 {
                std::fs::write(
                    fixture.fs.root().path().join(format!("note-{index:02}.md")),
                    format!("Coverage note {index} with exact content.\n"),
                )
                .unwrap();
            }
            fixture.app.index_rebuild_normalized().unwrap();
            let mut scopes = Vec::new();
            let result =
                fixture
                    .app
                    .embeddings_coverage_indexed_observed(Some(fixture.spec()), |visited| {
                        scopes.push(visited);
                        if drift_at == Some(visited) {
                            std::fs::write(
                                fixture.fs.root().path().join("new-note.md"),
                                "New publication.\n",
                            )
                            .unwrap();
                            fixture.app.index_sync(false)?;
                        }
                        Ok(())
                    });
            if drift_at.is_some() {
                assert_eq!(result.unwrap_err().code, ErrorCode::FreshnessConflict);
            } else {
                let (_, _, _, coverage) = result.unwrap();
                assert_eq!(scopes, vec![16, 17]);
                assert_eq!(coverage.eligible_units, 17);
                assert_eq!(coverage.missing_units, 17);
                assert_eq!(coverage.available_units, 0);
                assert_eq!(coverage.pending_units, 0);
            }
        }
    }
}

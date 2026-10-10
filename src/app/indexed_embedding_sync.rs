//! Paged normalized preparation. Compatible vectors survive owner publications.
use super::*;
use crate::{
    catalog::query_types::{QueryCatalog, QueryReadLimits},
    retrieval::unit_inventory_types::*,
};
use std::time::Instant;

const PAGE_OWNERS: usize = 128;
const MAX_PAGES: usize = 32;
const MAX_COUNTED_INPUTS: usize = 65_536;

fn changed(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}

/// Advance exactly the successfully published prefix. Catch-up starts only once
/// the original inclusive inventory interval has been completely acknowledged.
fn acknowledged_prefix_cursor(
    cursor: &PreparationCursor,
    bindings: &[UnitOwnerBinding],
    interval_complete: bool,
    current_generation: u64,
) -> PreparationCursor {
    let mut next = cursor.clone();
    if let Some(binding) = bindings.last() {
        next.after = Some(UnitOwnerCursor {
            modified_seq: binding.modified_seq,
            owner: binding.owner.clone(),
        });
    }
    next.complete = interval_complete;
    if interval_complete && current_generation > next.through_seq {
        next.since_seq = next.through_seq;
        next.through_seq = current_generation;
        next.after = None;
        next.complete = false;
    }
    next
}

impl OfflineApp {
    /// Each acknowledged page is durable. An invocation that reaches its finite
    /// page/deadline allowance returns partial progress and can be resumed.
    pub(super) fn embeddings_sync_indexed(
        &self,
        settings: &EmbeddingSettings,
        runtime: Option<&EmbeddingRuntime<'_>>,
    ) -> Result<EmbeddingReport> {
        settings.validate()?;
        let deadline = Instant::now() + Duration::from_secs(120);
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let old = match VectorStore::open(&self.fs, None) {
            Ok(store) => store.active()?,
            Err(e) if e.code == ErrorCode::OfflineUnavailable && runtime.is_some() => None,
            Err(e) => return Err(e),
        };
        let spec = match runtime {
            Some(r) => SpaceSpec::from_service(r.service, settings.clone())?,
            None => {
                let state = old.as_ref().ok_or_else(|| fail(ErrorCode::OfflineUnavailable,
                    "cache-only preparation needs a retained active space; missing blobs cannot be recovered from receipts"))?;
                if &state.spec.settings != settings {
                    return Err(fail(
                        ErrorCode::CapabilityUnavailable,
                        "cache-only sync settings differ from the retained active space",
                    ));
                }
                state.spec.clone()
            }
        };
        let space = spec.id()?;
        let mut report = EmbeddingReport {
            space: Some(space.clone()),
            active_space: old.as_ref().map(|x| x.id.clone()),
            settings: Some(settings.clone()),
            coverage: Coverage::default(),
            generated_inputs: 0,
            reused_inputs: 0,
            published: false,
            dry_run: false,
            network_used: false,
            run_id: None,
            warnings: vec!["Generated/reused input counts describe this invocation's selected work, excluding unchanged acknowledged owners.".into()],
        };
        let mut counted_inputs = BTreeSet::new();
        // Received/staged work is reconciled before admitting any new paid task.
        if let Some(runtime) = runtime {
            let recovered = self.recover_embedding_jobs(&spec, runtime)?;
            report.generated_inputs += recovered.generated;
            report.network_used |= recovered.generated > 0;
            report.run_id = recovered.run_id;
            report.warnings.extend(recovered.warnings);
            counted_inputs.extend(recovered.generated_hashes);
        }
        let mut inventory = None;
        for _ in 0..MAX_PAGES {
            if Instant::now() >= deadline {
                break;
            }
            let writer = self.embedding_writer()?;
            let page = catalog.prepare_unit_inventory_page(&writer, settings, PAGE_OWNERS)?;
            let complete = page.complete;
            inventory = Some(page);
            if complete {
                break;
            }
        }
        let Some(inventory) = inventory else {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "unit inventory registration deadline exceeded",
            ));
        };
        if !inventory.complete {
            report.coverage.eligible_units = inventory.unit_count;
            report.coverage.missing_units = inventory.unit_count;
            report.warnings.push("Unit inventory backfill is incomplete; repeat embeddings sync to resume its durable owner cursor. Semantic discovery remains unavailable until backfill completes.".into());
            return Ok(report);
        }
        let policy = inventory.policy;
        let initial = catalog.cached_query_snapshot(QueryReadLimits::default())?;
        let incarnation = catalog_incarnation(initial.vault_id(), initial.snapshot())?;
        let store = match VectorStore::open(&self.fs, None) {
            Ok(store) => Some(store),
            Err(e) if e.code == ErrorCode::OfflineUnavailable => None,
            Err(e) => return Err(e),
        };
        let retained = store
            .as_ref()
            .map(|s| s.preparation_cursor(&space, &policy, &incarnation))
            .transpose()?
            .flatten();
        let mut cursor = retained.unwrap_or(PreparationCursor {
            version: INVENTORY_VERSION,
            incarnation: incarnation.clone(),
            policy: policy.clone(),
            since_seq: 0,
            through_seq: initial.snapshot().generation,
            after: None,
            complete: false,
        });
        if cursor.complete && cursor.through_seq < initial.snapshot().generation {
            cursor.since_seq = cursor.through_seq;
            cursor.through_seq = initial.snapshot().generation;
            cursor.after = None;
            cursor.complete = false;
        }
        drop(store);
        drop(initial);
        let mut acknowledged_units = 0usize;
        'pages: for _ in 0..MAX_PAGES {
            if Instant::now() >= deadline {
                report.warnings.push("Preparation reached its command deadline; repeat embeddings sync to resume acknowledged owner pages.".into());
                break;
            }
            let phase = Self::embedding_phase()?;
            let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
            if catalog_incarnation(reader.vault_id(), reader.snapshot())? != incarnation {
                return Err(changed(
                    "catalog rebuilt during preparation; retained compatible vectors remain reusable",
                ));
            }
            let state = reader
                .unit_inventory_state(&policy)?
                .ok_or_else(|| changed("registered unit inventory disappeared"))?;
            let retained_available = if cursor.since_seq > 0 {
                state
                    .unit_count
                    .saturating_sub(reader.unit_changed_count(&policy, cursor.since_seq)?)
            } else {
                0
            };
            if !state.complete {
                return Err(changed("registered unit inventory became incomplete"));
            }
            let through = cursor.through_seq.min(reader.snapshot().generation);
            let bindings = reader.unit_owner_bindings_page(
                &policy,
                cursor.after.as_ref(),
                cursor.since_seq,
                through,
                PAGE_OWNERS,
            )?;
            let last = bindings.len() < PAGE_OWNERS;
            let writer = self.embedding_writer()?;
            let mut store = VectorStore::open(&self.fs, Some(&writer))?;
            store.prepare_space(&spec)?;
            store.bind_read_budget(&phase)?;
            let mut selected = Vec::new();
            let mut page_ready_units = 0usize;
            for binding in &bindings {
                if !binding.tombstone && !store.owner_binding_ready(&space, binding)? {
                    selected.push(binding.owner.clone());
                } else if !binding.tombstone {
                    page_ready_units = page_ready_units.saturating_add(binding.unit_count);
                }
            }
            let snapshot = reader.snapshot().clone();
            reader.verify_operations(&catalog)?;
            drop(reader);
            drop(store);
            drop(writer);
            let mut planning =
                indexed_embedding_inputs::EmbeddingPlanningSummary::new(snapshot.clone());
            for owners in selected.chunks(indexed_embedding_inputs::PROOF_SCOPE_OWNERS) {
                if Instant::now() >= deadline {
                    return Err(fail(
                        ErrorCode::BudgetExceeded,
                        "preparation planning command deadline exceeded",
                    ));
                }
                let scope_phase = Self::embedding_phase()?;
                let mut inputs = indexed_embedding_inputs::materialize(
                    &catalog,
                    settings,
                    Some(owners),
                    &Self::embedding_phase_proof_budget(&scope_phase)?,
                )?;
                inputs.recheck(&catalog)?;
                planning.append(inputs.into_planning_summary()?)?;
            }
            let phase = Self::embedding_phase()?;
            let units = &planning.units;
            let available = VectorStore::open_bounded_snapshot(&self.fs, &phase)?;
            let mut missing = Vec::new();
            let mut hashes = BTreeSet::new();
            for unit in units {
                if hashes.insert(unit.input_hash.clone()) {
                    if counted_inputs.len() >= MAX_COUNTED_INPUTS
                        && !counted_inputs.contains(&unit.input_hash)
                    {
                        return Err(fail(
                            ErrorCode::BudgetExceeded,
                            "preparation input-count allowance reached; acknowledged pages remain resumable",
                        ));
                    }
                    let first_use = counted_inputs.insert(unit.input_hash.clone());
                    if available.vector(&space, &unit.input_hash)?.is_some() {
                        report.reused_inputs += usize::from(first_use)
                    } else {
                        missing.push(unit.input())
                    }
                }
            }
            let selected_coverage = available.coverage(&space, units)?;
            drop(available);
            if !missing.is_empty() {
                if let Some(runtime) = runtime {
                    let scope = planning.paid_scope(&missing, 127, deadline)?;
                    let batches = self.embedding_supplier_batches(
                        runtime,
                        &missing,
                        &scope.source_bindings,
                        &scope.units,
                    )?;
                    if Instant::now() >= deadline {
                        return Err(fail(
                            ErrorCode::BudgetExceeded,
                            "preparation command deadline exceeded before paid Run admission",
                        ));
                    }
                    let (ledger, tasks) = self.embedding_job_with_bindings(
                        &spec,
                        &batches,
                        &scope.units,
                        runtime,
                        "embeddings_sync",
                        false,
                        Some(&scope.source_bindings),
                    )?;
                    report.run_id = Some(ledger.inspect()?.spec.run_id.clone());
                    for task in tasks {
                        if ledger
                            .inspect()?
                            .tasks
                            .get(&task.key)
                            .is_some_and(|t| t.state == TaskState::Completed)
                        {
                            continue;
                        }
                        let generated = self
                            .dispatch_embedding_task(
                                &ledger,
                                &task,
                                &spec,
                                &scope.units,
                                runtime,
                                TaskMaterialization {
                                    probe: false,
                                    corpus: true,
                                },
                            )
                            .map_err(|error| {
                                with_embedding_context(
                                    error,
                                    serde_json::json!({
                                        "run_id": report.run_id, "space": space,
                                        "generated_inputs": report.generated_inputs,
                                    }),
                                )
                            })?;
                        report.generated_inputs += generated;
                        report.network_used |= generated > 0;
                    }
                    if let Some(warning) = self.finish_embedding_job(&ledger)? {
                        report.warnings.push(warning)
                    }
                } else {
                    if planning.snapshot != snapshot {
                        return Err(changed(
                            "publication changed while checking offline preparation coverage",
                        ));
                    }
                    report.coverage.eligible_units = state.unit_count;
                    report.coverage.available_units = retained_available
                        .saturating_add(acknowledged_units)
                        .saturating_add(page_ready_units)
                        .saturating_add(selected_coverage.available_units)
                        .min(state.unit_count);
                    report.coverage.missing_units = state
                        .unit_count
                        .saturating_sub(report.coverage.available_units);
                    report.coverage.pending_units = report
                        .coverage
                        .missing_units
                        .saturating_sub(selected_coverage.missing_units)
                        .saturating_add(selected_coverage.pending_units)
                        .min(report.coverage.missing_units);
                    report.coverage.corrupt_units = selected_coverage.corrupt_units;
                    report.warnings.push(format!("Coverage is incomplete at indexed generation {}; available units are a conservative observed floor and unvisited owner units remain pending. Previously acknowledged prefixes from an earlier invocation are not recounted. No provider was called.",snapshot.generation));
                    report.warnings.push("Cache-only preparation found missing current inputs; compatible retained blobs are preserved and this owner page remains pending. Run explicitly authorized online preparation or restore the retained cache.".into());
                    return Ok(report);
                }
            }
            // Detached summaries are planning observations, never publication authority.
            drop(planning);
            let mut scopes: Vec<_> = bindings
                .chunks(indexed_embedding_inputs::PROOF_SCOPE_OWNERS)
                .collect();
            if scopes.is_empty() {
                // An empty final page still closes its original inventory interval.
                scopes.push(&[]);
            }
            for (scope_index, scope_bindings) in scopes.iter().enumerate() {
                if Instant::now() >= deadline {
                    report.warnings.push("Preparation reached its command deadline; repeat embeddings sync to resume the acknowledged owner prefix.".into());
                    break 'pages;
                }
                let writer = self.embedding_writer()?;
                let phase = Self::embedding_phase()?;
                let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
                if catalog_incarnation(reader.vault_id(), reader.snapshot())? != incarnation {
                    return Err(changed("catalog rebuilt before owner acknowledgment"));
                }
                for binding in *scope_bindings {
                    if reader.unit_owner_binding(&policy, &binding.owner)?.as_ref() != Some(binding)
                    {
                        return Err(changed(
                            "owner inventory version changed before acknowledgment",
                        ));
                    }
                }
                let owners: Vec<_> = scope_bindings
                    .iter()
                    .filter(|binding| selected.contains(&binding.owner))
                    .map(|binding| binding.owner.clone())
                    .collect();
                // Every newly acknowledged owner, including a cache hit, receives
                // a new bounded canonical proof after the page's paid work.
                let mut fresh = if owners.is_empty() {
                    None
                } else {
                    Some(indexed_embedding_inputs::materialize(
                        &catalog,
                        settings,
                        Some(&owners),
                        &Self::embedding_phase_proof_budget(&phase)?,
                    )?)
                };
                let mut ready = Vec::new();
                if let Some(inputs) = &mut fresh {
                    if &inputs.snapshot != reader.snapshot() {
                        return Err(changed(
                            "publication changed before owner acknowledgment proof",
                        ));
                    }
                    inputs.recheck(&catalog)?;
                    for binding in scope_bindings.iter().filter(|b| owners.contains(&b.owner)) {
                        let descriptors =
                            reader.unit_descriptors_for_owner(&policy, &binding.owner, 4096)?;
                        if descriptors.len() != binding.unit_count
                            || descriptors
                                .iter()
                                .any(|d| !inputs.units.iter().any(|u| d.matches_rendered(u)))
                        {
                            return Err(changed(
                                "authenticated owner units disagree with compact inventory",
                            ));
                        }
                        ready.push((binding.clone(), descriptors));
                    }
                    catalog.record_unit_owner_dependencies(
                        &writer,
                        &policy,
                        reader.snapshot(),
                        &inputs.owner_dependencies()?,
                    )?;
                }
                let mut store = VectorStore::open(&self.fs, Some(&writer))?;
                store.bind_read_budget(&phase)?;
                for binding in *scope_bindings {
                    if binding.tombstone {
                        ready.push((binding.clone(), Vec::new()));
                    } else if !owners.contains(&binding.owner)
                        && !store.owner_binding_ready(&space, binding)?
                    {
                        return Err(changed(
                            "retained owner acknowledgment changed before prefix publication",
                        ));
                    }
                }
                let final_scope = scope_index + 1 == scopes.len();
                let next = acknowledged_prefix_cursor(
                    &cursor,
                    scope_bindings,
                    last && final_scope,
                    reader.snapshot().generation,
                );
                let complete = next.complete;
                let dimensions = store.space(&space)?.and_then(|x| x.actual_dimensions);
                let activate = complete && state.unit_count > 0 && dimensions.is_some();
                store.acknowledge_unit_owners_checked(
                    &space,
                    &ready,
                    Some(&next),
                    activate,
                    &spec,
                    || {
                        if let Some(inputs) = &mut fresh {
                            inputs.recheck(&catalog)?;
                        }
                        reader.verify_operations(&catalog)?;
                        phase.remaining_ms()?;
                        Ok(())
                    },
                )?;
                // The cursor advances only after the matching owner transaction commits.
                let catches_up = next.since_seq != cursor.since_seq;
                cursor = next;
                acknowledged_units = acknowledged_units.saturating_add(
                    scope_bindings
                        .iter()
                        .filter(|b| !b.tombstone)
                        .map(|b| b.unit_count)
                        .sum::<usize>(),
                );
                report.active_space = store.active()?.map(|x| x.id);
                let available = if complete {
                    state.unit_count
                } else {
                    retained_available
                        .saturating_add(acknowledged_units)
                        .min(state.unit_count)
                };
                report.coverage = Coverage {
                    eligible_units: state.unit_count,
                    available_units: available,
                    missing_units: state.unit_count.saturating_sub(available),
                    pending_units: state.unit_count.saturating_sub(available),
                    ..Default::default()
                };
                report.published = activate;
                if complete {
                    break 'pages;
                }
                if catches_up {
                    acknowledged_units = 0;
                }
            }
        }
        report.warnings.push("Coverage describes current inventory and retained owner acknowledgments; it is not a full vector-blob integrity audit. Query discovery checks the compatible blobs it reads; canonical evidence is authenticated only for selected owners.".into());
        if !report.published {
            report.warnings.push("Preparation is pending or empty; repeat embeddings sync to resume. The retained active space is unchanged.".into())
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_cursor_catches_up_only_after_the_original_interval_finishes() {
        let policy = RenderPolicyId(Blake3Hash::digest("policy"));
        let cursor = PreparationCursor {
            version: INVENTORY_VERSION,
            incarnation: Blake3Hash::digest("catalog"),
            policy: policy.clone(),
            since_seq: 0,
            through_seq: 7,
            after: None,
            complete: false,
        };
        let binding = UnitOwnerBinding {
            incarnation: cursor.incarnation.clone(),
            policy,
            owner: VaultRelativePath::new("a.md").unwrap(),
            render_token: Blake3Hash::digest("render"),
            proof_version: 1,
            modified_seq: 4,
            tombstone: false,
            unit_count: 1,
        };
        let partial = acknowledged_prefix_cursor(&cursor, &[binding], false, 8);
        assert_eq!(
            cursor.after, None,
            "planning a prefix must not mutate the durable cursor"
        );
        assert_eq!(partial.since_seq, 0);
        assert_eq!(partial.through_seq, 7);
        assert_eq!(partial.after.as_ref().unwrap().modified_seq, 4);
        assert!(!partial.complete);
        let closed = acknowledged_prefix_cursor(&partial, &[], true, 7);
        assert!(
            closed.complete,
            "an empty final scheduling page closes the interval"
        );
        assert_eq!(closed.after, partial.after);
        let next = acknowledged_prefix_cursor(&partial, &[], true, 8);
        assert!(!next.complete);
        assert_eq!(next.since_seq, 7);
        assert_eq!(next.through_seq, 8);
        assert!(
            next.after.is_none(),
            "new interval must revisit every changed owner"
        );
    }
}

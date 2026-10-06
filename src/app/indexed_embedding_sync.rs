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
        for _ in 0..MAX_PAGES {
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
            let mut ready = Vec::new();
            for binding in &bindings {
                if binding.tombstone {
                    ready.push((binding.clone(), Vec::new()));
                } else if !store.owner_binding_ready(&space, binding)? {
                    selected.push(binding.owner.clone());
                }
            }
            let snapshot = reader.snapshot().clone();
            reader.verify_operations(&catalog)?;
            drop(reader);
            drop(store);
            drop(writer);
            let mut units = Vec::new();
            let mut prepared = None;
            if !selected.is_empty() {
                let mut inputs = indexed_embedding_inputs::materialize(
                    &catalog,
                    settings,
                    Some(&selected),
                    &Self::embedding_phase_proof_budget(&phase)?,
                )?;
                inputs.recheck(&catalog)?;
                units = std::mem::take(&mut inputs.units);
                prepared = Some(inputs);
            }
            let available = VectorStore::open_bounded_snapshot(&self.fs, &phase)?;
            let mut missing = Vec::new();
            let mut hashes = BTreeSet::new();
            for unit in &units {
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
            let selected_coverage = available.coverage(&space, &units)?;
            drop(available);
            let mut paid = false;
            if !missing.is_empty() {
                if let Some(runtime) = runtime {
                    paid = true;
                    let scope = prepared
                        .as_ref()
                        .ok_or_else(|| {
                            changed(
                                "missing embedding inputs lost their authenticated owner proofs",
                            )
                        })?
                        .paid_scope(&units, &missing, 127)?;
                    let batches = self.embedding_guard_batches(
                        runtime,
                        &missing,
                        Some(&scope.source_bindings),
                        &[],
                    )?;
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
                    if prepared
                        .as_ref()
                        .is_some_and(|inputs| inputs.snapshot != snapshot)
                    {
                        return Err(changed(
                            "publication changed while checking offline preparation coverage",
                        ));
                    }
                    report.coverage.eligible_units = state.unit_count;
                    report.coverage.available_units = retained_available
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
                    report.warnings.push(format!("Coverage is incomplete at indexed generation {}; unvisited owner units remain pending. No provider was called.",snapshot.generation));
                    report.warnings.push("Cache-only preparation found missing current inputs; compatible retained blobs are preserved and this owner page remains pending. Run explicitly authorized online preparation or restore the retained cache.".into());
                    return Ok(report);
                }
            }
            let writer = self.embedding_writer()?;
            let phase = Self::embedding_phase()?;
            let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
            if catalog_incarnation(reader.vault_id(), reader.snapshot())? != incarnation {
                return Err(changed("catalog rebuilt before owner acknowledgment"));
            }
            for binding in &bindings {
                if reader.unit_owner_binding(&policy, &binding.owner)?.as_ref() != Some(binding) {
                    return Err(changed(
                        "owner inventory version changed before acknowledgment",
                    ));
                }
            }
            let mut fresh = if paid
                || prepared
                    .as_ref()
                    .is_some_and(|inputs| &inputs.snapshot != reader.snapshot())
            {
                drop(prepared);
                drop(units);
                Some(indexed_embedding_inputs::materialize(
                    &catalog,
                    settings,
                    Some(&selected),
                    &Self::embedding_phase_proof_budget(&phase)?,
                )?)
            } else {
                if let Some(inputs) = &mut prepared {
                    inputs.units = units;
                }
                prepared
            };
            if let Some(inputs) = &mut fresh {
                inputs.recheck(&catalog)?;
                for binding in bindings.iter().filter(|b| selected.contains(&b.owner)) {
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
                let dependencies = inputs.owner_dependencies()?;
                catalog.record_unit_owner_dependencies(
                    &writer,
                    &policy,
                    &reader.snapshot().clone(),
                    &dependencies,
                )?;
            }
            if let Some(last_binding) = bindings.last() {
                cursor.after = Some(UnitOwnerCursor {
                    modified_seq: last_binding.modified_seq,
                    owner: last_binding.owner.clone(),
                });
            }
            cursor.complete = last;
            let current_generation = reader.snapshot().generation;
            if last && current_generation > cursor.through_seq {
                // Do not activate a stale interval. Preserve its inclusive
                // high-water then start a fresh interval on the next iteration.
                cursor.since_seq = cursor.through_seq;
                cursor.through_seq = current_generation;
                cursor.after = None;
                cursor.complete = false;
            }
            let complete = cursor.complete;
            let mut store = VectorStore::open(&self.fs, Some(&writer))?;
            store.bind_read_budget(&phase)?;
            let dimensions = store.space(&space)?.and_then(|x| x.actual_dimensions);
            let activate = complete && state.unit_count > 0 && dimensions.is_some();
            store.acknowledge_unit_owners_checked(
                &space,
                &ready,
                Some(&cursor),
                activate,
                &spec,
                || {
                    if let Some(inputs) = &mut fresh {
                        inputs.recheck(&catalog)?
                    }
                    reader.verify_operations(&catalog)?;
                    phase.remaining_ms()?;
                    Ok(())
                },
            )?;
            report.active_space = store.active()?.map(|x| x.id);
            report.coverage = Coverage {
                eligible_units: state.unit_count,
                available_units: if complete { state.unit_count } else { 0 },
                missing_units: if complete { 0 } else { state.unit_count },
                ..Default::default()
            };
            report.published = activate;
            let _ = snapshot;
            if complete {
                break;
            }
        }
        report.warnings.push("Coverage describes current inventory and retained owner acknowledgments; it is not a full vector-blob integrity audit. Query discovery checks the compatible blobs it reads; canonical evidence is authenticated only for selected owners.".into());
        if !report.published {
            report.warnings.push("Preparation is pending or empty; repeat embeddings sync to resume. The retained active space is unchanged.".into())
        }
        Ok(report)
    }
}

//! Common receipt settlement/checkpoint publication; purpose validators remain separate.
use super::OfflineApp;
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::ChangeEngine,
    domain::*,
    jobs::*,
    vault::WriterPermit,
};
use std::time::{Duration, Instant};

impl OfflineApp {
    /// Complete a sealed active publication before ledger replay inspects visible receipts.
    pub(crate) fn recover_graph_generation_publication(&self) -> Result<()> {
        let writer = WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(self.options.lock_timeout_ms),
        )?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        if catalog.operation_state()?.is_some() {
            crate::jobs::checkpoint::recover_job_active(&self.fs, &writer, &self.vault_id)?;
        } else {
            ChangeEngine::new(self.fs.clone())?.recover(
                &writer,
                &CatalogGraphValidator,
                &catalog,
            )?;
        }
        Ok(())
    }

    /// Finish exact canonical publication before reopening a transient response.
    /// The named Change proves the pair; the retained ledger authenticates usage.
    pub(crate) fn recover_committed_graph_generation(
        &self,
        ledger: &JobLedger,
        attempt: &AttemptRef,
        complete_run: bool,
    ) -> Result<Option<(DurableOutputRef, DurableOutputRef)>> {
        self.recover_graph_generation_publication()?;
        let Some(committed) = crate::jobs::checkpoint::graph_generation_committed(ledger, attempt)?
        else {
            return Ok(None);
        };
        let output = committed.usage.outputs[0].clone();
        ledger.outputs_committed(
            attempt,
            &committed.change,
            committed.receipt.clone(),
            committed.usage.outputs,
            vec![],
        )?;
        ledger.settle(attempt)?;
        ledger.finish_remote_task(
            &attempt.task_key,
            vec![output.clone()],
            vec![],
            |_: &VectorCacheRef| Ok(false),
        )?;
        if complete_run && ledger.inspect()?.state == RunState::Running {
            crate::app::remote::finish_provider_job(ledger)?;
        }
        ledger.remove_spool_after_verified_commit(attempt)?;
        self.checkpoint_generation(ledger)?;
        Ok(Some((output, committed.receipt)))
    }

    /// Resolve a named committed pair before touching transient generation input.
    /// The caller owns the invocation deadline and provider/task allowance.
    pub(crate) fn publish_source_relation_attempt(
        &self,
        ledger: &JobLedger,
        attempt: &AttemptRef,
        deadline: Instant,
        complete_run: bool,
    ) -> Result<(DurableOutputRef, Vec<DurableOutputRef>)> {
        use crate::jobs::checkpoint::SourceRelationPreparation;
        if Instant::now() >= deadline {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "relation invocation deadline exceeded",
            ));
        }
        // Complete any interrupted canonical publication before consulting current
        // Source inputs. No Run lock is held while writer authority is acquired.
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let writer = WriterPermit::acquire(
                self.fs.root(),
                Duration::from_millis(self.options.lock_timeout_ms).min(remaining),
            )?;
            let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
            if catalog.operation_state()?.is_some() {
                crate::jobs::checkpoint::recover_job_active(&self.fs, &writer, &self.vault_id)?;
            } else {
                ChangeEngine::new(self.fs.clone())?.recover(
                    &writer,
                    &CatalogGraphValidator,
                    &catalog,
                )?;
            }
        }
        let prepared = crate::jobs::checkpoint::source_relation_plan(ledger, attempt, deadline)?;
        if Instant::now() >= deadline {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "relation invocation deadline exceeded",
            ));
        }
        let result = match prepared {
            SourceRelationPreparation::Fresh(plan) => {
                let receipt = self.commit_generation_task(ledger, &plan, complete_run)?;
                (receipt, plan.receipt.outputs)
            }
            SourceRelationPreparation::Committed {
                change,
                receipt,
                usage,
            } => {
                // Existing ledger acknowledgement proves the receipt's accounting
                // against its retained attempt and handles identical replay.
                ledger.outputs_committed(
                    attempt,
                    &change,
                    receipt.clone(),
                    usage.outputs.clone(),
                    vec![],
                )?;
                ledger.settle(attempt)?;
                ledger.finish_remote_task(
                    &attempt.task_key,
                    usage.outputs.clone(),
                    vec![],
                    |_: &VectorCacheRef| Ok(false),
                )?;
                if complete_run && ledger.inspect()?.state == RunState::Running {
                    crate::app::remote::finish_provider_job(ledger)?;
                }
                ledger.remove_spool_after_verified_commit(attempt)?;
                (receipt, usage.outputs)
            }
        };
        if Instant::now() >= deadline {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "relation invocation deadline exceeded after publication; retained job recovery remains available",
            ));
        }
        self.checkpoint_generation(ledger)?;
        if Instant::now() >= deadline {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "relation invocation deadline exceeded after checkpoint; retained completion remains available",
            ));
        }
        Ok(result)
    }

    pub(crate) fn commit_generation_task(
        &self,
        ledger: &JobLedger,
        plan: &MaterializationPlan,
        complete_run: bool,
    ) -> Result<DurableOutputRef> {
        let receipt_id = &plan.receipt.receipt_id;
        let receipt_path = VaultRelativePath::new(format!(
            "runs/{}/events/{receipt_id}.md",
            plan.attempt.run_id
        ))?;
        let receipt_bytes = plan
            .draft
            .operations
            .iter()
            .find(|o| o.target == receipt_path)
            .and_then(|o| o.proposed.clone())
            .or(crate::changes::prepare::read_bounded(
                &self.fs,
                &receipt_path,
                EVENT_MAX_BYTES,
            )?)
            .ok_or_else(|| WikiError::invalid("receipt payload missing"))?;
        let reference = DurableOutputRef {
            record: RecordRef {
                vault_id: self.vault_id.clone(),
                record_id: receipt_id.clone(),
                expected_kind: RecordKind::RunEvent,
            },
            path: receipt_path,
            hash: Blake3Hash::digest(receipt_bytes),
        };
        let typed_generation = ledger
            .inspect()?
            .tasks
            .get(&plan.attempt.task_key)
            .is_some_and(|task| {
                matches!(
                    task.spec.stage,
                    TaskStage::PrepareSourceRelations | TaskStage::Extract
                )
            });
        let writer = WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(self.options.lock_timeout_ms),
        )?;
        let engine = ChangeEngine::new(self.fs.clone())?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        if !typed_generation || catalog.operation_state()?.is_none() {
            engine.recover(&writer, &CatalogGraphValidator, &catalog)?;
        }
        let prepared = if typed_generation {
            // The selected publisher regenerates the exact typed output pair and
            // receipt from retained Received authority before granting a seal.
            crate::jobs::checkpoint::publish_job_draft(
                &self.fs,
                &writer,
                ledger,
                plan.draft.clone(),
            )?
        } else {
            let prepared = engine.prepare(&writer, plan.draft.clone())?.prepared;
            engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
            prepared
        };
        drop(writer);
        ledger.outputs_committed(
            &plan.attempt,
            &prepared,
            reference.clone(),
            plan.receipt.outputs.clone(),
            vec![],
        )?;
        ledger.settle(&plan.attempt)?;
        if plan.receipt.output_disposition == OutputDisposition::Validated {
            ledger.finish_remote_task(
                &plan.attempt.task_key,
                plan.receipt.outputs.clone(),
                vec![],
                |_: &VectorCacheRef| Ok(false),
            )?;
            if complete_run {
                crate::app::remote::finish_provider_job(ledger)?;
            }
        }
        ledger.remove_spool_after_verified_commit(&plan.attempt)?;
        Ok(reference)
    }
    pub(crate) fn checkpoint_generation(&self, ledger: &JobLedger) -> Result<()> {
        let draft = ledger.checkpoint_plan()?;
        if draft.operations.is_empty() {
            return Ok(());
        }
        let writer = WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(self.options.lock_timeout_ms),
        )?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let prepared = if catalog.operation_state()?.is_some() {
            crate::jobs::checkpoint::publish_job_draft(&self.fs, &writer, ledger, draft)?
        } else {
            let engine = ChangeEngine::new(self.fs.clone())?;
            let prepared = engine.prepare(&writer, draft)?.prepared;
            engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
            prepared
        };
        drop(writer);
        ledger.checkpoint_committed(&prepared)?;
        Ok(())
    }
}

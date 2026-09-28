//! Exhaustive explicit entity decisions are validated locally and staged for apply.
use super::OfflineApp;
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{ChangeDraft, ChangeEngine},
    domain::*,
    graph::decisions::{plan_entity_decisions, stage_entity_decisions, validate_entity_decisions},
    sources::SourceView,
    vault::WriterPermit,
};
use serde_json::{Value, json};
use std::time::Duration;

impl OfflineApp {
    pub fn graph_decide(&self, bytes: &[u8]) -> Result<Value> {
        // Reject malformed input before acquiring authority or recovering writes.
        crate::graph::remap::normalize(crate::graph::packet::decode::<
            crate::graph::EntityDecisionRequest,
        >(bytes, crate::graph::MAX_ENTITY_DECISION_BYTES)?)?;
        let engine = ChangeEngine::new(self.fs.clone())?;
        let writer = if self.options.dry_run {
            None
        } else {
            let writer = WriterPermit::acquire(
                self.fs.root(),
                Duration::from_millis(self.options.lock_timeout_ms),
            )?;
            let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
            engine.recover(&writer, &CatalogGraphValidator, &catalog)?;
            catalog.guard_current(None)?;
            Some(writer)
        };
        let view = SourceView::from_fs_bounded(&self.fs, 64 * 1024 * 1024, 4096)?;
        let validated = validate_entity_decisions(&view, bytes)?;
        if self.options.dry_run {
            let plan = plan_entity_decisions(&validated)?;
            engine.plan(&ChangeDraft {
                title: "Validate explicit entity decisions".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: Default::default(),
                read_preconditions: validated.dependencies.clone(),
                operations: vec![],
            })?;
            return Ok(json!({
                "dry_run":true,"task_id":validated.task_id(),
                "request_hash":validated.request_hash(),"summary":plan.summary,
                "allocated_ids":null,"change":null,"cache_state_unknown":true
            }));
        }
        let outcome = stage_entity_decisions(
            &engine,
            writer.as_ref().expect("entity decision writer"),
            &validated,
        )?;
        serde_json::to_value(outcome)
            .map_err(|_| WikiError::new(ErrorCode::Internal, "serialize entity decisions"))
    }
}

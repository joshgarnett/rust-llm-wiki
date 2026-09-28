//! Explicit mention resolutions are validated locally and staged for apply.
use super::OfflineApp;
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{ChangeDraft, ChangeEngine},
    domain::*,
    graph::resolution::{plan_resolution, stage_resolution, validate_resolution},
    sources::SourceView,
    vault::WriterPermit,
};
use serde_json::{Value, json};
use std::time::Duration;

impl OfflineApp {
    pub fn graph_resolve(&self, bytes: &[u8]) -> Result<Value> {
        // Reject malformed input before acquiring authority or recovering writes.
        crate::graph::mention_state::normalize(crate::graph::packet::decode::<
            crate::graph::ResolutionRequest,
        >(
            bytes, crate::graph::MAX_RESOLUTION_BYTES
        )?)?;
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
        let validated = validate_resolution(&view, bytes)?;
        if self.options.dry_run {
            let plan = plan_resolution(&validated)?;
            engine.plan(&ChangeDraft {
                title: "Validate explicit mention resolution".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: Default::default(),
                read_preconditions: validated.dependencies.clone(),
                operations: vec![],
            })?;
            return Ok(json!({
                "dry_run":true,"extraction_id":validated.request().extraction_id,
                "request_hash":validated.request_hash(),"summary":plan.summary,
                "allocated_ids":null,"change":null,"cache_state_unknown":true
            }));
        }
        let outcome = stage_resolution(
            &engine,
            writer.as_ref().expect("resolution writer"),
            &validated,
        )?;
        serde_json::to_value(outcome)
            .map_err(|_| WikiError::new(ErrorCode::Internal, "serialize mention resolution"))
    }
}

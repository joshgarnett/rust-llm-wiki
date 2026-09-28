//! Persist packets before returning an executable task; imports always stage.
use super::OfflineApp;
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::*,
    domain::*,
    graph::{extraction_types::*, import, packet, wire},
    sources::SourceView,
    vault::WriterPermit,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;
const SOURCE_BYTES: usize = 64 * 1024 * 1024;
#[derive(Debug, Clone, Serialize)]
pub struct PacketExportOutcome {
    pub packet: ExtractionPacket,
    pub locator: DocumentLocator,
    pub coverage: ExtractionCoverage,
    pub persisted: bool,
    pub ready_to_import: bool,
    pub change: Option<PreparedChange>,
    pub snapshot: Option<ReadSnapshot>,
    pub reused: bool,
}
fn retained(mut error: WikiError, change: &PreparedChange) -> WikiError {
    error.details = json!({"change":{"change_id":change.change_id,"manifest_hash":change.manifest_hash},"cause_details":error.details});
    error
}
impl OfflineApp {
    pub fn extraction_candidates(&self, ids: &[RecordId]) -> Result<Vec<CandidateIdentity>> {
        if ids.len() > MAX_CANDIDATE_IDENTITIES {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "candidate identities exceed32",
            ));
        }
        let view = SourceView::from_fs_bounded(&self.fs, SOURCE_BYTES, 4096)?;
        let mut out = Vec::new();
        for id in ids {
            let (_, note) = view.resolve(id, RecordKind::Entity, None)?;
            let record = note
                .canonical
                .as_ref()
                .ok_or_else(|| WikiError::invalid("invalid candidate entity"))?;
            let entity_type = record
                .field("wiki_type")
                .and_then(Value::as_str)
                .ok_or_else(|| WikiError::invalid("candidate entity type missing"))?;
            out.push(CandidateIdentity {
                reference: RecordRef {
                    vault_id: self.vault_id.clone(),
                    record_id: id.clone(),
                    expected_kind: RecordKind::Entity,
                },
                title: record.title().to_owned(),
                entity_type: entity_type.to_owned(),
            });
        }
        Ok(out)
    }
    pub fn graph_extract_agent(&self, request: &ExportRequest) -> Result<PacketExportOutcome> {
        if self.options.stage_only {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "agent packet export must persist before returning; --stage is unavailable",
            ));
        }
        let engine = ChangeEngine::new(self.fs.clone())?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let writer = if self.options.dry_run {
            None
        } else {
            let writer = WriterPermit::acquire(
                self.fs.root(),
                Duration::from_millis(self.options.lock_timeout_ms),
            )?;
            engine.recover(&writer, &CatalogGraphValidator, &catalog)?;
            catalog.guard_current(None)?;
            Some(writer)
        };
        let view = SourceView::from_fs_bounded(&self.fs, SOURCE_BYTES, 4096)?;
        let plan = packet::build_packet(&view, request)?;
        let mut outcome = PacketExportOutcome {
            packet: plan.packet,
            locator: plan.locator,
            coverage: plan.coverage,
            persisted: false,
            ready_to_import: false,
            change: None,
            snapshot: None,
            reused: plan.reused,
        };
        if self.options.dry_run {
            return Ok(outcome);
        }
        let writer = writer.as_ref().expect("normal export writer");
        if let Some(draft) = plan.draft {
            let retained_change = engine.prepare(writer, draft)?;
            let applied = engine
                .apply(
                    writer,
                    &retained_change.prepared,
                    &CatalogGraphValidator,
                    &catalog,
                )
                .map_err(|e| retained(e, &retained_change.prepared))?;
            outcome.change = Some(applied.change);
            outcome.snapshot = applied.snapshot;
        } else {
            // Reused packets must still recheck every observed dependency.
            engine.plan(&ChangeDraft {
                title: "Verify persisted packet".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: Default::default(),
                read_preconditions: plan.dependencies,
                operations: vec![],
            })?;
        }
        let fresh = SourceView::from_fs_bounded(&self.fs, SOURCE_BYTES, 4096)?;
        let verified = packet::load_packet(&fresh, &outcome.packet.packet_id)?;
        if verified.packet() != &outcome.packet || verified.locator() != &outcome.locator {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "persisted packet changed before return",
            ));
        }
        outcome.persisted = true;
        outcome.ready_to_import = true;
        Ok(outcome)
    }
    pub fn graph_import(&self, bytes: &[u8], new_extraction: bool) -> Result<Value> {
        // The bounded strict decoder rejects duplicate keys before selecting a packet.
        let response: ExtractionResponse = packet::decode(bytes, 262144)?;
        let engine = ChangeEngine::new(self.fs.clone())?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let writer = if self.options.dry_run {
            None
        } else {
            let writer = WriterPermit::acquire(
                self.fs.root(),
                Duration::from_millis(self.options.lock_timeout_ms),
            )?;
            engine.recover(&writer, &CatalogGraphValidator, &catalog)?;
            catalog.guard_current(None)?;
            Some(writer)
        };
        let view = SourceView::from_fs_bounded(&self.fs, SOURCE_BYTES, 4096)?;
        let verified = packet::load_packet(&view, &response.packet_id)?;
        let validated = wire::validate_response(&verified, &view, bytes)?;
        if self.options.dry_run {
            engine.plan(&ChangeDraft {
                title: "Validate extraction import".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: Default::default(),
                read_preconditions: validated.dependencies.clone(),
                operations: vec![],
            })?;
            return Ok(
                json!({"dry_run":true,"packet_id":response.packet_id,"response_hash":validated.response_hash(),"mentions":response.mentions.len(),"assertions":response.assertions.len(),"unresolved":response.unresolved.len(),"allocated_ids":null,"change":null,"cache_state_unknown":true}),
            );
        }
        let policy = if new_extraction {
            OriginPolicy::AllowNewResponse
        } else {
            OriginPolicy::ReuseOrConflict
        };
        let outcome = import::stage_import(
            &engine,
            writer.as_ref().expect("import writer"),
            &validated,
            policy,
        )?;
        serde_json::to_value(outcome)
            .map_err(|_| WikiError::new(ErrorCode::Internal, "serialize extraction import"))
    }
}

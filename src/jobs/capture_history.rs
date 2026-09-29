//! Historical capture proof is distinct from the current mutable Source header.
//! Callers supply the already authenticated/replayed journal, and still enforce
//! current task/read/citation bindings separately. This never restores a Source.
#[cfg(test)]
use super::test_support as common;
use super::{checkpoint, events, types::*};
use crate::{
    changes::{ChangeEngine, ChangeStatus},
    domain::*,
    records::parse_note,
    vault::{ExpectedState, VaultFs},
};

/// Prefer the ordinary exact current output proof. Only a capture's mutable
/// Source may instead be proven by its acknowledged original commit, including
/// the crash window between OutputsCommitted and TaskFinished.
/// Current deletion or edits confer no authority and do not erase that history.
pub(super) fn output(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    frames: &[JournalFrame],
    task: &TaskInspection,
    reference: &DurableOutputRef,
) -> Result<()> {
    if reference.record.vault_id != inspection.spec.vault_id {
        return Err(events::corrupt("capture output belongs to another vault"));
    }
    let original_error = match checkpoint::output(fs, reference) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    if task.spec.stage != TaskStage::Capture
        || task.spec.capability != Some(Capability::Fetch)
        || !matches!(task.state, TaskState::Running | TaskState::Completed)
        || inspection.tasks.get(&task.spec.key) != Some(task)
        || task.state == TaskState::Completed && !task.outputs.contains(reference)
        || task.state == TaskState::Running
            && (!task.outputs.is_empty() || !task.cache_outputs.is_empty())
        || reference.record.expected_kind != RecordKind::Source
        || reference.path.as_str() != format!("sources/{}/source.md", reference.record.record_id)
    {
        return Err(original_error);
    }
    let mut owners = inspection.attempts.iter().filter(|attempt| {
        attempt.attempt.run_id == inspection.spec.run_id
            && attempt.attempt.task_key == task.spec.key
            && matches!(
                attempt.phase,
                AttemptPhase::OutputCommitted | AttemptPhase::Settled
            )
            && attempt.bound.capability == Capability::Fetch
            && attempt.bound.input_hash == task.spec.input_hash
            && (task.state == TaskState::Running || attempt.outputs == task.outputs)
            && attempt.outputs.contains(reference)
    });
    let owner = owners
        .next()
        .ok_or_else(|| events::corrupt("historical capture has no acknowledged attempt"))?;
    if owners.next().is_some() {
        return Err(events::corrupt(
            "historical capture attempt ownership is ambiguous",
        ));
    }
    let receipt_ref = owner
        .receipt
        .as_ref()
        .ok_or_else(|| events::corrupt("historical capture receipt missing"))?;
    if receipt_ref.record.vault_id != inspection.spec.vault_id
        || receipt_ref.record.expected_kind != RecordKind::RunEvent
        || receipt_ref.path
            != checkpoint::event_path(&inspection.spec.run_id, &receipt_ref.record.record_id)?
    {
        return Err(events::corrupt("historical capture receipt owner differs"));
    }
    let receipt = checkpoint::receipt(fs, receipt_ref)?;
    if receipt.attempt != owner.attempt
        || receipt.capability != Capability::Fetch
        || receipt.output_disposition != OutputDisposition::Validated
        || receipt.outputs != owner.outputs
        || receipt.cache_outputs != owner.cache_outputs
    {
        return Err(events::corrupt("historical capture receipt proof differs"));
    }
    let mut commits = frames
        .iter()
        .filter_map(|frame| match &frame.event.payload {
            EventPayload::OutputsCommitted {
                attempt,
                receipt,
                outputs,
                cache_outputs,
                change,
            } if attempt == &owner.attempt => {
                Some((frame, receipt, outputs, cache_outputs, change))
            }
            _ => None,
        });
    let (frame, acknowledged_receipt, outputs, cache_outputs, change) = commits
        .next()
        .ok_or_else(|| events::corrupt("historical capture commit event missing"))?;
    if commits.next().is_some()
        || frame.event.run_id != inspection.spec.run_id
        || acknowledged_receipt != receipt_ref
        || outputs != &owner.outputs
        || cache_outputs != &owner.cache_outputs
    {
        return Err(events::corrupt("historical capture commit event differs"));
    }
    let engine = ChangeEngine::new(fs.clone())?;
    let committed = engine.inspect(&change.change_id)?;
    if &committed.prepared != change
        || committed.status != ChangeStatus::Committed
        || committed.manifest.vault_id != inspection.spec.vault_id
    {
        return Err(events::corrupt(
            "historical capture changeset proof differs",
        ));
    }
    // Both exact acknowledged artifacts must belong to this recorded changeset;
    // a different committed change with an identical Source is not sufficient.
    let mut source_bytes = None;
    for expected in [reference, receipt_ref] {
        let mut operations = committed
            .manifest
            .operations
            .iter()
            .enumerate()
            .filter(|(_, op)| {
                op.target == expected.path && op.after == ExpectedState::Hash(expected.hash.clone())
            });
        let (index, op) = operations
            .next()
            .ok_or_else(|| events::corrupt("historical capture artifact absent from commit"))?;
        if operations.next().is_some() {
            return Err(events::corrupt("historical capture artifact is ambiguous"));
        }
        let bytes = engine
            .verify_payload(
                &change.change_id,
                index,
                "proposed",
                &op.target,
                &op.after,
                &op.after_payload,
            )?
            .ok_or_else(|| events::corrupt("historical capture payload missing"))?;
        if Blake3Hash::digest(&bytes) != expected.hash {
            return Err(events::corrupt("historical capture payload hash differs"));
        }
        if expected == reference {
            source_bytes = Some(bytes);
        }
    }
    let bytes = source_bytes.ok_or_else(|| events::corrupt("historical Source missing"))?;
    let note = parse_note(&bytes);
    let source = note
        .canonical
        .ok_or_else(|| events::corrupt("historical Source invalid"))?;
    if source.id() != &reference.record.record_id || source.kind() != RecordKind::Source {
        return Err(events::corrupt(
            "historical Source identity or kind differs",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::common::*;
    use super::*;
    use crate::{
        app::{OfflineApp, OperationOptions},
        catalog::{Catalog, CatalogGraphValidator},
        changes::PreparedChange,
        sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
        vault::WriterPermit,
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    struct Captured {
        _dir: tempfile::TempDir,
        fs: VaultFs,
        job: JobLedger,
        inspection: LedgerInspection,
        frames: Vec<JournalFrame>,
        source: DurableOutputRef,
        revision: DurableOutputRef,
        change: PreparedChange,
    }
    fn request(bytes: &[u8]) -> CaptureRequest {
        CaptureRequest {
            title: "Captured history".into(),
            origin_kind: SourceOrigin::Url,
            origin: "https://example.org/history".into(),
            original: bytes.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: Some("text/plain".into()),
        }
    }
    struct AckCrash(AtomicBool);
    impl LedgerFault for AckCrash {
        fn check(&self, point: LedgerCheckpoint) -> Result<()> {
            if point == LedgerCheckpoint::AfterOutputsCommitted
                && self.0.swap(false, Ordering::SeqCst)
            {
                return Err(WikiError::new(
                    ErrorCode::Internal,
                    "capture acknowledgment crash",
                ));
            }
            Ok(())
        }
    }
    fn captured() -> Captured {
        captured_with_crash(false)
    }
    fn captured_with_crash(crash: bool) -> Captured {
        let (dir, fs, mut job, spec, clock) = fixture(1, |spec| {
            let task = &mut spec.tasks[0];
            task.stage = TaskStage::Capture;
            task.capability = Some(Capability::Fetch);
            task.key = crate::jobs::tasks::task_key(task).unwrap();
        });
        let task = &spec.tasks[0];
        let mut allowance = bound(task);
        allowance.capability = Capability::Fetch;
        allowance.bounds_fingerprint = crate::jobs::budgets::bound_fingerprint(&allowance).unwrap();
        let reserved = job.reserve(&task.key, allowance).unwrap();
        let permit = job.dispatch_intent(reserved).unwrap();
        let send = job.begin_send(permit).unwrap();
        let attempt = send.attempt().clone();
        drop(send);
        job.record_response(
            &attempt,
            ResponseSpoolInput {
                bytes: b"original capture".to_vec(),
                metadata: ResponseMetadata {
                    acquisition: None,
                    provider_request_id: None,
                    returned_model: None,
                    status_code: Some(200),
                    terminal_response: true,
                    usage: KnownOrUnknown::Unknown,
                    computed_cost: KnownOrUnknown::Unknown,
                    failure_code: None,
                },
            },
        )
        .unwrap();
        let source_plan = SourceStore::new(fs.clone())
            .plan_capture(request(b"original capture"))
            .unwrap();
        let draft = source_plan.draft.unwrap();
        let reference = |kind, record_id, suffix: &str| {
            let op = draft
                .operations
                .iter()
                .find(|op| op.target.as_str().ends_with(suffix))
                .unwrap();
            DurableOutputRef {
                record: RecordRef {
                    vault_id: spec.vault_id.clone(),
                    record_id,
                    expected_kind: kind,
                },
                path: op.target.clone(),
                hash: Blake3Hash::digest(op.proposed.as_ref().unwrap()),
            }
        };
        let source = reference(RecordKind::Source, source_plan.source_id, "/source.md");
        let revision = reference(
            RecordKind::Revision,
            source_plan.revision_id,
            "/revision.md",
        );
        let outputs = vec![source.clone(), revision.clone()];
        let mut plan = checkpoint::receipt_plan(
            &job,
            &attempt,
            OutputDisposition::Validated,
            outputs.clone(),
            vec![],
            draft.operations,
        )
        .unwrap();
        plan.draft.allocated_ids.extend(draft.allocated_ids);
        plan.draft
            .read_preconditions
            .extend(draft.read_preconditions);
        let op = plan.draft.operations.last().unwrap();
        let receipt = DurableOutputRef {
            record: RecordRef {
                vault_id: spec.vault_id.clone(),
                record_id: plan.receipt.receipt_id,
                expected_kind: RecordKind::RunEvent,
            },
            path: op.target.clone(),
            hash: Blake3Hash::digest(op.proposed.as_ref().unwrap()),
        };
        let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
        let engine = ChangeEngine::new(fs.clone()).unwrap();
        let change = engine.prepare(&writer, plan.draft).unwrap().prepared;
        engine
            .apply(
                &writer,
                &change,
                &CatalogGraphValidator,
                &Catalog::new(fs.clone(), spec.vault_id.clone()),
            )
            .unwrap();
        drop(writer);
        if crash {
            let mut opts = options(clock);
            opts.fault = Some(Arc::new(AckCrash(AtomicBool::new(true))));
            job = JobLedger::new(fs.clone(), spec.vault_id.clone(), spec.run_id.clone(), opts)
                .unwrap();
        }
        let acknowledged = job.outputs_committed(&attempt, &change, receipt, outputs, vec![]);
        if crash {
            assert_eq!(acknowledged.unwrap_err().code, ErrorCode::Internal);
        } else {
            acknowledged.unwrap();
            job.settle(&attempt).unwrap();
        }
        let inspection = job.inspect().unwrap();
        let bytes = std::fs::read(
            fs.root()
                .path()
                .join(format!(".wiki/state/jobs/{}/journal.bin", spec.run_id)),
        )
        .unwrap();
        let frames = events::decode(&bytes, &spec.run_id).unwrap().frames;
        Captured {
            _dir: dir,
            fs,
            job,
            inspection,
            frames,
            source,
            revision,
            change,
        }
    }
    impl Captured {
        fn task(&self) -> &TaskInspection {
            self.inspection.tasks.values().next().unwrap()
        }
        fn verify(&self, reference: &DurableOutputRef) -> Result<()> {
            output(
                &self.fs,
                &self.inspection,
                &self.frames,
                self.task(),
                reference,
            )
        }
        fn refresh(&self) {
            OfflineApp::new(self.fs.clone(), OperationOptions::default())
                .unwrap()
                .source_refresh(
                    self.source.record.record_id.clone(),
                    request(b"new current capture"),
                )
                .unwrap();
            assert!(checkpoint::output(&self.fs, &self.source).is_err());
        }
    }

    #[test]
    fn refreshed_source_retains_original_capture_and_unknown_hold() {
        let f = captured();
        f.refresh();
        f.verify(&f.source).unwrap();
        f.verify(&f.revision).unwrap();
        assert_eq!(f.job.inspect().unwrap().budget, f.inspection.budget);
        assert_eq!(
            f.inspection.attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
        assert!(!f.inspection.budget.unknown_attempts.is_empty());
    }

    #[test]
    fn current_source_deletion_does_not_restore_or_authorize_current_content() {
        let f = captured();
        let path = f.fs.root().path().join(f.source.path.as_str());
        std::fs::remove_file(&path).unwrap();
        f.verify(&f.source).unwrap();
        assert!(!path.exists());
        assert!(checkpoint::output(&f.fs, &f.source).is_err());
        assert_eq!(f.job.inspect().unwrap().budget, f.inspection.budget);
    }

    #[test]
    fn acknowledged_capture_before_task_finish_survives_source_refresh() {
        let f = captured_with_crash(true);
        assert_eq!(f.task().state, TaskState::Running);
        assert!(f.task().outputs.is_empty());
        assert_eq!(
            f.inspection.attempts[0].phase,
            AttemptPhase::OutputCommitted
        );
        f.refresh();
        f.verify(&f.source).unwrap();
        let replayed = f.job.replay().unwrap().inspection;
        assert_eq!(
            replayed.tasks[&f.task().spec.key].state,
            TaskState::Completed
        );
        f.job.settle(&f.inspection.attempts[0].attempt).unwrap();
        assert_eq!(
            f.job.inspect().unwrap().attempts[0].billing,
            BillingDisposition::UnknownReserved
        );
    }

    #[test]
    fn immutable_revision_and_receipt_tampering_remain_errors() {
        let f = captured();
        f.refresh();
        let revision_path = f.fs.root().path().join(f.revision.path.as_str());
        let original = std::fs::read(&revision_path).unwrap();
        std::fs::write(&revision_path, b"tampered immutable revision").unwrap();
        assert!(f.verify(&f.revision).is_err());
        std::fs::write(&revision_path, original).unwrap();
        let receipt = f.inspection.attempts[0].receipt.as_ref().unwrap();
        std::fs::write(
            f.fs.root().path().join(receipt.path.as_str()),
            b"tampered receipt",
        )
        .unwrap();
        assert!(f.verify(&f.source).is_err());
    }

    #[test]
    fn retained_payload_or_recorded_manifest_tampering_cannot_prove_history() {
        let mut f = captured();
        f.refresh();
        let original_frames = f.frames.clone();
        for frame in &mut f.frames {
            if let EventPayload::OutputsCommitted { change, .. } = &mut frame.event.payload {
                change.manifest_hash = hash("forged manifest");
            }
        }
        assert!(f.verify(&f.source).is_err());
        f.frames = original_frames;
        let engine = ChangeEngine::new(f.fs.clone()).unwrap();
        let change = engine.inspect(&f.change.change_id).unwrap();
        let op = change
            .manifest
            .operations
            .iter()
            .find(|op| op.target == f.source.path)
            .unwrap();
        std::fs::write(
            f.fs.root()
                .path()
                .join(op.after_payload.as_ref().unwrap().path.as_str()),
            b"tampered historical Source",
        )
        .unwrap();
        assert!(f.verify(&f.source).is_err());
    }

    #[test]
    fn unrelated_stage_foreign_owner_or_missing_acknowledgment_cannot_use_fallback() {
        let mut f = captured();
        f.refresh();
        let mut task = f.task().clone();
        task.spec.stage = TaskStage::Extract;
        assert!(output(&f.fs, &f.inspection, &f.frames, &task, &f.source).is_err());
        let mut foreign = f.source.clone();
        foreign.record.vault_id = id("vault_other");
        assert!(f.verify(&foreign).is_err());
        let original_frames = f.frames.clone();
        for frame in &mut f.frames {
            if matches!(frame.event.payload, EventPayload::OutputsCommitted { .. }) {
                frame.event.run_id = id("run_other");
            }
        }
        assert!(f.verify(&f.source).is_err());
        f.frames = original_frames;
        f.frames
            .retain(|frame| !matches!(frame.event.payload, EventPayload::OutputsCommitted { .. }));
        assert!(f.verify(&f.source).is_err());
    }
}

//! Packet-bound extraction within a containing run, with durable research inputs.
use super::{
    VerifiedPacket,
    api_extract::{ApiExtractionOutcome, ExtractionTaskContext},
    generation_cache::{self, GenerationTaskPlan},
    packet,
};
use crate::{
    app::OfflineApp,
    config::providers::TrustedService,
    domain::*,
    jobs::*,
    providers::{dispatcher::Dispatcher, types::*},
    sources::SourceView,
    vault::{VaultFs, WriterPermit},
};
use std::collections::BTreeMap;

/// Pure planning; descriptors are immutable data outside the disposable cache.
pub fn plan_task(
    run_id: &RecordId,
    packet: &VerifiedPacket,
    service: &TrustedService,
    max_output_tokens: u64,
    priority: i32,
    dependencies: Vec<Blake3Hash>,
) -> Result<GenerationTaskPlan> {
    let mut plan = generation_cache::plan_task(packet, service, max_output_tokens)?;
    plan.task.input.path = VaultRelativePath::new(format!(
        "runs/{run_id}/inputs/extraction_{}.json",
        plan.task.input.hash.hex()
    ))?;
    plan.task.priority = priority;
    plan.task.dependencies = dependencies;
    plan.task.key = crate::jobs::tasks::task_key(&plan.task)?;
    Ok(plan)
}

/// Retain a planner-issued descriptor before admission using expected-state storage.
/// A conflicting descriptor cannot overwrite previously admitted actual input.
pub fn retain_input(fs: &VaultFs, writer: &WriterPermit, plan: &GenerationTaskPlan) -> Result<()> {
    let expected = format!("/inputs/extraction_{}.json", plan.task.input.hash.hex());
    if !plan.task.input.path.as_str().starts_with("runs/")
        || !plan.task.input.path.as_str().ends_with(&expected)
        || plan.descriptor.len() > 256 * 1024
        || plan.task.stage != TaskStage::Extract
        || plan.task.capability != Some(Capability::Generate)
        || plan.task.input.byte_len != plan.descriptor.len() as u64
        || plan.task.input.hash != Blake3Hash::digest(&plan.descriptor)
        || plan.task.key != crate::jobs::tasks::task_key(&plan.task)?
    {
        return Err(WikiError::invalid("research extraction descriptor binding"));
    }
    generation_cache::retain_input(fs, writer, plan)
}

impl OfflineApp {
    /// Execute/recover one admitted task, settle its paid attempt, and stage proposals.
    /// The caller owns run start/resume/completion; lifetime limits stay in `ledger`.
    pub fn execute_existing_task(
        &self,
        ledger: &JobLedger,
        task: &TaskSpec,
        verified_packet: &VerifiedPacket,
        service: &TrustedService,
        dispatcher: &Dispatcher,
        new_extraction: bool,
    ) -> Result<ApiExtractionOutcome> {
        let (fs, vault_id, run_id, options) = ledger.dispatcher_bindings();
        if fs.root().path() != self.fs.root().path() || vault_id != self.vault_id {
            return Err(WikiError::invalid(
                "extraction ledger belongs to another vault",
            ));
        }
        let view = SourceView::from_fs_bounded(&self.fs, packet::SOURCE_CAP, 4096)?;
        // Refresh the proof even when the caller retained an earlier VerifiedPacket.
        let current = packet::load_packet(&view, &verified_packet.packet().packet_id)?;
        if current.packet() != verified_packet.packet()
            || current.dependencies() != verified_packet.dependencies()
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "extraction packet changed",
            ));
        }
        let content = view.revision_content_bounded(
            &current.packet().source_id,
            &current.packet().source_revision,
            &mut BTreeMap::new(),
            packet::SOURCE_CAP,
            packet::SOURCE_CAP,
        )?;
        let coverage = packet::coverage(current.packet(), content.len());
        if self.options.dry_run || options.policy.dry_run {
            return Ok(ApiExtractionOutcome {
                packet: current.packet().clone(),
                coverage,
                run_id,
                task_key: Some(task.key.clone()),
                import: None,
                output: None,
                receipt: None,
                reused: false,
                dry_run: true,
            });
        }
        let inspection = ledger.inspect()?;
        if inspection
            .research
            .as_ref()
            .is_some_and(|r| !r.active_tasks.contains(&task.key))
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "extraction task is retired from current research",
            ));
        }
        let admitted = inspection
            .tasks
            .get(&task.key)
            .filter(|t| &t.spec == task)
            .ok_or_else(|| {
                WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "extraction task is not admitted",
                )
            })?;
        if (self.options.offline || options.policy.offline)
            && admitted.state != TaskState::Completed
            && !inspection.attempts.iter().any(|a| {
                a.attempt.task_key == task.key
                    && matches!(
                        a.phase,
                        AttemptPhase::Received
                            | AttemptPhase::OutputCommitted
                            | AttemptPhase::Settled
                    )
            })
        {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "research extraction has no retained response",
            ));
        }
        let bytes = crate::changes::prepare::read_bounded(&self.fs, &task.input.path, 256 * 1024)?
            .ok_or_else(|| {
                WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "research extraction descriptor missing",
                )
            })?;
        if Blake3Hash::digest(&bytes) != task.input.hash
            || bytes.len() as u64 != task.input.byte_len
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "research extraction descriptor changed",
            ));
        }
        let input: RemoteInput = crate::changes::prepare::strict_json(&bytes)?;
        let RemoteOperation::Generate {
            max_output_tokens, ..
        } = input.operation
        else {
            return Err(WikiError::invalid(
                "research extraction requires generation",
            ));
        };
        let expected = plan_task(
            &run_id,
            &current,
            service,
            max_output_tokens,
            task.priority,
            task.dependencies.clone(),
        )?;
        if expected.task != *task || expected.descriptor != bytes {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "research extraction task binding changed",
            ));
        }
        self.execute_generation_task(
            ledger,
            ExtractionTaskContext {
                task,
                packet: &current,
                service,
                dispatcher,
                options: &options,
                new_extraction,
                complete_run: false,
                coverage: &coverage,
            },
        )
    }
}

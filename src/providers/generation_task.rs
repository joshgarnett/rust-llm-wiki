//! Shared accounted generation task identity and immutable descriptor retention.
//! Purpose-specific validation and durable output authority remain with callers.
use crate::{
    changes::{ReadDependency, prepare::read_bounded},
    config::providers::TrustedService,
    domain::*,
    jobs::*,
    providers::{types::*, wire},
    vault::{ExpectedState, VaultFs, WriterPermit},
};

pub struct GenerationTaskPlan {
    pub task: TaskSpec,
    pub descriptor: Vec<u8>,
}

pub(crate) fn plan_task(
    service: &TrustedService,
    input: &RemoteInput,
    stage: TaskStage,
    source_bindings: Vec<ReadDependency>,
) -> Result<GenerationTaskPlan> {
    if service.summary().capability != Capability::Generate
        || !matches!(&input.operation, RemoteOperation::Generate { max_output_tokens, .. } if *max_output_tokens > 0)
        || !matches!(
            stage,
            TaskStage::Extract | TaskStage::PrepareSourceRelations
        )
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "generation task requires a generation service, purpose and positive output bound",
        ));
    }
    let descriptor = crate::graph::packet::canonical_json(input)?;
    if descriptor.len() > 256 * 1024 {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "generation descriptor exceeds dispatcher ceiling; complete input cannot be truncated",
        ));
    }
    let f = wire::task_fingerprints(service, input)?;
    let mut task = TaskSpec {
        key: Blake3Hash::digest([]),
        stage,
        capability: Some(Capability::Generate),
        priority: 0,
        dependencies: vec![],
        input_hash: f.input.clone(),
        prompt_hash: f.prompt,
        schema_hash: f.schema,
        model_hash: Some(f.model),
        settings_hash: f.settings,
        source_bindings,
        input: BoundedPayloadRef {
            path: VaultRelativePath::new(format!(
                ".wiki/cache/generation/inputs/{}.json",
                f.input.hex()
            ))?,
            hash: Blake3Hash::digest(&descriptor),
            byte_len: descriptor.len() as u64,
        },
    };
    task.key = crate::jobs::tasks::task_key(&task)?;
    Ok(GenerationTaskPlan { task, descriptor })
}

pub(crate) fn retain_input(
    fs: &VaultFs,
    writer: &WriterPermit,
    plan: &GenerationTaskPlan,
) -> Result<()> {
    if let Some(existing) = read_bounded(fs, &plan.task.input.path, 256 * 1024)? {
        if existing != plan.descriptor {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "immutable generation input conflicts",
            ));
        }
        return Ok(());
    }
    let parent = plan
        .task
        .input
        .path
        .as_str()
        .rsplit_once('/')
        .ok_or_else(|| WikiError::invalid("generation input parent missing"))?
        .0;
    fs.ensure_directory(&VaultRelativePath::new(parent)?, writer)?;
    let staged = fs.stage(&plan.task.input.path, &plan.descriptor, writer)?;
    fs.replace(staged, &ExpectedState::Absent, writer)?;
    Ok(())
}

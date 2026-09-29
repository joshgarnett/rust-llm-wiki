//! Immutable, nonsecret historical decoding contracts. No credential or transport
//! authority can be constructed here; source freshness is a publication concern.
use super::{generation_wire, types::*, wire_json};
use crate::{
    changes::prepare::read_bounded,
    config::providers::{InstructionRole, OutputLimitField, ResponseMode},
    domain::*,
    graph::packet::canonical_json,
    jobs::{AttemptBound, BoundedPayloadRef, Capability, TaskSpec, budgets, tasks},
    vault::{DirectorySync, ExpectedState, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

const MAX_CODEC_BYTES: usize = 256 * 1024;
const MAX_INPUT_BYTES: usize = 256 * 1024;
const CODEC_DIRECTORY: &str = ".wiki/state/provider-codecs";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    task_key: Blake3Hash,
    task_fingerprint: Blake3Hash,
    descriptor: BoundedPayloadRef,
    role: ServiceRole,
    purpose: DispatchPurpose,
    /// codec=None and fingerprint recomputed, avoiding a self-referential hash.
    original_bound: AttemptBound,
    contract: Contract,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "basis", rename_all = "snake_case", deny_unknown_fields)]
enum Basis {
    UnknownCompatible,
    OpenAiChatGpt41SnapshotV1,
    OpenAiEmbedding3SmallV1 { representation_revision: String },
    OpenAiEmbedding3LargeV1 { representation_revision: String },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    position: u32,
    input_hash: Blake3Hash,
    utf8_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "codec", rename_all = "snake_case", deny_unknown_fields)]
enum Contract {
    Embedding {
        basis: Basis,
        items: Vec<Item>,
        representation_fingerprint: Blake3Hash,
        requested_dimensions: Option<u32>,
        expected_dimensions: Option<u32>,
        maximum_dimensions: u32,
        maximum_coordinates: u64,
    },
    Generation {
        #[serde(default, skip_serializing_if = "GenerationSurface::is_chat")]
        surface: GenerationSurface,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_schema: Option<serde_json::Value>,
        basis: Basis,
        instruction_role: InstructionRole,
        output_limit_field: OutputLimitField,
        response_mode: ResponseMode,
        total_generated_token_limit: u64,
        schema_fingerprint: Blake3Hash,
        schema: serde_json::Value,
    },
}
fn bad(message: &str) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}
fn fingerprint(value: &impl Serialize) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(canonical_json(value)?))
}
fn normalized(bound: &AttemptBound) -> Result<AttemptBound> {
    let mut result = bound.clone();
    result.codec = None;
    result.bounds_fingerprint = budgets::bound_fingerprint(&result)?;
    Ok(result)
}
fn codec_path(hash: &Blake3Hash) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("{CODEC_DIRECTORY}/{}.json", hash.hex()))
}
fn input(fs: &VaultFs, task: &TaskSpec) -> Result<RemoteInput> {
    if task.input.byte_len > MAX_INPUT_BYTES as u64 || task.key != tasks::task_key(task)? {
        return Err(bad("historical task descriptor bound or identity differs"));
    }
    let bytes = read_bounded(fs, &task.input.path, MAX_INPUT_BYTES)?
        .ok_or_else(|| bad("historical descriptor missing; retained response remains protected"))?;
    if bytes.len() as u64 != task.input.byte_len
        || Blake3Hash::digest(&bytes) != task.input.hash
        || task.input_hash != task.input.hash
    {
        return Err(bad("historical descriptor bytes differ"));
    }
    let value = wire_json::parse(&bytes, MAX_INPUT_BYTES, 32768, 48)?;
    let input: RemoteInput =
        serde_json::from_value(value).map_err(|_| bad("historical descriptor invalid"))?;
    if input.version != 1 || canonical_json(&input)? != bytes {
        return Err(bad(
            "historical descriptor version or canonical encoding differs",
        ));
    }
    Ok(input)
}
impl Basis {
    fn capture(basis: &BoundBasis) -> Self {
        match basis {
            BoundBasis::UnknownCompatible => Self::UnknownCompatible,
            BoundBasis::OpenAiChatGpt41SnapshotV1 => Self::OpenAiChatGpt41SnapshotV1,
            BoundBasis::OpenAiEmbedding3SmallV1 {
                representation_revision,
            } => Self::OpenAiEmbedding3SmallV1 {
                representation_revision: representation_revision.clone(),
            },
            BoundBasis::OpenAiEmbedding3LargeV1 {
                representation_revision,
            } => Self::OpenAiEmbedding3LargeV1 {
                representation_revision: representation_revision.clone(),
            },
        }
    }
    fn restore(self, bound: &AttemptBound, role: ServiceRole) -> Result<BoundBasis> {
        let model = bound.requested_model.as_deref();
        Ok(match self {
            Self::UnknownCompatible => BoundBasis::UnknownCompatible,
            Self::OpenAiChatGpt41SnapshotV1
                if role == ServiceRole::Generate && model == Some("gpt-4.1-2025-04-14") =>
            {
                BoundBasis::OpenAiChatGpt41SnapshotV1
            }
            Self::OpenAiEmbedding3SmallV1 {
                representation_revision,
            } if role == ServiceRole::Embed
                && model == Some("text-embedding-3-small")
                && !representation_revision.is_empty()
                && bound.requested_model_revision.as_ref() == Some(&representation_revision) =>
            {
                BoundBasis::OpenAiEmbedding3SmallV1 {
                    representation_revision,
                }
            }
            Self::OpenAiEmbedding3LargeV1 {
                representation_revision,
            } if role == ServiceRole::Embed
                && model == Some("text-embedding-3-large")
                && !representation_revision.is_empty()
                && bound.requested_model_revision.as_ref() == Some(&representation_revision) =>
            {
                BoundBasis::OpenAiEmbedding3LargeV1 {
                    representation_revision,
                }
            }
            _ => return Err(bad("historical codec model/basis differs")),
        })
    }
}
impl Contract {
    fn capture(contract: &WireContract) -> Result<Self> {
        Ok(match contract {
            WireContract::Embedding(c) => Self::Embedding {
                basis: Basis::capture(&c.basis),
                items: c
                    .items
                    .iter()
                    .map(|item| Item {
                        position: item.position,
                        input_hash: item.input_hash.clone(),
                        utf8_bytes: item.utf8_bytes,
                    })
                    .collect(),
                representation_fingerprint: c.representation_fingerprint.clone(),
                requested_dimensions: c.requested_dimensions,
                expected_dimensions: c.expected_dimensions,
                maximum_dimensions: c.maximum_dimensions,
                maximum_coordinates: c.maximum_coordinates,
            },
            WireContract::Generation(c) => Self::Generation {
                surface: c.surface,
                provider_schema: c.provider_schema.as_deref().cloned(),
                basis: Basis::capture(&c.basis),
                instruction_role: c.instruction_role,
                output_limit_field: c.output_limit_field,
                response_mode: c.response_mode,
                total_generated_token_limit: c.total_generated_token_limit,
                schema_fingerprint: c.schema_fingerprint.clone(),
                schema: c.schema.as_ref().clone(),
            },
            #[cfg(test)]
            WireContract::Fixture => {
                return Err(bad("test fixture has no durable production codec"));
            }
        })
    }
    fn restore(
        self,
        input: &RemoteInput,
        bound: &AttemptBound,
        role: ServiceRole,
    ) -> Result<WireContract> {
        match (self, &input.operation) {
            (
                Self::Embedding {
                    basis,
                    items,
                    representation_fingerprint,
                    requested_dimensions,
                    expected_dimensions,
                    maximum_dimensions,
                    maximum_coordinates,
                },
                RemoteOperation::Embed {
                    inputs,
                    expected_dimensions: input_dimensions,
                    representation_fingerprint: input_representation,
                },
            ) if role == ServiceRole::Embed => {
                let basis = basis.restore(bound, role)?;
                let expected_maximum = match basis {
                    BoundBasis::OpenAiEmbedding3SmallV1 { .. } => 1536,
                    BoundBasis::OpenAiEmbedding3LargeV1 { .. } => 3072,
                    _ => 65536,
                };
                if items.is_empty()
                    || items.len() > 4096
                    || items.len() != inputs.len()
                    || representation_fingerprint != *input_representation
                    || maximum_dimensions != expected_maximum
                    || maximum_coordinates != 1_048_576
                    || expected_dimensions != requested_dimensions.or(*input_dimensions)
                    || requested_dimensions
                        .zip(*input_dimensions)
                        .is_some_and(|(a, b)| a != b)
                    || requested_dimensions
                        .into_iter()
                        .chain(expected_dimensions)
                        .any(|d| d == 0 || d > maximum_dimensions)
                    || items
                        .iter()
                        .zip(inputs)
                        .enumerate()
                        .any(|(position, (item, input))| {
                            item.position as usize != position
                                || item.input_hash != input.input_hash
                                || item.utf8_bytes != input.utf8.len() as u64
                                || input.utf8.is_empty()
                                || Blake3Hash::digest(input.utf8.as_bytes()) != input.input_hash
                        })
                {
                    return Err(bad("historical embedding membership or limits differ"));
                }
                Ok(WireContract::Embedding(EmbeddingContract {
                    basis,
                    items: items
                        .into_iter()
                        .map(|item| EmbeddingItemContract {
                            position: item.position,
                            input_hash: item.input_hash,
                            utf8_bytes: item.utf8_bytes,
                        })
                        .collect(),
                    representation_fingerprint,
                    requested_dimensions,
                    expected_dimensions,
                    maximum_dimensions,
                    maximum_coordinates,
                }))
            }
            (
                Self::Generation {
                    surface,
                    provider_schema,
                    basis,
                    instruction_role,
                    output_limit_field,
                    response_mode,
                    total_generated_token_limit,
                    schema_fingerprint,
                    schema,
                },
                RemoteOperation::Generate {
                    output_schema,
                    max_output_tokens,
                    instructions,
                    ..
                },
            ) if role == ServiceRole::Generate => {
                let basis = basis.restore(bound, role)?;
                if (surface == GenerationSurface::Responses
                    && !matches!(basis, BoundBasis::UnknownCompatible))
                    || &schema != output_schema
                    || total_generated_token_limit != *max_output_tokens
                    || *max_output_tokens == 0
                    || instructions.is_empty()
                    || schema_fingerprint != fingerprint(output_schema)?
                    || (matches!(basis, BoundBasis::OpenAiChatGpt41SnapshotV1)
                        && (*max_output_tokens > 32768
                            || !matches!(
                                output_limit_field,
                                OutputLimitField::MaxCompletionTokens
                            )))
                {
                    return Err(bad("historical generation schema or limits differ"));
                }
                match (surface, response_mode, &provider_schema) {
                    (GenerationSurface::Responses, ResponseMode::JsonSchema, Some(grammar)) => {
                        generation_wire::compile_schema(grammar, true)?;
                        if super::generation_schema::project(&schema)? != *grammar {
                            return Err(bad(
                                "historical provider grammar differs from original schema",
                            ));
                        }
                    }
                    (GenerationSurface::Responses, ResponseMode::JsonSchema, None)
                    | (GenerationSurface::ChatCompletions, _, Some(_))
                    | (_, ResponseMode::TextJson, Some(_)) => {
                        return Err(bad("historical provider grammar differs"));
                    }
                    _ => {}
                }
                let validator = generation_wire::compile_schema(
                    &schema,
                    surface == GenerationSurface::ChatCompletions
                        && matches!(response_mode, ResponseMode::JsonSchema),
                )?;
                Ok(WireContract::Generation(GenerationContract {
                    provider_schema: provider_schema.map(Arc::new),
                    surface,
                    basis,
                    instruction_role,
                    output_limit_field,
                    response_mode,
                    total_generated_token_limit,
                    schema_fingerprint,
                    schema: Arc::new(schema),
                    validator,
                }))
            }
            _ => Err(bad("historical codec and operation role differ")),
        }
    }
}
fn validate_identity(
    task: &TaskSpec,
    bound: &AttemptBound,
    input: &RemoteInput,
    role: ServiceRole,
    purpose: DispatchPurpose,
) -> Result<()> {
    let capability = match purpose {
        DispatchPurpose::Task => role.capability(),
        DispatchPurpose::Probe { role: expected } if expected == role => Capability::Probe,
        _ => return Err(bad("historical probe role differs")),
    };
    let (prompt, schema) = match &input.operation {
        RemoteOperation::Generate {
            instructions,
            data,
            output_schema,
            ..
        } => (
            Some(fingerprint(&(instructions, data))?),
            Some(fingerprint(output_schema)?),
        ),
        _ => (None, None),
    };
    if task.capability != Some(capability)
        || bound.capability != capability
        || bound.input_hash != task.input_hash
        || task.prompt_hash != prompt
        || task.schema_hash != schema
        || task.model_hash
            != Some(fingerprint(&(
                &bound.requested_model,
                &bound.requested_model_revision,
            ))?)
        || bound
            .profile_fingerprint
            .as_ref()
            .is_some_and(|p| task.settings_hash != Blake3Hash::digest(p.as_str()))
    {
        return Err(bad(
            "historical task model/prompt/schema/settings proof differs",
        ));
    }
    Ok(())
}
fn durable(sync: DirectorySync) -> Result<()> {
    if sync == DirectorySync::Unsupported {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "historical codec directory durability unavailable",
        ));
    }
    Ok(())
}
/// Called only after production preparation and policy preflight, before any
/// ledger lock/reservation. Never invoke while holding the run-ledger lock.
pub(super) fn retain(fs: &VaultFs, task: &TaskSpec, prepared: &mut PreparedWire) -> Result<()> {
    #[cfg(test)]
    if matches!(prepared.contract, WireContract::Fixture) {
        return Ok(());
    }
    let descriptor = input(fs, task)?;
    if descriptor != prepared.input
        || prepared.bound.bounds_fingerprint != budgets::bound_fingerprint(&prepared.bound)?
    {
        return Err(bad(
            "prepared historical descriptor or allowance fingerprint differs",
        ));
    }
    validate_identity(
        task,
        &prepared.bound,
        &descriptor,
        prepared.role,
        prepared.purpose,
    )?;
    let snapshot = Snapshot {
        version: 1,
        task_key: task.key.clone(),
        task_fingerprint: fingerprint(task)?,
        descriptor: task.input.clone(),
        role: prepared.role,
        purpose: prepared.purpose,
        original_bound: normalized(&prepared.bound)?,
        contract: Contract::capture(&prepared.contract)?,
    };
    let bytes = canonical_json(&snapshot)?;
    if bytes.len() > MAX_CODEC_BYTES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "historical codec snapshot exceeds bound",
        ));
    }
    let hash = Blake3Hash::digest(&bytes);
    let path = codec_path(&hash)?;
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(5))?;
    match read_bounded(fs, &path, MAX_CODEC_BYTES)? {
        Some(old) if old != bytes => return Err(bad("immutable historical codec bytes conflict")),
        Some(_) => durable(fs.sync_target(&path, &writer)?)?,
        None => {
            let parent = VaultRelativePath::new(CODEC_DIRECTORY)?;
            durable(fs.ensure_directory(&parent, &writer)?)?;
            let staged = fs.stage(&path, &bytes, &writer)?;
            durable(fs.replace(staged, &ExpectedState::Absent, &writer)?)?;
        }
    }
    prepared.bound.codec = Some(BoundedPayloadRef {
        path,
        hash,
        byte_len: bytes.len() as u64,
    });
    prepared.bound.bounds_fingerprint = budgets::bound_fingerprint(&prepared.bound)?;
    Ok(())
}
/// Return a decode-only prepared value authenticated by the original attempt.
/// No current service/config, source read preconditions, helper or network call.
pub(super) fn restore(
    fs: &VaultFs,
    task: &TaskSpec,
    bound: &AttemptBound,
    purpose: DispatchPurpose,
) -> Result<PreparedWire> {
    let reference = bound
        .codec
        .as_ref()
        .ok_or_else(|| bad("historical codec missing; retained response remains protected"))?;
    if reference.byte_len > MAX_CODEC_BYTES as u64
        || reference.path != codec_path(&reference.hash)?
        || bound.bounds_fingerprint != budgets::bound_fingerprint(bound)?
    {
        return Err(bad(
            "historical codec path, size or attempt fingerprint differs",
        ));
    }
    let bytes = read_bounded(fs, &reference.path, MAX_CODEC_BYTES)?
        .ok_or_else(|| bad("historical codec unavailable; retained response remains protected"))?;
    if bytes.len() as u64 != reference.byte_len || Blake3Hash::digest(&bytes) != reference.hash {
        return Err(bad("historical codec bytes differ"));
    }
    let value = wire_json::parse(&bytes, MAX_CODEC_BYTES, 32768, 48)?;
    let snapshot: Snapshot =
        serde_json::from_value(value).map_err(|_| bad("historical codec schema invalid"))?;
    if snapshot.version != 1
        || canonical_json(&snapshot)? != bytes
        || snapshot.task_key != task.key
        || snapshot.task_fingerprint != fingerprint(task)?
        || snapshot.descriptor != task.input
        || snapshot.purpose != purpose
        || snapshot.original_bound != normalized(bound)?
    {
        return Err(bad("historical codec version/task/attempt proof differs"));
    }
    let input = input(fs, task)?;
    validate_identity(task, bound, &input, snapshot.role, purpose)?;
    let contract = snapshot.contract.restore(&input, bound, snapshot.role)?;
    Ok(PreparedWire {
        role: snapshot.role,
        purpose,
        method: "HISTORICAL-DECODE-ONLY".into(),
        url: "lwiki:historical-decode-only".into(),
        headers: BTreeMap::new(),
        body: vec![],
        bound: bound.clone(),
        input,
        contract,
    })
}

//! Pure task fingerprints and sealed request construction. No credentials or IO.
use super::types::*;
use crate::config::providers::{InstructionRole, OutputLimitField, ResponseMode};
use crate::{
    config::providers::TrustedService,
    domain::*,
    graph::packet::canonical_json,
    jobs::{self, *},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Serialize)]
pub struct TaskFingerprints {
    pub input: Blake3Hash,
    pub model: Blake3Hash,
    pub settings: Blake3Hash,
    pub prompt: Option<Blake3Hash>,
    pub schema: Option<Blake3Hash>,
}
/// Data fingerprints help callers construct a task; they grant no send authority.
/// Retain `graph::packet::canonical_json(input)` as the task input descriptor.
pub fn task_fingerprints(
    service: &TrustedService,
    input: &RemoteInput,
) -> Result<TaskFingerprints> {
    let (prompt, schema) = match &input.operation {
        RemoteOperation::Generate {
            instructions,
            data,
            output_schema,
            ..
        } => (
            Some(Blake3Hash::digest(canonical_json(&(instructions, data))?)),
            Some(Blake3Hash::digest(canonical_json(output_schema)?)),
        ),
        _ => (None, None),
    };
    Ok(TaskFingerprints {
        input: Blake3Hash::digest(canonical_json(input)?),
        model: Blake3Hash::digest(canonical_json(&(
            service.service().model.clone(),
            service.service().revision.clone(),
        ))?),
        settings: Blake3Hash::digest(service.summary().profile_fingerprint.as_str().as_bytes()),
        prompt,
        schema,
    })
}
pub(super) fn verify_task(
    service: &TrustedService,
    task: &TaskSpec,
    input: &RemoteInput,
) -> Result<()> {
    let got = task_fingerprints(service, input)?;
    if task.input_hash != got.input
        || task.model_hash != Some(got.model)
        || task.settings_hash != got.settings
        || task.prompt_hash != got.prompt
        || task.schema_hash != got.schema
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "task wire inputs or settings differ",
        ));
    }
    Ok(())
}
pub(super) fn json_body<T: Serialize>(service: &TrustedService, value: &T) -> Result<Vec<u8>> {
    struct Limited {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > self.limit)
            {
                return Err(std::io::Error::other("request serialization ceiling"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let limit = usize::try_from(service.service().max_batch_bytes.unwrap_or(256 * 1024))
        .map_err(|_| {
            WikiError::new(
                ErrorCode::BudgetExceeded,
                "request ceiling exceeds platform",
            )
        })?
        .min(256 * 1024);
    let mut writer = Limited {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| WikiError::new(ErrorCode::BudgetExceeded, "request serialization ceiling"))?;
    Ok(writer.bytes)
}
pub(super) fn seal(
    service: &TrustedService,
    task: &TaskSpec,
    input: &RemoteInput,
    purpose: DispatchPurpose,
    role: ServiceRole,
    body: Vec<u8>,
    contract: WireContract,
) -> Result<PreparedWire> {
    verify_task(service, task, input)?;
    let capability = match purpose {
        DispatchPurpose::Task => role.capability(),
        DispatchPurpose::Probe { role: requested } if requested == role => Capability::Probe,
        _ => {
            return Err(WikiError::new(
                ErrorCode::ProfileUntrusted,
                "probe role differs",
            ));
        }
    };
    if task.capability != Some(capability) || service.summary().capability != role.capability() {
        return Err(WikiError::new(
            ErrorCode::ProfileUntrusted,
            "wire capability differs",
        ));
    }
    if body.len() as u64
        > service
            .service()
            .max_batch_bytes
            .unwrap_or(256 * 1024)
            .min(256 * 1024)
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "request body ceiling",
        ));
    }
    let summary = service.summary();
    let mut headers = service.service().headers.clone();
    headers.insert("Content-Type".into(), "application/json".into());
    let wire_hash = Blake3Hash::digest(
        serde_json::to_vec(&(
            "lwiki.wire.v1",
            "POST",
            &service.service().url,
            &headers,
            &body,
            &summary.profile_fingerprint,
        ))
        .map_err(|_| WikiError::invalid("wire fingerprint encoding"))?,
    );
    let (basis, counts): (&BoundBasis, BTreeMap<BillableClass, Option<u64>>) = match &contract {
        WireContract::Embedding(c) => {
            if role != ServiceRole::Embed {
                return Err(WikiError::invalid("embedding wire role differs"));
            }
            let count = match c.basis {
                BoundBasis::OpenAiEmbedding3SmallV1 { .. }
                | BoundBasis::OpenAiEmbedding3LargeV1 { .. } => Some(
                    8192u64
                        .checked_mul(c.items.len() as u64)
                        .ok_or_else(|| WikiError::invalid("embedding token bound overflow"))?
                        .min(300_000),
                ),
                _ => None,
            };
            (&c.basis, BTreeMap::from([(BillableClass::Input, count)]))
        }
        WireContract::Generation(c) => {
            if role != ServiceRole::Generate {
                return Err(WikiError::invalid("generation wire role differs"));
            }
            if std::mem::discriminant(&c.instruction_role)
                != std::mem::discriminant(
                    &service
                        .service()
                        .instruction_role
                        .unwrap_or(InstructionRole::System),
                )
                || std::mem::discriminant(&c.response_mode)
                    != std::mem::discriminant(
                        &service
                            .service()
                            .response_mode
                            .unwrap_or(ResponseMode::TextJson),
                    )
                || task.schema_hash.as_ref() != Some(&c.schema_fingerprint)
            {
                return Err(WikiError::invalid("generation sealed settings differ"));
            }
            let known = matches!(c.basis, BoundBasis::OpenAiChatGpt41SnapshotV1);
            if known
                && (c.total_generated_token_limit == 0
                    || c.total_generated_token_limit > 32768
                    || !matches!(c.output_limit_field, OutputLimitField::MaxCompletionTokens))
            {
                return Err(WikiError::invalid("documented completion bound differs"));
            }
            (
                &c.basis,
                BTreeMap::from([
                    (BillableClass::Input, known.then_some(1_047_576)),
                    (BillableClass::CachedInput, known.then_some(1_047_576)),
                    (
                        BillableClass::Output,
                        known.then_some(c.total_generated_token_limit),
                    ),
                    (
                        BillableClass::Reasoning,
                        known.then_some(c.total_generated_token_limit),
                    ),
                ]),
            )
        }
        #[cfg(test)]
        WireContract::Fixture => {
            return Err(WikiError::invalid(
                "fixture has no production wire authority",
            ));
        }
    };
    let (contract_name, model, endpoint, ceiling) = match basis {
        BoundBasis::UnknownCompatible => (None, "", "", 0),
        BoundBasis::OpenAiChatGpt41SnapshotV1 => (
            Some("openai.gpt41.contract.v1"),
            "gpt-4.1-2025-04-14",
            "https://api.openai.com/v1/chat/completions",
            1_047_576,
        ),
        BoundBasis::OpenAiEmbedding3SmallV1 {
            representation_revision,
        } => {
            if service.service().revision.as_ref() != Some(representation_revision)
                || representation_revision.is_empty()
            {
                return Err(WikiError::invalid(
                    "embedding representation revision differs",
                ));
            }
            (
                Some("openai.embedding3small.contract.v1"),
                "text-embedding-3-small",
                "https://api.openai.com/v1/embeddings",
                8192,
            )
        }
        BoundBasis::OpenAiEmbedding3LargeV1 {
            representation_revision,
        } => {
            if service.service().revision.as_ref() != Some(representation_revision)
                || representation_revision.is_empty()
            {
                return Err(WikiError::invalid(
                    "embedding representation revision differs",
                ));
            }
            (
                Some("openai.embedding3large.contract.v1"),
                "text-embedding-3-large",
                "https://api.openai.com/v1/embeddings",
                8192,
            )
        }
    };
    if contract_name.is_some()
        && (service.service().url != endpoint
            || service.service().model.as_deref() != Some(model)
            || service.service().ca_bytes.is_some())
    {
        return Err(WikiError::invalid(
            "documented model bound identity differs",
        ));
    }
    if service
        .service()
        .max_input_tokens
        .is_some_and(|n| contract_name.is_none() || n < ceiling)
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "configured input token ceiling has no sufficient proof",
        ));
    }
    let applicable_classes: BTreeSet<_> = counts.keys().copied().collect();
    let billable_bounds = counts
        .into_iter()
        .map(|(class, count)| {
            let bound = match (contract_name, count) {
                (Some(method), Some(count)) => TokenBound::ProvenUpper {
                    count,
                    method: method.into(),
                    fingerprint: Blake3Hash::digest(canonical_json(&(
                        method,
                        class,
                        count,
                        &wire_hash,
                        &summary.profile_fingerprint,
                        &service.service().revision,
                    ))?),
                },
                _ => TokenBound::Unknown,
            };
            Ok((class, bound))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut bound = AttemptBound {
        capability,
        profile_id: summary.profile_id,
        endpoint_fingerprint: summary.endpoint_fingerprint,
        config_fingerprint: summary.config_fingerprint,
        input_hash: task.input_hash.clone(),
        wire_hash,
        requested_model: service.service().model.clone(),
        requested_model_revision: service.service().revision.clone(),
        request_bytes: body.len() as u64,
        response_bytes: if role == ServiceRole::Generate {
            GENERATION_SPOOL_MAX_BYTES
        } else {
            OTHER_SPOOL_MAX_BYTES
        },
        timeout_ms: u64::from(service.service().timeout_seconds.unwrap_or(
            if role == ServiceRole::Generate {
                120
            } else {
                60
            },
        )) * 1000,
        applicable_classes,
        billable_bounds,
        rate_card: service.service().rate_card.clone(),
        quoted_allowance: None,
        bounds_fingerprint: Blake3Hash::digest([]),
    };
    if let Some(card) = &bound.rate_card {
        let complete = bound.applicable_classes.iter().all(|class| {
            matches!(
                bound.billable_bounds.get(class),
                Some(TokenBound::Exact { .. } | TokenBound::ProvenUpper { .. })
            ) && card.rates.contains_key(class)
        });
        if complete {
            let mut amount = Money::new(card.currency.clone(), card.request_fee_nanounits);
            for class in &bound.applicable_classes {
                let count = match &bound.billable_bounds[class] {
                    TokenBound::Exact { count, .. } | TokenBound::ProvenUpper { count, .. } => {
                        *count
                    }
                    TokenBound::Estimate { .. } | TokenBound::Unknown => {
                        return Err(WikiError::invalid(
                            "complete quote lacks proven class bound",
                        ));
                    }
                };
                amount = amount.checked_add(&Money::new(
                    card.currency.clone(),
                    card.rates[class].allowance_nanounits(count)?,
                ))?;
            }
            bound.quoted_allowance = Some(amount);
        }
    }
    bound.bounds_fingerprint = jobs::budgets::bound_fingerprint(&bound)?;
    Ok(PreparedWire {
        role,
        purpose,
        method: "POST".into(),
        url: service.service().url.clone(),
        headers,
        body,
        bound,
        input: input.clone(),
        contract,
    })
}

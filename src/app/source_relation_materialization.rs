//! One deterministic derivation for live coordination and typed publication replay.
use super::source_relation_inputs::{InputLimits, authenticate_captured};
use crate::{
    domain::*,
    jobs::{AttemptRef, TaskSpec},
    providers::types::{RemoteInput, RemoteOperation},
    retrieval::{
        source_relation_packet as packet, source_relation_store as store,
        source_relations as relations,
    },
    vault::VaultFs,
};
use std::{collections::BTreeMap, time::Instant};

#[derive(Clone, Copy)]
pub(crate) struct MaterializationLimits {
    pub max_captured_bytes: u64,
    pub max_hash_bytes: u64,
    pub deadline: Instant,
}
pub(crate) struct MaterializedRelations {
    pub raw: store::StoredArtifact,
    pub annotation: store::StoredArtifact,
    /// Charged work: opaque input reservation plus known downstream digests.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained proof and accounting metadata is inspected by focused contract tests."
        )
    )]
    pub reserved_hash_bytes: u64,
    /// Known actual work only; selected-source verification is opaque.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained proof and accounting metadata is inspected by focused contract tests."
        )
    )]
    pub known_hash_bytes: u64,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained proof and accounting metadata is inspected by focused contract tests."
        )
    )]
    pub captured_bytes: u64,
}
struct Meter {
    limits: MaterializationLimits,
    charged: u64,
    known: u64,
}
impl Meter {
    fn remaining(&self) -> Result<u64> {
        if Instant::now() >= self.limits.deadline {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "relation materialization deadline exceeded",
            ));
        }
        self.limits
            .max_hash_bytes
            .checked_sub(self.charged)
            .ok_or_else(|| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "relation materialization hash allowance exceeded",
                )
            })
    }
    fn add(&mut self, charged: u64, known: u64) -> Result<()> {
        if charged > self.limits.max_hash_bytes.saturating_sub(self.charged) {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "relation materialization hash allowance exceeded",
            ));
        }
        self.charged += charged;
        self.known = self.known.checked_add(known).ok_or_else(|| {
            WikiError::new(
                ErrorCode::BudgetExceeded,
                "relation materialization counter overflow",
            )
        })?;
        Ok(())
    }
    fn digest(&mut self, bytes: &[u8]) -> Result<Blake3Hash> {
        self.remaining()?;
        self.add(bytes.len() as u64, bytes.len() as u64)?;
        let hash = Blake3Hash::digest(bytes);
        self.remaining()?;
        Ok(hash)
    }
    fn child<T>(&mut self, result: Result<T>) -> Result<T> {
        result.inspect_err(|error| {
            // Child codecs retain consumed work even on rejection/deadline.
            let reservation = error
                .details
                .get("source_relation_inputs_reserved_hash_bytes")
                .and_then(|v| v.as_u64());
            let known = error
                .details
                .get("source_relation_inputs_known_hash_bytes")
                .and_then(|v| v.as_u64());
            let consumed = [
                "source_relation_store_hash_bytes_consumed",
                "source_relation_assembly_hash_bytes_consumed",
                "source_relation_hash_bytes_consumed",
            ]
            .iter()
            .filter_map(|k| error.details.get(*k).and_then(|v| v.as_u64()))
            .max()
            .unwrap_or(0);
            self.charged = self.charged.saturating_add(reservation.unwrap_or(consumed));
            self.known = self.known.saturating_add(known.unwrap_or(consumed));
        })
    }
}
fn invalid(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ExtractionInvalid, message)
}

fn bounded_envelope(envelope: &relations::Envelope) -> Result<Vec<u8>> {
    struct Buffer(Vec<u8>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .0
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > relations::MAX_ANNOTATION_BYTES)
            {
                return Err(std::io::Error::other(
                    "relation envelope byte ceiling exceeded",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer(Vec::new());
    serde_json::to_writer(&mut buffer, envelope)
        .map_err(|_| invalid("relation envelope byte ceiling exceeded"))?;
    Ok(buffer.0)
}

/// Descriptor and response are exact sealed input and decoded Generated text.
/// Caller supplies authenticated task authority, not proposed artifact metadata.
#[expect(
    clippy::too_many_arguments,
    reason = "Keep distinct proof, scope and budget authorities explicit at this boundary."
)]
pub(crate) fn materialize(
    fs: &VaultFs,
    vault: &RecordId,
    task: &TaskSpec,
    captured: &BTreeMap<VaultRelativePath, Vec<u8>>,
    descriptor: &[u8],
    response: &[u8],
    attempt: &AttemptRef,
    received_timestamp: &str,
    limits: MaterializationLimits,
) -> Result<MaterializedRelations> {
    let mut meter = Meter {
        limits,
        charged: 0,
        known: 0,
    };
    let result = (|| {
        meter.remaining()?;
        if descriptor.is_empty()
            || descriptor.len() > store::MAX_DESCRIPTOR_BYTES
            || response.is_empty()
            || response.len() > store::MAX_RESPONSE_BYTES
        {
            return Err(invalid(
                "relation descriptor/response exceeds finite byte bounds",
            ));
        }
        let authenticated = meter.child(authenticate_captured(
            fs,
            vault,
            task,
            captured,
            InputLimits {
                max_captured_bytes: limits.max_captured_bytes,
                max_hash_bytes: meter.remaining()?,
                deadline: limits.deadline,
            },
        ))?;
        meter.add(
            authenticated.reserved_hash_bytes,
            authenticated.known_hash_bytes,
        )?;
        let input: RemoteInput = serde_json::from_slice(descriptor)
            .map_err(|_| invalid("relation descriptor is not RemoteInput"))?;
        let tokens = match input.operation {
            RemoteOperation::Generate {
                max_output_tokens, ..
            } if max_output_tokens > 0 => max_output_tokens,
            _ => {
                return Err(invalid(
                    "relation descriptor requires positive Generate output bound",
                ));
            }
        };
        let original = || relations::AdmittedOriginal {
            binding: &authenticated.binding,
            text: &authenticated.text,
        };
        let packet = packet::build(original(), tokens, limits.deadline)?;
        let canonical = crate::graph::packet::canonical_json(packet.remote_input())?;
        if canonical != descriptor || task.input.byte_len != descriptor.len() as u64 {
            return Err(invalid(
                "sealed relation descriptor differs from complete canonical source packet",
            ));
        }
        let hash = meter.digest(descriptor)?;
        if hash != task.input.hash || hash != task.input_hash {
            return Err(invalid("relation descriptor task digest differs"));
        }
        let (instructions, data, schema) = match &packet.remote_input().operation {
            RemoteOperation::Generate {
                instructions,
                data,
                output_schema,
                ..
            } => (instructions, data, output_schema),
            _ => unreachable!(),
        };
        let prompt = meter.digest(&crate::graph::packet::canonical_json(&(
            instructions,
            data,
        ))?)?;
        let schema_hash = meter.digest(&crate::graph::packet::canonical_json(schema)?)?;
        if task.prompt_hash.as_ref() != Some(&prompt)
            || task.schema_hash.as_ref() != Some(&schema_hash)
        {
            return Err(invalid("relation prompt/schema task fingerprint differs"));
        }
        // Explicit fixed recipe bytes, including segmentation and both wire schemas.
        let recipe_bytes = crate::graph::packet::canonical_json(&(
            packet::RECIPE,
            packet::INPUT_SCHEMA,
            packet::RAW_SCHEMA,
            packet::FRAGMENT_BYTES,
            packet::INSTRUCTIONS,
            packet::output_schema(),
        ))?;
        let identity = store::PreparationIdentity {
            recipe_id: RecordId::new(packet::RECIPE)?,
            recipe_hash: meter.digest(&recipe_bytes)?,
            prompt_hash: prompt,
            schema_hash,
            model_identity_hash: task
                .model_hash
                .clone()
                .ok_or_else(|| invalid("relation authenticated model identity missing"))?,
            settings_hash: task.settings_hash.clone(),
        };
        let raw = meter.child(store::create_raw(
            vault,
            task,
            &authenticated.binding,
            &identity,
            attempt,
            descriptor,
            response,
            received_timestamp,
            store::StoreLimits {
                max_hash_bytes: meter.remaining()?,
                deadline: limits.deadline,
            },
        ))?;
        meter.add(raw.hash_bytes, raw.hash_bytes)?;
        let binding = identity.bind(&raw.reference);
        let assembled = meter.child(packet.assemble(
            response,
            &binding,
            packet::AssemblyLimits {
                max_hash_bytes: meter.remaining()?,
                deadline: limits.deadline,
            },
        ))?;
        meter.add(assembled.span_hash_bytes, assembled.span_hash_bytes)?;
        let envelope_bytes = bounded_envelope(&assembled.envelope)?;
        let validated = meter.child(relations::parse(
            &envelope_bytes,
            original(),
            &binding,
            relations::ValidationLimits {
                max_hash_bytes: meter.remaining()?,
                deadline: limits.deadline,
            },
        ))?;
        meter.add(
            validated.validation_hash_bytes(),
            validated.validation_hash_bytes(),
        )?;
        let annotation = meter.child(store::create_annotation(
            validated.envelope(),
            &raw.reference,
            task,
            attempt,
            received_timestamp,
            store::StoreLimits {
                max_hash_bytes: meter.remaining()?,
                deadline: limits.deadline,
            },
        ))?;
        meter.add(annotation.hash_bytes, annotation.hash_bytes)?;
        meter.remaining()?;
        Ok(MaterializedRelations {
            raw,
            annotation,
            reserved_hash_bytes: meter.charged,
            known_hash_bytes: meter.known,
            captured_bytes: authenticated.captured_bytes,
        })
    })();
    result.map_err(|mut error| { let child = error.details.clone(); error.details = serde_json::json!({"source_relation_materialization_reserved_hash_bytes":meter.charged,"source_relation_materialization_known_hash_bytes":meter.known,"source_relation_materialization_actual_total_hash_bytes":"unavailable","cause_details":child}); error })
}

#[cfg(test)]
#[path = "source_relation_materialization_tests.rs"]
mod tests;

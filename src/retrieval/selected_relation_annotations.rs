//! Optional preparation authority extends the existing selected closure only.
use super::{SelectedDocuments, capture, conflict, payload};
use crate::{
    catalog::{Catalog, query::QuerySnapshot},
    changes::ReadDependency,
    domain::*,
    providers::types::{RemoteInput, RemoteOperation},
    retrieval::{
        context_types::ContextOmission, source_relation_packet as packet,
        source_relation_store as store, source_relations as relations,
    },
    vault::ExpectedState,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub(crate) struct SourceRelationAnnotation {
    pub owner: VaultRelativePath,
    pub envelope: relations::Envelope,
}
pub(crate) struct LoadedSourceRelations {
    pub annotations: Vec<SourceRelationAnnotation>,
    pub warnings: Vec<String>,
    pub omissions: Vec<ContextOmission>,
    pub deadline: Instant,
}
fn invalid(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ExtractionInvalid, message)
}
// Codec-local finite digest allowance. This is not a new filesystem or global
// query allowance: all stages additionally share the selected proof deadline.
const LOCAL_HASH_BYTES: u64 = 64 * 1024 * 1024;
fn exact_guards(
    actual: &[ReadDependency],
    expected: &BTreeMap<VaultRelativePath, ExpectedState>,
) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .map(|d| (&d.path, &d.expected))
            .collect::<BTreeMap<_, _>>()
            == expected.iter().collect::<BTreeMap<_, _>>()
}
impl SelectedDocuments {
    pub(crate) fn load_source_relations(
        &mut self,
        catalog: &Catalog,
        reader: &QuerySnapshot,
        owner_paths: &[VaultRelativePath],
    ) -> Result<LoadedSourceRelations> {
        self.meter.check()?;
        let deadline = Instant::now() + Duration::from_millis(self.meter.remaining_ms());
        let mut loaded = LoadedSourceRelations {
            annotations: vec![],
            warnings: vec![],
            omissions: vec![],
            deadline,
        };
        let original_states = self.states.clone();
        for path in owner_paths.iter().collect::<BTreeSet<_>>() {
            self.meter.check()?;
            let Some(owner) = self.documents.get(path) else {
                return Err(conflict("relation owner was not selected"));
            };
            let omit = |loaded: &mut LoadedSourceRelations, reason: &str, count| {
                loaded.omissions.push(ContextOmission {
                    record_id: owner.source_id.clone(),
                    path: Some(path.clone()),
                    reason: reason.into(),
                    count,
                });
            };
            let (Some(source_id), Some(revision_id)) = (&owner.source_id, &owner.owner_revision)
            else {
                omit(&mut loaded, "source_relation_preparation_unavailable", 1);
                continue;
            };
            let source = self
                .records
                .get(source_id)
                .ok_or_else(|| conflict("relation source proof absent"))?;
            let revision = self
                .records
                .get(revision_id)
                .ok_or_else(|| conflict("relation revision proof absent"))?;
            if owner.eligibility != Eligibility::Current
                || source.eligibility != Eligibility::Current
                || revision.eligibility != Eligibility::Current
            {
                omit(&mut loaded, "source_relation_preparation_unavailable", 1);
                continue;
            }
            let mut guards = BTreeMap::new();
            for p in [
                VaultRelativePath::new("WIKI.md")?,
                source.path.clone(),
                revision.path.clone(),
                payload(revision, "wiki_original_path")?,
                payload(revision, "wiki_content_path")?,
            ] {
                guards.insert(
                    p.clone(),
                    self.states
                        .get(&p)
                        .ok_or_else(|| conflict("owner closed relation guard absent"))?
                        .clone(),
                );
            }
            let candidates = reader.source_relation_candidates(owner, 80)?;
            let Some(candidate) = candidates.first() else {
                omit(&mut loaded, "source_relation_preparation_unavailable", 1);
                continue;
            };
            if candidates.len() > 1 {
                omit(
                    &mut loaded,
                    "source_relation_additional_preparations",
                    candidates.len() - 1,
                );
            }
            let binding = relations::SourceBinding {
                source_id: source_id.clone(),
                source_revision: revision_id.clone(),
                raw_hash: owner.hash.clone(),
            };
            if candidate.source != binding
                || !exact_guards(&candidate.task.source_bindings, &guards)
            {
                omit(&mut loaded, "source_relation_stale_preparation", 1);
                continue;
            }
            let limits = BTreeMap::from([
                (candidate.annotation.path.clone(), store::MAX_ARTIFACT_BYTES),
                (candidate.raw.path.clone(), store::MAX_ARTIFACT_BYTES),
            ]);
            self.meter.extend_file_byte_limits(&limits);
            let mut bytes = BTreeMap::new();
            for reference in [&candidate.annotation, &candidate.raw] {
                capture(
                    catalog,
                    &reference.path,
                    &ExpectedState::Hash(reference.hash.clone()),
                    &mut bytes,
                    &mut self.states,
                    &mut self.meter,
                )?;
            }
            let codec_limits = store::StoreLimits {
                max_hash_bytes: LOCAL_HASH_BYTES,
                deadline,
            };
            let annotation = store::decode_annotation(
                bytes
                    .get(&candidate.annotation.path)
                    .ok_or_else(|| conflict("relation annotation not captured"))?,
                &candidate.annotation,
                &candidate.raw,
                codec_limits,
            )?
            .artifact;
            // Token bound is retained generation data, not current configuration.
            let raw_bytes = bytes
                .get(&candidate.raw.path)
                .ok_or_else(|| conflict("relation raw generation not captured"))?;
            let raw = store::decode_raw(
                raw_bytes,
                &candidate.raw,
                &candidate.task,
                &binding,
                &candidate.preparation,
                codec_limits,
            )?
            .artifact;
            let input: RemoteInput = serde_json::from_str(&raw.descriptor)
                .map_err(|_| invalid("relation descriptor is not RemoteInput"))?;
            let tokens = match input.operation {
                RemoteOperation::Generate {
                    max_output_tokens, ..
                } if max_output_tokens > 0 => max_output_tokens,
                _ => return Err(invalid("relation descriptor generation bound absent")),
            };
            let original = || relations::AdmittedOriginal {
                binding: &binding,
                text: &owner.raw_text,
            };
            let packet = packet::build(original(), tokens, deadline)?;
            let descriptor = crate::graph::packet::canonical_json(packet.remote_input())?;
            let (instructions, data, schema) = match &packet.remote_input().operation {
                RemoteOperation::Generate {
                    instructions,
                    data,
                    output_schema,
                    ..
                } => (instructions, data, output_schema),
                _ => return Err(invalid("relation packet operation differs")),
            };
            let identity = store::PreparationIdentity {
                recipe_id: RecordId::new(packet::RECIPE)?,
                recipe_hash: Blake3Hash::digest(crate::graph::packet::canonical_json(&(
                    packet::RECIPE,
                    packet::INPUT_SCHEMA,
                    packet::RAW_SCHEMA,
                    packet::FRAGMENT_BYTES,
                    packet::INSTRUCTIONS,
                    packet::output_schema(),
                ))?),
                prompt_hash: Blake3Hash::digest(crate::graph::packet::canonical_json(&(
                    instructions,
                    data,
                ))?),
                schema_hash: Blake3Hash::digest(crate::graph::packet::canonical_json(schema)?),
                model_identity_hash: candidate
                    .task
                    .model_hash
                    .clone()
                    .ok_or_else(|| invalid("relation model identity absent"))?,
                settings_hash: candidate.task.settings_hash.clone(),
            };
            if raw.descriptor.as_bytes() != descriptor
                || candidate.task.input.byte_len != descriptor.len() as u64
                || candidate.task.input.hash != Blake3Hash::digest(&descriptor)
                || candidate.task.input_hash != candidate.task.input.hash
                || candidate.preparation != identity
                || raw.attempt != candidate.attempt
                || annotation.attempt != candidate.attempt
                || annotation.task != candidate.task
                || annotation.envelope.source != binding
            {
                return Err(invalid(
                    "relation fixed recipe, packet or retained authority differs",
                ));
            }
            let preparation = identity.bind(&candidate.raw);
            let assembled = packet.assemble(
                raw.response.as_bytes(),
                &preparation,
                packet::AssemblyLimits {
                    max_hash_bytes: LOCAL_HASH_BYTES,
                    deadline,
                },
            )?;
            if assembled.envelope != annotation.envelope {
                return Err(invalid(
                    "relation annotation differs from retained response",
                ));
            }
            let envelope_bytes =
                serde_json::to_vec(&annotation.envelope).map_err(|e| invalid(&e.to_string()))?;
            relations::parse(
                &envelope_bytes,
                original(),
                &preparation,
                relations::ValidationLimits {
                    max_hash_bytes: LOCAL_HASH_BYTES,
                    deadline,
                },
            )?;
            self.meter.check()?;
            loaded.annotations.push(SourceRelationAnnotation {
                owner: path.clone(),
                envelope: annotation.envelope,
            });
        }
        if !loaded.annotations.is_empty() {
            loaded.warnings.push("Source relation preparations are unassessed navigation metadata; passages remain original source bytes.".into());
            let added: BTreeMap<_, _> = self
                .states
                .iter()
                .filter(|(path, _)| !original_states.contains_key(*path))
                .collect();
            self.fingerprint = Blake3Hash::digest(
                serde_json::to_vec(&(
                    "lwiki-selected-source-relation-proof-v1",
                    &self.fingerprint,
                    "first-stable-path-shortlist80-no-fallback",
                    added,
                ))
                .map_err(|e| invalid(&e.to_string()))?,
            );
        }
        reader.verify_operations(catalog)?;
        self.meter.check()?;
        Ok(loaded)
    }
}
#[cfg(test)]
#[path = "selected_relation_annotations_tests.rs"]
mod tests;

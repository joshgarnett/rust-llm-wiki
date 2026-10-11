//! Pure planning from one selected, completely captured Source. The caller owns
//! metered safe capture, operational identity guards and a final currentness
//! reread before dispatch/publication. No discovery, persistence or remote I/O.
use super::source_relation_inputs::{self, AuthenticatedRelationSource, InputLimits};
use crate::{
    config::providers::TrustedService,
    domain::{ErrorCode, Result, VaultRelativePath, WikiError},
    jobs::TaskStage,
    providers::generation_task::{self, GenerationTaskPlan},
    retrieval::{
        source_relation_packet,
        source_relations::{AdmittedOriginal, SourceBinding},
    },
    sources::{
        revision::canonical_path,
        selected::{SelectedSourceBinding, SelectedSourceProof, verify_captured_source},
    },
    vault::VaultFs,
};
use std::{collections::BTreeMap, time::Instant};

pub(crate) struct RelationTaskPlan {
    pub generation: GenerationTaskPlan,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained proof and accounting metadata is inspected by focused contract tests."
        )
    )]
    pub source: AuthenticatedRelationSource,
    /// Initial selected proof plus real-task authentication reservations. This
    /// is conservative charged work, not observed total digest consumption.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained proof and accounting metadata is inspected by focused contract tests."
        )
    )]
    pub reserved_hash_bytes: u64,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained proof and accounting metadata is inspected by focused contract tests."
        )
    )]
    pub serialized_input_bytes: usize,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained proof and accounting metadata is inspected by focused contract tests."
        )
    )]
    pub fragment_count: usize,
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(budget("source relation planning deadline exceeded"));
    }
    Ok(())
}

/// Preflight before the selected API's much larger general-purpose ceilings.
/// Its closed proof hashes controls and both original/content assets; reserve
/// seven times captured bytes conservatively, including aliases. A second 7T
/// is reserved by authenticate_captured against the actual assembled TaskSpec.
fn selected_proof(
    fs: &VaultFs,
    selected: &SelectedSourceBinding,
    captured: &BTreeMap<VaultRelativePath, Vec<u8>>,
    limits: InputLimits,
) -> Result<(SelectedSourceProof, u64)> {
    check(limits.deadline)?;
    if !(4..=5).contains(&captured.len()) {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "relation planning requires four or five complete selected dependencies",
        ));
    }
    let mut total = 0u64;
    for (path, bytes) in captured {
        check(limits.deadline)?;
        if path.as_str().len() > 4096
            || (canonical_path(path) && bytes.len() > source_relation_inputs::MAX_CONTROL_BYTES)
        {
            return Err(budget("relation planning control or path exceeds ceiling"));
        }
        // Original assets may differ from extracted content; both are retained
        // and authenticated completely within the shared aggregate bound.
        if path == &selected.content_path && bytes.len() > source_relation_inputs::MAX_CONTENT_BYTES
        {
            return Err(budget(
                "complete relation Source exceeds 256 KiB; no truncation performed",
            ));
        }
        total = total
            .checked_add(bytes.len() as u64)
            .filter(|&n| {
                n <= source_relation_inputs::MAX_CAPTURED_BYTES as u64
                    && n <= limits.max_captured_bytes
            })
            .ok_or_else(|| budget("relation planning capture exceeds aggregate allowance"))?;
    }
    let reserved = total
        .checked_mul(7)
        .ok_or_else(|| budget("relation planning hash reservation overflow"))?;
    // Admit both mandatory proof passes before doing either; never hide a
    // second verifier behind a reset allowance.
    if reserved
        .checked_mul(2)
        .is_none_or(|n| n > limits.max_hash_bytes)
    {
        return Err(budget(
            "relation planning proof passes exceed hash allowance",
        ));
    }
    let proof = verify_captured_source(fs, selected, captured).map_err(|mut error| {
        error.details = serde_json::json!({"source_relation_planning_initial_reserved_hash_bytes":reserved,"selected_error_details":error.details});
        error
    })?;
    check(limits.deadline).map_err(|mut error| {
        error.details =
            serde_json::json!({"source_relation_planning_initial_reserved_hash_bytes":reserved});
        error
    })?;
    Ok((proof, reserved))
}

pub(crate) fn plan_captured(
    fs: &VaultFs,
    selected: &SelectedSourceBinding,
    captured: &BTreeMap<VaultRelativePath, Vec<u8>>,
    service: &TrustedService,
    max_output_tokens: u64,
    limits: InputLimits,
) -> Result<RelationTaskPlan> {
    check(limits.deadline)?;
    service.validate_generation_output_limit(max_output_tokens)?;
    let (proof, initial_reserved) = selected_proof(fs, selected, captured, limits)?;
    let mut authenticated_reserved = 0u64;
    let result = (|| {
        check(limits.deadline)?;
        let text = std::str::from_utf8(&proof.content).map_err(|_| {
            WikiError::new(
                ErrorCode::SourceIntegrity,
                "complete relation Source is not UTF-8",
            )
        })?;
        let binding = SourceBinding {
            source_id: selected.source_id.clone(),
            source_revision: selected.revision_id.clone(),
            raw_hash: selected.content_hash.clone(),
        };
        let packet = source_relation_packet::build(
            AdmittedOriginal {
                binding: &binding,
                text,
            },
            max_output_tokens,
            limits.deadline,
        )?;
        check(limits.deadline)?;
        let generation = generation_task::plan_task(
            service,
            packet.remote_input(),
            TaskStage::PrepareSourceRelations,
            proof.dependencies.clone(),
        )?;
        check(limits.deadline)?;
        // Conservatively retain the second pass reservation once it is admitted,
        // including failures after authentication and the final deadline check.
        authenticated_reserved = initial_reserved;
        let source = source_relation_inputs::authenticate_captured(
            fs,
            &selected.vault_id,
            &generation.task,
            captured,
            InputLimits {
                max_hash_bytes: limits.max_hash_bytes - initial_reserved,
                ..limits
            },
        )?;
        if source.binding != binding
            || source.text.as_bytes() != proof.content.as_slice()
            || source.dependencies != proof.dependencies
        {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "real relation task differs from selected complete Source proof",
            ));
        }
        check(limits.deadline)?;
        Ok(RelationTaskPlan {
            reserved_hash_bytes: initial_reserved + source.reserved_hash_bytes,
            serialized_input_bytes: packet.serialized_input_bytes(),
            fragment_count: packet.fragment_count(),
            generation,
            source,
        })
    })();
    result.map_err(|mut error: WikiError| {
        error.details = serde_json::json!({"source_relation_planning_initial_reserved_hash_bytes":initial_reserved,"source_relation_planning_authenticated_reserved_hash_bytes":authenticated_reserved,"source_relation_planning_reserved_hash_bytes":initial_reserved + authenticated_reserved,"planning_error_details":error.details,"actual_total_hash_bytes":"unavailable"}); error
    })
}

#[cfg(test)]
#[path = "source_relation_planning_tests.rs"]
mod tests;

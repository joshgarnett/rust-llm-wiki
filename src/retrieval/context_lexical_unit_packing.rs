//! Preserve raw query-to-unit rank through exact original-membership admission.
use super::{
    context::{DocumentTrial, Packet, document_trial},
    context_types::ContextRequest,
};
use crate::{catalog::query_types::QueryCatalog, domain::*};

pub(super) const CHANNEL: &str = "context_lexical_unit_bm25";
pub(super) struct Selection {
    pub members: Vec<usize>,
    pub rejected: Vec<(usize, &'static str)>,
    pub state: Option<DocumentTrial>,
    pub trials: usize,
}

pub(super) fn raw_score(packet: &Packet) -> Result<f64> {
    if packet.passages.len() != 1
        || packet.bundle.is_some()
        || packet.navigation.is_some()
        || packet.unit_score.is_some()
        || packet.fallback.is_some()
    {
        return Err(WikiError::invalid(
            "lexical unit packet must be one complete document candidate",
        ));
    }
    let scores = packet.passages[0]
        .rank_contributions
        .iter()
        .filter(|r| r.channel == CHANNEL)
        .collect::<Vec<_>>();
    if scores.len() != 1 || scores[0].rank == 0 {
        return Err(WikiError::invalid(
            "lexical unit packet requires exactly one raw score",
        ));
    }
    let score = scores[0]
        .score
        .ok_or_else(|| WikiError::invalid("lexical unit score absent"))?;
    if !score.is_finite() || score < 0.0 {
        return Err(WikiError::invalid(
            "lexical unit score must be finite and nonnegative",
        ));
    }
    Ok(score)
}

pub(super) fn allocate(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    packets: &[Packet],
) -> Result<Selection> {
    let scores = packets.iter().map(raw_score).collect::<Result<Vec<_>>>()?;
    let mut order = (0..packets.len()).collect::<Vec<_>>();
    order.sort_by(|&a, &b| {
        scores[b]
            .total_cmp(&scores[a])
            .then(packets[b].score.total_cmp(&packets[a].score))
            .then(packets[a].key.cmp(&packets[b].key))
    });
    let mut selected = Selection {
        members: vec![],
        rejected: vec![],
        state: None,
        trials: 0,
    };
    let mut original_membership = Vec::new();
    for index in order {
        reader.check_query_budget()?;
        let mut trial_membership = original_membership.clone();
        trial_membership.extend(packets[index].passages.iter().cloned());
        let trial = document_trial(reader, request, &[], &trial_membership)?;
        selected.trials += 1;
        reader.check_query_budget()?;
        if let Some(reason) = trial.reason {
            selected.rejected.push((index, reason));
        } else {
            selected.members.push(index);
            original_membership = trial_membership;
            selected.state = Some(trial);
        }
    }
    reader.check_query_budget()?;
    Ok(selected)
}

#[cfg(test)]
#[path = "context_lexical_unit_packing_tests.rs"]
mod tests;

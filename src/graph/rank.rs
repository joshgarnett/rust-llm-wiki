//! Named reciprocal-rank contributions; degree never contributes relevance.
use super::types::*;
use crate::retrieval::RankContribution;
use std::cmp::Ordering;

pub(crate) fn rrf(contributions: &[RankContribution]) -> f64 {
    contributions
        .iter()
        .map(|c| 1.0 / (60.0 + c.rank as f64))
        .sum()
}
pub(crate) fn seed_order(a: &GraphSeed, b: &GraphSeed) -> Ordering {
    let tier = |seed: &GraphSeed| {
        if seed
            .rank_contributions
            .iter()
            .any(|c| c.channel.ends_with("exact_id"))
        {
            0
        } else if seed
            .rank_contributions
            .iter()
            .any(|c| c.channel.ends_with("exact_title") || c.channel.ends_with("exact_alias"))
        {
            1
        } else {
            2
        }
    };
    tier(a)
        .cmp(&tier(b))
        .then_with(|| b.rrf_score.total_cmp(&a.rrf_score))
        .then(a.record_ref.record_id.cmp(&b.record_ref.record_id))
}
pub(crate) fn assertion_order(a: &GraphAssertion, b: &GraphAssertion) -> Ordering {
    b.direct_seed
        .cmp(&a.direct_seed)
        .then_with(|| {
            let rank = |edge: &GraphAssertion| {
                if edge.direct_seed {
                    return edge.direct_seed_rank.unwrap_or(usize::MAX);
                }
                edge.rank_contributions
                    .iter()
                    .map(|c| c.rank)
                    .min()
                    .unwrap_or(usize::MAX)
            };
            rank(a).cmp(&rank(b))
        })
        .then(a.hop.cmp(&b.hop))
        .then(a.record_ref.record_id.cmp(&b.record_ref.record_id))
}

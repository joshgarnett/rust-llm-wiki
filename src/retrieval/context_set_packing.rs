//! Bounded lexical set assembly over unchanged, indivisible proposals.
//! The representation is a coverage hypothesis, never citation authority.
use super::{
    context::{DocumentTrial, Packet, document_trial},
    context_selection::normalized_lexical_tokens,
    context_types::ContextRequest,
    excerpts::Tokenizer,
};
use crate::{catalog::query_types::QueryCatalog, domain::*};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const MAX_POOL: usize = 320;
const ADDITION_TRIALS: usize = 3072;
const EXCHANGE_TRIALS: usize = 1024;

#[derive(Default, Debug, serde::Serialize)]
pub(super) struct Statistics {
    pub additions: usize,
    pub exchanges: usize,
    pub rejected: usize,
    pub addition_exhausted: bool,
    pub exchange_exhausted: bool,
    pub objective: f64,
}
impl Statistics {
    pub fn exhausted(&self) -> bool {
        self.addition_exhausted || self.exchange_exhausted
    }
}
pub(super) struct Selection<T> {
    /// Original proposal indices, ordered by stable key. Never merged passages.
    pub members: Vec<usize>,
    pub state: Option<T>,
    pub statistics: Statistics,
}
struct Realization<T> {
    state: T,
    bytes: usize,
}

struct Representation {
    similarities: Vec<f64>,
    weights: Vec<f64>,
}
impl Representation {
    fn from_tokens(
        tokens: &[BTreeSet<String>],
        relevance: &[f64],
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        let n = tokens.len();
        let mut frequencies = BTreeMap::<&str, usize>::new();
        for candidate in tokens {
            check()?;
            for term in candidate {
                *frequencies.entry(term).or_default() += 1;
            }
        }
        let idf = frequencies
            .into_iter()
            .map(|(term, df)| (term, 1.0 + ((n + 1) as f64 / (df + 1) as f64).ln()))
            .collect::<BTreeMap<_, _>>();
        let masses = tokens
            .iter()
            .map(|terms| terms.iter().map(|term| idf[term.as_str()]).sum::<f64>())
            .collect::<Vec<_>>();
        let mut similarities = vec![0.0; n * n];
        for i in 0..n {
            check()?;
            similarities[i * n + i] = 1.0;
            for j in 0..i {
                let intersection = tokens[i]
                    .intersection(&tokens[j])
                    .map(|term| idf[term.as_str()])
                    .sum::<f64>();
                let union = masses[i] + masses[j] - intersection;
                let similarity = if union > 0.0 {
                    (intersection / union).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                similarities[i * n + j] = similarity;
                similarities[j * n + i] = similarity;
            }
        }
        let weights = relevance
            .iter()
            .enumerate()
            .map(|(i, relevance)| relevance / similarities[i * n..(i + 1) * n].iter().sum::<f64>())
            .collect();
        Ok(Self {
            similarities,
            weights,
        })
    }

    fn coverage(&self, members: &[usize]) -> Vec<f64> {
        let n = self.weights.len();
        (0..n)
            .map(|i| {
                members
                    .iter()
                    .map(|&j| self.similarities[i * n + j])
                    .fold(0.0, f64::max)
            })
            .collect()
    }
    fn value(&self, coverage: &[f64]) -> f64 {
        self.weights.iter().zip(coverage).map(|(w, c)| w * c).sum()
    }
    fn addition_gain(&self, coverage: &[f64], candidate: usize) -> f64 {
        let n = self.weights.len();
        self.weights
            .iter()
            .zip(coverage)
            .enumerate()
            .map(|(i, (weight, old))| {
                weight * (self.similarities[i * n + candidate] - old).max(0.0)
            })
            .sum()
    }
}

/// Every realization sees the complete original membership, including exchanges
/// whose remove-then-add intermediate state would fail an admission constraint.
fn select<T>(
    keys: &[&str],
    representation: &Representation,
    initial_bytes: usize,
    caps: (usize, usize),
    mut check: impl FnMut() -> Result<()>,
    mut realize: impl FnMut(&[usize]) -> Result<Option<Realization<T>>>,
) -> Result<Selection<T>> {
    let mut order = (0..keys.len()).collect::<Vec<_>>();
    order.sort_by(|&a, &b| keys[a].cmp(keys[b]));
    let mut result = Selection {
        members: vec![],
        state: None,
        statistics: Statistics::default(),
    };
    let mut bytes = initial_bytes;
    let mut coverage = representation.coverage(&[]);
    loop {
        check()?;
        let mut best: Option<(f64, usize, Vec<usize>, Realization<T>)> = None;
        for &candidate in &order {
            if result.members.contains(&candidate) {
                continue;
            }
            let gain = representation.addition_gain(&coverage, candidate);
            if gain <= 0.0 {
                continue;
            }
            if result.statistics.additions == caps.0 {
                result.statistics.addition_exhausted = true;
                break;
            }
            let mut members = result.members.clone();
            members.push(candidate);
            members.sort_by(|&a, &b| keys[a].cmp(keys[b]));
            check()?;
            result.statistics.additions += 1;
            let trial = realize(&members)?;
            check()?;
            let Some(trial) = trial else {
                result.statistics.rejected += 1;
                continue;
            };
            let utility = gain / trial.bytes.saturating_sub(bytes).max(1) as f64;
            if best.as_ref().is_none_or(|(old, old_candidate, _, _)| {
                utility.total_cmp(old).is_gt()
                    || (utility.total_cmp(old).is_eq() && keys[candidate] < keys[*old_candidate])
            }) {
                best = Some((utility, candidate, members, trial));
            }
        }
        // A winner is committed only after the full stable-key round. The
        // prefix's tentative winner is not a completed selection decision.
        if result.statistics.addition_exhausted {
            break;
        }
        if let Some((_, _, members, trial)) = best {
            result.members = members;
            bytes = trial.bytes;
            result.state = Some(trial.state);
            coverage = representation.coverage(&result.members);
            super::context_lexical_unit_diagnostic::record_commit(
                &result.members,
                representation.value(&coverage),
            );
        } else {
            break;
        }
    }
    // One sweep only: stable incoming-key order, each candidate replaces at most
    // one currently selected origin. Neither intermediate set is admitted.
    for &candidate in &order {
        check()?;
        if result.members.contains(&candidate) {
            continue;
        }
        let current_value = representation.value(&coverage);
        let mut best: Option<(f64, usize, Vec<usize>, Realization<T>)> = None;
        for &removed in &result.members {
            let mut members = result
                .members
                .iter()
                .copied()
                .filter(|&i| i != removed)
                .collect::<Vec<_>>();
            members.push(candidate);
            members.sort_by(|&a, &b| keys[a].cmp(keys[b]));
            let gain = representation.value(&representation.coverage(&members)) - current_value;
            if gain <= 0.0 {
                continue;
            }
            if result.statistics.exchanges == caps.1 {
                result.statistics.exchange_exhausted = true;
                break;
            }
            check()?;
            result.statistics.exchanges += 1;
            let trial = realize(&members)?;
            check()?;
            let Some(trial) = trial else {
                result.statistics.rejected += 1;
                continue;
            };
            let utility = gain / trial.bytes.saturating_sub(bytes).max(1) as f64;
            if best.as_ref().is_none_or(|(old, old_removed, _, _)| {
                utility.total_cmp(old).is_gt()
                    || (utility.total_cmp(old).is_eq() && keys[removed] < keys[*old_removed])
            }) {
                best = Some((utility, removed, members, trial));
            }
        }
        // As with additions, an interrupted comparison cannot commit a
        // tentative winner from only a prefix of removable members.
        if result.statistics.exchange_exhausted {
            break;
        }
        if let Some((_, _, members, trial)) = best {
            result.members = members;
            bytes = trial.bytes;
            result.state = Some(trial.state);
            coverage = representation.coverage(&result.members);
            super::context_lexical_unit_diagnostic::record_commit(
                &result.members,
                representation.value(&coverage),
            );
        }
    }
    result.statistics.objective = representation.value(&coverage);
    check()?;
    Ok(result)
}

pub(super) fn allocate(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    packets: &[Packet],
    initial_bytes: usize,
) -> Result<Option<Selection<DocumentTrial>>> {
    allocate_with_caps(
        reader,
        request,
        packets,
        initial_bytes,
        (ADDITION_TRIALS, EXCHANGE_TRIALS),
    )
}

pub(super) fn allocate_with_caps(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    packets: &[Packet],
    initial_bytes: usize,
    caps: (usize, usize),
) -> Result<Option<Selection<DocumentTrial>>> {
    if packets.len() > MAX_POOL {
        return Ok(None);
    }
    let tokenizer = Tokenizer::new(reader.connection())?;
    let mut tokens = Vec::with_capacity(packets.len());
    let mut relevance = Vec::with_capacity(packets.len());
    for packet in packets {
        reader.check_query_budget()?;
        let Some(candidate) = &packet.selection else {
            return Ok(None);
        };
        if packet.passages.len() != 1
            || candidate.semantic_affinity.is_some()
            || packet.unit_score.is_some()
            || packet.bundle.is_some()
            || packet.navigation.is_some()
            || packet.fallback.is_some()
        {
            return Ok(None);
        }
        tokens.push(normalized_lexical_tokens(
            &tokenizer,
            &packet.passages[0].text,
        )?);
        relevance.push(packet.score * candidate.local_relevance as f64);
    }
    super::context_lexical_unit_diagnostic::record_tokens(&tokens);
    if relevance.iter().any(|r| !r.is_finite() || *r < 0.0) || relevance.iter().all(|r| *r == 0.0) {
        return Ok(None);
    }
    let representation =
        Representation::from_tokens(&tokens, &relevance, || reader.check_query_budget())?;
    super::context_lexical_unit_diagnostic::record_representation(
        &representation.weights,
        &representation.similarities,
    );
    let keys = packets
        .iter()
        .map(|packet| packet.key.as_str())
        .collect::<Vec<_>>();
    select(
        &keys,
        &representation,
        initial_bytes,
        caps,
        || {
            super::context_lexical_unit_diagnostic::check_limits()?;
            reader.check_query_budget().inspect_err(|error| {
                super::context_lexical_unit_diagnostic::record_error("query_budget", error)
            })
        },
        |members| {
            let original = members
                .iter()
                .flat_map(|&i| packets[i].passages.iter().cloned())
                .collect::<Vec<_>>();
            super::context_lexical_unit_diagnostic::before_realization()?;
            let trial = document_trial(reader, request, &[], &original).inspect_err(|error| {
                super::context_lexical_unit_diagnostic::record_trial_error(members, error)
            })?;
            super::context_lexical_unit_diagnostic::record_trial(members, &trial);
            Ok(trial.reason.is_none().then(|| Realization {
                bytes: trial.text.len(),
                state: trial,
            }))
        },
    )
    .map(Some)
}

#[cfg(test)]
#[path = "context_set_packing_tests.rs"]
pub(super) mod tests;

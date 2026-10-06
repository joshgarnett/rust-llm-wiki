//! Private, bounded set selection over already authenticated proposal origins.
//! Rendering/admission and the owning absolute deadline remain caller authority.
//! Coverage is representation coverage, never factual completeness or optimality.

const MAX_CANDIDATES: usize = 80;
const TERM_WORDS: usize = super::types::MAX_CONTEXT_QUERY_TERMS.div_ceil(64);

#[derive(Clone, Copy, Debug)]
pub(super) struct Candidate<'a> {
    pub stable_key: &'a str,
    /// Already clipped max(0, compatible query cosine), not a probability.
    pub relevance: f64,
    /// The caller has an existing parent genuinely different from its core.
    pub has_parent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arm {
    L,
    F0,
    F1,
}

pub(super) enum Objective<'a> {
    Lexical {
        term_weights: &'a [f64],
        core_terms: &'a [[u64; TERM_WORDS]],
    },
    /// Row i is represented by column j; caller computes each pair only once.
    Facility { similarities: &'a [f64] },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Variant {
    Core,
    Parent,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Choice {
    pub origin: usize,
    pub variant: Variant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Addition,
    Exchange,
    Expansion,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reason {
    Accepted,
    AdmissionRejected,
    NoPositiveGain,
    RemovedByExchange,
    NoDistinctParent,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Decision {
    pub phase: Phase,
    pub origin: usize,
    pub removed: Option<usize>,
    pub reason: Reason,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FallbackReason {
    ZeroTermWeight,
    ZeroRelevance,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Statistics {
    pub addition_trials: usize,
    pub exchange_trials: usize,
    pub expansion_trials: usize,
    pub scalar_steps: u64,
    pub deadline_checks: u64,
    pub objective_value: f64,
}
impl Statistics {
    pub(super) fn trials(&self) -> usize {
        self.addition_trials + self.exchange_trials + self.expansion_trials
    }
}
pub(super) enum Outcome<State> {
    Fallback(FallbackReason),
    Selected {
        /// Original membership, sorted by stable key, independent of coalescing.
        choices: Vec<Choice>,
        /// None means the caller's existing initial scope-only rendering.
        state: Option<State>,
        decisions: Vec<Decision>,
        statistics: Statistics,
    },
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SelectionError<E> {
    InvalidInput(&'static str),
    External(E),
}

struct Work<'a, Check> {
    check: &'a mut Check,
    statistics: Statistics,
}
impl<Check> Work<'_, Check> {
    fn step<E>(&mut self) -> Result<(), SelectionError<E>>
    where
        Check: FnMut() -> Result<(), E>,
    {
        self.statistics.scalar_steps += 1;
        self.statistics.deadline_checks += 1;
        (self.check)().map_err(SelectionError::External)
    }
}

struct Coverage<'a> {
    objective: Objective<'a>,
    weights: Vec<f64>,
    total: f64,
    count: usize,
}
impl Coverage<'_> {
    fn value(&self, feature: usize, origin: usize) -> f64 {
        match &self.objective {
            Objective::Facility { similarities } => similarities[feature * self.count + origin],
            Objective::Lexical { core_terms, .. } => {
                if core_terms[origin][feature / 64] & (1 << (feature % 64)) != 0 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

struct Cache {
    best: Vec<f64>,
    second: Vec<f64>,
    owner: Vec<Option<usize>>,
    objective: f64,
}
fn cache<E, Check>(
    coverage: &Coverage<'_>,
    selected: &[Choice],
    work: &mut Work<'_, Check>,
) -> Result<Cache, SelectionError<E>>
where
    Check: FnMut() -> Result<(), E>,
{
    let mut cache = Cache {
        best: vec![0.0; coverage.weights.len()],
        second: vec![0.0; coverage.weights.len()],
        owner: vec![None; coverage.weights.len()],
        objective: 0.0,
    };
    for feature in 0..coverage.weights.len() {
        work.step()?;
        for choice in selected {
            work.step()?;
            let value = coverage.value(feature, choice.origin);
            if value > cache.best[feature] {
                cache.second[feature] = cache.best[feature];
                cache.best[feature] = value;
                cache.owner[feature] = Some(choice.origin);
            } else {
                // An equal best from another origin must survive a removal.
                cache.second[feature] = cache.second[feature].max(value);
            }
        }
        cache.objective += coverage.weights[feature] * cache.best[feature];
    }
    cache.objective /= coverage.total;
    Ok(cache)
}
fn objective_after<E, Check>(
    coverage: &Coverage<'_>,
    cached: &Cache,
    added: usize,
    removed: Option<usize>,
    work: &mut Work<'_, Check>,
) -> Result<f64, SelectionError<E>>
where
    Check: FnMut() -> Result<(), E>,
{
    let mut result = 0.0;
    for feature in 0..coverage.weights.len() {
        work.step()?;
        let existing = if removed.is_some() && cached.owner[feature] == removed {
            cached.second[feature]
        } else {
            cached.best[feature]
        };
        result += coverage.weights[feature] * existing.max(coverage.value(feature, added));
    }
    Ok(result / coverage.total)
}

fn realize<State, E, Check, Realize>(
    phase: Phase,
    choices: &[Choice],
    work: &mut Work<'_, Check>,
    callback: &mut Realize,
) -> Result<Option<State>, SelectionError<E>>
where
    Check: FnMut() -> Result<(), E>,
    Realize: FnMut(&[Choice]) -> Result<Option<State>, E>,
{
    work.step()?;
    match phase {
        Phase::Addition => work.statistics.addition_trials += 1,
        Phase::Exchange => work.statistics.exchange_trials += 1,
        Phase::Expansion => work.statistics.expansion_trials += 1,
    }
    let result = callback(choices).map_err(SelectionError::External)?;
    work.step()?;
    Ok(result)
}
fn record(
    decisions: &mut Vec<Decision>,
    phase: Phase,
    origin: usize,
    removed: Option<usize>,
    reason: Reason,
) {
    decisions.push(Decision {
        phase,
        origin,
        removed,
        reason,
    });
}

/// None from realization is an ordinary cap rejection. Err from either caller
/// aborts the whole selection; no partially selected state is returned.
/// L has exchanges; F0 omits them; F1 includes the same one-sweep exchanges.
pub(super) fn select<State, E, Check, Realize>(
    candidates: &[Candidate<'_>],
    objective: Objective<'_>,
    arm: Arm,
    mut check: Check,
    mut callback: Realize,
) -> Result<Outcome<State>, SelectionError<E>>
where
    Check: FnMut() -> Result<(), E>,
    Realize: FnMut(&[Choice]) -> Result<Option<State>, E>,
{
    let mut work = Work {
        check: &mut check,
        statistics: Statistics::default(),
    };
    work.step()?;
    let count = candidates.len();
    if count > MAX_CANDIDATES {
        return Err(SelectionError::InvalidInput("candidate cap exceeded"));
    }
    let mut order = (0..count).collect::<Vec<_>>();
    for candidate in candidates {
        work.step()?;
        if candidate.stable_key.is_empty()
            || !candidate.relevance.is_finite()
            || !(0.0..=1.0).contains(&candidate.relevance)
        {
            return Err(SelectionError::InvalidInput(
                "invalid stable key or query relevance",
            ));
        }
    }
    order.sort_by_key(|&origin| candidates[origin].stable_key);
    for pair in order.windows(2) {
        work.step()?;
        if candidates[pair[0]].stable_key == candidates[pair[1]].stable_key {
            return Err(SelectionError::InvalidInput("duplicate stable origin key"));
        }
    }
    let (weights, fallback) = match &objective {
        Objective::Lexical {
            term_weights,
            core_terms,
        } => {
            if arm != Arm::L
                || term_weights.len() > super::types::MAX_CONTEXT_QUERY_TERMS
                || core_terms.len() != count
            {
                return Err(SelectionError::InvalidInput(
                    "lexical objective shape or arm mismatch",
                ));
            }
            for mask in *core_terms {
                for term in term_weights.len()..TERM_WORDS * 64 {
                    work.step()?;
                    if mask[term / 64] & (1 << (term % 64)) != 0 {
                        return Err(SelectionError::InvalidInput(
                            "term mask outside supplied vocabulary",
                        ));
                    }
                }
            }
            (term_weights.to_vec(), FallbackReason::ZeroTermWeight)
        }
        Objective::Facility { similarities } => {
            if arm == Arm::L || similarities.len() != count * count {
                return Err(SelectionError::InvalidInput(
                    "facility objective shape or arm mismatch",
                ));
            }
            for i in 0..count {
                for j in 0..count {
                    work.step()?;
                    let value = similarities[i * count + j];
                    if !value.is_finite()
                        || !(0.0..=1.0).contains(&value)
                        || value != similarities[j * count + i]
                        || (i == j && value != 1.0)
                    {
                        return Err(SelectionError::InvalidInput(
                            "invalid compatible similarity matrix",
                        ));
                    }
                }
            }
            (
                candidates
                    .iter()
                    .map(|candidate| candidate.relevance)
                    .collect(),
                FallbackReason::ZeroRelevance,
            )
        }
    };
    let mut total = 0.0;
    for &weight in &weights {
        work.step()?;
        if !weight.is_finite() || weight < 0.0 {
            return Err(SelectionError::InvalidInput("invalid objective weight"));
        }
        total += weight;
    }
    if !total.is_finite() {
        return Err(SelectionError::InvalidInput(
            "objective weight sum is not finite",
        ));
    }
    if total == 0.0 {
        return Ok(Outcome::Fallback(fallback));
    }
    let coverage = Coverage {
        objective,
        weights,
        total,
        count,
    };
    let sorted = |choices: &mut Vec<Choice>| {
        choices.sort_by_key(|choice| candidates[choice.origin].stable_key)
    };
    let mut selected = Vec::<Choice>::new();
    let mut state = None;
    let mut decisions = Vec::new();
    let mut cached = cache(&coverage, &selected, &mut work)?;
    let mut pending = order.clone();
    while !pending.is_empty() {
        work.step()?;
        let mut best: Option<(usize, f64)> = None;
        for (position, &origin) in pending.iter().enumerate() {
            work.step()?;
            let gain =
                objective_after(&coverage, &cached, origin, None, &mut work)? - cached.objective;
            // Stable-key iteration keeps the first exact tie. No epsilon.
            if gain > 0.0 && best.is_none_or(|(_, old)| gain.total_cmp(&old).is_gt()) {
                best = Some((position, gain));
            }
        }
        let Some((position, _)) = best else {
            for origin in pending {
                work.step()?;
                record(
                    &mut decisions,
                    Phase::Addition,
                    origin,
                    None,
                    Reason::NoPositiveGain,
                );
            }
            break;
        };
        let origin = pending.remove(position);
        let mut trial = selected.clone();
        trial.push(Choice {
            origin,
            variant: Variant::Core,
        });
        sorted(&mut trial);
        let admitted = realize(Phase::Addition, &trial, &mut work, &mut callback)?;
        record(
            &mut decisions,
            Phase::Addition,
            origin,
            None,
            if admitted.is_some() {
                Reason::Accepted
            } else {
                Reason::AdmissionRejected
            },
        );
        if let Some(next) = admitted {
            selected = trial;
            state = Some(next);
            cached = cache(&coverage, &selected, &mut work)?;
        }
    }
    if arm != Arm::F0 {
        // Freeze unselected origins at sweep entry; do not revisit evicted ones.
        let mut sweep = Vec::new();
        for &origin in &order {
            work.step()?;
            let mut present = false;
            for choice in &selected {
                work.step()?;
                present |= choice.origin == origin;
            }
            if !present {
                sweep.push(origin);
            }
        }
        for origin in sweep {
            work.step()?;
            let mut best: Option<(usize, f64)> = None;
            for choice in &selected {
                work.step()?;
                let gain =
                    objective_after(&coverage, &cached, origin, Some(choice.origin), &mut work)?
                        - cached.objective;
                if gain > 0.0 && best.is_none_or(|(_, old)| gain.total_cmp(&old).is_gt()) {
                    best = Some((choice.origin, gain));
                }
            }
            let Some((removed, _)) = best else {
                record(
                    &mut decisions,
                    Phase::Exchange,
                    origin,
                    None,
                    Reason::NoPositiveGain,
                );
                continue;
            };
            let mut trial = selected
                .iter()
                .copied()
                .filter(|choice| choice.origin != removed)
                .collect::<Vec<_>>();
            trial.push(Choice {
                origin,
                variant: Variant::Core,
            });
            sorted(&mut trial);
            let admitted = realize(Phase::Exchange, &trial, &mut work, &mut callback)?;
            record(
                &mut decisions,
                Phase::Exchange,
                origin,
                Some(removed),
                if admitted.is_some() {
                    Reason::Accepted
                } else {
                    Reason::AdmissionRejected
                },
            );
            if let Some(next) = admitted {
                record(
                    &mut decisions,
                    Phase::Exchange,
                    removed,
                    Some(origin),
                    Reason::RemovedByExchange,
                );
                selected = trial;
                state = Some(next);
                cached = cache(&coverage, &selected, &mut work)?;
            }
        }
    }
    let mut expansions = selected
        .iter()
        .map(|choice| choice.origin)
        .collect::<Vec<_>>();
    expansions.sort_by(|&a, &b| {
        if candidates[a].relevance == candidates[b].relevance {
            candidates[a].stable_key.cmp(candidates[b].stable_key)
        } else {
            candidates[b].relevance.total_cmp(&candidates[a].relevance)
        }
    });
    for origin in expansions {
        work.step()?;
        if !candidates[origin].has_parent {
            record(
                &mut decisions,
                Phase::Expansion,
                origin,
                None,
                Reason::NoDistinctParent,
            );
            continue;
        }
        let mut trial = selected.clone();
        trial
            .iter_mut()
            .find(|choice| choice.origin == origin)
            .unwrap()
            .variant = Variant::Parent;
        let admitted = realize(Phase::Expansion, &trial, &mut work, &mut callback)?;
        record(
            &mut decisions,
            Phase::Expansion,
            origin,
            None,
            if admitted.is_some() {
                Reason::Accepted
            } else {
                Reason::AdmissionRejected
            },
        );
        if let Some(next) = admitted {
            selected = trial;
            state = Some(next);
        }
    }
    work.step()?;
    work.statistics.objective_value = cached.objective;
    debug_assert!(work.statistics.trials() <= 3 * count);
    Ok(Outcome::Selected {
        choices: selected,
        state,
        decisions,
        statistics: work.statistics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn candidates<'a>(keys: &'a [&'a str], relevance: &[f64], parent: bool) -> Vec<Candidate<'a>> {
        keys.iter()
            .zip(relevance)
            .map(|(&stable_key, &relevance)| Candidate {
                stable_key,
                relevance,
                has_parent: parent,
            })
            .collect()
    }
    fn selected(
        outcome: Outcome<Vec<Choice>>,
    ) -> (Vec<Choice>, Option<Vec<Choice>>, Vec<Decision>, Statistics) {
        let Outcome::Selected {
            choices,
            state,
            decisions,
            statistics,
        } = outcome
        else {
            panic!("unexpected fallback")
        };
        (choices, state, decisions, statistics)
    }
    fn bits(value: u64) -> [u64; TERM_WORDS] {
        let mut mask = [0; TERM_WORDS];
        mask[0] = value;
        mask
    }

    #[test]
    fn complementary_core_displaces_repeated_topic_without_extra_votes() {
        let input = candidates(&["b-repeat", "c-complement", "a-topic"], &[0.5; 3], false);
        let masks = [bits(1), bits(2), bits(1)];
        let outcome = select(
            &input,
            Objective::Lexical {
                term_weights: &[1.0; 2],
                core_terms: &masks,
            },
            Arm::L,
            || Ok::<_, &'static str>(()),
            |choices| Ok((choices.len() <= 2).then(|| choices.to_vec())),
        )
        .unwrap();
        let (choices, state, _, stats) = selected(outcome);
        assert_eq!(
            choices.iter().map(|c| c.origin).collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(state, Some(choices));
        assert_eq!(stats.addition_trials, 2);
        assert_eq!(stats.objective_value, 1.0);
    }

    #[test]
    fn exchange_rebuilds_original_membership_instead_of_removing_coalesced_output() {
        // Each term occurs in two of five cores, hence equal positive IDF.
        // A broad initial core + B covers five terms; B+C covers all six.
        let input = candidates(
            &["a-broad", "b-first", "c-second", "d-repeat", "e-repeat"],
            &[0.5; 5],
            false,
        );
        let masks = [
            bits(0b001111),
            bits(0b010011),
            bits(0b101100),
            bits(0b010000),
            bits(0b100000),
        ];
        let mut trials = Vec::new();
        let weights = [(2.4f64).ln(); 6];
        let outcome = select(
            &input,
            Objective::Lexical {
                term_weights: &weights,
                core_terms: &masks,
            },
            Arm::L,
            || Ok::<_, &'static str>(()),
            |choices| {
                trials.push(choices.to_vec());
                Ok((choices.len() <= 2).then(|| choices.to_vec()))
            },
        )
        .unwrap();
        let (choices, state, decisions, stats) = selected(outcome);
        assert_eq!(
            choices.iter().map(|c| c.origin).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(state, Some(choices.clone()));
        assert_eq!(stats.objective_value, 1.0);
        assert!(
            decisions
                .iter()
                .any(|d| d.origin == 0 && d.reason == Reason::RemovedByExchange)
        );
        assert!(trials.iter().any(|trial| trial.len() == 3));
        assert_eq!(trials.last(), Some(&choices));
        assert!(stats.trials() <= 3 * input.len());
    }

    #[test]
    fn facility_exchange_recovers_a_complementary_pair_that_additions_cannot_fit() {
        // Compatible cosines of binary six-coordinate vectors: A=0123,
        // B=014,C=235,D=4,E=5; query covers all six coordinates.
        let t = 1.0 / 3.0f64.sqrt();
        let relevance = [
            2.0 / 6.0f64.sqrt(),
            (0.5f64).sqrt(),
            (0.5f64).sqrt(),
            1.0 / 6.0f64.sqrt(),
            1.0 / 6.0f64.sqrt(),
        ];
        let input = candidates(&["a", "b", "c", "d", "e"], &relevance, false);
        let matrix = [
            1.0, t, t, 0.0, 0.0, t, 1.0, 0.0, t, 0.0, t, 0.0, 1.0, 0.0, t, 0.0, t, 0.0, 1.0, 0.0,
            0.0, 0.0, t, 0.0, 1.0,
        ];
        let run = |arm| {
            selected(
                select(
                    &input,
                    Objective::Facility {
                        similarities: &matrix,
                    },
                    arm,
                    || Ok::<_, &'static str>(()),
                    |choices| Ok((choices.len() <= 2).then(|| choices.to_vec())),
                )
                .unwrap(),
            )
        };
        let (without, _, _, f0) = run(Arm::F0);
        let (with, state, _, f1) = run(Arm::F1);
        assert!(without.iter().any(|c| c.origin == 0));
        assert_eq!(f0.exchange_trials, 0);
        assert_eq!(
            with.iter().map(|c| c.origin).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(state, Some(with));
        assert!(f1.objective_value > f0.objective_value);
        assert!(f1.trials() <= 3 * input.len());
    }

    #[test]
    fn failed_expansion_preserves_the_core_and_parent_is_one_origin() {
        let input = candidates(&["a", "b"], &[0.8, 0.7], true);
        let masks = [bits(1), bits(2)];
        let outcome = select(
            &input,
            Objective::Lexical {
                term_weights: &[1.0; 2],
                core_terms: &masks,
            },
            Arm::L,
            || Ok::<_, &'static str>(()),
            |choices| {
                assert_eq!(
                    choices.len(),
                    choices
                        .iter()
                        .map(|c| c.origin)
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                );
                Ok((!choices
                    .iter()
                    .any(|c| c.origin == 0 && c.variant == Variant::Parent))
                .then(|| choices.to_vec()))
            },
        )
        .unwrap();
        let (choices, state, decisions, stats) = selected(outcome);
        assert_eq!(
            choices,
            vec![
                Choice {
                    origin: 0,
                    variant: Variant::Core
                },
                Choice {
                    origin: 1,
                    variant: Variant::Parent
                }
            ]
        );
        assert_eq!(state, Some(choices));
        assert_eq!(stats.expansion_trials, 2);
        assert!(decisions.iter().any(|d| d.phase == Phase::Expansion
            && d.origin == 0
            && d.reason == Reason::AdmissionRejected));
        assert_eq!(stats.objective_value, 1.0);
    }

    #[test]
    fn zero_signal_falls_back_but_uncovered_positive_query_terms_do_not_invent_evidence() {
        let input = candidates(&["a"], &[0.0], false);
        let masks = [bits(0)];
        let fallback = select(
            &input,
            Objective::Facility {
                similarities: &[1.0],
            },
            Arm::F1,
            || Ok::<_, &'static str>(()),
            |_| -> Result<Option<()>, _> { panic!("fallback cannot render") },
        )
        .unwrap();
        assert!(matches!(
            fallback,
            Outcome::Fallback(FallbackReason::ZeroRelevance)
        ));
        let fallback = select(
            &input,
            Objective::Lexical {
                term_weights: &[],
                core_terms: &masks,
            },
            Arm::L,
            || Ok::<_, &'static str>(()),
            |_| -> Result<Option<()>, _> { panic!("fallback cannot render") },
        )
        .unwrap();
        assert!(matches!(
            fallback,
            Outcome::Fallback(FallbackReason::ZeroTermWeight)
        ));
        let outcome = select(
            &input,
            Objective::Lexical {
                term_weights: &[1.0],
                core_terms: &masks,
            },
            Arm::L,
            || Ok::<_, &'static str>(()),
            |_| -> Result<Option<Vec<Choice>>, _> { panic!("zero gain cannot render") },
        )
        .unwrap();
        let (choices, state, _, stats) = selected(outcome);
        assert!(choices.is_empty());
        assert!(state.is_none());
        assert_eq!(stats.trials(), 0);
    }

    #[test]
    fn deadline_after_realization_aborts_without_returning_partial_packet() {
        let input = candidates(&["a"], &[0.5], false);
        let masks = [bits(1)];
        let rendered = Cell::new(false);
        let result = select(
            &input,
            Objective::Lexical {
                term_weights: &[1.0],
                core_terms: &masks,
            },
            Arm::L,
            || {
                if rendered.get() {
                    Err("deadline")
                } else {
                    Ok(())
                }
            },
            |choices| {
                rendered.set(true);
                Ok(Some(choices.to_vec()))
            },
        );
        assert!(matches!(result, Err(SelectionError::External("deadline"))));
        assert!(rendered.get());
    }

    #[test]
    fn freshness_failure_after_first_admission_aborts_the_entire_packet() {
        let input = candidates(&["a", "b"], &[0.5; 2], false);
        let masks = [bits(1), bits(2)];
        let calls = Cell::new(0);
        let result = select(
            &input,
            Objective::Lexical {
                term_weights: &[1.0; 2],
                core_terms: &masks,
            },
            Arm::L,
            || Ok::<_, &'static str>(()),
            |choices| {
                calls.set(calls.get() + 1);
                if choices.len() == 1 {
                    Ok(Some(choices.to_vec()))
                } else {
                    Err("changed_source")
                }
            },
        );
        assert!(matches!(
            result,
            Err(SelectionError::External("changed_source"))
        ));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn invalid_representation_or_duplicate_origin_never_reaches_renderer() {
        let input = candidates(&["a"], &[0.5], false);
        let result = select(
            &input,
            Objective::Facility {
                similarities: &[f64::NAN],
            },
            Arm::F1,
            || Ok::<_, &'static str>(()),
            |_| -> Result<Option<()>, _> { panic!("invalid matrix cannot render") },
        );
        assert!(matches!(result, Err(SelectionError::InvalidInput(_))));
        let duplicate = candidates(&["a", "a"], &[0.5; 2], false);
        let masks = [bits(1); 2];
        let result = select(
            &duplicate,
            Objective::Lexical {
                term_weights: &[1.0],
                core_terms: &masks,
            },
            Arm::L,
            || Ok::<_, &'static str>(()),
            |_| -> Result<Option<()>, _> { panic!("duplicate cannot render") },
        );
        assert!(matches!(result, Err(SelectionError::InvalidInput(_))));
    }

    #[test]
    fn eighty_origins_have_bounded_trials_and_identical_parents_are_skipped() {
        let keys = (0..80)
            .map(|i| format!("origin-{i:03}"))
            .collect::<Vec<_>>();
        let borrowed = keys.iter().map(String::as_str).collect::<Vec<_>>();
        let input = candidates(&borrowed, &[0.5; 80], false);
        let masks = (0..80)
            .map(|i| {
                let mut mask = [0; TERM_WORDS];
                mask[i / 64] = 1 << (i % 64);
                mask
            })
            .collect::<Vec<_>>();
        let calls = Cell::new(0);
        let outcome = select(
            &input,
            Objective::Lexical {
                term_weights: &[1.0; 80],
                core_terms: &masks,
            },
            Arm::L,
            || Ok::<_, &'static str>(()),
            |choices| {
                calls.set(calls.get() + 1);
                Ok(Some(choices.to_vec()))
            },
        )
        .unwrap();
        let (choices, _, _, stats) = selected(outcome);
        assert_eq!(choices.len(), 80);
        assert_eq!(calls.get(), 80);
        assert_eq!(stats.expansion_trials, 0);
        assert!(stats.trials() <= 240);
    }
}

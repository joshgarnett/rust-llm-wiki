//! Private allocation experiment. Canonical proof and exact rendering remain
//! the coordinator's authority; these objectives do not establish answerability.
use super::{
    context::{self, DocumentTrial, Packet},
    context_types::{ContextPassage, ContextRequest},
    evidence_set_selection::{self as sets, Arm, Candidate, Objective, Outcome, Variant},
    excerpts::{SourceMap, Tokenizer},
    render::RenderedUnit,
    spaces::SpaceSpec,
    types::MAX_CONTEXT_QUERY_TERMS,
    vectors,
};
use crate::{catalog::query_types::QueryCatalog, domain::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Policy {
    Baseline,
    Candidate(Arm),
}

// No public flag or persisted policy. Promotion requires the independent
// development comparison before changing this default.
pub(super) fn policy() -> Policy {
    #[cfg(test)]
    {
        TEST_POLICY.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        Policy::Baseline
    }
}

#[cfg(test)]
std::thread_local! {
    static TEST_POLICY: std::cell::Cell<Policy> = const { std::cell::Cell::new(Policy::Baseline) };
    static TEST_SUMMARY: std::cell::RefCell<Option<serde_json::Value>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(crate) fn with_summary_for_test<T>(f: impl FnOnce() -> T) -> (T, Option<serde_json::Value>) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_SUMMARY.with(|s| {
                s.borrow_mut().take();
            });
        }
    }
    TEST_SUMMARY.with(|s| {
        assert!(s.borrow().is_none(), "evidence-set summary cannot nest");
        *s.borrow_mut() = Some(serde_json::json!({"selection": null}));
    });
    let _reset = Reset;
    let result = f();
    let summary = TEST_SUMMARY.with(|s| s.borrow_mut().take());
    (result, summary)
}
#[cfg(test)]
pub(super) fn record_summary_for_test(key: &str, value: serde_json::Value) {
    TEST_SUMMARY.with(|s| {
        if let Some(summary) = s.borrow_mut().as_mut() {
            summary[key] = value;
        }
    });
}
#[cfg(test)]
pub(crate) fn with_policy_for_test<T>(arm: &str, f: impl FnOnce() -> T) -> T {
    let policy = match arm {
        "B" => Policy::Baseline,
        "L" => Policy::Candidate(Arm::L),
        "F0" => Policy::Candidate(Arm::F0),
        "F1" => Policy::Candidate(Arm::F1),
        _ => panic!("unknown frozen evidence-set arm"),
    };
    struct Reset(Policy);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_POLICY.with(|p| p.set(self.0));
        }
    }
    let _reset = Reset(TEST_POLICY.with(|p| p.replace(policy)));
    f()
}

pub(super) fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "context evidence-set shared deadline exceeded",
        ));
    }
    Ok(())
}

/// Original identity survives focusing and parent deduplication. Vectors are
/// moved from the already bounded compatible selected-owner scoring pass.
#[derive(PartialEq)]
pub(super) struct Representation {
    unit_id: Blake3Hash,
    input_hash: Blake3Hash,
    owner: VaultRelativePath,
    source_hash: Blake3Hash,
    span: ByteSpan,
    space: Blake3Hash,
    cosine: f64,
    vector: Option<Vec<f32>>,
}
pub(super) struct Inputs {
    pub arm: Arm,
    pub deadline: Instant,
    pub space: Blake3Hash,
    pub spec: SpaceSpec,
    pub dimensions: u32,
    representations: Vec<Representation>,
}
impl Inputs {
    pub fn new(arm: Arm, deadline: Instant, state: &vectors::SpaceState) -> Result<Self> {
        check(deadline)?;
        let dimensions = state
            .actual_dimensions
            .ok_or_else(|| WikiError::invalid("evidence-set space has no actual dimensions"))?;
        Ok(Self {
            arm,
            deadline,
            space: state.id.clone(),
            spec: state.spec.clone(),
            dimensions,
            representations: Vec::new(),
        })
    }
    pub fn retain(&mut self, unit: &RenderedUnit, cosine: f64, vector: Vec<f32>) -> Result<()> {
        check(self.deadline)?;
        let span = unit
            .source_span
            .ok_or_else(|| WikiError::invalid("evidence-set origin has no span"))?;
        if unit.target != super::render::TargetKind::Document
            || vector.len() != self.dimensions as usize
            || !cosine.is_finite()
            || !(-1.0..=1.0).contains(&cosine)
        {
            return Err(WikiError::invalid("incompatible evidence-set origin"));
        }
        self.representations.push(Representation {
            unit_id: unit.unit_id.clone(),
            input_hash: unit.input_hash.clone(),
            owner: unit.owner.clone(),
            source_hash: unit.source_hash.clone(),
            span,
            space: self.space.clone(),
            cosine,
            vector: (self.arm != Arm::L).then_some(vector),
        });
        Ok(())
    }
}

pub(super) fn allocate(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    packets: &[Packet],
    query: &str,
    mut input: Inputs,
) -> Result<Outcome<DocumentTrial>> {
    check(input.deadline)?;
    let count = packets.len();
    if count > 80 {
        return Err(WikiError::invalid("evidence-set candidate cap exceeded"));
    }
    if input.spec.id()? != input.space {
        return Err(WikiError::invalid(
            "evidence-set space specification differs",
        ));
    }
    // Bind the strongest original representative retained by select_units.
    // Destroy all noncandidate vectors before allocating the pair matrix.
    let mut originals = BTreeMap::new();
    for representation in std::mem::take(&mut input.representations) {
        check(input.deadline)?;
        let key = (
            representation.owner.clone(),
            representation.span.start(),
            representation.span.end(),
        );
        match originals.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(representation);
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                if entry.get() != &representation {
                    return Err(WikiError::invalid(
                        "conflicting duplicate evidence-set original representation",
                    ));
                }
            }
        }
    }
    let mut retained = Vec::with_capacity(count);
    let mut candidates = Vec::with_capacity(count);
    for packet in packets {
        check(input.deadline)?;
        let parent = packet
            .passages
            .first()
            .ok_or_else(|| WikiError::invalid("empty evidence-set proposal"))?;
        let (span, cosine) = packet
            .unit_origin
            .ok_or_else(|| WikiError::invalid("evidence-set proposal lost original unit"))?;
        let representation = originals
            .remove(&(parent.locator.path.clone(), span.start(), span.end()))
            .ok_or_else(|| WikiError::invalid("evidence-set original representation missing"))?;
        if packet.passages.len() != 1
            || packet.bundle.is_some()
            || packet.navigation.is_some()
            || representation.source_hash != parent.locator.observed_hash
            || representation.space != input.space
            || representation.cosine != cosine
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "evidence-set original binding differs from proposal",
            ));
        }
        // Keep these exact identities with the candidate, never regenerate from
        // focused text. Their embedding identity was authenticated upstream.
        let _identity = (&representation.unit_id, &representation.input_hash);
        candidates.push(Candidate {
            stable_key: &packet.key,
            relevance: cosine.max(0.0),
            has_parent: packet
                .fallback
                .as_ref()
                .is_some_and(|core| core.span != parent.span),
        });
        retained.push(representation);
    }
    drop(originals);
    check(input.deadline)?;
    let mut weights = Vec::new();
    let mut masks = vec![[0u64; 4]; count];
    let mut similarities = Vec::new();
    let objective =
        if input.arm == Arm::L {
            let tokenizer = Tokenizer::new(reader.connection())?;
            let terms = tokenizer
                .tokens(query)?
                .into_iter()
                .map(|t| t.text)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(MAX_CONTEXT_QUERY_TERMS)
                .collect::<Vec<_>>();
            let vocabulary = terms
                .iter()
                .enumerate()
                .map(|(i, term)| (term.as_str(), i))
                .collect::<BTreeMap<_, _>>();
            for (i, packet) in packets.iter().enumerate() {
                check(input.deadline)?;
                let core = core(packet)?;
                let readable = SourceMap::markdown(&core.text, 0);
                for token in tokenizer.tokens(&readable.text)? {
                    check(input.deadline)?;
                    if let Some(&term) = vocabulary.get(token.text.as_str()) {
                        masks[i][term / 64] |= 1 << (term % 64);
                    }
                }
            }
            for term in 0..terms.len() {
                check(input.deadline)?;
                let df = masks
                    .iter()
                    .filter(|mask| mask[term / 64] & (1 << (term % 64)) != 0)
                    .count();
                weights.push((1.0 + ((count - df) as f64 + 0.5) / (df as f64 + 0.5)).ln());
            }
            Objective::Lexical {
                term_weights: &weights,
                core_terms: &masks,
            }
        } else {
            similarities.resize(count * count, 0.0);
            for i in 0..count {
                check(input.deadline)?;
                similarities[i * count + i] = 1.0;
                for j in 0..i {
                    check(input.deadline)?;
                    let a = retained[i].vector.as_deref().ok_or_else(|| {
                        WikiError::invalid("missing compatible evidence-set vector")
                    })?;
                    let b = retained[j].vector.as_deref().ok_or_else(|| {
                        WikiError::invalid("missing compatible evidence-set vector")
                    })?;
                    let value = vectors::cosine(a, b)?.max(0.0);
                    check(input.deadline)?;
                    similarities[i * count + j] = value;
                    similarities[j * count + i] = value;
                }
            }
            Objective::Facility {
                similarities: &similarities,
            }
        };
    let mut cap_reasons = BTreeMap::<&str, usize>::new();
    let outcome = sets::select(
        &candidates,
        objective,
        input.arm,
        || check(input.deadline),
        |choices| {
            check(input.deadline)?;
            let mut proposals = Vec::with_capacity(choices.len());
            for choice in choices {
                check(input.deadline)?;
                let packet = &packets[choice.origin];
                let passage = match choice.variant {
                    Variant::Core => core(packet)?,
                    Variant::Parent => &packet.passages[0],
                };
                proposals.push(passage.clone());
            }
            let trial = context::document_trial(reader, request, &[], &proposals)?;
            check(input.deadline)?;
            if let Some(reason) = trial.reason {
                *cap_reasons.entry(reason).or_default() += 1;
            }
            Ok(if trial.reason.is_none() {
                Some(trial)
            } else {
                None
            })
        },
    )
    .map_err(|error| match error {
        sets::SelectionError::External(error) => error,
        sets::SelectionError::InvalidInput(reason) => WikiError::invalid(reason),
    })?;
    check(input.deadline)?;
    #[cfg(test)]
    record_summary_for_test(
        "selection",
        match &outcome {
            Outcome::Fallback(reason) => {
                serde_json::json!({"arm": format!("{:?}", input.arm), "fallback": format!("{:?}", reason), "candidate_count": count})
            }
            Outcome::Selected {
                choices,
                statistics,
                ..
            } => serde_json::json!({
                "arm": format!("{:?}", input.arm), "candidate_count": count,
                "selected_origins": choices.iter().map(|c| serde_json::json!({
                    "key": packets[c.origin].key, "variant": format!("{:?}", c.variant),
                    "unit_id": retained[c.origin].unit_id, "input_hash": retained[c.origin].input_hash,
                    "source_span": retained[c.origin].span
                })).collect::<Vec<_>>(),
                "render_trials": statistics.trials(), "cap_reasons": cap_reasons,
                "pair_cosines": if input.arm == Arm::L {0} else {count * count.saturating_sub(1) / 2},
                "matrix_bytes": similarities.len() * 8,
                "statistics": {"addition_trials": statistics.addition_trials,
                    "exchange_trials": statistics.exchange_trials, "expansion_trials": statistics.expansion_trials,
                    "scalar_steps": statistics.scalar_steps, "deadline_checks": statistics.deadline_checks,
                    "objective_value": statistics.objective_value}
            }),
        },
    );
    Ok(outcome)
}

fn core(packet: &Packet) -> Result<&ContextPassage> {
    packet
        .fallback
        .as_ref()
        .or_else(|| packet.passages.first())
        .ok_or_else(|| WikiError::invalid("evidence-set proposal has no core"))
}

#[cfg(test)]
#[path = "context_evidence_tests.rs"]
mod tests;

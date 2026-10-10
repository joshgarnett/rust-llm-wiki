use super::*;
use crate::retrieval::{
    RankContribution,
    context_selection::{SelectionDocument, select_lexical_units},
    context_set_packing::tests::{Fixture, candidate, request},
};

fn span(a: usize, b: usize) -> ByteSpan {
    ByteSpan::new(a as u64, b as u64).unwrap()
}
fn scored(f: &Fixture, key: &str, span: ByteSpan, score: f64, owner_score: f64) -> Packet {
    let mut p = f.packet(key, candidate(span, 1));
    p.score = owner_score;
    p.passages[0].rank_contributions.push(RankContribution {
        channel: CHANNEL.into(),
        rank: 1,
        score: Some(score),
    });
    p
}

#[test]
fn unit_bm25_uses_all_constructed_units_and_all_readable_token_lengths() {
    let raw =
        "# A\n\nalpha alpha omega.\n\n# B\n\nalpha beta.\n\n# C\n\nzeta zeta zeta zeta zeta.\n";
    let f = Fixture::new(raw);
    let units = select_lexical_units(
        &f.selected(),
        "alpha",
        &[SelectionDocument {
            owner_index: 0,
            document: &f.document,
            seed_spans: &[],
        }],
        1024,
    )
    .unwrap();
    assert_eq!(units.selection.candidates.len(), 2);
    let idf = (1.0_f64 + (3.0 - 2.0 + 0.5) / (2.0 + 0.5)).ln();
    let average = 10.0 / 3.0;
    let expected = idf * 2.0 * 2.2 / (2.0 + 1.2 * (0.25 + 0.75 * 3.0 / average));
    assert!((units.ranks[0].score.unwrap() - expected).abs() < 1e-12);
    assert_eq!(units.ranks[0].channel, CHANNEL);
    assert_eq!(units.ranks[0].rank, 1);
    assert!(
        units.selection.candidates[0]
            .span
            .slice(raw)
            .unwrap()
            .contains("alpha alpha omega")
    );
    assert!(units.ranks[0].score.unwrap() > units.ranks[1].score.unwrap());
}

#[test]
fn fitting_procedure_and_qualification_have_no_overlapping_discovery_fragment() {
    let raw = "# Procedure\n\nRun ignored cases with the complete procedure.\n\n```sh\nrunner test --ignored\nrunner test --include-ignored\n```\n\nConfirm the exit status unless emergency deployment is explicitly authorized.\n";
    let f = Fixture::new(raw);
    let start = raw.find("ignored").unwrap();
    let seeds = [span(start, start + 7)];
    let units = select_lexical_units(
        &f.selected(),
        "ignored cases",
        &[SelectionDocument {
            owner_index: 0,
            document: &f.document,
            seed_spans: &seeds,
        }],
        1024,
    )
    .unwrap();
    assert_eq!(units.selection.candidates.len(), 1);
    let c = &units.selection.candidates[0];
    assert!(!c.clipped);
    assert!(c.seed_overlap);
    let text = c.span.slice(raw).unwrap();
    assert!(text.contains("--include-ignored"));
    assert!(text.contains("unless emergency deployment"));
}

#[test]
fn raw_score_precedes_owner_rank_and_equal_scores_use_owner_then_stable_key() {
    let f = Fixture::new(&format!("{}\n", "a".repeat(180)));
    let mut r = request();
    let p = [
        scored(&f, "a", span(0, 40), 1.0, 1.0 / 61.0),
        scored(&f, "b", span(60, 100), 2.0, 1.0 / 160.0),
    ];
    let one = document_trial(&f.selected(), &r, &[], &p[1].passages).unwrap();
    r.budget.max_bytes = one.text.len();
    r.budget.max_tokens = one.text.len().div_ceil(4);
    let selected = allocate(&f.selected(), &r, &p).unwrap();
    assert_eq!(selected.members, vec![1]);
    assert_eq!(raw_score(&p[1]).unwrap(), 2.0);
    assert_eq!(p[1].score, 1.0 / 160.0);
    let tied = [
        scored(&f, "a", span(0, 40), 2.0, 0.1),
        scored(&f, "c", span(60, 100), 2.0, 0.2),
        scored(&f, "b", span(120, 160), 2.0, 0.2),
    ];
    // Different decimal offsets have different citation/header byte costs.
    // Every original must fit alone so this assertion isolates ordering.
    r.budget.max_bytes = tied
        .iter()
        .map(|p| {
            document_trial(&f.selected(), &request(), &[], &p.passages)
                .unwrap()
                .text
                .len()
        })
        .max()
        .unwrap();
    r.budget.max_tokens = r.budget.max_bytes.div_ceil(4);
    let selected = allocate(&f.selected(), &r, &tied).unwrap();
    assert_eq!(selected.members, vec![2]);
    assert_eq!(selected.trials, 3);
}

#[test]
fn owner_retention_follows_all_unit_statistics_and_keeps_positive_units_before_seed_zeros() {
    let mut raw = (0..33)
        .map(|i| format!("# Block {i}\n\nalpha item{i}.\n\n"))
        .collect::<String>();
    raw.push_str("# Seed\n\nseedonly tail.\n");
    let f = Fixture::new(&raw);
    let start = raw.find("seedonly").unwrap();
    let seeds = [span(start, start + 8)];
    let units = select_lexical_units(
        &f.selected(),
        "alpha",
        &[SelectionDocument {
            owner_index: 0,
            document: &f.document,
            seed_spans: &seeds,
        }],
        1024,
    )
    .unwrap();
    assert_eq!(units.selection.candidates.len(), 32);
    assert!(
        units
            .selection
            .candidates
            .iter()
            .all(|c| c.span.slice(&raw).unwrap().contains("alpha"))
    );
    assert!(
        units
            .selection
            .omissions
            .iter()
            .any(|o| o.reason == "context_source_candidate_cap")
    );
    // The 33rd positive and seed-only zero participate in DF/average before
    // retention. All 34 units have two readable tokens, so the TF factor is 1.
    let expected = (1.0_f64 + (34.0 - 33.0 + 0.5) / (33.0 + 0.5)).ln();
    assert!(
        units
            .ranks
            .iter()
            .all(|r| (r.score.unwrap() - expected).abs() < 1e-12)
    );
    assert_eq!(units.ranks.last().unwrap().rank, 32);
}

#[test]
fn rejected_merge_preserves_original_membership_and_exact_current_citations() {
    let f = Fixture::new(&format!("{}\n", "a".repeat(240)));
    let r = request();
    let p = [
        scored(&f, "a", span(0, 100), 3.0, 0.1),
        scored(&f, "b", span(90, 150), 2.0, 0.1),
        scored(&f, "c", span(180, 200), 1.0, 0.1),
    ];
    let selected = allocate(&f.selected(), &r, &p).unwrap();
    assert_eq!(selected.members, vec![0, 2]);
    assert_eq!(
        selected.rejected,
        vec![(1, "merged_passage_exceeds_excerpt_bound")]
    );
    assert_eq!(selected.trials, 3);
    let state = selected.state.unwrap();
    assert_eq!(state.passages[0].span, span(0, 100));
    assert!(state.text.contains("Citation (Current):"));
    for passage in &state.passages {
        f.assert_quote(passage);
    }
}

#[test]
fn invalid_or_duplicate_raw_rank_metadata_never_becomes_selection_authority() {
    let f = Fixture::new("alpha behavior.\n");
    let p = scored(&f, "a", span(0, 15), 1.0, 0.1);
    for invalid in 0..4 {
        let mut p = scored(&f, "a", span(0, 15), 1.0, 0.1);
        match invalid {
            0 => p.passages[0].rank_contributions[0].rank = 0,
            1 => p.passages[0].rank_contributions[0].score = Some(f64::NAN),
            2 => p.passages[0].rank_contributions[0].score = None,
            _ => {
                let duplicate = p.passages[0].rank_contributions[0].clone();
                p.passages[0].rank_contributions.push(duplicate);
            }
        }
        assert!(allocate(&f.selected(), &request(), &[p]).is_err());
    }
    assert_eq!(raw_score(&p).unwrap(), 1.0);
}

#[test]
fn public_default_preserves_raw_bm25_metadata_and_host_preparation_keeps_old_builder() {
    use crate::retrieval::{
        context_selection_packet::SelectionAction, context_types::ContextOptions,
    };
    let raw =
        "# A\n\nalpha alpha omega.\n\n# B\n\nalpha beta.\n\n# C\n\nzeta zeta zeta zeta zeta.\n";
    let f = Fixture::new(raw);
    let mut r = request();
    r.documents.limits.excerpt_bytes = 1024;
    r.budget.instruction_bytes = 73;
    r.budget.output_bytes = 91;
    r.budget.instruction_tokens = 19;
    r.budget.output_tokens = 23;
    let native = crate::retrieval::verification::context(&f.catalog, None, "alpha", &r).unwrap();
    assert!(
        native
            .warnings()
            .iter()
            .any(|w| w.starts_with("query-ranked lexical units:"))
    );
    assert!(
        !native
            .warnings()
            .iter()
            .any(|w| w.contains("native lexical set assembly"))
    );
    for p in native.passages() {
        f.assert_quote(p);
        let scores = p
            .rank_contributions
            .iter()
            .filter(|r| r.channel == CHANNEL)
            .map(|rank| {
                assert!(rank.rank > 0);
                rank.score.unwrap()
            })
            .collect::<Vec<_>>();
        assert!(!scores.is_empty());
        assert!(
            scores
                .iter()
                .all(|score| score.is_finite() && *score >= 0.0)
        );
        assert!(
            p.rank_contributions
                .iter()
                .any(|r| r.channel == "direct_document_owner")
        );
    }
    for query_evidence in ["alpha alpha omega", "alpha beta"] {
        let passage = native
            .passages()
            .iter()
            .find(|p| p.text.contains(query_evidence))
            .unwrap();
        assert!(passage.rank_contributions.iter().any(|rank| {
            rank.channel == CHANNEL && rank.score.is_some_and(|score| score > 0.0)
        }));
    }
    // The 1024-byte discovery seed covers this entire short source, so its
    // unrelated complete C unit is intentionally eligible with raw score 0.
    let seed_only = native
        .passages()
        .iter()
        .find(|p| p.text.contains("zeta zeta"))
        .unwrap();
    assert!(
        seed_only
            .rank_contributions
            .iter()
            .any(|rank| { rank.channel == CHANNEL && rank.score == Some(0.0) })
    );
    assert!(native.usage().rendered_bytes <= r.budget.max_bytes - 164);
    assert!(native.usage().estimated_tokens <= r.budget.max_tokens - 42);
    let host = crate::retrieval::verification::context_with_options(
        &f.catalog,
        None,
        "alpha",
        &r,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            ..Default::default()
        },
    )
    .unwrap();
    let packet = host.selection_packet().unwrap();
    assert!(packet.cards.iter().all(|c| {
        c.passage
            .rank_contributions
            .iter()
            .all(|r| r.channel != CHANNEL)
    }));
    assert!(
        !host
            .warnings()
            .iter()
            .any(|w| w.starts_with("query-ranked lexical units:"))
    );
    // Explicit nondefault owner pools also retain the original proposal path.
    r.documents.limits.hits = 11;
    let original = crate::retrieval::verification::context(&f.catalog, None, "alpha", &r).unwrap();
    assert!(
        original
            .passages()
            .iter()
            .all(|p| p.rank_contributions.iter().all(|r| r.channel != CHANNEL))
    );
}

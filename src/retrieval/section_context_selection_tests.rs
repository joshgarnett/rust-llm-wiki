use super::*;
use std::cell::Cell;

fn run(raw: &str, query: &str, bytes: usize, sections: bool) -> SelectionResult {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let tokenizer = Tokenizer::new(&connection).unwrap();
    select_with_sections(
        &tokenizer,
        query,
        &[Parent {
            owner: 0,
            raw,
            body: 0,
            anchors: &[],
            semantic: vec![],
        }],
        bytes,
        sections,
    )
    .unwrap()
}

#[test]
fn governing_ancestry_survives_repeated_heading_labels_without_uncited_coverage() {
    let raw = "# Engine\n\nEngine uses the current mode.\n\n## Safety\n\nMixing is forbidden.\n\n# Engine\n\nEngine uses the earlier mode.\n\n## Safety\n\nMixing is permitted.\n";
    let selected = run(raw, "engine safety mixing", 256, true);
    let safety = selected
        .candidates
        .iter()
        .filter(|candidate| candidate.span.slice(raw).unwrap().contains("## Safety"))
        .collect::<Vec<_>>();
    assert_eq!(safety.len(), 2);
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let tokenizer = Tokenizer::new(&connection).unwrap();
    let (terms, _) = query_terms(&tokenizer, "engine safety mixing").unwrap();
    for candidate in &safety {
        let section = candidate.section.as_ref().unwrap();
        assert_eq!(section.ancestors.len(), 2);
        assert!(
            section.ancestors[0]
                .slice(raw)
                .unwrap()
                .contains("# Engine")
        );
        assert_eq!(section.ancestors[1], section.heading_span);
        assert!(!candidate.covered_terms.contains(&terms["engin"]));
        assert!(!section.covered_terms.contains(&terms["engin"]));
        assert_eq!(candidate.span, section.section_span);
    }
    assert_ne!(
        safety[0].section.as_ref().unwrap().heading_span,
        safety[1].section.as_ref().unwrap().heading_span
    );
}

#[test]
fn fitting_sections_keep_governing_heading_prose_list_and_example_together() {
    let raw = "# Scheduling\n\nScheduling follows this order:\n\n- Local setting\n- Shared setting\n\n```text\nworkers = 2\n```\n\nOnly idle workers may start.\n\n# Other\n\nUnrelated material.\n";
    let selected = run(raw, "scheduling", 512, true);
    assert_eq!(selected.candidates.len(), 1);
    let candidate = &selected.candidates[0];
    let text = candidate.span.slice(raw).unwrap();
    assert!(text.starts_with("# Scheduling"));
    assert!(text.contains("Shared setting"));
    assert!(text.contains("workers = 2"));
    assert!(text.contains("Only idle workers"));
    assert!(!text.contains("# Other"));
    assert!(!candidate.clipped);
}

#[test]
fn heading_inside_a_fitting_list_retains_its_existing_governing_parent() {
    let raw = "# Parent\n\nParent governs these settings:\n\n- First item\n\n  ## Nested\n\n  Nested changes remain governed here.\n\n- Last item\n";
    let selected = run(raw, "parent nested", 512, true);
    assert_eq!(selected.candidates.len(), 1);
    let candidate = &selected.candidates[0];
    let section = candidate.section.as_ref().unwrap();
    assert_eq!(section.ancestors, vec![section.heading_span]);
    assert_eq!(section.heading_span.slice(raw).unwrap(), "# Parent\n");
    let text = candidate.span.slice(raw).unwrap();
    assert!(text.contains("## Nested") && text.contains("Last item"));
    assert!(text.len() <= 512);
    assert!(!candidate.clipped);
}

#[test]
fn fitting_teaching_group_stays_intact_when_its_section_requires_multiple_children() {
    let raw = format!(
        "# Scheduling\n\n{}\n\nScheduling follows this order:\n\n- Local setting\n- Shared setting\n\n{}\n",
        "Background information is useful. ".repeat(4),
        "Further details remain contextual. ".repeat(4)
    );
    let selected = run(&raw, "scheduling", 112, true);
    let grouped = selected
        .candidates
        .iter()
        .find(|candidate| {
            candidate
                .span
                .slice(&raw)
                .unwrap()
                .contains("follows this order")
        })
        .unwrap();
    let text = grouped.span.slice(&raw).unwrap();
    assert!(text.contains("Local setting") && text.contains("Shared setting"));
    assert!(!grouped.clipped);
    assert!(
        selected
            .candidates
            .iter()
            .any(|candidate| candidate.covered_terms.is_empty())
    );
}

#[test]
fn oversized_intro_list_has_exact_nonoverlapping_continuations_and_one_nominee() {
    let raw = format!(
        "# Limits\n\nLimits apply in the following order:\n\n{}",
        (0..16)
            .map(|index| format!("- Option {index}: preserve the complete explanation.\n"))
            .collect::<String>()
    );
    let selected = run(&raw, "limits option", 128, true);
    let mut children = selected.candidates.iter().collect::<Vec<_>>();
    children.sort_by_key(|candidate| candidate.span.start());
    assert!(children.len() > 2);
    assert!(children.iter().any(|candidate| candidate.clipped));
    let nominee = children[0].section.as_ref().unwrap().representative_ordinal;
    for candidate in &children {
        let section = candidate.section.as_ref().unwrap();
        assert_eq!(section.representative_ordinal, nominee);
        assert!(candidate.span.slice(&raw).unwrap().len() <= 128);
    }
    assert_eq!(
        children
            .iter()
            .filter(|candidate| candidate.section.as_ref().unwrap().child_ordinal == nominee)
            .count(),
        1
    );
    for pair in children.windows(2) {
        assert!(pair[0].span.end() <= pair[1].span.start());
        if pair[0].clipped && pair[1].clipped {
            assert_eq!(pair[0].span.end(), pair[1].span.start());
        }
    }
}

#[test]
fn distant_governing_section_survives_many_children_under_the_same_pool_cap() {
    let raw = format!(
        "# Quasar\n\n{}# Orbit\n\nOrbit has the complementary behavior.\n",
        "Quasar uses the documented ordinary mechanism.\n\n".repeat(90)
    );
    let selected = run(&raw, "quasar orbit", 128, true);
    let baseline = run(&raw, "quasar orbit", 128, false);
    assert_eq!(selected.term_weights, baseline.term_weights);
    assert_eq!(selected.scanned_bytes, baseline.scanned_bytes);
    assert_eq!(selected.scanned_blocks, baseline.scanned_blocks);
    assert_eq!(selected.candidates.len(), MAX_CANDIDATES);
    assert!(
        selected.candidates.iter().any(|candidate| candidate
            .span
            .slice(&raw)
            .unwrap()
            .contains("Orbit has"))
    );
    assert!(
        selected
            .omissions
            .iter()
            .any(|omission| omission.reason == "context_source_candidate_cap")
    );
    let headings = selected
        .candidates
        .iter()
        .map(|candidate| candidate.section.as_ref().unwrap().heading_span.start())
        .collect::<BTreeSet<_>>();
    for heading in headings {
        assert_eq!(
            selected
                .candidates
                .iter()
                .filter(|candidate| {
                    let section = candidate.section.as_ref().unwrap();
                    section.heading_span.start() == heading
                        && section.child_ordinal == section.representative_ordinal
                })
                .count(),
            1
        );
    }
}

#[test]
fn utf8_continuations_remain_exact_even_when_a_scalar_cannot_fit() {
    let raw = "# Café\n\nCafé 中文 🦀 restricts the ordinary behavior.\n\n".repeat(4);
    for bytes in [1, 3, 7, 23] {
        let selected = run(&raw, "café", bytes, true);
        for candidate in selected.candidates {
            assert!(candidate.span.slice(&raw).unwrap().len() <= bytes);
        }
        if bytes == 1 {
            assert!(
                selected
                    .omissions
                    .iter()
                    .any(|omission| omission.reason == "context_source_excerpt_byte_cap")
            );
        }
    }
}

#[test]
fn unstructured_text_keeps_the_complete_legacy_candidate_sequence_and_accounting() {
    for raw in [
        "Quasar uses a cached record.\n\nOnly an explicit refresh changes that record.\n",
        "<h1>Quasar</h1><p>The record is cached.</p><p>Only refresh changes it.</p>",
    ] {
        let old = run(raw, "quasar cached refresh", 72, false);
        let new = run(raw, "quasar cached refresh", 72, true);
        let signature = |result: &SelectionResult| {
            result
                .candidates
                .iter()
                .map(|candidate| {
                    (
                        candidate.span,
                        candidate.covered_terms.clone(),
                        candidate.local_relevance,
                        candidate.clipped,
                        candidate.seed_overlap,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(signature(&old), signature(&new));
        assert_eq!(old.term_weights, new.term_weights);
        assert_eq!(old.scanned_bytes, new.scanned_bytes);
        assert_eq!(old.scanned_blocks, new.scanned_blocks);
        assert!(
            new.candidates
                .iter()
                .all(|candidate| candidate.section.is_none())
        );
    }
}

#[test]
fn scanned_anchor_cannot_create_an_ungated_window_inside_a_section() {
    let raw = "# Offline\n\nOffline requests use cached records.\n\nThe exception is a deliberate refresh.\n";
    let anchor = ByteSpan::new(15, 28).unwrap();
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let tokenizer = Tokenizer::new(&connection).unwrap();
    let selected = select_with_sections(
        &tokenizer,
        "offline",
        &[Parent {
            owner: 0,
            raw,
            body: 0,
            anchors: &[anchor],
            semantic: vec![],
        }],
        40,
        true,
    )
    .unwrap();
    assert!(!selected.candidates.is_empty());
    assert!(
        selected
            .candidates
            .iter()
            .all(|candidate| candidate.section.is_some())
    );
    assert!(
        selected
            .candidates
            .iter()
            .any(|candidate| candidate.covered_terms.is_empty()
                && candidate.span.slice(raw).unwrap().contains("exception"))
    );
}

#[test]
fn legacy_semantic_location_retains_affinity_without_section_metadata() {
    let raw = "# Heading\n\nVocabulary unrelated to the question.\n";
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let tokenizer = Tokenizer::new(&connection).unwrap();
    let selected = select(
        &tokenizer,
        "different query",
        &[Parent {
            owner: 0,
            raw,
            body: 0,
            anchors: &[],
            semantic: vec![(ByteSpan::new(0, raw.len() as u64).unwrap(), 0.8)],
        }],
        128,
    )
    .unwrap();
    assert!(!selected.candidates.is_empty());
    assert!(selected.candidates.iter().all(|candidate| {
        candidate.section.is_none()
            && candidate
                .semantic_affinity
                .is_some_and(|affinity| (affinity - 0.8).abs() < 1e-12)
    }));
}

#[test]
fn structural_cap_does_not_turn_the_unscanned_tail_into_a_section_child() {
    let raw = format!(
        "# Limit\n\n{}TailSecret is outside structural work.\n",
        "Limit applies to one record.\n\n".repeat(MAX_BLOCKS + 10)
    );
    let selected = run(&raw, "limit tailsecret", 128, true);
    assert_eq!(selected.scanned_blocks, MAX_BLOCKS);
    assert!(selected.scanned_bytes <= OWNER_SCAN_BYTES);
    assert!(selected.candidates.len() <= MAX_CANDIDATES);
    assert!(
        selected
            .omissions
            .iter()
            .any(|omission| omission.reason == "context_source_scan_block_cap")
    );
    assert!(selected.candidates.iter().all(|candidate| {
        !candidate
            .section
            .as_ref()
            .unwrap()
            .section_span
            .slice(&raw)
            .unwrap()
            .contains("TailSecret")
    }));
}

#[test]
fn one_byte_heading_and_list_construction_share_a_finite_bound_and_cooperate_with_guard() {
    let raw = "x".repeat(OWNER_SCAN_BYTES);
    let mut constructed = 0;
    for kind in [BlockKind::Heading, BlockKind::List] {
        let blocks = [Block {
            range: 0..raw.len(),
            kind,
            terms: vec![],
            clipped: false,
            heading_level: (kind == BlockKind::Heading).then_some(1),
        }];
        let checks = Cell::new(0);
        let check = || {
            checks.set(checks.get() + 1);
            Ok(())
        };
        let (ranges, missing, limited) = section_ranges(
            &raw,
            &blocks,
            0..raw.len(),
            1,
            &mut constructed,
            Some(&check),
        )
        .unwrap();
        assert!(limited);
        assert!(!missing);
        assert!(ranges.len() <= MAX_BLOCKS);
        assert_eq!(constructed, MAX_BLOCKS);
        assert!(ranges.iter().all(|(range, _)| range.len() == 1));
        if kind == BlockKind::Heading {
            assert_eq!(ranges.len(), MAX_BLOCKS);
            assert!(checks.get() >= MAX_BLOCKS / 64);
        } else {
            assert!(ranges.is_empty());
        }
    }
    let blocks = [Block {
        range: 0..raw.len(),
        kind: BlockKind::List,
        terms: vec![],
        clipped: false,
        heading_level: None,
    }];
    let checks = Cell::new(0);
    let check = || {
        checks.set(checks.get() + 1);
        if checks.get() >= 4 {
            Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "test query guard expired",
            ))
        } else {
            Ok(())
        }
    };
    let mut constructed = 0;
    let error = section_ranges(
        &raw,
        &blocks,
        0..raw.len(),
        1,
        &mut constructed,
        Some(&check),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert!(constructed <= 64);
}

#[test]
fn bounded_construction_discloses_omissions_for_later_owners_without_losing_nominee() {
    let raw = format!("# Limit\n\n{}", "a".repeat(OWNER_SCAN_BYTES - 10));
    let late = "# Limit\n\nLimit also governs this later owner.\n";
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let tokenizer = Tokenizer::new(&connection).unwrap();
    let selected = select_with_sections(
        &tokenizer,
        "limit",
        &[
            Parent {
                owner: 0,
                raw: &raw,
                body: 0,
                anchors: &[],
                semantic: vec![],
            },
            Parent {
                owner: 1,
                raw: late,
                body: 0,
                anchors: &[],
                semantic: vec![],
            },
        ],
        1,
        true,
    )
    .unwrap();
    for owner in [0, 1] {
        assert!(
            selected
                .omissions
                .iter()
                .any(|omission| omission.owner_index == owner
                    && omission.reason == "context_source_section_child_cap")
        );
    }
    assert!(
        selected
            .candidates
            .iter()
            .all(|candidate| candidate.owner_index == 0)
    );
    assert_eq!(
        selected
            .candidates
            .iter()
            .filter(|candidate| {
                let section = candidate.section.as_ref().unwrap();
                section.child_ordinal == section.representative_ordinal
            })
            .count(),
        1
    );
}

#[test]
fn anchor_crossing_first_heading_preserves_exact_preheading_piece() {
    let raw = "Preface caveat café applies.\n\n# Limits\n\nLimits use cached records.\n";
    let heading = raw.find("# Limits").unwrap();
    let anchor = ByteSpan::new(raw.find("café").unwrap() as u64, (heading + 8) as u64).unwrap();
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let tokenizer = Tokenizer::new(&connection).unwrap();
    let selected = select_with_sections(
        &tokenizer,
        "limits caveat",
        &[Parent {
            owner: 0,
            raw,
            body: 0,
            anchors: &[anchor],
            semantic: vec![],
        }],
        128,
        true,
    )
    .unwrap();
    let outside = selected
        .candidates
        .iter()
        .find(|candidate| candidate.section.is_none() && candidate.span.end() == heading as u64)
        .unwrap();
    assert_eq!(outside.span.slice(raw).unwrap(), &raw[..heading]);
    assert!(outside.seed_overlap);
    assert!(
        selected
            .candidates
            .iter()
            .filter(|candidate| candidate.section.is_none())
            .all(|candidate| candidate.span.end() <= heading as u64)
    );
}

#[test]
fn anchor_crossing_structural_prefix_preserves_exact_outside_piece() {
    let raw = format!(
        "# Limits\n\n{}Outside qualification café remains required.\n",
        "Background material.\n\n".repeat(MAX_BLOCKS - 1)
    );
    let outside_start = raw.find("Outside qualification").unwrap();
    let structural_end = outside_start - 1;
    let anchor = ByteSpan::new((structural_end - 1) as u64, raw.len() as u64).unwrap();
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let tokenizer = Tokenizer::new(&connection).unwrap();
    let selected = select_with_sections(
        &tokenizer,
        "limits qualification",
        &[Parent {
            owner: 0,
            raw: &raw,
            body: 0,
            anchors: &[anchor],
            semantic: vec![],
        }],
        128,
        true,
    )
    .unwrap();
    let outside = selected
        .candidates
        .iter()
        .find(|candidate| {
            candidate.section.is_none()
                && candidate
                    .span
                    .slice(&raw)
                    .unwrap()
                    .contains("Outside qualification")
        })
        .unwrap();
    assert_eq!(outside.span.start(), structural_end as u64);
    assert_eq!(outside.span.slice(&raw).unwrap(), &raw[structural_end..]);
    assert!(outside.seed_overlap);
    assert!(outside.span.len() <= 128);
    assert!(
        selected
            .omissions
            .iter()
            .any(|omission| omission.reason == "context_source_scan_block_cap")
    );
}

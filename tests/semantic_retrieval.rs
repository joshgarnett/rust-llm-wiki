#[path = "fixtures/p17/common.rs"]
mod common;
#[path = "../test_support/paths.rs"]
mod test_paths;
use common::*;
use lwiki::{
    app::OperationOptions,
    catalog::Catalog,
    domain::*,
    graph::*,
    jobs::*,
    retrieval::{
        render::{self, TargetKind},
        spaces::*,
        vectors::*,
        *,
    },
};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering},
};
#[test]
fn oversized_hybrid_queries_fail_before_provider_or_space_access() {
    let f = Fixture::new();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses.clone());
    let runtime = runtime(&f.service, &dispatch);
    let plan = QueryPlan {
        mode: SearchMode::Hybrid,
        ..Default::default()
    };
    let request = ContextRequest {
        documents: plan.clone(),
        ..Default::default()
    };
    for query in [
        "x".repeat(16 * 1024 + 1),
        std::iter::repeat_n("word", 257)
            .collect::<Vec<_>>()
            .join(" "),
    ] {
        assert_eq!(
            f.app
                .semantic_search(&query, &plan, Some(&runtime), false, false, None)
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
        assert_eq!(
            f.app
                .semantic_context(&query, &request, Some(&runtime), false, false)
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
    }
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn query_bytes_and_embedding_space_budget_are_independent() {
    let f = Fixture::new();
    let mut spec = f.spec();
    let text = "界".repeat(5461) + "x";
    assert_eq!(text.len(), 16 * 1024);
    assert_eq!(
        spec.query(&text).err().unwrap().code,
        ErrorCode::BudgetExceeded
    );
    spec.settings.query_prefix = "query: ".into();
    spec.settings.max_input_bytes = text.len() + spec.settings.query_prefix.len();
    let input = spec.query(&text).unwrap();
    assert_eq!(input.utf8, format!("query: {text}"));
    assert_eq!(input.input_hash, Blake3Hash::digest(input.utf8.as_bytes()));
    spec.settings.max_input_bytes -= 1;
    assert_eq!(
        spec.query(&text).err().unwrap().code,
        ErrorCode::BudgetExceeded
    );
    spec.settings.max_input_bytes = 20_000;
    for invalid in [text + "x", " \n ".into(), "NUL\0word".into()] {
        assert_eq!(spec.query(&invalid).err().unwrap().code, ErrorCode::Usage);
    }
    // Semantic input is not subject to the lexical whitespace-phrase ceiling.
    let many = std::iter::repeat_n("word", 257)
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(spec.query(&many).unwrap().utf8, format!("query: {many}"));
}

#[test]
fn short_whole_long_unicode_split_header_limit() {
    let f = Fixture::new();
    f.page("short", "# Heading\n\nA small note.\n");
    let reader = f.reader();
    let units = render::corpus(&reader, &EmbeddingSettings::default()).unwrap();
    assert_eq!(units.len(), 1);
    assert!(units[0].utf8.contains("# Heading\n\nA small note."));
    let long = ("🦀 café 中文\n\n").repeat(100);
    f.page("long", &long);
    let settings = EmbeddingSettings {
        max_input_bytes: 130,
        ..Default::default()
    };
    let reader = f.reader();
    let units = render::corpus(&reader, &settings).unwrap();
    let mut long_units = units
        .iter()
        .filter(|u| u.owner.as_str().ends_with("long.md"))
        .collect::<Vec<_>>();
    long_units.sort_by_key(|u| u.source_span.unwrap().start());
    assert!(long_units.len() > 1);
    let doc = reader
        .projection()
        .documents
        .iter()
        .find(|d| d.path.as_str().ends_with("long.md"))
        .unwrap();
    let mut previous = None;
    for unit in long_units {
        assert!(unit.utf8.len() <= 130);
        let span = unit.source_span.unwrap();
        if let Some(end) = previous {
            assert_eq!(span.start(), end);
        }
        assert!(
            doc.raw_text
                .get(span.start() as usize..span.end() as usize)
                .is_some()
        );
        previous = Some(span.end());
    }
    assert_eq!(previous, Some(doc.raw_text.len() as u64));
    let tiny = EmbeddingSettings {
        document_prefix: "X".repeat(130),
        max_input_bytes: 130,
        ..Default::default()
    };
    assert_eq!(
        render::corpus(&reader, &tiny).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
}
#[test]
fn exact_cosine_known_order_corrupt_blob_unavailable() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    f.page("b", "Beta");
    f.page("c", "Gamma");
    let reader = f.reader();
    let units = render::corpus(&reader, &EmbeddingSettings::default()).unwrap();
    let spec = f.spec();
    let writer = f.writer();
    let mut store = VectorStore::open(&f.fs, Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    let vectors = vec![vec![4.0, 0.0], vec![1.0, 1.0], vec![-1.0, 0.0]];
    let refs = store
        .put_batch(
            &space,
            &units
                .iter()
                .map(|u| u.input_hash.clone())
                .collect::<Vec<_>>(),
            &vectors,
            true,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    assert!(refs.iter().all(|r| store.verify_ref(r).unwrap()));
    store
        .memberships(&space, reader.snapshot(), &units, true)
        .unwrap();
    let ranked = store
        .exact(&space, &[1.0, 0.0], &units, TargetKind::Document, 2)
        .unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].owner, units[0].owner);
    assert!((ranked[1].score - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-7);
    drop(store);
    let connection =
        rusqlite::Connection::open(f.fs.root().path().join(".wiki/cache/embeddings.sqlite3"))
            .unwrap();
    connection
        .execute(
            "UPDATE embedding_vectors SET blob=?1 WHERE input=?2",
            rusqlite::params![
                f32::NAN.to_le_bytes().to_vec(),
                units[0].input_hash.as_str()
            ],
        )
        .unwrap();
    let store = VectorStore::open(&f.fs, None).unwrap();
    assert!(
        store
            .vector(&space, &units[0].input_hash)
            .unwrap()
            .is_none()
    );
    let coverage = store.coverage(&space, &units).unwrap();
    assert_eq!(coverage.corrupt_units, 1);
    assert_eq!(
        store
            .exact(&space, &[1.0, 0.0], &units, TargetKind::Document, 3)
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn equal_dimensions_different_model_no_mixed_spaces() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    let units = corpus(&f);
    let first = f.spec();
    let mut second = first.clone();
    second.model = "different-model".into();
    assert_ne!(first.id().unwrap(), second.id().unwrap());
    f.seed(&first, &units, true);
    let store = VectorStore::open(&f.fs, None).unwrap();
    assert!(
        store
            .vector(&second.id().unwrap(), &units[0].input_hash)
            .unwrap()
            .is_none()
    );
    for mutate in [0, 1, 2, 3, 4] {
        let mut changed = first.clone();
        match mutate {
            0 => changed.revision = Some("r2".into()),
            1 => changed.settings.query_prefix = "q:".into(),
            2 => changed.settings.document_prefix = "d:".into(),
            3 => changed.dimensions = Some(2),
            _ => changed.endpoint_fingerprint = Blake3Hash::digest("other"),
        };
        assert_ne!(first.id().unwrap(), changed.id().unwrap());
    }
}
#[test]
fn rrf_owner_collapse_no_duplicate_votes() {
    let f = Fixture::new();
    f.page("a", &("Alpha\n\n").repeat(80));
    f.page("b", "Beta");
    let reader = f.reader();
    let settings = EmbeddingSettings {
        max_input_bytes: 120,
        ..Default::default()
    };
    let units = render::corpus(&reader, &settings).unwrap();
    let hits = units
        .iter()
        .enumerate()
        .map(|(i, u)| DenseHit {
            unit_id: u.unit_id.clone(),
            target: u.target,
            owner: u.owner.clone(),
            target_id: u.target_id.clone(),
            source_span: u.source_span,
            input_hash: u.input_hash.clone(),
            score: 1.0 - i as f64 / 100.0,
        })
        .collect::<Vec<_>>();
    let collapsed = fusion::collapse_dense(&hits);
    assert_eq!(collapsed.len(), 2);
    assert!(collapsed.iter().all(|(_, passages)| passages.len() <= 2));
    let reversed = fusion::collapse_dense(&hits.iter().rev().cloned().collect::<Vec<_>>());
    assert_eq!(collapsed, reversed);
    let contributions = vec![RankContribution {
        channel: "dense".into(),
        rank: 1,
        score: Some(1.0),
    }];
    assert_eq!(fusion::rrf(&contributions), 1.0 / 61.0);
}
#[test]
fn dense_cap_keeps_two_owners_and_two_passages_across_226_units() {
    let f = Fixture::new();
    f.page("long", &"x".repeat(300));
    f.page("short", "Short owner text.");
    let reader = f.reader();
    let base = render::corpus(&reader, &EmbeddingSettings::default()).unwrap();
    let long = base
        .iter()
        .find(|u| u.owner.as_str().ends_with("long.md"))
        .unwrap();
    let short = base
        .iter()
        .find(|u| u.owner.as_str().ends_with("short.md"))
        .unwrap();
    let mut units = Vec::new();
    let mut vectors = Vec::new();
    for index in 0..225 {
        let mut unit = long.clone();
        unit.unit_id = Blake3Hash::digest(format!("long-unit-{index}"));
        unit.input_hash = Blake3Hash::digest(format!("long-input-{index}"));
        let start = long.source_span.unwrap().start() + index;
        unit.source_span = Some(ByteSpan::new(start, start + 1).unwrap());
        units.push(unit);
        vectors.push(vec![1.0, 0.0]);
    }
    units.push(short.clone());
    vectors.push(vec![0.8, 0.6]);
    let spec = f.spec();
    let writer = f.writer();
    let mut store = VectorStore::open(&f.fs, Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    store
        .put_batch(
            &space,
            &units
                .iter()
                .map(|u| u.input_hash.clone())
                .collect::<Vec<_>>(),
            &vectors,
            true,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    store
        .memberships(&space, reader.snapshot(), &units, true)
        .unwrap();
    let scan = store
        .exact_stream(
            &space,
            &[1.0, 0.0],
            || Ok(units.iter().cloned().map(Ok)),
            &[TargetKind::Document],
            2,
            |_| Ok(true),
        )
        .unwrap();
    let dense = &scan.hits[&TargetKind::Document];
    let owners = fusion::collapse_dense(dense);
    assert_eq!(scan.coverage.available_units, 226);
    assert_eq!(scan.available_by_target[&TargetKind::Document], 2);
    assert!(
        !scan
            .owner_cap_reached_by_target
            .get(&TargetKind::Document)
            .copied()
            .unwrap_or(false)
    );
    assert_eq!(owners.len(), 2);
    assert!(owners[0].0.owner.as_str().ends_with("long.md"));
    assert_eq!(owners[0].1.len(), 2);
    assert_ne!(owners[0].1[0].source_span, owners[0].1[1].source_span);
    assert!(owners[1].0.owner.as_str().ends_with("short.md"));
    assert_eq!(owners[1].1.len(), 1);
}
#[test]
fn dense_replay_recovers_earlier_second_passage_after_late_wins_and_reentry() {
    let f = Fixture::new();
    for owner in ["a", "b", "c"] {
        f.page(owner, &format!("Owner {owner} text."));
    }
    let reader = f.reader();
    let base = render::corpus(&reader, &EmbeddingSettings::default()).unwrap();
    let make = |owner: &str, number: usize| {
        let mut unit = base
            .iter()
            .find(|unit| {
                unit.owner
                    .as_str()
                    .ends_with(format!("/{owner}.md").as_str())
            })
            .unwrap()
            .clone();
        unit.unit_id = Blake3Hash::digest(format!("{owner}-unit-{number}"));
        unit.input_hash = Blake3Hash::digest(format!("{owner}-input-{number}"));
        unit
    };
    // B initially falls below the k=1 cutoff, later wins after C evicts A.
    // A then reenters and wins; both winners need their earlier second unit.
    let units = vec![
        make("a", 0),
        make("b", 0),
        make("c", 0),
        make("b", 1),
        make("a", 1),
    ];
    let scores: [f32; 5] = [0.8, 0.7, 0.85, 0.9, 0.95];
    let vectors = scores
        .iter()
        .map(|score| vec![*score, (1.0 - score * score).sqrt()])
        .collect::<Vec<_>>();
    let spec = f.spec();
    let writer = f.writer();
    let mut store = VectorStore::open(&f.fs, Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    store
        .put_batch(
            &space,
            &units
                .iter()
                .map(|u| u.input_hash.clone())
                .collect::<Vec<_>>(),
            &vectors,
            true,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    store
        .memberships(&space, reader.snapshot(), &units, true)
        .unwrap();
    let scan = |count| {
        store
            .exact_stream(
                &space,
                &[1.0, 0.0],
                || Ok(units[..count].iter().cloned().map(Ok)),
                &[TargetKind::Document],
                1,
                |_| Ok(true),
            )
            .unwrap()
    };
    let late_b = scan(4);
    let b = &late_b.hits[&TargetKind::Document];
    assert_eq!(b.len(), 2);
    assert_eq!(b[0].unit_id, units[3].unit_id);
    assert_eq!(b[1].unit_id, units[1].unit_id);
    assert_eq!(late_b.coverage.available_units, 4);
    assert!(late_b.owner_cap_reached_by_target[&TargetKind::Document]);
    let reentered_a = scan(5);
    let a = &reentered_a.hits[&TargetKind::Document];
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].unit_id, units[4].unit_id);
    assert_eq!(a[1].unit_id, units[0].unit_id);
    assert_eq!(reentered_a.coverage.available_units, 5);
    assert_eq!(reentered_a.available_by_target[&TargetKind::Document], 1);
}
#[test]
fn semantic_hit_and_context_keep_two_disjoint_passages_for_one_owner() {
    let f = Fixture::new();
    let body = (0..90)
        .map(|i| format!("A long separated passage number {i:03}.\n\n"))
        .collect::<String>();
    f.page("long", &body);
    f.page("short", "A short page.");
    let mut spec = f.spec();
    spec.settings.max_input_bytes = 300;
    let reader = f.reader();
    let units = render::corpus(&reader, &spec.settings).unwrap();
    let mut long = units
        .iter()
        .filter(|u| u.owner.as_str().ends_with("long.md"))
        .collect::<Vec<_>>();
    long.sort_by_key(|u| u.source_span.unwrap().start());
    assert!(long.len() >= 4);
    let first = long.first().unwrap().unit_id.clone();
    let last = long.last().unwrap().unit_id.clone();
    let first_span = long.first().unwrap().source_span.unwrap();
    let last_span = long.last().unwrap().source_span.unwrap();
    let vectors = units
        .iter()
        .map(|u| {
            if u.unit_id == first || u.unit_id == last {
                vec![1.0, 0.0]
            } else if u.owner.as_str().ends_with("short.md") {
                vec![0.8, 0.6]
            } else {
                vec![0.0, 1.0]
            }
        })
        .collect::<Vec<_>>();
    let writer = f.writer();
    let mut store = VectorStore::open(&f.fs, Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    store
        .put_batch(
            &space,
            &units
                .iter()
                .map(|u| u.input_hash.clone())
                .collect::<Vec<_>>(),
            &vectors,
            true,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    store
        .memberships(&space, reader.snapshot(), &units, true)
        .unwrap();
    drop(store);
    drop(writer);
    f.query_seed(&spec, "two passages", vec![1.0, 0.0]);
    let plan = QueryPlan {
        mode: SearchMode::Semantic,
        ..Default::default()
    };
    let result = f
        .offline()
        .semantic_search("two passages", &plan, None, false, false, None)
        .unwrap();
    let hit = result
        .hits
        .iter()
        .find(|h| h.locator.path.as_str().ends_with("long.md"))
        .unwrap();
    assert_eq!(hit.secondary_excerpts.len(), 1);
    let second = &hit.secondary_excerpts[0];
    assert!(
        hit.excerpt.span.end() <= second.span.start()
            || second.span.end() <= hit.excerpt.span.start()
    );
    assert!(hit.excerpt.text.len() + second.text.len() <= plan.limits.excerpt_bytes);
    let request = ContextRequest {
        documents: plan,
        ..Default::default()
    };
    let context = f
        .offline()
        .semantic_context("two passages", &request, None, false, false)
        .unwrap();
    let passages = context
        .passages()
        .iter()
        .filter(|p| p.locator.path.as_str().ends_with("long.md"))
        .collect::<Vec<_>>();
    assert!((2..=4).contains(&passages.len()));
    for span in [first_span, last_span] {
        assert!(
            passages
                .iter()
                .any(|p| p.span.start() < span.end() && span.start() < p.span.end())
        );
    }
    assert!(!context.network_used);
    assert!(
        context
            .warnings()
            .iter()
            .any(|warning| warning.contains("cached unit affinities"))
    );
    assert!(context.usage().rendered_bytes <= request.budget.max_bytes);
    assert!(context.usage().estimated_tokens <= request.budget.max_tokens);
}
#[test]
fn cached_unit_affinity_prefers_distant_paraphrase_over_exact_word_distractor() {
    let f = Fixture::new();
    let query = "quasarbreak termination";
    let distractor =
        "quasarbreak termination is a printed glossary label, not an operational guarantee.";
    let answer = "When one job fails, its caller receives a recoverable error. Sibling tasks keep running, and the enclosing process stays alive.";
    let filler = "Ordinary appendix material discusses release labels, edition numbering, and directory layouts.\n\n";
    let body = format!(
        "# Worker failure guide\n\n{distractor}\n\n{}## Behaviour\n\n{answer}\n",
        filler.repeat(40)
    );
    // Only the irrelevant glossary repeats the content query terms. The
    // deliberately assigned vectors test selection mechanics, not whether
    // an embedding model understands this paraphrase.
    assert!(!answer.contains("quasarbreak") && !answer.contains("termination"));
    f.page("distant", &body);
    let mut spec = f.spec();
    spec.settings.max_input_bytes = 384;
    let reader = f.reader();
    let document = reader
        .projection()
        .documents
        .iter()
        .find(|document| document.path.as_str().ends_with("distant.md"))
        .unwrap();
    let raw = document.raw_text.clone();
    let path = document.path.clone();
    let observed_hash = document.hash.clone();
    let answer_start = raw.find(answer).unwrap() as u64;
    let answer_span = ByteSpan::new(answer_start, answer_start + answer.len() as u64).unwrap();
    assert!(answer_start > 3000);
    let units = render::corpus(&reader, &spec.settings).unwrap();
    assert!(units.len() >= 4);
    assert!(units.iter().all(|unit| unit.owner == path));
    let overlaps_answer = |unit: &render::RenderedUnit| {
        let span = unit.source_span.unwrap();
        span.start() < answer_span.end() && answer_span.start() < span.end()
    };
    assert!(units.iter().any(overlaps_answer));
    assert!(units.iter().any(|unit| !overlaps_answer(unit)));
    let hashes = units
        .iter()
        .map(|unit| unit.input_hash.clone())
        .collect::<Vec<_>>();
    let vectors = units
        .iter()
        .map(|unit| {
            if overlaps_answer(unit) {
                vec![1.0, 0.0]
            } else {
                vec![0.0, 1.0]
            }
        })
        .collect::<Vec<_>>();
    let writer = f.writer();
    let mut store = VectorStore::open(&f.fs, Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    store
        .put_batch(&space, &hashes, &vectors, true, &Blake3Hash::digest([]))
        .unwrap();
    store
        .memberships(&space, reader.snapshot(), &units, true)
        .unwrap();
    // An incompatible model has the same dimensions and deliberately opposite
    // affinities. Cached passage guidance must use the active exact space.
    let mut inactive = spec.clone();
    inactive.model = "inactive-opposite-model".into();
    let other_space = store.prepare_space(&inactive).unwrap();
    assert_ne!(space, other_space);
    let opposite = vectors
        .iter()
        .map(|vector| vec![vector[1], vector[0]])
        .collect::<Vec<_>>();
    store
        .put_batch(
            &other_space,
            &hashes,
            &opposite,
            true,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    store
        .memberships(&other_space, reader.snapshot(), &units, false)
        .unwrap();
    assert_eq!(store.active().unwrap().unwrap().id, space);
    drop(store);
    drop(reader);
    drop(writer);
    f.query_seed(&spec, query, vec![1.0, 0.0]);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses.clone());
    let runtime = runtime(&f.service, &dispatch);
    let request = ContextRequest {
        documents: QueryPlan {
            mode: SearchMode::Lexical,
            limits: SearchLimits {
                candidates: 8,
                hits: 1,
                excerpt_bytes: 192,
            },
            ..Default::default()
        },
        budget: ContextBudget {
            max_bytes: 400,
            max_tokens: 100,
            ..Default::default()
        },
        ..Default::default()
    };
    let offline = f.offline();
    let lexical = offline
        .semantic_context(query, &request, Some(&runtime), false, false)
        .unwrap();
    assert!(
        lexical
            .passages()
            .first()
            .unwrap()
            .text
            .contains(distractor)
    );
    assert!(!lexical.passages().first().unwrap().text.contains(answer));
    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let mut semantic = request.clone();
        semantic.documents.mode = mode;
        let context = offline
            .semantic_context(query, &semantic, Some(&runtime), false, false)
            .unwrap();
        let first = context.passages().first().unwrap();
        assert!(first.text.contains(answer), "{mode:?}: {:?}", first.text);
        assert!(!first.text.contains("quasarbreak"));
        assert!(
            first
                .rank_contributions
                .iter()
                .any(|rank| { rank.channel == "context_unit_dense" && rank.score == Some(1.0) })
        );
        assert_eq!(
            first
                .rank_contributions
                .iter()
                .filter(|rank| { rank.channel == "direct_document_owner" })
                .count(),
            1
        );
        assert!(matches!(
            context.verification(),
            lwiki::catalog::SnapshotVerification::VerifiedSnapshot { .. }
        ));
        for passage in context.passages() {
            assert_eq!(passage.locator.path, path);
            assert_eq!(passage.locator.observed_hash, observed_hash);
            assert_eq!(
                passage.locator.record.as_ref().unwrap().record_id.as_str(),
                "page_distant"
            );
            assert_eq!(passage.span.slice(&raw).unwrap(), passage.text);
        }
        assert!(!context.network_used);
        assert!(context.usage().rendered_bytes <= request.budget.max_bytes);
        assert!(context.usage().estimated_tokens <= request.budget.max_tokens);
        assert!(
            context
                .warnings()
                .iter()
                .any(|warning| warning.contains("cached unit affinities"))
        );
    }
    // The explicit host stage selects the same exact answer from authenticated
    // cards without making a provider request or rewriting the returned bytes.
    let mut semantic = request.clone();
    semantic.documents.mode = SearchMode::Hybrid;
    let prepared = offline
        .semantic_context_with_selection(
            query,
            &semantic,
            Some(&runtime),
            false,
            false,
            &lwiki::retrieval::context_selection_packet::SelectionAction::Prepare,
        )
        .unwrap();
    let packet = prepared.selection_packet().unwrap();
    let card = packet
        .cards
        .iter()
        .find(|card| card.passage.text.contains(answer))
        .unwrap();
    let selected = offline
        .semantic_context_with_selection(
            query,
            &semantic,
            Some(&runtime),
            false,
            false,
            &lwiki::retrieval::context_selection_packet::SelectionAction::Apply(
                lwiki::retrieval::context_selection_packet::SelectionReply {
                    packet_fingerprint: packet.fingerprint.clone(),
                    ordered_ids: vec![card.id.clone()],
                },
            ),
        )
        .unwrap();
    assert_eq!(selected.passages()[0].text, card.passage.text);
    assert!(
        selected.passages()[0]
            .rank_contributions
            .iter()
            .any(|rank| rank.channel == "host_selection")
    );
    assert!(!prepared.network_used && !selected.network_used);
    // Lose a lexical region's cached vector while the distant semantic answer
    // remains available. Partial coverage must not silently discard that region.
    let missing = units
        .iter()
        .find(|unit| {
            unit.source_span
                .unwrap()
                .slice(&raw)
                .unwrap()
                .contains(distractor)
        })
        .unwrap();
    let connection =
        rusqlite::Connection::open(f.fs.root().path().join(".wiki/cache/embeddings.sqlite3"))
            .unwrap();
    connection
        .execute(
            "UPDATE embedding_vectors SET blob=?1 WHERE input=?2",
            rusqlite::params![f32::NAN.to_le_bytes().to_vec(), missing.input_hash.as_str()],
        )
        .unwrap();
    drop(connection);
    let mut hybrid = request.clone();
    hybrid.documents.mode = SearchMode::Hybrid;
    let partial = offline
        .semantic_context(query, &hybrid, Some(&runtime), false, false)
        .unwrap();
    assert!(
        partial
            .passages()
            .iter()
            .any(|p| p.text.contains(distractor))
    );
    assert!(
        partial
            .warnings()
            .iter()
            .any(|warning| warning.contains("unit coverage is incomplete"))
    );
    assert!(!partial.network_used);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read_to_string(f.fs.root().path().join(path.as_str())).unwrap(),
        raw
    );
}

#[test]
fn offline_missing_query_vector_no_remote() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    let units = corpus(&f);
    let spec = f.spec();
    f.seed(&spec, &units, true);
    let response = Arc::new(Responses::new());
    let dispatcher = dispatcher(&f.fs, response.clone());
    let runtime = runtime(&f.service, &dispatcher);
    let plan = QueryPlan {
        mode: SearchMode::Semantic,
        ..Default::default()
    };
    let err = f
        .offline()
        .semantic_search("missing", &plan, Some(&runtime), false, false, None)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::OfflineUnavailable);
    assert_eq!(response.calls.load(Ordering::SeqCst), 0);
    f.query_seed(&spec, "cached", vec![1.0, 0.0]);
    let hits = f
        .offline()
        .semantic_search("cached", &plan, None, false, false, None)
        .unwrap();
    assert_eq!(hits.hits.len(), 1);
    assert!(
        hits.hits[0]
            .rank_contributions
            .iter()
            .any(|c| c.channel == "dense")
    );
}
#[test]
fn partial_replacement_queries_old_reproducible_space() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    f.page("b", "Beta");
    let units = corpus(&f);
    let old = f.spec();
    f.seed(&old, &units, true);
    f.query_seed(&old, "query", vec![1.0, 0.0]);
    let mut new = old.clone();
    new.model = "new-model".into();
    f.seed(&new, &units[..1], false);
    let store = VectorStore::open(&f.fs, None).unwrap();
    assert_eq!(store.active().unwrap().unwrap().id, old.id().unwrap());
    assert_eq!(
        store
            .coverage(&new.id().unwrap(), &units)
            .unwrap()
            .missing_units,
        1
    );
    let plan = QueryPlan {
        mode: SearchMode::Semantic,
        ..Default::default()
    };
    assert_eq!(
        f.offline()
            .semantic_search("query", &plan, None, false, false, None)
            .unwrap()
            .hits
            .len(),
        2
    );
    let config = std::fs::read_to_string(&f.config)
        .unwrap()
        .replace("model='test-model'", "model='new-model'");
    private_write(&f.config, config);
    let service = lwiki::config::providers::ProviderConfig::load(&f.config)
        .unwrap()
        .authorize(&f.fs, f.app.vault_id(), "primary", Capability::Embed)
        .unwrap();
    let response = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, response.clone());
    let runtime = runtime(&service, &dispatch);
    assert_eq!(
        f.app
            .semantic_search("uncached", &plan, Some(&runtime), false, false, None)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    assert_eq!(response.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn explicit_sync_receipts_settlement_query_cache_and_no_index_embedding() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    f.page("b", "Beta");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses.clone());
    let runtime = runtime(&f.service, &dispatch);
    let report = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap();
    assert!(report.published);
    assert_eq!(report.generated_inputs, 2);
    let ledger = JobLedger::new(
        f.fs.clone(),
        f.app.vault_id().clone(),
        report.run_id.unwrap(),
        options(),
    )
    .unwrap();
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.state, RunState::Completed);
    assert!(
        inspection
            .tasks
            .values()
            .all(|t| t.state == TaskState::Completed)
    );
    assert!(
        inspection
            .attempts
            .iter()
            .all(|a| a.phase == AttemptPhase::Settled && !a.cache_outputs.is_empty())
    );
    let before = responses.calls.load(Ordering::SeqCst);
    f.app.index_sync(true).unwrap();
    assert_eq!(responses.calls.load(Ordering::SeqCst), before);
    let plan = QueryPlan {
        mode: SearchMode::Semantic,
        ..Default::default()
    };
    assert_eq!(
        f.app
            .semantic_search("query", &plan, Some(&runtime), false, false, None)
            .unwrap()
            .hits
            .len(),
        2
    );
    let calls = responses.calls.load(Ordering::SeqCst);
    f.offline()
        .semantic_search("query", &plan, None, false, false, None)
        .unwrap();
    assert_eq!(responses.calls.load(Ordering::SeqCst), calls);
    assert_eq!(
        f.app
            .embeddings_sync(&EmbeddingSettings::default(), &runtime)
            .unwrap()
            .generated_inputs,
        0
    );
}
#[test]
fn source_edit_inflight_vector_membership_rejected() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    let file = f.fs.root().path().join("knowledge/pages/a.md");
    let original = std::fs::read_to_string(&file).unwrap();
    let responses = Arc::new(Responses {
        calls: Default::default(),
        hook: Some(Box::new(move |_| {
            std::fs::write(&file, original.replace("Alpha", "Bravo")).unwrap();
        })),
        vectors: vec![],
    });
    let dispatch = dispatcher(&f.fs, responses.clone());
    let runtime = runtime(&f.service, &dispatch);
    let error = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FreshnessConflict);
    let run: RecordId = serde_json::from_value(error.details["run_id"].clone()).unwrap();
    let ledger = JobLedger::new(f.fs.clone(), f.app.vault_id().clone(), run, options()).unwrap();
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.attempts.len(), 1);
    assert_eq!(inspection.attempts[0].phase, AttemptPhase::Settled);
    assert!(inspection.attempts[0].cache_outputs.is_empty());
    assert!(
        inspection
            .tasks
            .values()
            .all(|t| t.state != TaskState::Completed)
    );
    assert!(
        VectorStore::open(&f.fs, None)
            .unwrap()
            .active()
            .unwrap()
            .is_none()
    );
}
#[test]
fn auto_dimension_probe_and_invalid_batch_do_not_fix_corpus() {
    let f = Fixture::new();
    let spec = f.spec();
    let writer = f.writer();
    let mut store = VectorStore::open(&f.fs, Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    assert!(
        store
            .put_batch(
                &space,
                &[Blake3Hash::digest("query")],
                &[vec![1.0, 0.0]],
                false,
                &Blake3Hash::digest([])
            )
            .is_err()
    );
    assert_eq!(
        store.space(&space).unwrap().unwrap().actual_dimensions,
        None
    );
    assert!(
        store
            .put_batch(
                &space,
                &[Blake3Hash::digest("a"), Blake3Hash::digest("b")],
                &[vec![1.0, 0.0], vec![0.0]],
                true,
                &Blake3Hash::digest([])
            )
            .is_err()
    );
    assert_eq!(
        store.space(&space).unwrap().unwrap().actual_dimensions,
        None
    );
    drop(store);
    drop(writer);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses);
    let report = f
        .app
        .embeddings_check(
            &EmbeddingSettings::default(),
            Some(&runtime(&f.service, &dispatch)),
            true,
        )
        .unwrap();
    assert!(report.network_used);
    assert_eq!(
        VectorStore::open(&f.fs, None)
            .unwrap()
            .space(&space)
            .unwrap()
            .unwrap()
            .actual_dimensions,
        None
    );
}

fn bootstrap() -> (tempfile::TempDir, lwiki::app::OfflineApp, Catalog) {
    let temp = tempfile::tempdir().unwrap();
    copy(
        &test_paths::fixture(env!("CARGO_MANIFEST_DIR"), "tests/fixtures/bootstrap/vault"),
        temp.path(),
    );
    let fs = lwiki::vault::VaultFs::new(lwiki::vault::VaultRoot::explicit(temp.path()).unwrap());
    let app = lwiki::app::OfflineApp::new(fs.clone(), OperationOptions::default()).unwrap();
    let catalog = Catalog::new(fs, app.vault_id().clone());
    (temp, app, catalog)
}
#[test]
fn render_qualifiers_rename_and_description_dependency_invalidation() {
    let (temp, app, catalog) = bootstrap();
    let writer =
        lwiki::vault::WriterPermit::acquire(app.fs().root(), std::time::Duration::from_secs(1))
            .unwrap();
    let reader = catalog.verified_snapshot(Some(&writer)).unwrap();
    let settings = EmbeddingSettings::default();
    let before = render::corpus(&reader, &settings).unwrap();
    let forward = before
        .iter()
        .find(|u| {
            u.target_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "assertion_00000000-0000-7000-8000-00000000000a")
        })
        .unwrap();
    assert!(
        forward
            .utf8
            .contains("Subject: {\"label\":\"North Lab\",\"type\":\"organization\"}")
    );
    assert!(
        forward
            .utf8
            .contains("Negated: false\nModality: \"asserted\"")
    );
    let negated = before
        .iter()
        .find(|u| u.utf8.contains("Negated: true"))
        .unwrap();
    assert_ne!(forward.input_hash, negated.input_hash);
    let dated = before
        .iter()
        .find(|u| u.utf8.contains("ValidFrom:"))
        .unwrap();
    assert!(dated.utf8.find("Modality:").unwrap() < dated.utf8.find("ValidFrom:").unwrap());
    drop(reader);
    drop(writer);
    let entity = temp.path().join("knowledge/entities/north_lab.md");
    let text = std::fs::read_to_string(&entity)
        .unwrap()
        .replace("title: \"North Lab\"", "title: \"Northern Lab\"");
    std::fs::write(&entity, text).unwrap();
    let writer =
        lwiki::vault::WriterPermit::acquire(app.fs().root(), std::time::Duration::from_secs(1))
            .unwrap();
    let reader = catalog.verified_snapshot(Some(&writer)).unwrap();
    let renamed = render::corpus(&reader, &settings).unwrap();
    let new_forward = renamed
        .iter()
        .find(|u| u.target_id == forward.target_id)
        .unwrap();
    assert_ne!(new_forward.input_hash, forward.input_hash);
    assert!(new_forward.utf8.contains("Northern Lab"));
    drop(reader);
    drop(writer);
    let person = temp.path().join("knowledge/entities/alex_north.md");
    let bytes = std::fs::read(&person).unwrap();
    let mut updates = BTreeMap::new();
    updates.insert(
        "wiki_depends_on_ids".into(),
        serde_json::json!(["source_00000000-0000-7000-8000-000000000005"]),
    );
    let changed = lwiki::records::edit_note(
        &lwiki::records::parse_note(&bytes),
        &updates,
        None,
        &Blake3Hash::digest(&bytes),
    )
    .unwrap();
    std::fs::write(&person, changed).unwrap();
    app.source_withdraw(
        RecordId::new("source_00000000-0000-7000-8000-000000000005").unwrap(),
        "synthetic withdrawal",
    )
    .unwrap();
    let writer =
        lwiki::vault::WriterPermit::acquire(app.fs().root(), std::time::Duration::from_secs(1))
            .unwrap();
    let reader = catalog.verified_snapshot(Some(&writer)).unwrap();
    let units = render::corpus(&reader, &settings).unwrap();
    let alex = units
        .iter()
        .find(|u| {
            u.target_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "entity_00000000-0000-7000-8000-000000000001")
        })
        .unwrap();
    assert!(alex.utf8.contains("Entity: \"Alex Kim\""));
    assert!(!alex.utf8.contains("North Lab engineer"));
    assert!(!units.iter().any(|u| u.owner.as_str().starts_with("runs/")));
}
#[test]
fn cached_graph_query_withdrawn_support_cannot_seed_stale_assertion() {
    let (_temp, app, catalog) = bootstrap();
    let f = Fixture::new();
    let spec = f.spec();
    let writer =
        lwiki::vault::WriterPermit::acquire(app.fs().root(), std::time::Duration::from_secs(1))
            .unwrap();
    let reader = catalog.verified_snapshot(Some(&writer)).unwrap();
    let units = render::corpus(&reader, &spec.settings).unwrap();
    let mut store = VectorStore::open(app.fs(), Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    let input_hashes = units
        .iter()
        .map(|u| u.input_hash.clone())
        .collect::<Vec<_>>();
    store
        .put_batch(
            &space,
            &input_hashes,
            &vec![vec![1.0, 0.0]; units.len()],
            true,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    store
        .memberships(&space, reader.snapshot(), &units, true)
        .unwrap();
    let query = spec.query("uses").unwrap();
    store
        .put_batch(
            &space,
            &[query.input_hash],
            &[vec![1.0, 0.0]],
            false,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    drop(store);
    drop(reader);
    drop(writer);
    let offline = lwiki::app::OfflineApp::new(
        app.fs().clone(),
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    let plan = GraphPlan {
        strategy: GraphStrategy::Relationship,
        seed_mode: GraphSeedMode::Semantic,
        ..Default::default()
    };
    let initial = offline
        .semantic_graph("uses", &plan, None, false, false)
        .unwrap();
    assert!(initial.assertions.iter().any(
        |a| a.record_ref.record_id.as_str() == "assertion_00000000-0000-7000-8000-00000000000a"
    ));
    app.source_withdraw(
        RecordId::new("source_00000000-0000-7000-8000-000000000005").unwrap(),
        "withdraw primary",
    )
    .unwrap();
    app.source_withdraw(
        RecordId::new("source_00000000-0000-7000-8000-000000000006").unwrap(),
        "withdraw mirror",
    )
    .unwrap();
    let after = offline
        .semantic_graph("uses", &plan, None, false, false)
        .unwrap();
    assert!(!after.seeds.iter().any(
        |s| s.record_ref.record_id.as_str() == "assertion_00000000-0000-7000-8000-00000000000a"
    ));
    assert!(after.assertions.is_empty());
}
#[test]
fn cache_deletion_retains_receipts_and_explicit_sync_replaces_missing() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses.clone());
    let runtime = runtime(&f.service, &dispatch);
    let initial = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap();
    assert!(initial.published);
    let active_check = f
        .app
        .embeddings_check(
            &EmbeddingSettings {
                max_input_bytes: 20_000,
                ..Default::default()
            },
            None,
            false,
        )
        .unwrap();
    // A local check uses the active generation, not this invocation's defaults.
    assert_eq!(active_check.settings.unwrap().max_input_bytes, 12_000);
    let run = initial.run_id.unwrap();
    let before = responses.calls.load(Ordering::SeqCst);
    let writer = f.writer();
    for name in [
        "embeddings.sqlite3",
        "embeddings.sqlite3-wal",
        "embeddings.sqlite3-shm",
    ] {
        let path = f.fs.root().path().join(".wiki/cache").join(name);
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    drop(writer);
    f.app.index_sync(true).unwrap();
    assert_eq!(responses.calls.load(Ordering::SeqCst), before);
    let check = f
        .app
        .embeddings_check(&EmbeddingSettings::default(), Some(&runtime), false)
        .unwrap();
    assert_eq!(check.coverage.missing_units, 1);
    assert_eq!(
        JobLedger::new(f.fs.clone(), f.app.vault_id().clone(), run, options())
            .unwrap()
            .inspect()
            .unwrap()
            .attempts[0]
            .phase,
        AttemptPhase::Settled
    );
    assert!(
        f.app
            .embeddings_sync(&EmbeddingSettings::default(), &runtime)
            .unwrap()
            .published
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), before + 1);
}

struct OnceFault {
    point: LedgerCheckpoint,
    armed: std::sync::atomic::AtomicBool,
}
impl LedgerFault for OnceFault {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == self.point && self.armed.swap(false, Ordering::SeqCst) {
            Err(WikiError::new(
                ErrorCode::Internal,
                "injected embedding recovery boundary",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn paid_response_blob_receipt_settlement_recovery_no_repeat_send() {
    for point in [
        LedgerCheckpoint::AfterSpoolMetadataSync,
        LedgerCheckpoint::AfterReceived,
        LedgerCheckpoint::BeforeOutputsCommitted,
        LedgerCheckpoint::BeforeSettlement,
        LedgerCheckpoint::AfterSettlement,
    ] {
        let f = Fixture::new();
        f.page("a", "Alpha");
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&f.fs, responses.clone());
        let mut interrupted = runtime(&f.service, &dispatch);
        interrupted.job_options.fault = Some(Arc::new(OnceFault {
            point,
            armed: std::sync::atomic::AtomicBool::new(true),
        }));
        assert!(
            f.app
                .embeddings_sync(&EmbeddingSettings::default(), &interrupted)
                .is_err(),
            "{point:?}"
        );
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1, "{point:?}");
        let store = VectorStore::open(&f.fs, None).unwrap();
        assert!(store.active().unwrap().is_none(), "{point:?}");
        drop(store);
        let resumed = f
            .app
            .embeddings_sync(
                &EmbeddingSettings::default(),
                &runtime(&f.service, &dispatch),
            )
            .unwrap();
        assert!(resumed.published, "{point:?}");
        assert!(!resumed.network_used, "{point:?}");
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1, "{point:?}");
    }
}
#[test]
fn body_only_orphan_preserves_unknown_no_automatic_resend() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses.clone());
    let mut interrupted = runtime(&f.service, &dispatch);
    interrupted.limits.concurrency = 2;
    interrupted.limits.attempts_per_task = 2;
    interrupted.job_options.fault = Some(Arc::new(OnceFault {
        point: LedgerCheckpoint::AfterSpoolBytesSync,
        armed: std::sync::atomic::AtomicBool::new(true),
    }));
    assert!(
        f.app
            .embeddings_sync(&EmbeddingSettings::default(), &interrupted)
            .is_err()
    );
    let error = f
        .app
        .embeddings_sync(
            &EmbeddingSettings::default(),
            &runtime(&f.service, &dispatch),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    let mut authorized = runtime(&f.service, &dispatch);
    authorized.limits = interrupted.limits.clone();
    authorized.job_options.policy.retry_uncertain = true;
    let retried = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &authorized)
        .unwrap();
    assert!(retried.network_used);
    assert_eq!(retried.generated_inputs, 1);
    assert_eq!(retried.reused_inputs, 0);
    assert!(retried.run_id.is_some());
    assert!(!retried.warnings.is_empty());
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    let inspection = JobLedger::new(
        f.fs.clone(),
        f.app.vault_id().clone(),
        retried.run_id.unwrap(),
        options(),
    )
    .unwrap()
    .inspect()
    .unwrap();
    assert_eq!(inspection.attempts.len(), 2);
    assert_eq!(
        inspection.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
}
#[test]
fn authorized_unknown_embedding_retry_rechecks_corpus_before_sending() {
    let f = Fixture::new();
    f.page("a", "Original authored passage");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses.clone());
    let mut interrupted = runtime(&f.service, &dispatch);
    interrupted.limits.concurrency = 2;
    interrupted.limits.attempts_per_task = 2;
    interrupted.job_options.fault = Some(Arc::new(OnceFault {
        point: LedgerCheckpoint::AfterSpoolBytesSync,
        armed: std::sync::atomic::AtomicBool::new(true),
    }));
    let initial = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &interrupted)
        .unwrap_err();
    let run: RecordId = serde_json::from_value(initial.details["run_id"].clone()).unwrap();
    let ledger = JobLedger::new(
        f.fs.clone(),
        f.app.vault_id().clone(),
        run.clone(),
        options(),
    )
    .unwrap();
    let before = ledger.inspect().unwrap();
    assert_eq!(before.budget.unknown_attempts.len(), 1);
    if before.state == RunState::Running {
        ledger.pause(StopReason::OutcomeUnknown).unwrap();
    }
    let mut effective = interrupted.limits.clone();
    effective.requests += 1;
    ledger
        .amend_retained_limits(
            effective.clone(),
            before.effective_deadline_utc_ms,
            "Explicitly raise cumulative request limit".into(),
        )
        .unwrap();
    let mut old_limits = runtime(&f.service, &dispatch);
    old_limits.limits = interrupted.limits.clone();
    old_limits.job_options.policy.retry_uncertain = true;
    let error = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &old_limits)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert_eq!(error.details["reason"], "retained_limits_require_amendment");
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    f.page("a", "Human changed the passage before the retry");
    let mut retry = runtime(&f.service, &dispatch);
    retry.limits = effective;
    retry.job_options.policy.retry_uncertain = true;
    let error = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &retry)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FreshnessConflict, "{error:?}");
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    let after = ledger.inspect().unwrap();
    assert_eq!(after.spec.run_id, run);
    assert_eq!(after.attempts.len(), before.attempts.len());
    assert_eq!(
        after.budget.unknown_attempts,
        before.budget.unknown_attempts
    );
    assert_eq!(after.budget.dispatched_requests, 1);
}
#[test]
fn later_dimension_mismatch_charged_partial_cache_retained() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    f.page("b", "Beta");
    let responses = Arc::new(Responses {
        calls: Default::default(),
        hook: None,
        vectors: vec![vec![1.0, 0.0], vec![1.0, 0.0, 0.0]],
    });
    let dispatch = dispatcher(&f.fs, responses.clone());
    let error = f
        .app
        .embeddings_sync(
            &EmbeddingSettings::default(),
            &runtime(&f.service, &dispatch),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ProviderResponse);
    let run: RecordId = serde_json::from_value(error.details["run_id"].clone()).unwrap();
    let inspection = JobLedger::new(f.fs.clone(), f.app.vault_id().clone(), run, options())
        .unwrap()
        .inspect()
        .unwrap();
    assert_eq!(inspection.attempts.len(), 2);
    assert!(
        inspection
            .attempts
            .iter()
            .all(|a| a.phase == AttemptPhase::Settled)
    );
    assert_eq!(
        inspection
            .attempts
            .iter()
            .filter(|a| !a.cache_outputs.is_empty())
            .count(),
        1
    );
    let store = VectorStore::open(&f.fs, None).unwrap();
    assert!(store.active().unwrap().is_none());
    assert_eq!(
        store
            .coverage(&f.spec().id().unwrap(), &corpus(&f))
            .unwrap()
            .available_units,
        1
    );
}
#[test]
fn dense_filters_before_limit_hybrid_context_verified_source_citations() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    f.page("b", "Beta");
    let units = corpus(&f);
    let mut spec = f.spec();
    spec.settings.max_input_bytes = 20_000;
    let long_query = format!("uses {}a", "x".repeat(16 * 1024 - 6));
    let changed_query = format!("uses {}b", "x".repeat(16 * 1024 - 6));
    f.seed(&spec, &units, true);
    f.query_seed(&spec, "Alpha", vec![1.0, 0.0]);
    let plan = QueryPlan {
        mode: SearchMode::Semantic,
        filters: SearchFilters {
            path_prefix: Some("knowledge/pages/b".into()),
            ..Default::default()
        },
        limits: SearchLimits {
            hits: 1,
            candidates: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let hits = f
        .offline()
        .semantic_search("Alpha", &plan, None, false, false, None)
        .unwrap();
    assert_eq!(hits.hits.len(), 1);
    assert!(hits.hits[0].locator.path.as_str().ends_with("b.md"));
    assert!(!hits.network_used);
    let (_temp, app, catalog) = bootstrap();
    let writer =
        lwiki::vault::WriterPermit::acquire(app.fs().root(), std::time::Duration::from_secs(1))
            .unwrap();
    let reader = catalog.verified_snapshot(Some(&writer)).unwrap();
    let units = render::corpus(&reader, &spec.settings).unwrap();
    let mut store = VectorStore::open(app.fs(), Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    store
        .put_batch(
            &space,
            &units
                .iter()
                .map(|u| u.input_hash.clone())
                .collect::<Vec<_>>(),
            &vec![vec![1.0, 0.0]; units.len()],
            true,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    store
        .memberships(&space, reader.snapshot(), &units, true)
        .unwrap();
    store
        .put_batch(
            &space,
            &["uses", &long_query, &changed_query]
                .iter()
                .map(|query| spec.query(query).unwrap().input_hash)
                .collect::<Vec<_>>(),
            &vec![vec![1.0, 0.0]; 3],
            false,
            &Blake3Hash::digest([]),
        )
        .unwrap();
    drop(store);
    drop(reader);
    drop(writer);
    let offline = lwiki::app::OfflineApp::new(
        app.fs().clone(),
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    let request = ContextRequest {
        target: ContextTarget::Combined,
        documents: QueryPlan {
            mode: SearchMode::Hybrid,
            ..Default::default()
        },
        graph: Some(GraphPlan {
            strategy: GraphStrategy::Combined,
            seed_mode: GraphSeedMode::Semantic,
            ..Default::default()
        }),
        ..Default::default()
    };
    let context = offline
        .semantic_context("uses", &request, None, false, false)
        .unwrap();
    assert!(matches!(
        context.verification(),
        lwiki::catalog::SnapshotVerification::VerifiedSnapshot { .. }
    ));
    assert!(context.passages().iter().any(|p| {
        p.citations
            .iter()
            .any(|c| matches!(c, CitationRef::Assertion(_)))
    }));
    assert!(!context.network_used);
    let hits = offline
        .semantic_search(
            "uses",
            &QueryPlan {
                mode: SearchMode::Hybrid,
                ..Default::default()
            },
            None,
            false,
            false,
            request.graph.as_ref(),
        )
        .unwrap();
    let graph = hits.graph.unwrap();
    assert!(graph.seeds.iter().any(|s| {
        s.rank_contributions
            .iter()
            .any(|c| c.channel.ends_with("dense"))
            && s.rank_contributions
                .iter()
                .any(|c| c.channel.contains("lexical"))
    }));
    assert!(
        graph
            .assertions
            .iter()
            .any(|a| !a.support.is_empty() && !a.path.is_empty())
    );

    // Full accepted input must work through all graph-enabled paths; the
    // internal space/query key must not impose a smaller hidden byte ceiling.
    let mut graph_plan = request.graph.clone().unwrap();
    graph_plan.limits.hits = 1;
    let first = offline
        .semantic_graph(&long_query, &graph_plan, None, false, false)
        .unwrap();
    assert!(first.next_cursor.is_some());
    graph_plan.cursor = first.next_cursor;
    assert!(
        offline
            .semantic_graph(&long_query, &graph_plan, None, false, false)
            .is_ok()
    );
    assert_eq!(
        offline
            .semantic_graph(&changed_query, &graph_plan, None, false, false)
            .unwrap_err()
            .code,
        ErrorCode::CursorStale
    );
    let long_context = offline
        .semantic_context(&long_query, &request, None, false, false)
        .unwrap();
    assert!(!long_context.passages().is_empty());
    assert!(!long_context.network_used);
    let long_hits = offline
        .semantic_search(
            &long_query,
            &request.documents,
            None,
            false,
            false,
            request.graph.as_ref(),
        )
        .unwrap();
    assert!(!long_hits.graph.unwrap().seeds.is_empty());
    assert!(!long_hits.network_used);
}

#[test]
fn native_http_fixture_corpus_and_query_accounted_end_to_end() {
    use std::io::{Read, Write};
    let f = Fixture::new();
    f.page("a", "Alpha");
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let received = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = received.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let start = std::time::Instant::now();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            start.elapsed() < std::time::Duration::from_secs(15),
                            "local mock request timeout"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(e) => panic!("local mock accept {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            let body = loop {
                let n = stream.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                data.extend_from_slice(&buf[..n]);
                assert!(data.len() < 128 * 1024);
                if let Some(header_end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&data[..header_end]).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|n| n.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if data.len() >= header_end + 4 + length {
                        break data[header_end + 4..header_end + 4 + length].to_vec();
                    }
                }
            };
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["encoding_format"], "float");
            assert_eq!(value["model"], "test-model");
            captured.lock().unwrap().push(value);
            let response=serde_json::to_vec(&serde_json::json!({"model":"test-model","data":[{"index":0,"embedding":[1.0,0.0]}],"usage":{"prompt_tokens":2,"total_tokens":2}})).unwrap();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len()).unwrap();
            stream.write_all(&response).unwrap();
            stream.flush().unwrap();
        }
    });
    let text = std::fs::read_to_string(&f.config).unwrap().replace(
        "url='https://mock.example/v1/embeddings'",
        &format!("url='http://{address}/embeddings'\nallow_loopback_http=true"),
    );
    private_write(&f.config, text);
    let service = lwiki::config::providers::ProviderConfig::load(&f.config)
        .unwrap()
        .authorize(&f.fs, f.app.vault_id(), "primary", Capability::Embed)
        .unwrap();
    let broker = Arc::new(lwiki::providers::credentials::CredentialBroker::new(
        lwiki::providers::credentials::CredentialOptions {
            clock: Arc::new(TestClock),
            inputs: Arc::new(lwiki::providers::credentials::NativeSecretInputs),
            runner: Arc::new(lwiki::providers::credentials::NativeHelperRunner),
        },
    ));
    let dispatch = lwiki::providers::dispatcher::Dispatcher::native(f.fs.clone(), broker);
    let runtime = runtime(&service, &dispatch);
    let corpus = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap();
    assert!(corpus.network_used && corpus.published);
    let hits = f
        .app
        .semantic_search(
            "question",
            &QueryPlan {
                mode: SearchMode::Semantic,
                ..Default::default()
            },
            Some(&runtime),
            false,
            false,
            None,
        )
        .unwrap();
    assert!(hits.network_used);
    assert_eq!(hits.hits.len(), 1);
    server.join().unwrap();
    let inputs = received.lock().unwrap();
    assert_eq!(inputs.len(), 2);
    assert!(
        inputs[0]["input"][0]
            .as_str()
            .unwrap()
            .contains("Title: \"a\"")
    );
    assert_eq!(inputs[1]["input"][0], "question");
}
#[test]
fn descriptor_swap_preserves_paid_response_then_restores_without_send() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    let saved = Arc::new(std::sync::Mutex::new(None));
    let saved_hook = saved.clone();
    let root = f.fs.root().path().to_path_buf();
    let responses = Arc::new(Responses {
        calls: Default::default(),
        vectors: vec![],
        hook: Some(Box::new(move |_| {
            let directory = root.join(".wiki/state/embedding-inputs");
            let path = std::fs::read_dir(directory)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let bytes = std::fs::read(&path).unwrap();
            *saved_hook.lock().unwrap() = Some((path.clone(), bytes));
            std::fs::write(path, b"{}").unwrap();
        })),
    });
    let dispatch = dispatcher(&f.fs, responses.clone());
    let error = f
        .app
        .embeddings_sync(
            &EmbeddingSettings::default(),
            &runtime(&f.service, &dispatch),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    let run: RecordId = serde_json::from_value(error.details["run_id"].clone()).unwrap();
    let ledger = JobLedger::new(f.fs.clone(), f.app.vault_id().clone(), run, options()).unwrap();
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.attempts[0].phase, AttemptPhase::Received);
    assert!(inspection.attempts[0].cache_outputs.is_empty());
    let (path, bytes) = saved.lock().unwrap().take().unwrap();
    std::fs::write(path, bytes).unwrap();
    let resumed = f
        .app
        .embeddings_sync(
            &EmbeddingSettings::default(),
            &runtime(&f.service, &dispatch),
        )
        .unwrap();
    assert!(resumed.published);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn settled_cache_loss_explicit_sync_replaces_without_faking_old_completion() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses.clone());
    let mut interrupted = runtime(&f.service, &dispatch);
    interrupted.job_options.fault = Some(Arc::new(OnceFault {
        point: LedgerCheckpoint::AfterSettlement,
        armed: std::sync::atomic::AtomicBool::new(true),
    }));
    let error = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &interrupted)
        .unwrap_err();
    let run: RecordId = serde_json::from_value(error.details["run_id"].clone()).unwrap();
    let old_ledger =
        JobLedger::new(f.fs.clone(), f.app.vault_id().clone(), run, options()).unwrap();
    let before = old_ledger.inspect().unwrap();
    assert!(
        before
            .tasks
            .values()
            .all(|t| t.state != TaskState::Completed)
    );
    for name in [
        "embeddings.sqlite3",
        "embeddings.sqlite3-wal",
        "embeddings.sqlite3-shm",
    ] {
        let path = f.fs.root().path().join(".wiki/cache").join(name);
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    let resumed = f
        .app
        .embeddings_sync(
            &EmbeddingSettings::default(),
            &runtime(&f.service, &dispatch),
        )
        .unwrap();
    assert!(resumed.published);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    let after = old_ledger.inspect().unwrap();
    assert!(
        after
            .tasks
            .values()
            .all(|t| t.state != TaskState::Completed)
    );
    assert_eq!(before.attempts[0].receipt, after.attempts[0].receipt);
    assert_eq!(before.attempts[0].billing, after.attempts[0].billing);
    assert_eq!(after.attempts[0].phase, AttemptPhase::Settled);
}
#[test]
fn runtime_dry_run_mismatch_rejected_before_index_or_cache() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses.clone());
    let mut mismatched = runtime(&f.service, &dispatch);
    mismatched.job_options.policy.dry_run = true;
    let error = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &mismatched)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(
        !f.temp
            .path()
            .join(".wiki/cache/embeddings.sqlite3")
            .exists()
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn corrupted_settled_receipt_cannot_publish_staged_vectors() {
    let f = Fixture::new();
    f.page("a", "Alpha");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&f.fs, responses.clone());
    let mut interrupted = runtime(&f.service, &dispatch);
    interrupted.job_options.fault = Some(Arc::new(OnceFault {
        point: LedgerCheckpoint::AfterSettlement,
        armed: std::sync::atomic::AtomicBool::new(true),
    }));
    let error = f
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &interrupted)
        .unwrap_err();
    let run: RecordId = serde_json::from_value(error.details["run_id"].clone()).unwrap();
    let ledger = JobLedger::new(f.fs.clone(), f.app.vault_id().clone(), run, options()).unwrap();
    let inspection = ledger.inspect().unwrap();
    let reference = inspection.attempts[0].receipt.as_ref().unwrap();
    let path = f.fs.root().path().join(reference.path.as_str());
    let original = std::fs::read(&path).unwrap();
    let mut altered = original.clone();
    altered.extend_from_slice(b"\nchanged receipt\n");
    std::fs::write(&path, altered).unwrap();
    assert!(
        f.app
            .embeddings_sync(
                &EmbeddingSettings::default(),
                &runtime(&f.service, &dispatch)
            )
            .is_err()
    );
    let connection =
        rusqlite::Connection::open(f.fs.root().path().join(".wiki/cache/embeddings.sqlite3"))
            .unwrap();
    let ready: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM embedding_vectors WHERE ready=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ready, 0);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    drop(connection);
    std::fs::write(path, original).unwrap();
    assert!(
        f.app
            .embeddings_sync(
                &EmbeddingSettings::default(),
                &runtime(&f.service, &dispatch)
            )
            .unwrap()
            .published
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
}

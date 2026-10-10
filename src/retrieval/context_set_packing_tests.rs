use super::*;
use crate::{
    app::{OfflineApp, OperationOptions, offline},
    catalog::{Catalog, DocumentRow, query::QuerySnapshot},
    domain::DocumentLocator,
    retrieval::{
        ContextPassage, ContextScope, ExcerptLabel,
        context_selection::{
            SelectionCandidate, SelectionDocument, select_candidates_with_semantics,
        },
        indexed_documents::SelectedCatalog,
        selected_documents::{SelectedDocuments, authenticate},
    },
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{VaultFs, VaultRoot},
};
use std::cell::Cell;

pub(crate) struct Fixture {
    _temp: tempfile::TempDir,
    pub(crate) catalog: Catalog,
    pub(crate) reader: QuerySnapshot,
    pub(crate) proof: SelectedDocuments,
    pub(crate) document: DocumentRow,
}
impl Fixture {
    pub(crate) fn new(raw: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        offline::init(&root, "Native set fixture", OperationOptions::default()).unwrap();
        let app = OfflineApp::new(
            VaultFs::new(VaultRoot::explicit(&root).unwrap()),
            OperationOptions::default(),
        )
        .unwrap();
        // Exercise the public normalized publication workflow, then acquire
        // exactly the authenticated selected-source closure used by context.
        app.index_rebuild_normalized().unwrap();
        let added = app
            .source_add(CaptureRequest {
                title: "Synthetic captured source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "synthetic.md".into(),
                original: raw.as_bytes().to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/markdown".into()),
            })
            .unwrap();
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        let reader = catalog.cached_query_snapshot(Default::default()).unwrap();
        assert!(reader.normalized_layout());
        let revision = reader
            .record(&added.allocated_ids["revision"])
            .unwrap()
            .unwrap();
        let parent = revision.path.as_str().rsplit_once('/').unwrap().0;
        let content = revision.record.string("wiki_content_path").unwrap();
        let path = VaultRelativePath::new(format!("{parent}/{content}")).unwrap();
        let proof = authenticate(
            &catalog,
            &reader,
            std::slice::from_ref(&path),
            &request().verification_budget,
        )
        .unwrap();
        let document = proof.documents[&path].clone();
        assert_eq!(document.raw_text, raw);
        Self {
            _temp: temp,
            catalog,
            reader,
            proof,
            document,
        }
    }
    pub(crate) fn selected(&self) -> SelectedCatalog<'_> {
        SelectedCatalog {
            reader: &self.reader,
            proof: &self.proof,
        }
    }
    pub(crate) fn passage(&self, span: ByteSpan) -> ContextPassage {
        let text = span.slice(&self.document.raw_text).unwrap().to_owned();
        ContextPassage {
            locator: DocumentLocator {
                record: Some(RecordRef {
                    vault_id: self.catalog.vault_id().clone(),
                    record_id: self.document.owner_revision.clone().unwrap(),
                    expected_kind: RecordKind::Revision,
                }),
                path: self.document.path.clone(),
                observed_hash: self.document.hash.clone(),
            },
            text: text.clone(),
            span,
            label: ExcerptLabel::CapturedSource,
            eligibility: self.document.eligibility,
            citations: vec![CitationRef::Source(SourceSpanRef {
                source_id: self.document.source_id.clone().unwrap(),
                source_revision: self.document.owner_revision.clone().unwrap(),
                span,
                quote_hash: Blake3Hash::digest(text.as_bytes()),
            })],
            contributors: vec![],
            rank_contributions: vec![],
            support_group: Some(self.document.hash.clone()),
        }
    }
    pub(crate) fn packet(&self, key: &str, candidate: SelectionCandidate) -> Packet {
        Packet {
            passages: vec![self.passage(candidate.span)],
            bundle: None,
            navigation: None,
            key: key.into(),
            score: 1.0 / 61.0,
            selection_ordinal: None,
            selection: Some(candidate),
            unit_score: None,
            unit_origin: None,
            fallback: None,
            unit_clipped: false,
        }
    }
    pub(crate) fn assert_quote(&self, passage: &ContextPassage) {
        assert_eq!(
            passage.text,
            passage.span.slice(&self.document.raw_text).unwrap()
        );
        assert_eq!(passage.locator.observed_hash, self.document.hash);
        assert_eq!(passage.citations.len(), 1);
        let CitationRef::Source(c) = &passage.citations[0] else {
            panic!("missing SourceRef");
        };
        assert_eq!(c.source_id, self.document.source_id.clone().unwrap());
        assert_eq!(
            c.source_revision,
            self.document.owner_revision.clone().unwrap()
        );
        assert_eq!(c.span, passage.span);
        assert_eq!(c.quote_hash, Blake3Hash::digest(passage.text.as_bytes()));
    }
}
fn span(start: usize, end: usize) -> ByteSpan {
    ByteSpan::new(start as u64, end as u64).unwrap()
}
pub(crate) fn request() -> ContextRequest {
    let mut r = ContextRequest::default();
    r.scope = ContextScope::IndexedDocuments;
    r.documents.limits.excerpt_bytes = 128;
    r
}
fn identity(weights: &[f64]) -> Representation {
    let n = weights.len();
    let mut similarities = vec![0.0; n * n];
    for i in 0..n {
        similarities[i * n + i] = 1.0;
    }
    Representation {
        similarities,
        weights: weights.to_vec(),
    }
}

#[test]
fn coalesced_delta_changes_choice_instead_of_standalone_cost() {
    let selection = select(
        &["a", "b", "c"],
        &identity(&[2.0, 1.0, 1.0]),
        0,
        (3072, 1024),
        || Ok(()),
        |members| {
            let bytes = match members {
                [0] => 10,
                [1] => 100,
                [2] => 8,
                [0, 1] => 11,
                [0, 2] => 18,
                [1, 2] => 108,
                _ => return Ok(None),
            };
            Ok(Some(Realization {
                bytes,
                state: members.to_vec(),
            }))
        },
    )
    .unwrap();
    assert_eq!(selection.members, vec![0, 1]);
    assert_eq!(selection.state, Some(vec![0, 1]));
}

#[test]
fn distinct_exception_survives_redundant_windows_with_identical_query_terms() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let tokenizer = Tokenizer::new(&connection).unwrap();
    let mut tokens = (0..8)
        .map(|_| {
            normalized_lexical_tokens(&tokenizer, "permit application fee form permit fee").unwrap()
        })
        .collect::<Vec<_>>();
    tokens.push(
        normalized_lexical_tokens(&tokenizer, "permit fee exempt emergency unless restricted")
            .unwrap(),
    );
    assert_eq!(
        tokens[0],
        normalized_lexical_tokens(&tokenizer, "permit application fee form").unwrap()
    );
    let representation = Representation::from_tokens(&tokens, &[1.0; 9], || Ok(())).unwrap();
    assert_eq!(representation.similarities[0], 1.0);
    assert_eq!(
        representation.similarities[8],
        representation.similarities[8 * 9]
    );
    assert!(representation.similarities[8] < 1.0);
    let keys = ["a", "b", "c", "d", "e", "f", "g", "h", "exception"];
    let selected = select(
        &keys,
        &representation,
        0,
        (3072, 1024),
        || Ok(()),
        |members| {
            Ok((members.len() <= 2).then(|| Realization {
                bytes: 100 * members.len(),
                state: members.to_vec(),
            }))
        },
    )
    .unwrap();
    assert!(selected.members.contains(&8));
    assert_eq!(selected.members.len(), 2);
    assert_eq!(selected.members.iter().filter(|&&i| i < 8).count(), 1);
}

#[test]
fn replacement_rebuilds_overlapping_originals_and_fresh_exact_citations() {
    let fixture = Fixture::new(&format!("{}\n", "a".repeat(240)));
    let proposals = [
        fixture.passage(span(100, 150)),
        fixture.passage(span(130, 160)),
        fixture.passage(span(180, 210)),
    ];
    let mut r = request();
    let merged = document_trial(&fixture.selected(), &r, &[], &proposals[..2]).unwrap();
    let standalone = document_trial(&fixture.selected(), &r, &[], &proposals[..1]).unwrap();
    let other_standalone = document_trial(&fixture.selected(), &r, &[], &proposals[1..2]).unwrap();
    assert!(merged.text.len() - standalone.text.len() < other_standalone.text.len());
    assert_eq!(merged.passages[0].span, span(100, 160));
    let replacement = document_trial(&fixture.selected(), &r, &[], &proposals[1..]).unwrap();
    r.budget.max_bytes = replacement.text.len();
    r.budget.max_tokens = replacement.text.len().div_ceil(4);
    assert!(
        document_trial(
            &fixture.selected(),
            &r,
            &[],
            &[proposals[0].clone(), proposals[2].clone()]
        )
        .unwrap()
        .reason
        .is_some()
    );
    let representation = Representation {
        similarities: vec![1.0, 0.5, 0.6, 0.5, 1.0, 0.0, 0.6, 0.0, 1.0],
        weights: vec![2.0, 2.0, 3.0],
    };
    let selected = select(
        &["a", "b", "c"],
        &representation,
        0,
        (3072, 1024),
        || Ok(()),
        |members| {
            let originals = members
                .iter()
                .map(|&i| proposals[i].clone())
                .collect::<Vec<_>>();
            let trial = document_trial(&fixture.selected(), &r, &[], &originals)?;
            Ok(trial.reason.is_none().then(|| Realization {
                bytes: trial.text.len(),
                state: trial,
            }))
        },
    )
    .unwrap();
    assert_eq!(selected.members, vec![1, 2]);
    assert!(selected.statistics.exchanges > 0);
    let final_state = selected.state.unwrap();
    assert_eq!(final_state.text, replacement.text);
    assert_eq!(final_state.passages[0].span, span(130, 160));
    for p in &final_state.passages {
        fixture.assert_quote(p);
    }
    assert!(final_state.text.contains("Citation (Current):"));
}

#[test]
fn bridging_admits_final_set_even_when_original_order_prefix_fails_owner_cap() {
    let fixture = Fixture::new(&format!("{}\n", "a".repeat(150)));
    let r = request();
    let mut proposals = (0..5)
        .map(|i| fixture.passage(span(i * 20, i * 20 + 10)))
        .collect::<Vec<_>>();
    assert_eq!(
        document_trial(&fixture.selected(), &r, &[], &proposals)
            .unwrap()
            .reason,
        Some("document_passage_cap")
    );
    proposals.push(fixture.passage(span(0, 90)));
    let trial = document_trial(&fixture.selected(), &r, &[], &proposals).unwrap();
    assert_eq!(trial.reason, None);
    assert_eq!(trial.passages.len(), 1);
    fixture.assert_quote(&trial.passages[0]);
    let selected = select(
        &["a", "b", "c", "d", "e", "z"],
        &identity(&[1.0; 6]),
        0,
        (3072, 1024),
        || Ok(()),
        |members| {
            let originals = members
                .iter()
                .map(|&i| proposals[i].clone())
                .collect::<Vec<_>>();
            let trial = document_trial(&fixture.selected(), &r, &[], &originals)?;
            Ok(trial.reason.is_none().then(|| Realization {
                bytes: trial.text.len(),
                state: trial,
            }))
        },
    )
    .unwrap();
    assert_eq!(selected.members.len(), 6);
    assert_eq!(selected.state.unwrap().passages[0].span, span(0, 90));
}

#[test]
fn admission_preserves_excerpt_owner_byte_and_token_reservations() {
    let fixture = Fixture::new(&format!("{}\n", "a".repeat(200)));
    let mut r = request();
    let oversized = [
        fixture.passage(span(0, 100)),
        fixture.passage(span(90, 150)),
    ];
    assert_eq!(
        document_trial(&fixture.selected(), &r, &[], &oversized)
            .unwrap()
            .reason,
        Some("merged_passage_exceeds_excerpt_bound")
    );
    let disjoint = (0..5)
        .map(|i| fixture.passage(span(i * 20, i * 20 + 10)))
        .collect::<Vec<_>>();
    assert_eq!(
        document_trial(&fixture.selected(), &r, &[], &disjoint)
            .unwrap()
            .reason,
        Some("document_passage_cap")
    );
    let p = [fixture.passage(span(0, 50))];
    let trial = document_trial(&fixture.selected(), &r, &[], &p).unwrap();
    r.budget.instruction_bytes = 17;
    r.budget.output_bytes = 23;
    r.budget.instruction_tokens = 3;
    r.budget.output_tokens = 5;
    r.budget.max_bytes = trial.text.len() + 40;
    r.budget.max_tokens = trial.text.len().div_ceil(4) + 8;
    assert_eq!(
        document_trial(&fixture.selected(), &r, &[], &p)
            .unwrap()
            .reason,
        None
    );
    r.budget.max_bytes -= 1;
    assert_eq!(
        document_trial(&fixture.selected(), &r, &[], &p)
            .unwrap()
            .reason,
        Some("required_bundle_or_passage_does_not_fit")
    );
    r.budget.max_bytes += 1;
    r.budget.max_tokens -= 1;
    assert_eq!(
        document_trial(&fixture.selected(), &r, &[], &p)
            .unwrap()
            .reason,
        Some("required_bundle_or_passage_does_not_fit")
    );
}

#[test]
fn exhausted_search_returns_only_its_last_fully_admitted_membership_deterministically() {
    let run = || {
        select(
            &["a", "b", "c"],
            &identity(&[3.0, 2.0, 1.0]),
            0,
            (4, 0),
            || Ok(()),
            |members| {
                Ok(Some(Realization {
                    bytes: 100 * members.len(),
                    state: members.to_vec(),
                }))
            },
        )
        .unwrap()
    };
    let a = run();
    let b = run();
    assert_eq!(a.members, vec![0]);
    assert_eq!(a.state, Some(a.members.clone()));
    assert_eq!(a.members, b.members);
    assert_eq!(a.statistics.additions, 4);
    assert!(a.statistics.addition_exhausted);
    assert!(a.statistics.exhausted());
}

#[test]
fn production_addition_limit_never_spends_exchange_reserve_or_returns_unrealized_members() {
    let keys = (0..MAX_POOL).map(|i| format!("{i:04}")).collect::<Vec<_>>();
    let borrowed = keys.iter().map(String::as_str).collect::<Vec<_>>();
    let calls = Cell::new(0);
    let selected = select(
        &borrowed,
        &identity(&vec![1.0; MAX_POOL]),
        0,
        (ADDITION_TRIALS, EXCHANGE_TRIALS),
        || Ok(()),
        |members| {
            calls.set(calls.get() + 1);
            Ok(Some(Realization {
                bytes: 100 * members.len(),
                state: members.to_vec(),
            }))
        },
    )
    .unwrap();
    assert_eq!(calls.get(), ADDITION_TRIALS);
    assert_eq!(selected.statistics.additions, ADDITION_TRIALS);
    assert_eq!(selected.statistics.exchanges, 0);
    assert!(selected.statistics.addition_exhausted);
    // 320 + 319 + ... + 312 = 2844 completes nine rounds; the
    // remaining 228 trials cannot commit the tenth round's winner.
    assert_eq!(selected.members, (0..9).collect::<Vec<_>>());
    assert_eq!(selected.state, Some(selected.members));
}

#[test]
fn exchange_exhaustion_keeps_the_original_admitted_state() {
    let r = Representation {
        similarities: vec![1.0, 0.5, 0.6, 0.5, 1.0, 0.0, 0.6, 0.0, 1.0],
        weights: vec![2.0, 2.0, 3.0],
    };
    let selected = select(
        &["a", "b", "c"],
        &r,
        0,
        (ADDITION_TRIALS, 0),
        || Ok(()),
        |members| {
            Ok(
                (members.len() <= 2 && members != [0, 2]).then(|| Realization {
                    bytes: 100 * members.len(),
                    state: members.to_vec(),
                }),
            )
        },
    )
    .unwrap();
    assert_eq!(selected.members, vec![0, 1]);
    assert_eq!(selected.state, Some(selected.members.clone()));
    assert_eq!(selected.statistics.exchanges, 0);
    assert!(selected.statistics.exchange_exhausted);
}

#[test]
fn interrupted_exchange_discards_a_positive_admitted_prefix_winner() {
    let r = Representation {
        similarities: vec![1.0, 0.5, 0.6, 0.5, 1.0, 0.0, 0.6, 0.0, 1.0],
        weights: vec![2.0, 2.0, 3.0],
    };
    let tentative_admitted = Cell::new(false);
    let selected = select(
        &["a", "b", "c"],
        &r,
        0,
        (ADDITION_TRIALS, 1),
        || Ok(()),
        |members| {
            if members == [1, 2] {
                tentative_admitted.set(true);
            }
            Ok(
                (members.len() <= 2 && members != [0, 2]).then(|| Realization {
                    bytes: 100 * members.len(),
                    state: members.to_vec(),
                }),
            )
        },
    )
    .unwrap();
    assert!(
        tentative_admitted.get(),
        "the first replacement was admissible with positive gain"
    );
    assert_eq!(selected.members, vec![0, 1]);
    assert_eq!(selected.state, Some(selected.members.clone()));
    assert_eq!(selected.statistics.exchanges, 1);
    assert!(selected.statistics.exchange_exhausted);
}

#[test]
fn deadline_or_freshness_error_never_returns_an_unchecked_partial_state() {
    let rendered = Cell::new(false);
    let result = select(
        &["a"],
        &identity(&[1.0]),
        0,
        (3072, 1024),
        || {
            if rendered.get() {
                Err(WikiError::new(ErrorCode::BudgetExceeded, "owning deadline"))
            } else {
                Ok(())
            }
        },
        |members| {
            rendered.set(true);
            Ok(Some(Realization {
                bytes: 1,
                state: members.to_vec(),
            }))
        },
    );
    assert!(matches!(result,Err(error) if error.code==ErrorCode::BudgetExceeded));
    let result = select(
        &["a", "b"],
        &identity(&[1.0; 2]),
        0,
        (3072, 1024),
        || Ok(()),
        |members| {
            if members.contains(&1) {
                Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "changed source",
                ))
            } else {
                Ok(Some(Realization {
                    bytes: 1,
                    state: members.to_vec(),
                }))
            }
        },
    );
    assert!(matches!(result,Err(error) if error.code==ErrorCode::FreshnessConflict));
}

#[test]
fn complete_teaching_procedure_stays_indivisible_in_native_allocation() {
    let raw = "# Operation\n\nRun ignored cases with the complete procedure.\n\n```sh\nrunner test --ignored\nrunner test --include-ignored\n```\n\nConfirm the exit status before deployment.\n";
    let fixture = Fixture::new(raw);
    let selected = select_candidates_with_semantics(
        &fixture.selected(),
        "ignored cases",
        &[SelectionDocument {
            owner_index: 0,
            document: &fixture.document,
            seed_spans: &[],
        }],
        1024,
        &[],
    )
    .unwrap();
    let packets = selected
        .candidates
        .into_iter()
        .enumerate()
        .map(|(i, c)| fixture.packet(&format!("{i:04}"), c))
        .collect::<Vec<_>>();
    assert!(!packets.is_empty());
    for packet in &packets {
        if packet.passages[0].text.contains("Run ignored cases") {
            assert!(
                packet.passages[0]
                    .text
                    .contains("runner test --include-ignored")
            );
            assert!(packet.passages[0].text.contains("Confirm the exit status"));
        }
    }
    let mut r = request();
    r.documents.limits.excerpt_bytes = 1024;
    let native = allocate(&fixture.selected(), &r, &packets, 0)
        .unwrap()
        .unwrap();
    let state = native.state.unwrap();
    assert!(state.text.contains("runner test --ignored"));
    assert!(state.text.contains("runner test --include-ignored"));
    assert!(state.text.contains("Confirm the exit status"));
    for p in &state.passages {
        fixture.assert_quote(p);
    }
}

pub(crate) fn candidate(span: ByteSpan, local_relevance: u64) -> SelectionCandidate {
    SelectionCandidate {
        owner_index: 0,
        span,
        covered_terms: vec![0],
        local_relevance,
        seed_overlap: false,
        clipped: false,
        semantic_affinity: None,
    }
}

#[test]
fn raw_local_relevance_has_no_base_seed_or_clipping_bonus_and_zero_pool_falls_back() {
    let fixture = Fixture::new("alpha condition exception\n");
    let mut c = candidate(span(0, 25), 0);
    c.seed_overlap = true;
    c.clipped = true;
    let packets = [fixture.packet("a", c)];
    assert!(
        allocate(&fixture.selected(), &request(), &packets, 0)
            .unwrap()
            .is_none()
    );
    let tokens = [
        BTreeSet::from(["alpha".to_owned()]),
        BTreeSet::from(["alpha".to_owned(), "exception".to_owned()]),
    ];
    let r = Representation::from_tokens(&tokens, &[0.0, 3.0], || Ok(())).unwrap();
    assert_eq!(r.weights[0], 0.0);
    assert!(
        r.addition_gain(&[0.0; 2], 0) > 0.0,
        "zero-weight origins remain eligible columns"
    );
    assert_eq!(r.weights[1], 3.0 / (1.0 + r.similarities[1]));
}

#[test]
fn unmarked_legacy_packets_do_not_activate_failed_facility_in_production_packing() {
    use crate::retrieval::{
        HitSet,
        context::{PackingInput, pack},
        context_selection_packet::SelectionAction,
        context_types::ContextSelectionSignals,
    };
    let fixture = Fixture::new("alpha condition exception\n");
    let r = request();
    let hits = HitSet {
        network_used: false,
        graph: None,
        hits: vec![],
        next_cursor: None,
        truncated: false,
        candidate_count: 0,
        omitted_candidates: 0,
        snapshot: fixture.reader.snapshot().clone(),
        verification: fixture.reader.verification().clone(),
        dependency_fingerprint: Blake3Hash::digest("test dependency"),
        warnings: vec![],
    };
    let signals = ContextSelectionSignals::default();
    let action = SelectionAction::Automatic;
    let run = |n: usize| {
        let packets = (0..n)
            .map(|i| fixture.packet(&format!("{i:04}"), candidate(span(0, 25), 1)))
            .collect();
        pack(
            &fixture.selected(),
            &r,
            PackingInput {
                packets,
                omissions: vec![],
                term_weights: vec![1],
                selection_warnings: vec![],
                source_aware: false,
                native_lexical_units: false,
                query: Some("alpha"),
                signals: &signals,
                selection_action: &action,
                hits: &hits,
                graph: None,
                dependency_fingerprint: hits.dependency_fingerprint.clone(),
                evidence_sets: None,
            },
        )
        .unwrap()
    };
    let native = run(3);
    assert_eq!(native.passages.len(), 1);
    assert!(!native.truncated);
    assert!(native.omissions.is_empty());
    assert!(
        !native
            .warnings
            .iter()
            .any(|w| w.contains("native lexical set assembly"))
    );
    fixture.assert_quote(&native.passages[0]);
    let fallback = run(MAX_POOL + 1);
    assert_eq!(fallback.passages.len(), 1);
    assert!(!fallback.truncated);
    assert!(fallback.omissions.is_empty());
    assert!(
        !fallback
            .warnings
            .iter()
            .any(|w| w.contains("native lexical set assembly"))
    );
}

#[test]
fn public_mixed_owner_with_zero_authored_proposals_retains_prior_automatic_path() {
    let fixture = Fixture::new("alpha captured condition and exception.\n");
    let app = OfflineApp::new(fixture.catalog.fs().clone(), OperationOptions::default()).unwrap();
    let path = VaultRelativePath::new("pages/empty_authored.md").unwrap();
    let note = CanonicalRecord::from_value(serde_json::json!({
        "wiki_schema":"1","wiki_id":"page_empty_authored","wiki_kind":"page",
        "title":"alpha","wiki_status":"reviewed"
    }))
    .unwrap();
    // Title discovery admits this authored owner with a nonempty excerpt,
    // while markup-only body windows produce no readable old-builder proposal.
    let bytes = crate::sources::revision::record_bytes(
        note,
        b"<!-- authored owner has no readable content -->\n",
    )
    .unwrap();
    app.page_put(path.clone(), bytes, None).unwrap();
    let r = request();
    let reader = fixture
        .catalog
        .cached_query_snapshot(Default::default())
        .unwrap();
    let hits =
        crate::retrieval::lexical::search_context_catalog(&reader, "alpha", &r.documents, false)
            .unwrap();
    let authored = hits
        .hits
        .iter()
        .position(|h| h.locator.path == path)
        .unwrap();
    assert!(!hits.hits[authored].excerpt.span.is_empty());
    let paths = hits
        .hits
        .iter()
        .map(|h| h.locator.path.clone())
        .collect::<Vec<_>>();
    let proof = authenticate(&fixture.catalog, &reader, &paths, &r.verification_budget).unwrap();
    assert!(proof.documents[&path].owner_revision.is_none());
    assert!(proof.documents.values().any(|d| d.owner_revision.is_some()));
    let selected = SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let anchors = hits
        .hits
        .iter()
        .map(|h| vec![h.excerpt.span])
        .collect::<Vec<_>>();
    let documents = hits
        .hits
        .iter()
        .enumerate()
        .map(|(owner_index, h)| SelectionDocument {
            owner_index,
            document: &proof.documents[&h.locator.path],
            seed_spans: &anchors[owner_index],
        })
        .collect::<Vec<_>>();
    let proposals = select_candidates_with_semantics(
        &selected,
        "alpha",
        &documents,
        r.documents.limits.excerpt_bytes,
        &[],
    )
    .unwrap();
    assert!(!proposals.candidates.is_empty());
    assert!(
        proposals
            .candidates
            .iter()
            .all(|c| c.owner_index != authored)
    );
    let result =
        crate::retrieval::verification::context(&fixture.catalog, None, "alpha", &r).unwrap();
    assert!(result.text().contains("captured condition"));
    assert!(result.passages().iter().all(|p| p.locator.path != path));
    assert!(result.passages().iter().all(|p| {
        p.rank_contributions
            .iter()
            .all(|rank| rank.channel != crate::retrieval::context_lexical_unit_packing::CHANNEL)
    }));
    assert!(
        !result
            .warnings()
            .iter()
            .any(|w| w.starts_with("query-ranked lexical units:"))
    );
    for passage in result.passages() {
        fixture.assert_quote(passage);
    }
}

#[test]
fn public_automatic_context_seals_complete_procedure_with_current_citations_and_reservations() {
    let raw = "# Operation\n\nRun ignored cases with the complete procedure.\n\n```sh\nrunner test --ignored\nrunner test --include-ignored\n```\n\nConfirm the exit status before deployment.\n";
    let fixture = Fixture::new(raw);
    let mut r = request();
    r.documents.limits.excerpt_bytes = 1024;
    r.budget.instruction_bytes = 73;
    r.budget.output_bytes = 91;
    r.budget.instruction_tokens = 19;
    r.budget.output_tokens = 23;
    let result =
        crate::retrieval::verification::context(&fixture.catalog, None, "ignored cases", &r)
            .unwrap();
    assert!(
        result
            .warnings()
            .iter()
            .any(|w| w.starts_with("query-ranked lexical units:"))
    );
    assert!(result.text().contains("runner test --ignored"));
    assert!(result.text().contains("runner test --include-ignored"));
    assert!(result.text().contains("Confirm the exit status"));
    assert!(result.text().contains("Citation (Current):"));
    assert_eq!(result.usage().reserved_bytes, 164);
    assert_eq!(result.usage().reserved_tokens, 42);
    assert!(result.usage().rendered_bytes <= r.budget.max_bytes - 164);
    assert!(result.usage().estimated_tokens <= r.budget.max_tokens - 42);
    assert_eq!(result.usage().rendered_bytes, result.text().len());
    for passage in result.passages() {
        fixture.assert_quote(passage);
    }
    assert!(
        matches!(result.verification(),crate::catalog::SnapshotVerification::IndexedEvidence {evidence_domain,global_membership_verified:false,..} if evidence_domain=="selected_documents")
    );
    // The final verifier must reject an external byte edit before another
    // Automatic packet can authenticate those cached locations as Current.
    std::fs::write(
        fixture
            .catalog
            .fs()
            .root()
            .path()
            .join(fixture.document.path.as_str()),
        b"Changed source bytes\n",
    )
    .unwrap();
    assert!(
        crate::retrieval::verification::context(&fixture.catalog, None, "ignored cases", &r)
            .is_err()
    );
}

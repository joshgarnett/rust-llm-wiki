//! Fictional metadata and originals only; no provider, corpus or answer labels.
use super::*;
use crate::{
    catalog::{CatalogDiagnostic, DocumentRow, RecordRow, SnapshotVerification},
    domain::{Blake3Hash, ByteSpan, ReadSnapshot, VaultRelativePath},
    retrieval::source_relations::{
        AdmittedOriginal, Edge, Envelope, Node, NodeKind, NominationLimits, PreparationBinding,
        RelationKind, SCHEMA, SourceBinding, SpanBinding, ValidationLimits, parse,
    },
};
use rusqlite::{Connection, Row};
use std::{cell::Cell, time::Duration};

struct Reader {
    connection: Connection,
    checks: Cell<usize>,
    fail_after: Option<usize>,
}
impl Reader {
    fn new() -> Self {
        Self {
            connection: Connection::open_in_memory().unwrap(),
            checks: Cell::new(0),
            fail_after: None,
        }
    }
}
impl QueryCatalog for Reader {
    fn connection(&self) -> &Connection {
        &self.connection
    }
    fn check_query_budget(&self) -> Result<()> {
        self.checks.set(self.checks.get() + 1);
        if self
            .fail_after
            .is_some_and(|limit| self.checks.get() > limit)
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "fixture query budget",
            ));
        }
        Ok(())
    }
    fn snapshot(&self) -> &ReadSnapshot {
        panic!("no snapshot lookup permitted")
    }
    fn vault_id(&self) -> &RecordId {
        panic!("no vault lookup permitted")
    }
    fn verification(&self) -> &SnapshotVerification {
        panic!("no verification lookup permitted")
    }
    fn record(&self, _: &RecordId) -> Result<Option<RecordRow>> {
        panic!("no record read permitted")
    }
    fn document(&self, _: &VaultRelativePath) -> Result<Option<DocumentRow>> {
        panic!("no document read permitted")
    }
    fn diagnostics(&self, _: &BTreeSet<VaultRelativePath>) -> Result<Vec<CatalogDiagnostic>> {
        panic!("no diagnostics read permitted")
    }
    fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
        panic!("no fingerprint read permitted")
    }
    fn query_scope(&self) -> &'static str {
        panic!("no query scope lookup permitted")
    }
    fn decode_document(&self, _: &Row<'_>, _: usize) -> Result<DocumentRow> {
        panic!("no document decode permitted")
    }
}

struct Fixture {
    text: String,
    source: SourceBinding,
    preparation: PreparationBinding,
    envelope: Envelope,
}
impl Fixture {
    fn new(nodes: &[(&str, &str)]) -> Self {
        let text = format!(
            "General rule.\n{}Written notice is required.\n",
            "Background.\n".repeat(400)
        );
        let source = SourceBinding {
            source_id: id("fictional_source"),
            source_revision: id("fictional_revision"),
            raw_hash: Blake3Hash::digest(&text),
        };
        let hash = Blake3Hash::digest("fixture preparation");
        let preparation = PreparationBinding {
            recipe_id: id("fictional_recipe"),
            recipe_hash: hash.clone(),
            prompt_hash: hash.clone(),
            schema_hash: hash.clone(),
            model_identity_hash: hash.clone(),
            settings_hash: hash.clone(),
            retained_generation_id: id("fictional_generation"),
            retained_generation_hash: hash,
        };
        let evidence = SpanBinding {
            span: ByteSpan::new(0, "General rule.".len() as u64).unwrap(),
            quote_hash: Blake3Hash::digest("General rule."),
        };
        let envelope = Envelope {
            schema: SCHEMA.into(),
            source: source.clone(),
            preparation: preparation.clone(),
            nodes: nodes
                .iter()
                .map(|(name, scope)| Node {
                    id: id(name),
                    scope: (*scope).into(),
                    kind: NodeKind::Rule,
                    evidence: evidence.clone(),
                })
                .collect(),
            edges: vec![],
        };
        Self {
            text,
            source,
            preparation,
            envelope,
        }
    }
    fn validated(&self) -> ValidatedRelations<'_> {
        parse(
            &serde_json::to_vec(&self.envelope).unwrap(),
            AdmittedOriginal {
                binding: &self.source,
                text: &self.text,
            },
            &self.preparation,
            ValidationLimits {
                max_hash_bytes: 4 * 1024 * 1024,
                deadline: deadline(),
            },
        )
        .unwrap()
    }
}
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

#[test]
fn anchor_scope_can_find_unseeded_rule_and_distant_required_original() {
    let reader = Reader::new();
    let mut fixture = Fixture::new(&[
        ("rule", "application notice"),
        ("requirement", "written requirement"),
    ]);
    let quote = "Written notice is required.";
    let start = fixture.text.find(quote).unwrap() as u64;
    fixture.envelope.nodes[1].evidence = SpanBinding {
        span: ByteSpan::new(start, start + quote.len() as u64).unwrap(),
        quote_hash: Blake3Hash::digest(quote),
    };
    fixture.envelope.nodes[1].kind = NodeKind::Requirement;
    fixture.envelope.edges.push(Edge {
        from: id("rule"),
        to: id("requirement"),
        kind: RelationKind::Requires,
        evidence: fixture.envelope.nodes[1].evidence.clone(),
    });
    let valid = fixture.validated();
    let hash_work = valid.validation_hash_bytes();
    let out = rank_anchors(
        &reader,
        "application notice",
        &[(3, &valid)],
        &[9, 4],
        80,
        deadline(),
    )
    .unwrap();
    assert_eq!(
        out.anchors,
        vec![RelationAnchor {
            owner_index: 3,
            node_id: id("rule"),
            covered_terms: vec![0, 1],
            local_relevance: 13
        }]
    );
    // The root evidence contains neither query term and lies outside this
    // hypothetical discovery seed. Discovery overlap is not an adapter input.
    let seed = ByteSpan::new(start, start + quote.len() as u64).unwrap();
    assert!(valid.envelope().nodes[0].evidence.span.end() < seed.start());
    let nominated = valid
        .nominate(
            &[out.anchors[0].node_id.clone()],
            NominationLimits {
                max_candidates: 80,
                max_span_bytes: 1024,
                max_text_bytes: 12000,
                max_node_visits: 4,
                max_edge_visits: 4,
                max_depth: 4,
                deadline: deadline(),
            },
        )
        .unwrap();
    assert!(
        nominated
            .spans
            .iter()
            .any(|span| span.evidence.span.start() > 4000 && span.text == quote)
    );
    assert_eq!(nominated.dependencies[0].kind, RelationKind::Requires);
    assert_eq!(valid.validation_hash_bytes(), hash_work);
    assert!(!format!("{:?}", out.anchors).contains("application notice"));
}

#[test]
fn distinct_terms_use_exact_global_weights_and_existing_unicode_stemming() {
    let reader = Reader::new();
    let fixture = Fixture::new(&[
        ("all", "Permits permit permit NOTICE notices 中文"),
        ("one", "permit"),
    ]);
    let valid = fixture.validated();
    let out = rank_anchors(
        &reader,
        "permit notice 中文 permit",
        &[(0, &valid)],
        &[2, 17, 5],
        80,
        deadline(),
    )
    .unwrap();
    assert_eq!(out.anchors[0].covered_terms, vec![0, 1, 2]);
    assert_eq!(out.anchors[0].local_relevance, 24);
    assert_eq!(out.anchors[1].local_relevance, 2);
    assert_eq!(out.scanned_nodes, 2);
    assert_eq!(
        out.scanned_scope_bytes,
        fixture
            .envelope
            .nodes
            .iter()
            .map(|node| node.scope.len())
            .sum::<usize>()
    );
    assert!(!out.query_terms_capped);
}

#[test]
fn ties_use_existing_owner_rank_then_node_id_and_zero_weights_still_match() {
    let reader = Reader::new();
    let first = Fixture::new(&[("zeta", "aqua"), ("alpha", "aqua")]);
    let second = Fixture::new(&[("beta", "aqua"), ("unmatched", "terra")]);
    let a = first.validated();
    let b = second.validated();
    let out = rank_anchors(&reader, "aqua", &[(7, &a), (2, &b)], &[0], 80, deadline()).unwrap();
    assert_eq!(
        out.anchors
            .iter()
            .map(|anchor| (anchor.owner_index, anchor.node_id.clone()))
            .collect::<Vec<_>>(),
        vec![(2, id("beta")), (7, id("alpha")), (7, id("zeta"))]
    );
    assert!(
        out.anchors
            .iter()
            .all(|anchor| anchor.local_relevance == 0 && anchor.covered_terms == vec![0])
    );
}

#[test]
fn empty_terms_and_empty_owners_are_explicitly_empty() {
    let reader = Reader::new();
    let fixture = Fixture::new(&[("node", "aqua")]);
    let valid = fixture.validated();
    let out = rank_anchors(&reader, "the how", &[(0, &valid)], &[], 80, deadline()).unwrap();
    assert!(out.anchors.is_empty());
    assert_eq!(out.matched_anchors, 0);
    assert_eq!(out.omitted_anchors, 0);
    assert_eq!(out.scanned_nodes, 1);
    let out = rank_anchors(&reader, "aqua", &[], &[3], 0, deadline()).unwrap();
    assert!(out.anchors.is_empty());
    assert_eq!(out.scanned_nodes, 0);
}

#[test]
fn candidate_caps_report_exact_omissions_and_scan_late_high_weight_node() {
    let reader = Reader::new();
    let mut fixture = Fixture::new(&[("early", "aqua")]);
    let template = fixture.envelope.nodes[0].clone();
    for index in 1..MAX_NODES - 1 {
        let mut node = template.clone();
        node.id = id(&format!("node_{index:04}"));
        fixture.envelope.nodes.push(node);
    }
    let mut late = template;
    late.id = id("late");
    late.scope = "terra".into();
    fixture.envelope.nodes.push(late);
    let valid = fixture.validated();
    let out = rank_anchors(
        &reader,
        "aqua terra",
        &[(0, &valid)],
        &[1, 20],
        1,
        deadline(),
    )
    .unwrap();
    assert_eq!(out.anchors[0].node_id, id("late"));
    assert_eq!(out.matched_anchors, MAX_NODES);
    assert_eq!(out.omitted_anchors, MAX_NODES - 1);
    assert_eq!(out.scanned_nodes, MAX_NODES);
    let zero = rank_anchors(
        &reader,
        "aqua terra",
        &[(0, &valid)],
        &[1, 20],
        0,
        deadline(),
    )
    .unwrap();
    assert_eq!(zero.omitted_anchors, MAX_NODES);
    assert!(zero.anchors.is_empty());
    assert!(rank_anchors(&reader, "aqua", &[(0, &valid)], &[1], 81, deadline()).is_err());
    let owners = (0..11).map(|index| (index, &valid)).collect::<Vec<_>>();
    assert!(rank_anchors(&reader, "aqua", &owners, &[1], 80, deadline()).is_err());
    assert!(
        rank_anchors(
            &reader,
            "aqua",
            &[(0, &valid), (0, &valid)],
            &[1],
            80,
            deadline()
        )
        .is_err()
    );
}

#[test]
fn mismatched_weights_overflow_and_unchanged_deadlines_fail() {
    let reader = Reader::new();
    let fixture = Fixture::new(&[("node", "aqua terra")]);
    let valid = fixture.validated();
    assert!(rank_anchors(&reader, "aqua terra", &[(0, &valid)], &[1], 80, deadline()).is_err());
    assert!(
        rank_anchors(
            &reader,
            "aqua terra",
            &[(0, &valid)],
            &[u64::MAX, 1],
            80,
            deadline()
        )
        .is_err()
    );
    assert_eq!(
        rank_anchors(
            &reader,
            "aqua",
            &[(0, &valid)],
            &[1],
            80,
            Instant::now() - Duration::from_secs(1)
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
    let limited = Reader {
        fail_after: Some(7),
        ..Reader::new()
    };
    assert_eq!(
        rank_anchors(&limited, "aqua", &[(0, &valid)], &[1], 80, deadline())
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
}

#[test]
fn inherited_query_term_cap_is_disclosed_and_requires_exact_capped_weights() {
    let reader = Reader::new();
    let fixture = Fixture::new(&[("included", "item000"), ("excluded", "item299")]);
    let valid = fixture.validated();
    let query = (0..300)
        .map(|index| format!("item{index:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    let out = rank_anchors(
        &reader,
        &query,
        &[(0, &valid)],
        &vec![1; 256],
        80,
        deadline(),
    )
    .unwrap();
    assert!(out.query_terms_capped);
    assert_eq!(out.anchors.len(), 1);
    assert_eq!(out.anchors[0].node_id, id("included"));
    assert!(
        rank_anchors(
            &reader,
            &query,
            &[(0, &valid)],
            &vec![1; 300],
            80,
            deadline()
        )
        .is_err()
    );
}

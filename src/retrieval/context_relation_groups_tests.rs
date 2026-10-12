//! Fictional content and metadata only. These fixtures exercise nomination,
//! not model quality, provider compatibility or final packing acceptance.
use super::*;
use crate::{
    catalog::{CatalogDiagnostic, DocumentRow, RecordRow, SnapshotVerification},
    retrieval::{
        context_types::{ContextScope, ContextTarget},
        selected_documents::relation_annotations::SourceRelationAnnotation,
        source_relations::{
            Edge, Envelope, Node, NodeKind, PreparationBinding, RelationKind, SpanBinding,
        },
        types::{ExcerptLabel, SearchHit},
    },
};
use rusqlite::{Connection, Row};
use std::{cell::Cell, time::Duration};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(20)
}

struct Reader {
    connection: Connection,
    documents: BTreeMap<VaultRelativePath, DocumentRow>,
    vault: RecordId,
    snapshot: ReadSnapshot,
    verification: SnapshotVerification,
    deny_reads: Cell<bool>,
}
impl QueryCatalog for Reader {
    fn connection(&self) -> &Connection {
        &self.connection
    }
    fn normalized_layout(&self) -> bool {
        true
    }
    fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }
    fn vault_id(&self) -> &RecordId {
        &self.vault
    }
    fn verification(&self) -> &SnapshotVerification {
        &self.verification
    }
    fn record(&self, _: &RecordId) -> Result<Option<RecordRow>> {
        panic!("no canonical record read expected")
    }
    fn document(&self, path: &VaultRelativePath) -> Result<Option<DocumentRow>> {
        assert!(
            !self.deny_reads.get(),
            "nomination must reuse admitted originals"
        );
        Ok(self.documents.get(path).cloned())
    }
    fn diagnostics(&self, _: &BTreeSet<VaultRelativePath>) -> Result<Vec<CatalogDiagnostic>> {
        panic!("no diagnostics read")
    }
    fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
        panic!("no fingerprint read")
    }
    fn query_scope(&self) -> &'static str {
        "fixture"
    }
    fn decode_document(&self, _: &Row<'_>, _: usize) -> Result<DocumentRow> {
        panic!("no document decode")
    }
}

struct Fixture {
    document: DocumentRow,
    envelope: Envelope,
    hit: SearchHit,
}
impl Fixture {
    fn pairs(owner: usize, roots: usize, shared: bool, distant: bool) -> Self {
        let hash = Blake3Hash::digest("fictional preparation");
        let preparation = PreparationBinding {
            recipe_id: id("fixture_recipe"),
            recipe_hash: hash.clone(),
            prompt_hash: hash.clone(),
            schema_hash: hash.clone(),
            model_identity_hash: hash.clone(),
            settings_hash: hash.clone(),
            retained_generation_id: id("fixture_generation"),
            retained_generation_hash: hash,
        };
        let mut text = String::from("Background seed.\n");
        if distant {
            text.push_str(&"Unrelated background.\n".repeat(400));
        }
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for root in 0..roots {
            let quote = format!("Rule {root}: apply only with the stated requirement.\n");
            let evidence = append_span(&mut text, &quote);
            nodes.push(Node {
                id: id(&format!("rule_{root:03}")),
                kind: NodeKind::Rule,
                scope: "aqua".into(),
                evidence,
            });
        }
        for child in 0..if shared { 1 } else { roots } {
            let quote =
                format!("Requirement {child}: written approval is required; do not assume it.\n");
            let evidence = append_span(&mut text, &quote);
            nodes.push(Node {
                id: id(&format!("requirement_{child:03}")),
                kind: NodeKind::Condition,
                scope: "prerequisite".into(),
                evidence,
            });
        }
        for root in 0..roots {
            let child = if shared { 0 } else { root };
            edges.push(Edge {
                from: nodes[root].id.clone(),
                to: nodes[roots + child].id.clone(),
                kind: RelationKind::Requires,
                evidence: nodes[roots + child].evidence.clone(),
            });
        }
        let source_id = id(&format!("source_{owner}"));
        let revision = id(&format!("revision_{owner}"));
        let hash = Blake3Hash::digest(&text);
        let path = VaultRelativePath::new(format!("sources/fixture_{owner}/content.txt")).unwrap();
        let document = DocumentRow {
            path: path.clone(),
            hash: hash.clone(),
            record_id: None,
            kind: None,
            title: "Fictional source".into(),
            aliases: vec![],
            headings: String::new(),
            tags: vec![],
            body: text.clone(),
            raw_text: text,
            source_id: Some(source_id.clone()),
            owner_revision: Some(revision.clone()),
            eligibility: Eligibility::Current,
            reasons: vec![],
        };
        let hit = SearchHit {
            locator: DocumentLocator {
                record: Some(RecordRef {
                    vault_id: id("fixture_vault"),
                    record_id: revision.clone(),
                    expected_kind: RecordKind::Revision,
                }),
                path,
                observed_hash: hash.clone(),
            },
            title: document.title.clone(),
            kind: None,
            authored_status: None,
            eligibility: Eligibility::Current,
            identity_eligibility: None,
            excerpt: SearchExcerpt {
                text: "Background seed.".into(),
                span: ByteSpan::new(0, 16).unwrap(),
                matched_spans: vec![],
                label: ExcerptLabel::CapturedSource,
                citation: None,
            },
            secondary_excerpts: vec![],
            declared_source_refs: vec![],
            reasons: vec![],
            rank_contributions: vec![],
            source_id: Some(source_id.clone()),
            owner_revision: Some(revision.clone()),
        };
        let envelope = Envelope {
            schema: source_relations::SCHEMA.into(),
            source: SourceBinding {
                source_id,
                source_revision: revision,
                raw_hash: hash,
            },
            preparation,
            nodes,
            edges,
        };
        Self {
            document,
            envelope,
            hit,
        }
    }
    fn rebind(&mut self) {
        self.document.hash = Blake3Hash::digest(&self.document.raw_text);
        self.document.body = self.document.raw_text.clone();
        self.hit.locator.observed_hash = self.document.hash.clone();
        self.envelope.source.raw_hash = self.document.hash.clone();
    }
}
fn append_span(text: &mut String, quote: &str) -> SpanBinding {
    let start = text.len() as u64;
    text.push_str(quote);
    SpanBinding {
        span: ByteSpan::new(start, text.len() as u64).unwrap(),
        quote_hash: Blake3Hash::digest(quote),
    }
}

struct Harness {
    reader: Reader,
    request: ContextRequest,
    hits: HitSet,
    owners: Vec<DocumentOwner>,
    signals: ContextSelectionSignals,
}
impl Harness {
    fn new(fixtures: Vec<Fixture>) -> Self {
        let connection = Connection::open_in_memory().unwrap();
        // Only document_owner's ordinary default/path filter lookup is needed.
        connection.execute_batch("CREATE TABLE documents(path TEXT PRIMARY KEY,record_id TEXT,source_id TEXT,owner_revision TEXT); CREATE TABLE records(id TEXT PRIMARY KEY);").unwrap();
        for fixture in &fixtures {
            connection
                .execute(
                    "INSERT INTO documents(path,source_id,owner_revision) VALUES (?1,?2,?3)",
                    rusqlite::params![
                        fixture.document.path.as_str(),
                        fixture.document.source_id.as_ref().map(RecordId::as_str),
                        fixture
                            .document
                            .owner_revision
                            .as_ref()
                            .map(RecordId::as_str)
                    ],
                )
                .unwrap();
        }
        let hash = Blake3Hash::digest("fixture snapshot");
        let snapshot = ReadSnapshot::canonical(1, hash.clone(), hash.clone());
        let reader = Reader {
            connection,
            documents: fixtures
                .iter()
                .map(|fixture| (fixture.document.path.clone(), fixture.document.clone()))
                .collect(),
            vault: id("fixture_vault"),
            snapshot: snapshot.clone(),
            verification: SnapshotVerification::IndexSnapshot,
            deny_reads: Cell::new(false),
        };
        let mut request = ContextRequest {
            scope: ContextScope::IndexedDocuments,
            target: ContextTarget::Documents,
            ..Default::default()
        };
        request.documents.limits.excerpt_bytes = 1024;
        let hits = HitSet {
            network_used: false,
            graph: None,
            hits: fixtures.iter().map(|fixture| fixture.hit.clone()).collect(),
            next_cursor: None,
            truncated: false,
            candidate_count: fixtures.len(),
            omitted_candidates: 0,
            snapshot,
            verification: SnapshotVerification::IndexSnapshot,
            dependency_fingerprint: hash,
            warnings: vec![],
        };
        let owners = hits
            .hits
            .iter()
            .map(|hit| context::document_owner(&reader, hit, &request).unwrap())
            .collect();
        reader.deny_reads.set(true);
        let signals = ContextSelectionSignals {
            relations: fixtures
                .into_iter()
                .map(|fixture| SourceRelationAnnotation {
                    owner: fixture.document.path,
                    envelope: fixture.envelope,
                })
                .collect(),
            relation_deadline: Some(deadline()),
            ..Default::default()
        };
        Self {
            reader,
            request,
            hits,
            owners,
            signals,
        }
    }
    fn run(
        &self,
        query: &str,
        weights: &[u64],
        omissions: &mut Vec<ContextOmission>,
    ) -> Result<GroupSelection> {
        nominate_groups(
            &self.reader,
            &self.request,
            &self.hits,
            &self.owners,
            query,
            &self.signals,
            weights,
            omissions,
        )
    }
}

#[test]
fn distant_scope_anchor_returns_complete_original_dependency_and_unknown_condition() {
    let mut fixture = Fixture::pairs(0, 1, false, true);
    fixture.envelope.nodes[0].scope = "aqua navigation-marker".into();
    let harness = Harness::new(vec![fixture]);
    let mut omissions = vec![];
    let result = harness.run("aqua", &[11], &mut omissions).unwrap();
    assert!(omissions.is_empty());
    assert_eq!(result.packets.len(), 1);
    let packet = &result.packets[0];
    assert_eq!(packet.passages.len(), 2);
    assert!(packet.passages[0].span.start() > 4000);
    assert!(
        packet.fallback.is_none() && packet.selection.is_none() && packet.unit_origin.is_none()
    );
    assert_eq!(packet.relation.as_ref().unwrap().covered_terms, [0]);
    assert_eq!(packet.relation.as_ref().unwrap().local_relevance, 11);
    assert_eq!(result.reserved_spans[&0].len(), 2);
    let original = &harness.owners[0].document.raw_text;
    for passage in &packet.passages {
        assert_eq!(passage.text, passage.span.slice(original).unwrap());
        assert!(!passage.text.contains("navigation-marker"));
        let CitationRef::Source(citation) = &passage.citations[0] else {
            panic!("original citation")
        };
        assert_eq!(citation.quote_hash, Blake3Hash::digest(&passage.text));
    }
    assert!(packet.passages[1].text.contains("do not assume it"));
    assert_eq!(result.node_visits, 2);
    assert_eq!(result.edge_visits, 1);
    assert_eq!(
        result.validation_hash_bytes,
        original.len() as u64 + packet.passages[0].span.len() + 2 * packet.passages[1].span.len()
    );
    assert_eq!(
        result.passage_hash_bytes,
        harness.hits.hits[0].excerpt.span.len()
            + packet
                .passages
                .iter()
                .map(|passage| passage.span.len())
                .sum::<u64>()
    );
}

#[test]
fn too_large_dependency_omits_whole_group_and_never_rescues_head() {
    let mut fixture = Fixture::pairs(0, 1, false, false);
    let evidence = append_span(&mut fixture.document.raw_text, &"x".repeat(1025));
    fixture.envelope.nodes[1].evidence = evidence.clone();
    fixture.envelope.edges[0].evidence = evidence;
    fixture.rebind();
    let harness = Harness::new(vec![fixture]);
    let mut omissions = vec![];
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert!(result.packets.is_empty() && result.reserved_spans.is_empty());
    assert_eq!(result.node_visits, 2);
    assert_eq!(omissions.len(), 1);
    assert!(omissions[0].reason.contains("SpanLimit"));
}

#[test]
fn five_passage_closure_is_rejected_before_reserving_any_owner_slots() {
    let mut fixture = Fixture::pairs(0, 4, false, false);
    // Only the first root is an anchor; it requires four distinct conditions.
    for node in &mut fixture.envelope.nodes[1..4] {
        node.scope = "unrelated".into();
    }
    fixture.envelope.edges = (0..4)
        .map(|child| Edge {
            from: fixture.envelope.nodes[0].id.clone(),
            to: fixture.envelope.nodes[4 + child].id.clone(),
            kind: RelationKind::Requires,
            evidence: fixture.envelope.nodes[4 + child].evidence.clone(),
        })
        .collect();
    let harness = Harness::new(vec![fixture]);
    let mut omissions = vec![];
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert!(result.packets.is_empty() && result.reserved_spans.is_empty());
    assert_eq!(omissions[0].reason, "source_relation_group_passage_cap");
}

#[test]
fn shared_ranges_count_once_but_disjoint_groups_stop_at_owner_32() {
    let harness = Harness::new(vec![Fixture::pairs(0, 20, true, false)]);
    let mut omissions = vec![];
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert_eq!(result.packets.len(), 20);
    assert_eq!(result.reserved_spans[&0].len(), 21);
    assert!(omissions.is_empty());

    let harness = Harness::new(vec![Fixture::pairs(0, 17, false, false)]);
    let mut omissions = vec![];
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert_eq!(result.packets.len(), 16);
    assert_eq!(result.reserved_spans[&0].len(), 32);
    assert_eq!(result.node_visits, 34); // rejected work remains charged
    assert_eq!(omissions[0].reason, "source_relation_group_owner_span_cap");
}

#[test]
fn discovery_80_does_not_shrink_retained_32_per_owner_pool() {
    let harness = Harness::new(
        (0..3)
            .map(|owner| Fixture::pairs(owner, 16, false, false))
            .collect(),
    );
    let mut omissions = vec![];
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert_eq!(result.packets.len(), 48);
    assert_eq!(
        result
            .reserved_spans
            .values()
            .map(BTreeSet::len)
            .sum::<usize>(),
        96
    );
    assert!(omissions.is_empty());
}

#[test]
fn zero_anchor_or_absent_preparation_reserves_nothing_for_ordinary_fallback() {
    let mut harness = Harness::new(vec![Fixture::pairs(0, 1, false, false)]);
    let mut omissions = vec![];
    let result = harness.run("absent", &[7], &mut omissions).unwrap();
    assert!(result.packets.is_empty() && result.reserved_spans.is_empty() && omissions.is_empty());
    harness.signals = ContextSelectionSignals::default();
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert!(result.packets.is_empty() && result.reserved_spans.is_empty());
    assert_eq!(result.validation_hash_bytes, 0);
    assert_eq!(result.passage_hash_bytes, 0);
}

#[test]
fn ineligible_owner_and_empty_seed_cannot_expand_scope() {
    let mut harness = Harness::new(vec![Fixture::pairs(0, 1, false, false)]);
    harness.owners[0].allowed = false;
    let mut omissions = vec![];
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert!(result.packets.is_empty());
    assert_eq!(result.validation_hash_bytes, 0);
    assert_eq!(result.passage_hash_bytes, 0);
    assert_eq!(omissions[0].reason, "source_relation_owner_ineligible");
    harness.owners[0].allowed = true;
    harness.hits.hits[0].excerpt.span = ByteSpan::new(0, 0).unwrap();
    omissions.clear();
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert!(result.packets.is_empty());
    assert_eq!(result.validation_hash_bytes, 0);
    assert_eq!(omissions[0].reason, "source_relation_seed_ineligible");
}

#[test]
fn expired_or_missing_deadline_and_stale_binding_fail_closed() {
    let mut harness = Harness::new(vec![Fixture::pairs(0, 1, false, false)]);
    harness.signals.relation_deadline = Some(Instant::now());
    assert_eq!(
        harness.run("aqua", &[7], &mut vec![]).err().unwrap().code,
        ErrorCode::BudgetExceeded
    );
    harness.signals.relation_deadline = None;
    assert!(harness.run("aqua", &[7], &mut vec![]).is_err());
    harness.signals.relation_deadline = Some(deadline());
    harness.signals.relations[0].envelope.source.raw_hash =
        Blake3Hash::digest("original.bin is different");
    let error = harness.run("aqua", &[7], &mut vec![]).err().unwrap();
    assert_eq!(error.code, ErrorCode::ExtractionInvalid);
    assert_eq!(
        error.details["source_relation_group_validation_hash_bytes"],
        0
    );
}

#[test]
fn cycles_keep_complete_membership_and_union_density_uses_original_overlap() {
    let mut fixture = Fixture::pairs(0, 1, false, false);
    let first = fixture.envelope.nodes[0].evidence.span;
    let overlap = ByteSpan::new(first.start() + 5, first.end()).unwrap();
    fixture.envelope.nodes[1].evidence = SpanBinding {
        span: overlap,
        quote_hash: Blake3Hash::digest(overlap.slice(&fixture.document.raw_text).unwrap()),
    };
    fixture.envelope.edges[0].evidence = fixture.envelope.nodes[0].evidence.clone();
    fixture.envelope.edges.push(Edge {
        from: fixture.envelope.nodes[1].id.clone(),
        to: fixture.envelope.nodes[0].id.clone(),
        kind: RelationKind::Qualifies,
        evidence: fixture.envelope.nodes[1].evidence.clone(),
    });
    let harness = Harness::new(vec![fixture]);
    let result = harness.run("aqua", &[7], &mut vec![]).unwrap();
    assert_eq!(result.packets[0].passages.len(), 2);
    assert_eq!(
        result.packets[0].relation.as_ref().unwrap().original_bytes,
        first.len()
    );
    assert_eq!(result.node_visits, 2);
    assert_eq!(result.edge_visits, 2);
}

#[test]
fn over_owner_bound_and_inconsistent_weights_fail_without_widening_limits() {
    let harness = Harness::new(
        (0..11)
            .map(|owner| Fixture::pairs(owner, 1, false, false))
            .collect(),
    );
    assert!(harness.run("aqua", &[7], &mut vec![]).is_err());
    let harness = Harness::new(vec![Fixture::pairs(0, 1, false, false)]);
    assert!(harness.run("aqua", &[], &mut vec![]).is_err());
}

#[test]
fn depth_cap_rejects_complete_group_even_when_all_nodes_share_one_span() {
    let mut fixture = Fixture::pairs(0, 1, false, false);
    let evidence = fixture.envelope.nodes[0].evidence.clone();
    fixture.envelope.nodes = (0..35)
        .map(|index| Node {
            id: id(&format!("chain_{index:03}")),
            kind: NodeKind::Condition,
            scope: if index == 0 { "aqua" } else { "prerequisite" }.into(),
            evidence: evidence.clone(),
        })
        .collect();
    fixture.envelope.edges = (0..34)
        .map(|index| Edge {
            from: fixture.envelope.nodes[index].id.clone(),
            to: fixture.envelope.nodes[index + 1].id.clone(),
            kind: RelationKind::Qualifies,
            evidence: evidence.clone(),
        })
        .collect();
    let harness = Harness::new(vec![fixture]);
    let mut omissions = vec![];
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert!(result.packets.is_empty() && result.reserved_spans.is_empty());
    assert_eq!(result.node_visits, 33);
    assert_eq!(result.edge_visits, 32);
    assert!(omissions[0].reason.contains("DepthLimit"));
}

#[test]
fn traversal_allowance_is_shared_across_anchors_including_rejected_work() {
    let mut fixture = Fixture::pairs(0, 80, true, false);
    let evidence = fixture.envelope.nodes[0].evidence.clone();
    fixture.envelope.nodes = (0..144)
        .map(|index| Node {
            id: id(&format!("node_{index:03}")),
            kind: NodeKind::Condition,
            scope: if index < 80 { "aqua" } else { "prerequisite" }.into(),
            evidence: evidence.clone(),
        })
        .collect();
    fixture.envelope.edges = (0..80)
        .flat_map(|root| (80..144).map(move |child| (root, child)))
        .map(|(root, child)| Edge {
            from: fixture.envelope.nodes[root].id.clone(),
            to: fixture.envelope.nodes[child].id.clone(),
            kind: RelationKind::Requires,
            evidence: evidence.clone(),
        })
        .collect();
    let harness = Harness::new(vec![fixture]);
    let mut omissions = vec![];
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert_eq!(result.node_visits, source_relations::MAX_NODES);
    assert_eq!(result.edge_visits, 64 * 64);
    assert_eq!(result.packets.len(), 63);
    assert_eq!(result.reserved_spans[&0].len(), 1);
    assert_eq!(omissions.len(), 17);
    assert!(omissions[0].reason.contains("NodeWorkLimit"));
    assert!(
        omissions[1..]
            .iter()
            .all(|omission| omission.reason == "source_relation_group_node_work_cap")
    );
}

#[test]
fn bounded_dto_serializer_refuses_oversize_without_returning_partial_annotations() {
    let mut fixture = Fixture::pairs(0, 1, false, false);
    fixture.envelope.nodes[0].scope = "x".repeat(source_relations::MAX_ANNOTATION_BYTES + 1);
    let harness = Harness::new(vec![fixture]);
    let error = harness.run("aqua", &[7], &mut vec![]).err().unwrap();
    assert_eq!(error.code, ErrorCode::ExtractionInvalid);
    assert_eq!(
        error.details["source_relation_group_validation_hash_bytes"],
        0
    );
}

#[test]
fn edge_work_cap_is_global_and_never_leaks_a_partial_cyclic_group() {
    let mut fixture = Fixture::pairs(0, 80, true, false);
    let evidence = fixture.envelope.nodes[0].evidence.clone();
    fixture.envelope.nodes = (0..96)
        .map(|index| Node {
            id: id(&format!("node_{index:03}")),
            kind: NodeKind::Condition,
            scope: if index < 80 { "aqua" } else { "prerequisite" }.into(),
            evidence: evidence.clone(),
        })
        .collect();
    let pairs = (0..80)
        .flat_map(|root| (80..96).map(move |child| (root, child)))
        .chain((80..96).flat_map(|from| {
            (80..96)
                .filter(move |to| *to != from)
                .map(move |to| (from, to))
        }));
    fixture.envelope.edges = pairs
        .map(|(from, to)| Edge {
            from: fixture.envelope.nodes[from].id.clone(),
            to: fixture.envelope.nodes[to].id.clone(),
            kind: RelationKind::Qualifies,
            evidence: evidence.clone(),
        })
        .collect();
    let harness = Harness::new(vec![fixture]);
    let mut omissions = vec![];
    let result = harness.run("aqua", &[7], &mut omissions).unwrap();
    assert_eq!(result.edge_visits, source_relations::MAX_EDGES);
    assert_eq!(result.packets.len(), 32);
    assert_eq!(result.node_visits, 32 * 17 + 48);
    assert_eq!(result.reserved_spans[&0].len(), 1);
    assert_eq!(omissions.len(), 48);
    assert!(
        omissions
            .iter()
            .all(|omission| omission.reason.contains("EdgeWorkLimit"))
    );
}

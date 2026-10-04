use super::*;
use crate::{
    catalog::{
        Catalog, CatalogDiagnostic, DocumentRow, GraphRow, IdentityClaimRow, RetrievalSink,
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        query_types::QueryReadLimits,
        selector,
        source_refresh::IndexedRefreshSession,
    },
    changes::{ChangeEngine, ChangeStatus},
    sources::{
        CaptureRequest, ExtractionInput, SourceOrigin, SourceRefreshLimits, SourceStore,
        evidence::exact_quote_body,
    },
    vault::{VaultRoot, WriterPermit},
};
use serde_json::json;
use std::fs;
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn path(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn note(kind: &str, id: &str, extra: serde_json::Value, body: &[u8]) -> Vec<u8> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(id)),
        ("wiki_kind".into(), json!(kind)),
        ("title".into(), json!(id)),
    ]);
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    CanonicalRecord::new(fields.clone()).unwrap();
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields {
        bytes.extend(format!("{key}: {value}\n").as_bytes());
    }
    bytes.extend(b"---\n");
    bytes.extend(body);
    bytes
}
#[derive(Default)]
struct Sink {
    documents: Vec<DocumentRow>,
    graph: Vec<GraphRow>,
    links: Vec<LinkRow>,
}
impl RetrievalSink for Sink {
    fn identity_claim(&mut self, _: IdentityClaimRow) -> Result<()> {
        Ok(())
    }
    fn document(&mut self, row: DocumentRow) -> Result<()> {
        self.documents.push(row);
        Ok(())
    }
    fn graph(&mut self, row: GraphRow) -> Result<()> {
        self.graph.push(row);
        Ok(())
    }
    fn link(&mut self, row: LinkRow) -> Result<()> {
        self.links.push(row);
        Ok(())
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    writer: WriterPermit,
    catalog: Catalog,
    source: RecordId,
    first: RecordId,
}
impl Fixture {
    fn request(bytes: &[u8]) -> CaptureRequest {
        CaptureRequest {
            title: "Original capture title".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.txt".into(),
            original: bytes.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        }
    }
    fn seed(fs_handle: &VaultFs, draft: ChangeDraft) {
        for op in draft.operations {
            let target = fs_handle.root().path().join(op.target.as_str());
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, op.proposed.unwrap()).unwrap();
        }
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let target = self.fs.root().path().join(name);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    fn new(fanout: bool) -> Self {
        Self::with_setup(fanout, |_| {})
    }
    fn with_setup(fanout: bool, setup: impl FnOnce(&Self)) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            note("vault", "vault_projector", json!({}), b"Fixture"),
        )
        .unwrap();
        let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
        let store = SourceStore::new(fs_handle.clone());
        let first = store
            .plan_capture(Self::request(b"first capture quote"))
            .unwrap();
        Self::seed(&fs_handle, first.draft.unwrap());
        let fixture = Self {
            _temp: temp,
            fs: fs_handle.clone(),
            writer,
            catalog: Catalog::new(fs_handle, id("vault_projector")),
            source: first.source_id,
            first: first.revision_id,
        };
        fixture.write(
            "navigation.md",
            &note(
                "page",
                "page_navigation",
                json!({"wiki_status":"reviewed"}),
                b"See [[revision]].",
            ),
        );
        if fanout {
            fixture.write(
                "entity.md",
                &note(
                    "entity",
                    "entity_fixture",
                    json!({"wiki_status":"active","wiki_entity_type":"component"}),
                    b"Entity description",
                ),
            );
            fixture.write("claim.md",&note("assertion","assertion_keep",json!({"wiki_status":"accepted","wiki_subject_id":"entity_fixture","wiki_object_id":"entity_fixture","wiki_predicate":"uses","wiki_evidence":["[[revision]]"]}),b"Claim"));
            fixture.write("opposite.md",&note("assertion","assertion_opposite",json!({"wiki_status":"accepted","wiki_subject_id":"entity_fixture","wiki_object_id":"entity_fixture","wiki_predicate":"uses","wiki_negated":true}),b"Opposite"));
            fixture.write(
                "supported.md",
                &note(
                    "page",
                    "page_supported",
                    json!({"wiki_status":"reviewed","wiki_depends_on_ids":["assertion_opposite"]}),
                    b"Derived page",
                ),
            );
            fixture.write(
                "derived-entity.md",
                &note(
                    "entity",
                    "entity_derived",
                    json!({"wiki_status":"active","wiki_entity_type":"component","wiki_depends_on_ids":["assertion_opposite"]}),
                    b"Description supported by the opposite assertion",
                ),
            );
            let other = store
                .plan_capture(Self::request(b"other capture quote"))
                .unwrap();
            Self::seed(&fixture.fs, other.draft.unwrap());
            for (name, claim, source, revision, quote) in [
                (
                    "evidence_old",
                    "assertion_keep",
                    &fixture.source,
                    &fixture.first,
                    b"first capture quote".as_slice(),
                ),
                (
                    "evidence_opposite",
                    "assertion_opposite",
                    &fixture.source,
                    &fixture.first,
                    b"first capture quote".as_slice(),
                ),
                (
                    "evidence_other",
                    "assertion_keep",
                    &other.source_id,
                    &other.revision_id,
                    b"other capture quote".as_slice(),
                ),
            ] {
                fixture.write(&format!("{name}.md"),&note("evidence",name,json!({"wiki_status":"active","wiki_assertion_id":claim,"wiki_source_id":source,"wiki_source_revision":revision,"wiki_stance":"supports","wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":quote.len(),"wiki_quote_hash":Blake3Hash::digest(quote)}),&exact_quote_body(quote,"\n","Fixture").unwrap()));
            }
            let fingerprint = Blake3Hash::digest(b"fixture packet");
            let packet = RecordId::packet(&fingerprint);
            fixture.write("packet.md",&note("extraction_packet",packet.as_str(),json!({"wiki_source_id":fixture.source,"wiki_source_revision":fixture.first,"wiki_packet_fingerprint":fingerprint,"wiki_output_schema":"fixture","wiki_created_at":"2026-09-28T00:00:00Z"}),b"Retained packet fixture"));
        }
        setup(&fixture);
        let identity = BuildIdentity {
            selection: CatalogSelection::new(id("vault_projector"), 1).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&fixture.fs, &fixture.writer, &identity.selection).unwrap();
        let mut builder = NormalizedBuilder::begin(
            &fixture.fs,
            &fixture.writer,
            identity,
            BuildLimits::default(),
        )
        .unwrap();
        let input = scan::scan_input(&fixture.fs, &id("vault_projector")).unwrap();
        let projection =
            scan::project_normalized_with_sink(&fixture.fs, &input, false, &mut builder).unwrap();
        let completed = builder.finish_normalized(&projection).unwrap();
        selector::publish(
            &fixture.fs,
            &fixture.writer,
            &completed.identity.selection,
            Duration::ZERO,
        )
        .unwrap();
        fixture
    }
    fn plan(
        &self,
        request: CaptureRequest,
        title: Option<&str>,
    ) -> (QuerySnapshot, IndexedSourceRefreshPlan) {
        let reader = self
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let plan = SourceStore::new(self.fs.clone())
            .plan_refresh_indexed(
                &reader,
                &self.source,
                request,
                title,
                &SourceRefreshLimits::default(),
            )
            .unwrap();
        (reader, plan)
    }
    fn apply(&self, request: CaptureRequest, title: Option<&str>) -> Option<CatalogDelta> {
        let (reader, plan) = self.plan(request, title);
        let projected =
            project_refresh(&self.fs, &reader, plan, &RefreshProjectionLimits::default())
                .unwrap()?;
        let retained = projected.parts.delta.clone();
        let engine = ChangeEngine::new(self.fs.clone()).unwrap();
        let mut session =
            IndexedRefreshSession::prepare_projected(&self.catalog, &self.writer, projected)
                .unwrap();
        assert_eq!(
            engine
                .apply_indexed_refresh(&self.writer, &mut session)
                .unwrap()
                .status,
            ChangeStatus::Committed
        );
        Some(retained)
    }
    fn oracle(&self) {
        let input = scan::scan_input(&self.fs, &id("vault_projector")).unwrap();
        let mut sink = Sink::default();
        let projection =
            scan::project_normalized_with_sink(&self.fs, &input, false, &mut sink).unwrap();
        let reader = self
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        for (id, expected) in &projection.validation.records {
            assert_eq!(
                reader.record(id).unwrap().as_ref(),
                Some(expected),
                "record {id}"
            );
            assert_eq!(
                reader.eligibility_fact(id).unwrap().as_ref(),
                projection.facts.records.get(id),
                "fact {id}"
            );
        }
        for doc in &sink.documents {
            assert_eq!(
                reader.document(&doc.path).unwrap().as_ref(),
                Some(doc),
                "document {}",
                doc.path
            );
        }
        let actual_graph: Vec<GraphRow> = reader.connection().prepare(
            "SELECT target_id,target_kind,name,aliases_json,endpoints,predicate,qualifiers,description FROM graph_rows ORDER BY target_id"
        ).unwrap().query_map([], |r| Ok(GraphRow {
            target_id:id(&r.get::<_,String>(0)?),
            target_kind:r.get::<_,String>(1)?.parse().unwrap(),
            name:r.get(2)?, aliases:serde_json::from_str(&r.get::<_,String>(3)?).unwrap(),
            endpoints:r.get(4)?, predicate:r.get(5)?, qualifiers:r.get(6)?, description:r.get(7)?,
        })).unwrap().collect::<std::result::Result<_,_>>().unwrap();
        sink.graph.sort_by(|a, b| a.target_id.cmp(&b.target_id));
        assert_eq!(actual_graph, sink.graph);
        let mut actual_links:Vec<LinkRow>=reader.connection().prepare("SELECT from_path,byte_start,target_id,target_path,resolution FROM links ORDER BY from_path,byte_start").unwrap().query_map([],|r|Ok(LinkRow{from_path:path(&r.get::<_,String>(0)?),byte_start:u64::try_from(r.get::<_,i64>(1)?).unwrap(),target_id:r.get::<_,Option<String>>(2)?.map(|v|id(&v)),target_path:r.get::<_,Option<String>>(3)?.map(|v|path(&v)),resolution:r.get(4)?})).unwrap().collect::<std::result::Result<_,_>>().unwrap();
        sink.links
            .sort_by(|a, b| (&a.from_path, a.byte_start).cmp(&(&b.from_path, b.byte_start)));
        actual_links
            .sort_by(|a, b| (&a.from_path, a.byte_start).cmp(&(&b.from_path, b.byte_start)));
        assert_eq!(actual_links, sink.links);
        let paths = projection
            .validation
            .dependencies
            .iter()
            .filter(|d| canonical_path(&d.path))
            .map(|d| d.path.clone())
            .collect();
        let actual: Vec<CatalogDiagnostic> = reader.diagnostics(&paths).unwrap();
        assert_eq!(actual, projection.validation.diagnostics);
        for edge in &projection.facts.edges {
            assert!(
                reader
                    .outgoing_edges(&edge.owner_id, std::slice::from_ref(&edge.role))
                    .unwrap()
                    .contains(edge)
            );
        }
    }
}
#[test]
fn source_projector_title_new_history_empty_and_noop_match_complete_oracle() {
    let fixture = Fixture::new(false);
    assert!(
        fixture
            .apply(Fixture::request(b"first capture quote"), None)
            .is_none()
    );
    let title = fixture
        .apply(
            Fixture::request(b"first capture quote"),
            Some("Changed source title"),
        )
        .unwrap();
    assert_eq!(title.records.len(), 1);
    fixture.oracle();
    let new = fixture
        .apply(Fixture::request(b"second capture quote"), None)
        .unwrap();
    assert_eq!(new.revisions.len(), 1);
    fixture.oracle();
    let reuse = fixture
        .apply(Fixture::request(b"first capture quote"), None)
        .unwrap();
    assert!(reuse.revisions.is_empty());
    fixture.oracle();
    fixture.apply(Fixture::request(b""), None).unwrap();
    fixture.oracle();
    let mut unsupported = Fixture::request(b"binary original");
    unsupported.extraction = ExtractionInput::Unsupported {
        extractor: "fixture".into(),
        fingerprint: Blake3Hash::digest(b"unsupported-v1"),
    };
    fixture.apply(unsupported, None).unwrap();
    fixture.oracle();
}
#[test]
fn source_projector_preserves_alternative_support_opposition_operational_and_navigation_fanout() {
    let fixture = Fixture::new(true);
    let before = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert!(
        before
            .record(&id("assertion_keep"))
            .unwrap()
            .unwrap()
            .disputed
    );
    let delta = fixture
        .apply(Fixture::request(b"new capture with no evidence"), None)
        .unwrap();
    assert!(!delta.diagnostics.is_empty());
    fixture.oracle();
    assert!(
        before
            .record(&id("assertion_keep"))
            .unwrap()
            .unwrap()
            .disputed
    );
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let kept = reader.record(&id("assertion_keep")).unwrap().unwrap();
    assert_eq!(kept.eligibility, Eligibility::Current);
    assert!(!kept.disputed);
    assert_eq!(
        reader
            .record(&id("assertion_opposite"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Stale
    );
    assert_eq!(
        reader
            .record(&id("page_supported"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Stale
    );
    let entity = reader.record(&id("entity_derived")).unwrap().unwrap();
    assert_eq!(entity.identity_eligibility, Some(Eligibility::Current));
    assert_eq!(entity.description_eligibility, Some(Eligibility::Stale));
    let packet = RecordId::packet(&Blake3Hash::digest(b"fixture packet"));
    assert_eq!(
        reader.record(&packet).unwrap().unwrap().eligibility,
        Eligibility::Historical
    );
    fixture
        .apply(Fixture::request(b"first capture quote"), None)
        .unwrap();
    fixture.oracle();
}
#[test]
fn source_projector_scope_and_budget_refuse_before_canonical_mutation() {
    let fixture = Fixture::new(false);
    let source = fixture
        .fs
        .root()
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let bytes = fs::read(&source).unwrap();
    let (reader, mut plan) = fixture.plan(Fixture::request(b"changed"), None);
    let op = plan
        .plan
        .draft
        .as_mut()
        .unwrap()
        .operations
        .iter_mut()
        .find(|op| op.target.as_str().ends_with("source.md"))
        .unwrap();
    op.proposed.as_mut().unwrap().extend(b"Changed source body");
    assert!(
        project_refresh(
            &fixture.fs,
            &reader,
            plan,
            &RefreshProjectionLimits::default()
        )
        .is_err()
    );
    assert_eq!(fs::read(&source).unwrap(), bytes);
    let (reader, plan) = fixture.plan(Fixture::request(b"changed"), None);
    let mut limits = RefreshProjectionLimits::default();
    limits.max_rows = 1;
    assert!(project_refresh(&fixture.fs, &reader, plan, &limits).is_err());
    assert_eq!(fs::read(&source).unwrap(), bytes);
}

#[test]
fn admitted_new_revision_recovers_on_both_sides_of_sql_commit() {
    use crate::catalog::{CatalogOptions, PublicationCheckpoint, PublicationFault};
    use crate::changes::indexed_refresh::IndexedRefreshPhase;
    struct FailAt(PublicationCheckpoint);
    impl PublicationFault for FailAt {
        fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
            if checkpoint == self.0 {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "injected production projector publication interruption",
                ));
            }
            Ok(())
        }
    }
    for checkpoint in [
        PublicationCheckpoint::AfterPointer,
        PublicationCheckpoint::AfterCommit,
    ] {
        let fixture = Fixture::new(true);
        let (held_reader, plan) = fixture.plan(Fixture::request(b"new captured revision"), None);
        let old_source = held_reader.record(&fixture.source).unwrap().unwrap();
        let projected = project_refresh(
            &fixture.fs,
            &held_reader,
            plan,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap();
        let faulty = Catalog::with_options(
            fixture.fs.clone(),
            id("vault_projector"),
            CatalogOptions {
                busy_timeout_ms: 1000,
                fault: Some(std::sync::Arc::new(FailAt(checkpoint))),
            },
        );
        let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
        let mut session =
            IndexedRefreshSession::prepare_projected(&faulty, &fixture.writer, projected).unwrap();
        let proof = session.proof().clone();
        assert_eq!(
            engine
                .apply_indexed_refresh(&fixture.writer, &mut session)
                .unwrap_err()
                .code,
            ErrorCode::RecoveryRequired,
        );
        assert_eq!(
            session.phase(),
            if checkpoint == PublicationCheckpoint::AfterCommit {
                IndexedRefreshPhase::AlreadyPublished
            } else {
                IndexedRefreshPhase::AtBase
            }
        );
        assert_eq!(
            held_reader.record(&fixture.source).unwrap(),
            Some(old_source)
        );
        drop(session);
        let source_path = fixture
            .fs
            .root()
            .path()
            .join(format!("sources/{}/source.md", fixture.source));
        let bytes = fs::read(&source_path).unwrap();
        let modified = fs::metadata(&source_path).unwrap().modified().unwrap();
        let mut resumed =
            IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof.clone())
                .unwrap();
        let report = engine
            .apply_indexed_refresh(&fixture.writer, &mut resumed)
            .unwrap();
        assert_eq!(report.status, ChangeStatus::Committed);
        assert_eq!(report.snapshot, Some(proof.intended.clone()));
        assert_eq!(fs::read(&source_path).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&source_path).unwrap().modified().unwrap(),
            modified
        );
        drop(resumed);
        fixture.oracle();
        assert_eq!(
            engine
                .indexed_refresh_terminal_report(&fixture.writer, &proof.change)
                .unwrap(),
            Some(report),
        );
    }
}

#[test]
fn generic_and_exact_navigation_refresh_work_is_independent_of_candidate_population() {
    let mut selected_work = Vec::new();
    for population in [16, 4100] {
        let fixture = Fixture::with_setup(false, |fixture| {
            for index in 0..population {
                fixture.write(
                    &format!("noise/{index}/revision.md"),
                    &note(
                        "page",
                        &format!("page_noise_{index}"),
                        json!({"wiki_status":"reviewed"}),
                        b"Unrelated navigation candidate",
                    ),
                );
            }
            fixture.write(
                "exact.md",
                &note(
                    "page",
                    "page_exact",
                    json!({"wiki_status":"reviewed"}),
                    b"See [[noise/0/revision#Details]].",
                ),
            );
        });
        let (reader, plan) = fixture.plan(Fixture::request(b"updated capture"), None);
        let before = reader.usage();
        let projected = project_refresh(
            &fixture.fs,
            &reader,
            plan,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap();
        selected_work.push(reader.usage().rows - before.rows);
        let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
        let mut session =
            IndexedRefreshSession::prepare_projected(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        engine
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .unwrap();
        drop(session);
        // Full oracle is deliberately test-only. The update above is bounded.
        let input = scan::scan_input(&fixture.fs, &id("vault_projector")).unwrap();
        let mut sink = Sink::default();
        let oracle =
            scan::project_normalized_with_sink(&fixture.fs, &input, false, &mut sink).unwrap();
        let after = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        for name in ["navigation.md", "exact.md"] {
            let expected = sink
                .links
                .iter()
                .find(|row| row.from_path.as_str() == name)
                .unwrap();
            let actual: (Option<String>, Option<String>, String) = after
                .connection()
                .query_row(
                    "SELECT target_id,target_path,resolution FROM links WHERE from_path=?1",
                    [name],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                actual,
                (
                    expected.target_id.as_ref().map(|id| id.as_str().to_owned()),
                    expected
                        .target_path
                        .as_ref()
                        .map(|path| path.as_str().to_owned()),
                    expected.resolution.clone(),
                )
            );
        }
        assert_eq!(
            after.record(&fixture.source).unwrap().as_ref(),
            oracle.validation.records.get(&fixture.source)
        );
        assert_eq!(
            after
                .connection()
                .query_row(
                    "SELECT resolution FROM links WHERE from_path='navigation.md'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "Ambiguous"
        );
    }
    assert_eq!(
        selected_work[0], selected_work[1],
        "unrelated matching candidates must not increase update work"
    );
}

#[test]
fn forged_capture_cannot_adopt_a_dangling_reference_or_reserved_exact_path() {
    for reserve_id in [true, false] {
        let future = RecordId::generate(RecordKind::Revision).unwrap();
        let fixture = Fixture::with_setup(true, |fixture| {
            if reserve_id {
                fixture.write(
                    "future-evidence.md",
                    &note(
                        "evidence",
                        "evidence_future",
                        json!({"wiki_status":"active","wiki_assertion_id":"assertion_keep",
                        "wiki_source_id":fixture.source,"wiki_source_revision":future,
                        "wiki_stance":"supports","wiki_locator_kind":"utf8-bytes",
                        "wiki_span_start":0,"wiki_span_end":13,
                        "wiki_quote_hash":Blake3Hash::digest(b"future bytes!")}),
                        &exact_quote_body(b"future bytes!", "\n", "Future").unwrap(),
                    ),
                );
            } else {
                fixture.write(
                    "reserved-path.md",
                    &note(
                        "page",
                        "page_reserved_path",
                        json!({"wiki_status":"reviewed"}),
                        format!("[[sources/{}/revisions/{future}/revision]]", fixture.source)
                            .as_bytes(),
                    ),
                );
            }
        });
        let (reader, mut plan) = fixture.plan(Fixture::request(b"future bytes!"), None);
        let source_path = fixture
            .fs
            .root()
            .path()
            .join(format!("sources/{}/source.md", fixture.source));
        let before = fs::read(&source_path).unwrap();
        let old_id = plan.plan.revision_id.clone();
        plan.plan.revision_id = future.clone();
        let draft = plan.plan.draft.as_mut().unwrap();
        draft
            .allocated_ids
            .insert("revision".into(), future.clone());
        for op in &mut draft.operations {
            op.target = path(&op.target.as_str().replace(old_id.as_str(), future.as_str()));
            for dependency in &mut op.apply_after {
                *dependency = path(
                    &dependency
                        .as_str()
                        .replace(old_id.as_str(), future.as_str()),
                );
            }
            let proposed = op.proposed.as_mut().unwrap();
            *proposed = std::str::from_utf8(proposed)
                .unwrap()
                .replace(old_id.as_str(), future.as_str())
                .into_bytes();
        }
        let error = project_refresh(
            &fixture.fs,
            &reader,
            plan,
            &RefreshProjectionLimits::default(),
        )
        .err()
        .unwrap();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert!(error.message.contains("referenced"), "{error:?}");
        assert_eq!(fs::read(&source_path).unwrap(), before);
        let unchanged = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert_eq!(
            QueryCatalog::snapshot(&unchanged),
            QueryCatalog::snapshot(&reader)
        );
        drop(unchanged);
        drop(reader);
        // A normal capture allocates a different identity and completes; no user
        // reconciliation or full-audit fallback is needed for the dangling link.
        let delta = fixture
            .apply(Fixture::request(b"future bytes!"), None)
            .unwrap();
        assert!(delta.records.iter().all(|row| row.record.id() != &future));
        fixture.oracle();
    }
}

#[test]
fn generated_revision_admission_rejects_injected_graph_metadata_and_body() {
    let fixture = Fixture::new(true);
    for injection in [
        "aliases: [\"injected\"]\n",
        "wiki_depends_on_ids: [\"assertion_keep\"]\n",
        "body",
    ] {
        let (reader, mut plan) = fixture.plan(Fixture::request(b"new capture"), None);
        let op = plan
            .plan
            .draft
            .as_mut()
            .unwrap()
            .operations
            .iter_mut()
            .find(|op| op.target.as_str().ends_with("/revision.md"))
            .unwrap();
        let bytes = op.proposed.as_mut().unwrap();
        if injection == "body" {
            bytes.extend(b"Injected body");
        } else {
            bytes.splice(4..4, injection.bytes());
        }
        assert!(
            parse_note(bytes).canonical.is_some(),
            "fixture must reach generated-shape check"
        );
        let error = project_refresh(
            &fixture.fs,
            &reader,
            plan,
            &RefreshProjectionLimits::default(),
        )
        .err()
        .unwrap();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert!(
            error.message.contains("generated capture metadata"),
            "{error:?}"
        );
    }
}

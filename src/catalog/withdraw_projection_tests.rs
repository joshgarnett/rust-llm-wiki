use super::*;
use crate::{
    catalog::{
        Catalog, CatalogDiagnostic, CatalogOptions, DocumentRow, GraphRow, IdentityClaimRow,
        LinkRow, PublicationCheckpoint, PublicationFault, RetrievalSink,
        eligibility_facts::EligibilityEdge,
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        policy_facts::PolicyRow,
        query_types::QueryReadLimits,
        selector,
        source_refresh::IndexedRefreshSession,
    },
    changes::{ChangeEngine, ChangeStatus, indexed_refresh::IndexedRefreshPhase},
    domain::CanonicalRecord,
    graph::{generation_cache::GenerationOutput, packet::render_fence},
    jobs::AttemptRef,
    sources::{
        CaptureRequest, ExtractionInput, SourceOrigin, SourceStore, evidence::exact_quote_body,
        revision::canonical_path,
    },
    vault::{VaultRoot, WriterPermit},
};
use serde_json::{Value, json};
use std::{fs, time::Duration};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn note(kind: &str, name: &str, extra: Value, body: &[u8]) -> Vec<u8> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(name)),
        ("wiki_kind".into(), json!(kind)),
        ("title".into(), json!(name)),
    ]);
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
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
    revisions: Vec<RecordId>,
    other_source: RecordId,
    packets: Vec<RecordId>,
}
impl Fixture {
    fn request(bytes: &[u8]) -> CaptureRequest {
        CaptureRequest {
            title: "Retained source title".into(),
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
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            note("vault", "vault_withdraw", json!({}), b"Withdrawal fixture"),
        )
        .unwrap();
        let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
        let store = SourceStore::new(fs_handle.clone());
        let first = store
            .plan_capture(Self::request(b"old immutable quote"))
            .unwrap();
        Self::seed(&fs_handle, first.draft.unwrap());
        let second = store
            .plan_refresh(&first.source_id, Self::request(b"new immutable quote"))
            .unwrap();
        Self::seed(&fs_handle, second.draft.unwrap());
        let other = store
            .plan_capture(Self::request(b"other immutable quote"))
            .unwrap();
        Self::seed(&fs_handle, other.draft.unwrap());
        let mut fixture = Self {
            _temp: temp,
            fs: fs_handle.clone(),
            writer,
            catalog: Catalog::new(fs_handle, id("vault_withdraw")),
            source: first.source_id,
            revisions: vec![first.revision_id, second.revision_id],
            other_source: other.source_id,
            packets: vec![],
        };
        fixture.write(
            "entity.md",
            &note(
                "entity",
                "entity_fixture",
                json!({"wiki_status":"active","wiki_entity_type":"component"}),
                b"Entity",
            ),
        );
        fixture.write("claim.md", &note("assertion", "assertion_keep", json!({"wiki_status":"accepted","wiki_subject_id":"entity_fixture","wiki_object_id":"entity_fixture","wiki_predicate":"uses"}), b"Claim"));
        fixture.write("opposite.md", &note("assertion", "assertion_opposite", json!({"wiki_status":"accepted","wiki_subject_id":"entity_fixture","wiki_object_id":"entity_fixture","wiki_predicate":"uses","wiki_negated":true}), b"Opposition"));
        fixture.write(
            "derived.md",
            &note(
                "page",
                "page_derived",
                json!({"wiki_status":"reviewed","wiki_depends_on_ids":["assertion_opposite"]}),
                b"Derived",
            ),
        );
        fixture.write(
            "unrelated.md",
            &note(
                "page",
                "page_unrelated",
                json!({"wiki_status":"reviewed"}),
                b"Unrelated",
            ),
        );
        for (name, assertion, source, revision, quote) in [
            (
                "evidence_old",
                "assertion_keep",
                &fixture.source,
                &fixture.revisions[0],
                b"old immutable quote".as_slice(),
            ),
            (
                "evidence_current",
                "assertion_opposite",
                &fixture.source,
                &fixture.revisions[1],
                b"new immutable quote".as_slice(),
            ),
            (
                "evidence_other",
                "assertion_keep",
                &fixture.other_source,
                &other.revision_id,
                b"other immutable quote".as_slice(),
            ),
        ] {
            fixture.write(&format!("{name}.md"), &note("evidence", name, json!({"wiki_status":"active","wiki_assertion_id":assertion,"wiki_source_id":source,"wiki_source_revision":revision,"wiki_stance":"supports","wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":quote.len(),"wiki_quote_hash":Blake3Hash::digest(quote)}), &exact_quote_body(quote, "\n", "Fixture").unwrap()));
        }
        fixture.write(
            "runs/run_fixture/run.md",
            &note(
                "run",
                "run_fixture",
                json!({"wiki_status":"completed","wiki_created_at":"2026-09-28T00:00:00Z"}),
                b"Run",
            ),
        );
        for (index, revision) in fixture.revisions.clone().iter().enumerate() {
            let fingerprint = Blake3Hash::digest(format!("packet {index}"));
            let packet = RecordId::packet(&fingerprint);
            fixture.write(&format!("packet-{index}.md"), &note("extraction_packet", packet.as_str(), json!({"wiki_source_id":fixture.source,"wiki_source_revision":revision,"wiki_packet_fingerprint":fingerprint,"wiki_output_schema":"fixture","wiki_created_at":"2026-09-28T00:00:00Z"}), b"Retained packet"));
            let task = Blake3Hash::digest(format!("task {index}"));
            let output = GenerationOutput {
                version: 1,
                task_key: task.clone(),
                packet_id: packet.clone(),
                packet_fingerprint: fingerprint.clone(),
                attempt: AttemptRef {
                    run_id: id("run_fixture"),
                    task_key: task,
                    attempt_id: id(&format!("attempt_{index}")),
                    number: index as u32 + 1,
                    request_hash: Blake3Hash::digest(b"request"),
                },
                response: "Response".into(),
                response_hash: Blake3Hash::digest(b"Response"),
            };
            fixture.write(&format!("runs/run_fixture/outputs/event_{index}.md"), &note("run_event", &format!("event_{index}"), json!({"wiki_run_id":"run_fixture","wiki_sequence":index + 1,"wiki_event_type":"generation_output","wiki_occurred_at":"2026-09-28T00:00:00Z"}), &render_fence(&output, "lwiki-api-extraction-output-v1", crate::graph::MAX_ARTIFACT_BYTES).unwrap()));
            fixture.write(&format!("extraction-{index}.md"), &note("extraction", &format!("extraction_{index}"), json!({"wiki_status":"completed","wiki_packet_id":packet,"wiki_input_hash":fingerprint,"wiki_extractor_fingerprint":Blake3Hash::digest(b"extractor"),"wiki_executor":"agent","wiki_source_ids":[fixture.source, fixture.other_source],"wiki_source_revision_ids":[revision,other.revision_id],"wiki_completed_at":"2026-09-28T00:00:00Z"}), b"Extraction"));
            fixture.packets.push(packet);
        }
        let identity = BuildIdentity {
            selection: CatalogSelection::new(id("vault_withdraw"), 1).unwrap(),
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
        let input = scan::scan_input(&fixture.fs, &id("vault_withdraw")).unwrap();
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
    fn project(&self, reason: &str) -> Result<Option<ProjectedWrite>> {
        let reader = self.catalog.query_snapshot(QueryReadLimits::default())?;
        project_withdraw(
            &self.fs,
            &reader,
            &self.source,
            reason,
            &RefreshProjectionLimits::default(),
        )
    }
    fn apply(&self) {
        let projected = self
            .project("Obsolete source; preserve evidence history")
            .unwrap()
            .unwrap();
        let mut session =
            IndexedRefreshSession::prepare_write(&self.catalog, &self.writer, projected).unwrap();
        let result = ChangeEngine::new(self.fs.clone())
            .unwrap()
            .apply_indexed_refresh(&self.writer, &mut session)
            .unwrap();
        assert_eq!(result.status, ChangeStatus::Committed);
        drop(session);
        self.oracle();
    }
    fn oracle(&self) {
        let input = scan::scan_input(&self.fs, &id("vault_withdraw")).unwrap();
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
        let paths = projection
            .validation
            .dependencies
            .iter()
            .filter(|d| canonical_path(&d.path))
            .map(|d| d.path.clone())
            .collect();
        let mut diagnostics: Vec<CatalogDiagnostic> = reader.diagnostics(&paths).unwrap();
        diagnostics.sort_by(|a, b| {
            (&a.path, format!("{:?}", a.code), a.details.to_string()).cmp(&(
                &b.path,
                format!("{:?}", b.code),
                b.details.to_string(),
            ))
        });
        assert_eq!(diagnostics, projection.validation.diagnostics);
        let actual_edges: BTreeSet<_> = reader
            .connection()
            .prepare("SELECT owner_id,target_id,role_json FROM semantic_edges")
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .map(|r| {
                let (owner, target, role) = r.unwrap();
                EligibilityEdge {
                    owner_id: id(&owner),
                    target_id: id(&target),
                    role: serde_json::from_str(&role).unwrap(),
                }
            })
            .collect();
        assert_eq!(actual_edges, projection.facts.edges);
        let actual_graph: Vec<GraphRow> = reader.connection().prepare("SELECT target_id,target_kind,name,aliases_json,endpoints,predicate,qualifiers,description FROM graph_rows ORDER BY target_id").unwrap().query_map([], |r| Ok(GraphRow { target_id:id(&r.get::<_,String>(0)?), target_kind:r.get::<_,String>(1)?.parse().unwrap(), name:r.get(2)?, aliases:serde_json::from_str(&r.get::<_,String>(3)?).unwrap(), endpoints:r.get(4)?, predicate:r.get(5)?, qualifiers:r.get(6)?, description:r.get(7)? })).unwrap().collect::<std::result::Result<_,_>>().unwrap();
        sink.graph.sort_by(|a, b| a.target_id.cmp(&b.target_id));
        assert_eq!(actual_graph, sink.graph);
        let actual_links: Vec<LinkRow> = reader.connection().prepare("SELECT from_path,byte_start,target_id,target_path,resolution FROM links ORDER BY from_path,byte_start").unwrap().query_map([],|r| Ok(LinkRow { from_path:path(&r.get::<_,String>(0)?), byte_start:u64::try_from(r.get::<_,i64>(1)?).unwrap(), target_id:r.get::<_,Option<String>>(2)?.map(|s|id(&s)), target_path:r.get::<_,Option<String>>(3)?.map(|s|path(&s)), resolution:r.get(4)? })).unwrap().collect::<std::result::Result<_,_>>().unwrap();
        sink.links
            .sort_by(|a, b| (&a.from_path, a.byte_start).cmp(&(&b.from_path, b.byte_start)));
        assert_eq!(actual_links, sink.links);
        let mut expected_policy = vec![];
        projection
            .facts
            .policy
            .as_ref()
            .unwrap()
            .visit(&mut |row| {
                expected_policy.push(row.columns()?);
                Ok(())
            })
            .unwrap();
        expected_policy.sort();
        let actual_policy: Vec<[String; 4]> = reader
            .connection()
            .prepare(
                "SELECT family,key,owner,value FROM policy_facts ORDER BY family,key,owner,value",
            )
            .unwrap()
            .query_map([], |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?]))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for columns in &actual_policy {
            PolicyRow::from_columns(columns.clone()).unwrap();
        }
        assert_eq!(actual_policy, expected_policy);
    }
    fn immutable_bytes(&self) -> BTreeMap<VaultRelativePath, Vec<u8>> {
        self.revisions
            .iter()
            .flat_map(|revision| {
                ["revision.md", "original.bin", "content.md"]
                    .into_iter()
                    .map(move |name| {
                        path(&format!(
                            "sources/{}/revisions/{revision}/{name}",
                            self.source
                        ))
                    })
            })
            .map(|path| {
                let bytes = fs::read(self.fs.root().path().join(path.as_str())).unwrap();
                (path, bytes)
            })
            .collect()
    }
}

#[test]
fn withdrawal_all_history_dependents_support_opposition_and_policy_match_full_oracle() {
    let fixture = Fixture::new();
    fixture.oracle();
    let immutable = fixture.immutable_bytes();
    fixture.apply();
    assert_eq!(fixture.immutable_bytes(), immutable);
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    for member in std::iter::once(fixture.source.clone())
        .chain(fixture.revisions.clone())
        .chain(fixture.packets.clone())
        .chain([
            id("evidence_old"),
            id("evidence_current"),
            id("extraction_0"),
            id("extraction_1"),
            id("event_0"),
            id("event_1"),
        ])
    {
        let row = reader.record(&member).unwrap().unwrap();
        assert_eq!(
            row.eligibility,
            Eligibility::Withdrawn,
            "{member}: {:?}",
            row.reasons
        );
    }
    let keep = reader.record(&id("assertion_keep")).unwrap().unwrap();
    assert_eq!(keep.eligibility, Eligibility::Current);
    assert!(!keep.disputed);
    assert_ne!(
        reader
            .record(&id("assertion_opposite"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Current
    );
    assert_ne!(
        reader
            .record(&id("page_derived"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Current
    );
    assert_eq!(
        reader
            .record(&fixture.other_source)
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Current
    );
}

#[test]
fn withdrawal_one_source_operation_zero_new_owners_and_no_historical_payload_reads() {
    let fixture = Fixture::new();
    let parts = fixture.project("Outdated").unwrap().unwrap().into_parts();
    assert_eq!(parts.draft.operations.len(), 1);
    assert!(parts.draft.allocated_ids.is_empty());
    assert!(parts.delta.owners.is_empty());
    assert!(parts.delta.revisions.is_empty());
    assert!(
        matches!(&parts.operation, IndexedWriteOperation::SourceWithdraw { source_id } if source_id == &fixture.source)
    );
    assert!(
        parts
            .before
            .iter()
            .all(|dep| !dep.path.as_str().ends_with("original.bin")
                && !dep.path.as_str().ends_with("content.md"))
    );
    assert!(parts.before.iter().any(|dep| dep.path == path("WIKI.md")));
    assert!(
        parts
            .before
            .iter()
            .any(|dep| dep.path == path("evidence_old.md"))
    );
    assert!(
        !parts
            .before
            .iter()
            .any(|dep| dep.path == path("unrelated.md"))
    );
    for revision in &fixture.revisions {
        assert!(parts.before.iter().any(|dep| dep.path
            == path(&format!(
                "sources/{}/revisions/{revision}/revision.md",
                fixture.source
            ))));
    }
}

#[test]
fn withdrawn_noop_still_authenticates_source_vault_and_reason() {
    let fixture = Fixture::new();
    fixture.apply();
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let generation = QueryCatalog::snapshot(&reader).clone();
    drop(reader);
    assert!(fixture.project("Already removed").unwrap().is_none());
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(QueryCatalog::snapshot(&reader), &generation);
    drop(reader);
    assert_eq!(
        fixture.project(" \n").err().unwrap().code,
        ErrorCode::RecordInvalid
    );
    assert_eq!(
        fixture
            .project(&"x".repeat(MAX_REASON_BYTES + 1))
            .err()
            .unwrap()
            .code,
        ErrorCode::RecordInvalid
    );
    fixture.write(
        &format!("sources/{}/source.md", fixture.source),
        b"external source edit",
    );
    assert_eq!(
        fixture.project("Already removed").err().unwrap().code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn withdrawal_stale_historical_boundary_fails_without_mutation() {
    let fixture = Fixture::new();
    let source_path = fixture
        .fs
        .root()
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let before = fs::read(&source_path).unwrap();
    fixture.write("evidence_old.md", b"external evidence edit");
    assert_eq!(
        fixture.project("Outdated").err().unwrap().code,
        ErrorCode::ContentConflict
    );
    assert_eq!(fs::read(source_path).unwrap(), before);
}

#[test]
fn withdrawal_selected_fanout_exhaustion_fails_before_canonical_write() {
    let fixture = Fixture::new();
    let source_path = fixture
        .fs
        .root()
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let before = fs::read(&source_path).unwrap();
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let limits = RefreshProjectionLimits {
        max_rows: 2,
        ..RefreshProjectionLimits::default()
    };
    assert_eq!(
        project_withdraw(&fixture.fs, &reader, &fixture.source, "Outdated", &limits)
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(fs::read(source_path).unwrap(), before);
}

#[test]
fn withdrawal_prepared_manifest_guards_late_historical_changes() {
    let fixture = Fixture::new();
    let source_path = fixture
        .fs
        .root()
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let before = fs::read(&source_path).unwrap();
    let projected = fixture.project("Outdated").unwrap().unwrap();
    let mut session =
        IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected).unwrap();
    fixture.write("evidence_old.md", b"late external edit");
    assert!(
        ChangeEngine::new(fixture.fs.clone())
            .unwrap()
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .is_err()
    );
    assert_eq!(fs::read(source_path).unwrap(), before);
}

#[test]
fn withdrawn_noop_authenticates_vault_marker() {
    let fixture = Fixture::new();
    fixture.apply();
    fixture.write("WIKI.md", b"external marker edit");
    assert_eq!(
        fixture.project("Already removed").err().unwrap().code,
        ErrorCode::ContentConflict
    );
}

struct WithdrawSqlFault(PublicationCheckpoint);
impl PublicationFault for WithdrawSqlFault {
    fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
        if checkpoint == self.0 {
            Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "injected withdrawal SQL interruption",
            ))
        } else {
            Ok(())
        }
    }
}

fn native_withdrawal_recovery(committed: bool) {
    let fixture = Fixture::new();
    let immutable = fixture.immutable_bytes();
    let old = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let old_source = old.record(&fixture.source).unwrap().unwrap();
    let projected = fixture.project("Obsolete").unwrap().unwrap();
    let proposed = projected.draft().operations[0]
        .proposed
        .as_ref()
        .unwrap()
        .clone();
    let source_path = fixture
        .fs
        .root()
        .path()
        .join(projected.draft().operations[0].target.as_str());
    let faulty = Catalog::with_options(
        fixture.fs.clone(),
        id("vault_withdraw"),
        CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(WithdrawSqlFault(if committed {
                PublicationCheckpoint::AfterCommit
            } else {
                PublicationCheckpoint::AfterPointer
            }))),
        },
    );
    let mut session =
        IndexedRefreshSession::prepare_write(&faulty, &fixture.writer, projected).unwrap();
    let proof = session.proof().clone();
    assert_eq!(proof.version, 3);
    assert!(
        matches!(&proof.operation, Some(IndexedWriteOperation::SourceWithdraw { source_id }) if source_id == &fixture.source)
    );
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    assert_eq!(
        engine
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .unwrap_err()
            .code,
        ErrorCode::RecoveryRequired
    );
    let phase = if committed {
        IndexedRefreshPhase::AlreadyPublished
    } else {
        IndexedRefreshPhase::AtBase
    };
    assert_eq!(session.phase(), phase);
    drop(session);
    drop(faulty);
    drop(engine);
    assert_eq!(fs::read(&source_path).unwrap(), proposed);
    assert_eq!(fixture.immutable_bytes(), immutable);
    assert_eq!(QueryCatalog::snapshot(&old), &proof.base);
    assert_eq!(
        old.record(&fixture.source).unwrap(),
        Some(old_source.clone())
    );
    {
        let reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert_eq!(
            QueryCatalog::snapshot(&reader),
            if committed {
                &proof.intended
            } else {
                &proof.base
            }
        );
    }
    let modified = fs::metadata(&source_path).unwrap().modified().unwrap();
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    let retained = engine
        .load_indexed_refresh_proof(&proof.change)
        .unwrap()
        .unwrap();
    assert_eq!(retained, proof);
    let mut recovered =
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, retained).unwrap();
    assert_eq!(recovered.phase(), phase);
    let report = engine
        .apply_indexed_refresh(&fixture.writer, &mut recovered)
        .unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    assert_eq!(report.snapshot, Some(proof.intended.clone()));
    drop(recovered);
    fixture.oracle();
    assert_eq!(
        fixture
            .catalog
            .check_normalized(&fixture.writer)
            .unwrap()
            .snapshot,
        proof.intended
    );
    assert_eq!(old.record(&fixture.source).unwrap(), Some(old_source));
    let retained = engine
        .load_indexed_refresh_proof(&proof.change)
        .unwrap()
        .unwrap();
    let mut repeated =
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, retained).unwrap();
    assert_eq!(repeated.phase(), IndexedRefreshPhase::AlreadyPublished);
    assert_eq!(
        engine
            .apply_indexed_refresh(&fixture.writer, &mut repeated)
            .unwrap(),
        report
    );
    drop(repeated);
    assert_eq!(fs::read(&source_path).unwrap(), proposed);
    assert_eq!(
        fs::metadata(source_path).unwrap().modified().unwrap(),
        modified
    );
    assert_eq!(fixture.immutable_bytes(), immutable);
}

#[test]
fn sealed_withdrawal_v3_native_precommit_replay_preserves_reader_and_history() {
    native_withdrawal_recovery(false);
}

#[test]
fn sealed_withdrawal_v3_native_postcommit_replay_preserves_reader_and_history() {
    native_withdrawal_recovery(true);
}

#[test]
fn withdrawal_rows_cannot_use_refresh_or_untyped_authority() {
    let fixture = Fixture::new();
    let source_path = fixture
        .fs
        .root()
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let source_before = fs::read(&source_path).unwrap();
    let history_before = fixture.immutable_bytes();
    let mut parts = fixture
        .project("Preserve old evidence")
        .unwrap()
        .unwrap()
        .into_parts();
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let connection = reader.connection();
    assert_eq!(
        parts
            .delta
            .check_before_operation(connection, None)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    parts.operation = crate::changes::indexed_refresh::IndexedWriteOperation::SourceRefresh {
        source_id: fixture.source.clone(),
    };
    drop(reader);
    let rejected = IndexedRefreshSession::prepare_write(
        &fixture.catalog,
        &fixture.writer,
        ProjectedWrite::from_parts(parts),
    );
    let error = match rejected {
        Ok(_) => panic!("refresh admitted withdrawal fields"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert_eq!(fs::read(source_path).unwrap(), source_before);
    assert_eq!(fixture.immutable_bytes(), history_before);
    assert!(!fixture.fs.root().path().join("changes").exists());
    fixture.oracle();
}

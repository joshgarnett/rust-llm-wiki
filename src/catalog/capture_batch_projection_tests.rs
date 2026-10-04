use super::*;
use crate::{
    catalog::{
        Catalog, CatalogOptions, DocumentRow, GraphRow, IdentityClaimRow, LinkRow,
        PublicationCheckpoint, PublicationFault, RetrievalSink,
        eligibility_facts::EligibilityEdge,
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        policy_facts::PolicyRow,
        query_types::QueryReadLimits,
        selector,
        source_refresh::IndexedRefreshSession,
    },
    changes::{
        ChangeEngine, ChangeStatus, RevisionOwnershipLookup, indexed_refresh::IndexedRefreshPhase,
    },
    domain::{CanonicalRecord, ErrorCode, RecordId},
    records::parse_note,
    sources::{
        CaptureAllocation, CaptureRequest, ExtractionInput, SourceOrigin, SourceStore,
        revision::{canonical_path, record_bytes},
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
    record_bytes(CanonicalRecord::new(fields).unwrap(), body).unwrap()
}
fn request(bytes: &[u8]) -> CaptureRequest {
    CaptureRequest {
        title: "Fresh captured source".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "outside vault/source with spaces.txt".into(),
        original: bytes.to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/plain".into()),
    }
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
}
impl Fixture {
    fn new(setup: impl FnOnce(&VaultFs)) -> Self {
        Self::with_layout(false, setup)
    }
    fn with_layout(retained: bool, setup: impl FnOnce(&VaultFs)) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            note("vault", "vault_capture", json!({}), b""),
        )
        .unwrap();
        let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
        if retained {
            crate::storage::cleanup(
                &fs_handle,
                &writer,
                &crate::storage::StorageOptions::default(),
            )
            .unwrap();
            assert!(crate::storage::layout::active(fs_handle.root()).unwrap());
        }
        setup(&fs_handle);
        let identity = BuildIdentity {
            selection: CatalogSelection::new(id("vault_capture"), 1).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&fs_handle, &writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default())
                .unwrap();
        let input = scan::scan_input(&fs_handle, &id("vault_capture")).unwrap();
        let projection =
            scan::project_normalized_with_sink(&fs_handle, &input, false, &mut builder).unwrap();
        let completed = builder.finish_normalized(&projection).unwrap();
        selector::publish(
            &fs_handle,
            &writer,
            &completed.identity.selection,
            Duration::ZERO,
        )
        .unwrap();
        Self {
            _temp: temp,
            catalog: Catalog::new(fs_handle.clone(), id("vault_capture")),
            fs: fs_handle,
            writer,
        }
    }
    fn plan(&self, request: CaptureRequest) -> SourcePlan {
        SourceStore::new(self.fs.clone())
            .plan_capture(request)
            .unwrap()
    }
    fn project(&self, plan: SourcePlan) -> Result<ProjectedWrite> {
        let reader = self.catalog.query_snapshot(QueryReadLimits::default())?;
        project_capture(&self.fs, &reader, plan, &RefreshProjectionLimits::default())
    }
    fn oracle(&self) {
        let input = scan::scan_input(&self.fs, &id("vault_capture")).unwrap();
        let mut sink = Sink::default();
        let projection =
            scan::project_normalized_with_sink(&self.fs, &input, false, &mut sink).unwrap();
        let reader = self
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let count: i64 = reader
            .connection()
            .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, projection.validation.records.len() as i64);
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
        let count: i64 = reader
            .connection()
            .query_row("SELECT count(*) FROM documents", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, sink.documents.len() as i64);
        for document in &sink.documents {
            assert_eq!(
                reader.document(&document.path).unwrap().as_ref(),
                Some(document),
                "document {}",
                document.path
            );
        }
        let count: i64 = reader
            .connection()
            .query_row("SELECT count(*) FROM graph_rows", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, sink.graph.len() as i64);
        let paths = projection
            .validation
            .dependencies
            .iter()
            .filter(|d| canonical_path(&d.path))
            .map(|d| d.path.clone())
            .collect();
        let mut diagnostics = reader.diagnostics(&paths).unwrap();
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
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .map(|row| {
                let (owner, target, role) = row.unwrap();
                EligibilityEdge {
                    owner_id: id(&owner),
                    target_id: id(&target),
                    role: serde_json::from_str(&role).unwrap(),
                }
            })
            .collect();
        assert_eq!(actual_edges, projection.facts.edges);
        let actual_links:Vec<LinkRow> = reader.connection().prepare("SELECT from_path,byte_start,target_id,target_path,resolution FROM links ORDER BY from_path,byte_start").unwrap().query_map([],|row|Ok(LinkRow {from_path:path(&row.get::<_,String>(0)?),byte_start:row.get::<_,i64>(1)? as u64,target_id:row.get::<_,Option<String>>(2)?.map(|s|id(&s)),target_path:row.get::<_,Option<String>>(3)?.map(|s|path(&s)),resolution:row.get(4)?})).unwrap().collect::<std::result::Result<_,_>>().unwrap();
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
            .query_map([], |row| {
                Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?])
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for columns in &actual_policy {
            PolicyRow::from_columns(columns.clone()).unwrap();
        }
        assert_eq!(actual_policy, expected_policy);
        drop(reader);
        self.catalog.check_normalized(&self.writer).unwrap();
    }
    fn plans(&self, count: usize) -> Vec<SourcePlan> {
        (0..count)
            .map(|index| {
                self.plan(request(
                    format!("独立 Café capture {index}: exact fact.\n").as_bytes(),
                ))
            })
            .collect()
    }
    fn project_batch(&self, plans: Vec<SourcePlan>) -> Result<ProjectedWrite> {
        let reader = self.catalog.query_snapshot(QueryReadLimits::default())?;
        project_capture_batch(
            &self.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default(),
        )
    }
    fn apply_batch(
        &self,
        plans: Vec<SourcePlan>,
    ) -> crate::changes::indexed_refresh::IndexedRefreshProof {
        let projected = self.project_batch(plans).unwrap();
        let mut session =
            IndexedRefreshSession::prepare_write(&self.catalog, &self.writer, projected).unwrap();
        let proof = session.proof().clone();
        let report = ChangeEngine::new(self.fs.clone())
            .unwrap()
            .apply_indexed_refresh(&self.writer, &mut session)
            .unwrap();
        assert_eq!(report.status, ChangeStatus::Committed);
        drop(session);
        self.oracle();
        proof
    }
}
fn write(fs_handle: &VaultFs, name: &str, bytes: &[u8]) {
    let target = fs_handle.root().path().join(name);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, bytes).unwrap();
}
fn change_record(
    plan: &mut SourcePlan,
    kind: RecordKind,
    edit: impl FnOnce(&mut BTreeMap<String, Value>),
) {
    let operation = plan
        .draft
        .as_mut()
        .unwrap()
        .operations
        .iter_mut()
        .find(|op| {
            op.proposed.as_ref().is_some_and(|bytes| {
                parse_note(bytes)
                    .canonical
                    .as_ref()
                    .is_some_and(|r| r.kind() == kind)
            })
        })
        .unwrap();
    let parsed = parse_note(operation.proposed.as_ref().unwrap());
    let mut fields = parsed.canonical.as_ref().unwrap().fields().clone();
    edit(&mut fields);
    operation.proposed =
        Some(record_bytes(CanonicalRecord::new(fields).unwrap(), parsed.body()).unwrap());
}

#[test]
fn capture_batch_one_four_eight_share_one_publication_and_match_full_oracle() {
    for retained in [false, true] {
        for count in [1, 4, 8] {
            let fixture = Fixture::with_layout(retained, |_| {});
            let plans = fixture.plans(count);
            let mut targets = plans
                .iter()
                .map(|plan| (plan.source_id.clone(), plan.revision_id.clone()))
                .collect::<Vec<_>>();
            targets.sort();
            let old = fixture
                .catalog
                .query_snapshot(QueryReadLimits::default())
                .unwrap();
            let generation = QueryCatalog::snapshot(&old).generation;
            let proof = fixture.apply_batch(plans);
            assert_eq!(proof.base.generation, generation);
            assert_eq!(proof.intended.generation, generation + 1);
            let IndexedWriteOperation::SourceCaptureBatch { captures } =
                proof.operation.as_ref().unwrap()
            else {
                panic!("batch descriptor")
            };
            assert_eq!(
                captures
                    .iter()
                    .map(|capture| (capture.source_id.clone(), capture.revision_id.clone()))
                    .collect::<Vec<_>>(),
                targets
            );
            let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
            let manifest = engine
                .load_manifest_structure(&proof.change.change_id)
                .unwrap()
                .0;
            assert_eq!(manifest.allocated_ids.len(), count * 2);
            for (index, (source, revision)) in targets.iter().enumerate() {
                assert_eq!(manifest.allocated_ids[&format!("source_{index}")], *source);
                assert_eq!(
                    manifest.allocated_ids[&format!("revision_{index}")],
                    *revision
                );
                assert!(old.record(source).unwrap().is_none());
                let reader = fixture
                    .catalog
                    .query_snapshot(QueryReadLimits::default())
                    .unwrap();
                assert_eq!(
                    reader.record(source).unwrap().unwrap().eligibility,
                    Eligibility::Current
                );
                assert!(
                    reader
                        .document(&path(&format!(
                            "sources/{source}/revisions/{revision}/content.md"
                        )))
                        .unwrap()
                        .unwrap()
                        .raw_text
                        .contains("exact fact")
                );
                assert_eq!(
                    reader
                        .revision_owner(&crate::changes::RevisionTreeKey {
                            source_component: source.to_string(),
                            revision_component: revision.to_string()
                        })
                        .unwrap(),
                    Some(proof.change.clone())
                );
            }
            let mut replay =
                IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof.clone())
                    .unwrap();
            assert_eq!(
                engine
                    .apply_indexed_refresh(&fixture.writer, &mut replay)
                    .unwrap()
                    .status,
                ChangeStatus::Committed
            );
        }
    }
}

#[test]
fn capture_batch_overlay_resolves_exact_links_but_keeps_shared_basename_ambiguous_and_policy() {
    let seed = Fixture::new(|_| {});
    let plans = seed.plans(4);
    let first = plans[0].source_id.clone();
    let revision = plans[0].revision_id.clone();
    let receipt = format!(
        "```{}\n{{}}\n```\n",
        crate::graph::review_types::GRAPH_REVIEW_FENCE
    );
    let fixture = Fixture::new(|handle| {
        write(
            handle,
            "pages/navigation.md",
            &note(
                "page",
                "page_batch_navigation",
                json!({"wiki_status":"reviewed"}),
                format!("[[source]] [[{first}]] [[{revision}]]").as_bytes(),
            ),
        );
        write(
            handle,
            "pages/policy.md",
            &note(
                "page",
                "page_batch_policy",
                json!({"wiki_status":"reviewed"}),
                receipt.as_bytes(),
            ),
        );
    });
    let before = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let policy = before.record(&id("page_batch_policy")).unwrap().unwrap();
    assert_eq!(policy.eligibility, Eligibility::Invalid);
    let parts = fixture.project_batch(plans).unwrap().into_parts();
    assert_eq!(
        parts
            .delta
            .facts
            .as_ref()
            .unwrap()
            .policy
            .as_ref()
            .unwrap()
            .memberships
            .len(),
        8
    );
    let mut session = IndexedRefreshSession::prepare_write(
        &fixture.catalog,
        &fixture.writer,
        ProjectedWrite::from_parts(parts),
    )
    .unwrap();
    ChangeEngine::new(fixture.fs.clone())
        .unwrap()
        .apply_indexed_refresh(&fixture.writer, &mut session)
        .unwrap();
    drop(session);
    fixture.oracle();
    let after = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        after.record(&id("page_batch_policy")).unwrap(),
        Some(policy)
    );
    let links: Vec<(Option<String>, String)> = after.connection().prepare("SELECT target_id,resolution FROM links WHERE from_path='pages/navigation.md' ORDER BY byte_start").unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?))).unwrap().collect::<std::result::Result<_,_>>().unwrap();
    assert_eq!(links.len(), 3);
    assert!(
        links[0].0.is_none() && links[0].1.starts_with("Ambiguous"),
        "{:?}",
        links[0]
    );
    // Untyped wiki links resolve paths/aliases, not bare record IDs. The
    // generated typed companion links resolve within the complete group.
    assert!(links[1].0.is_none() && links[1].1.starts_with("Missing"));
    assert!(links[2].0.is_none() && links[2].1.starts_with("Missing"));
    let source_path = format!("sources/{first}/source.md");
    let revision_path = format!("sources/{first}/revisions/{revision}/revision.md");
    let typed: i64 = after
        .connection()
        .query_row(
            "SELECT count(*) FROM links WHERE from_path=?1 AND target_id=?2 AND target_path=?3",
            rusqlite::params![revision_path, first.as_str(), source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(typed, 1);
}

#[test]
fn capture_batch_single_shares_scalar_rows_without_changing_legacy_descriptor() {
    let fixture = Fixture::new(|_| {});
    let plan = fixture.plan(request(b"Scalar compatibility exact quote."));
    let scalar = fixture.project(plan.clone()).unwrap().into_parts();
    let group = fixture.project_batch(vec![plan]).unwrap().into_parts();
    assert_eq!(scalar.delta, group.delta);
    assert_eq!(scalar.before, group.before);
    assert_eq!(scalar.after, group.after);
    assert_eq!(
        scalar
            .draft
            .operations
            .iter()
            .map(|operation| (
                &operation.target,
                &operation.expected,
                &operation.proposed,
                &operation.apply_after
            ))
            .collect::<Vec<_>>(),
        group
            .draft
            .operations
            .iter()
            .map(|operation| (
                &operation.target,
                &operation.expected,
                &operation.proposed,
                &operation.apply_after
            ))
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        scalar.operation,
        IndexedWriteOperation::SourceCapture { .. }
    ));
    assert!(matches!(
        group.operation,
        IndexedWriteOperation::SourceCaptureBatch { .. }
    ));
    assert!(scalar.draft.allocated_ids.contains_key("source"));
    assert!(group.draft.allocated_ids.contains_key("source_0"));
    // Nondecimal legacy identities retain generic folded collision checks and
    // normal replay, rather than being rewritten into new numeric identities.
    let plan = SourceStore::new(fixture.fs.clone())
        .plan_capture_named(
            request(b"Legacy exact bytes."),
            &CaptureAllocation {
                source_id: id("source_legacy_batch"),
                revision_id: id("revision_legacy_batch"),
                captured_at: "2026-10-04T00:00:00Z".into(),
            },
        )
        .unwrap();
    fixture.apply_batch(vec![plan]);
}

#[test]
fn capture_batch_rejects_empty_oversize_duplicate_and_cross_kind_id_groups() {
    let fixture = Fixture::new(|_| {});
    assert!(fixture.project_batch(vec![]).is_err());
    assert!(fixture.project_batch(fixture.plans(9)).is_err());
    let plans = fixture.plans(2);
    assert!(
        fixture
            .project_batch(vec![plans[0].clone(), plans[0].clone()])
            .is_err()
    );
    let mut overlap = plans.clone();
    overlap[1].revision_id = overlap[0].source_id.clone();
    assert!(fixture.project_batch(overlap).is_err());
    assert!(!fixture.fs.root().path().join("sources").exists());
}

#[test]
fn capture_batch_validates_every_member_generated_envelope_asset_order_and_tree() {
    for variant in 0..10 {
        let fixture = Fixture::new(|_| {});
        let mut plans = fixture.plans(4);
        let plan = &mut plans[3];
        match variant {
            0 => plan.reused = true,
            1 => plan.capture_state = Some(SourceCaptureState::Empty),
            2 => plan.draft.as_mut().unwrap().allocated_ids.clear(),
            3 => {
                plan.draft.as_mut().unwrap().operations[0].expected =
                    ExpectedState::Hash(Blake3Hash::digest(b"old"))
            }
            4 => plan.draft.as_mut().unwrap().operations[0].proposed = None,
            5 => {
                plan.draft
                    .as_mut()
                    .unwrap()
                    .operations
                    .iter_mut()
                    .find(|operation| operation.target.as_str().ends_with("/original.bin"))
                    .unwrap()
                    .proposed = Some(b"tampered original".to_vec())
            }
            6 => change_record(plan, RecordKind::Source, |fields| {
                fields.insert("aliases".into(), json!(["foreign"]));
            }),
            7 => change_record(plan, RecordKind::Revision, |fields| {
                fields.insert("wiki_depends_on_ids".into(), json!(["foreign"]));
            }),
            8 => plan
                .draft
                .as_mut()
                .unwrap()
                .operations
                .iter_mut()
                .find(|operation| operation.target.as_str().ends_with("/source.md"))
                .unwrap()
                .apply_after
                .clear(),
            9 => {
                plan.draft
                    .as_mut()
                    .unwrap()
                    .operations
                    .iter_mut()
                    .find(|operation| operation.target.as_str().ends_with("/content.md"))
                    .unwrap()
                    .target = path("pages/foreign.md")
            }
            _ => unreachable!(),
        }
        assert!(fixture.project_batch(plans).is_err(), "variant {variant}");
        assert!(!fixture.fs.root().path().join("sources").exists());
        fixture.oracle();
    }
}

#[test]
fn capture_batch_preserves_scalar_large_item_and_enforces_combined_actual_budgets() {
    let fixture = Fixture::new(|_| {});
    let large = fixture.plan(request(&vec![0xff; 4 * 1024 * 1024 + 1]));
    assert!(fixture.project(large.clone()).is_ok());
    assert!(fixture.project_batch(vec![large.clone()]).is_ok());
    assert_eq!(
        fixture
            .project_batch(vec![large, fixture.plan(request(b"x"))])
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    let plans = fixture.plans(4);
    let bytes = plans
        .iter()
        .flat_map(|plan| &plan.draft.as_ref().unwrap().operations)
        .map(|operation| operation.proposed.as_ref().unwrap().len())
        .sum::<usize>();
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let limits = RefreshProjectionLimits {
        max_canonical_bytes: bytes - 1,
        ..RefreshProjectionLimits::default()
    };
    assert_eq!(
        project_capture_batch(&fixture.fs, &reader, plans, &limits)
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert!(!fixture.fs.root().path().join("sources").exists());
}

#[test]
fn capture_batch_reserves_every_existing_identity_and_companion_path() {
    for source_reserved in [false, true] {
        for companion in [false, true] {
            let seed = Fixture::new(|_| {});
            let plans = seed.plans(4);
            let last = &plans[3];
            let reserved = if source_reserved {
                &last.source_id
            } else {
                &last.revision_id
            };
            let target = if source_reserved {
                format!("sources/{}/source.md", last.source_id)
            } else {
                format!(
                    "sources/{}/revisions/{}/revision.md",
                    last.source_id, last.revision_id
                )
            };
            let fixture = Fixture::new(|handle| {
                write(
                    handle,
                    "entity.md",
                    &note(
                        "entity",
                        "entity_batch",
                        json!({"wiki_status":"active","wiki_entity_type":"component"}),
                        b"",
                    ),
                );
                let mut fields = json!({"wiki_status":"accepted","wiki_subject_id":"entity_batch","wiki_object_id":"entity_batch","wiki_predicate":"uses"});
                if companion {
                    fields["wiki_subject"] = json!(format!("[[{target}]]"));
                } else {
                    fields["wiki_subject_id"] = json!(reserved);
                }
                write(
                    handle,
                    "claim.md",
                    &note("assertion", "assertion_batch", fields, b""),
                );
            });
            assert_eq!(
                fixture.project_batch(plans).err().unwrap().code,
                ErrorCode::ContentConflict
            );
            assert!(!fixture.fs.root().path().join("sources").exists());
        }
    }
}

#[test]
fn capture_batch_fresh_roots_and_prepared_replay_do_not_adopt_foreign_members() {
    for after_prepare in [false, true] {
        let fixture = Fixture::new(|_| {});
        let plans = fixture.plans(4);
        let occupied = plans[3].source_id.clone();
        let projected = fixture.project_batch(plans).unwrap();
        if !after_prepare {
            fs::create_dir_all(fixture.fs.root().path().join(format!("sources/{occupied}")))
                .unwrap();
            assert_eq!(
                IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::ContentConflict
            );
        } else {
            let mut session =
                IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                    .unwrap();
            let foreign = fixture
                .fs
                .root()
                .path()
                .join(format!("sources/{occupied}/foreign.bin"));
            fs::create_dir_all(foreign.parent().unwrap()).unwrap();
            fs::write(&foreign, b"preserve unfamiliar bytes").unwrap();
            let modified = fs::metadata(&foreign).unwrap().modified().unwrap();
            assert_eq!(
                ChangeEngine::new(fixture.fs.clone())
                    .unwrap()
                    .apply_indexed_refresh(&fixture.writer, &mut session)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::ContentConflict
            );
            assert_eq!(fs::read(&foreign).unwrap(), b"preserve unfamiliar bytes");
            assert_eq!(
                fs::metadata(&foreign).unwrap().modified().unwrap(),
                modified
            );
        }
    }
}

struct BatchSqlFault(PublicationCheckpoint);
impl PublicationFault for BatchSqlFault {
    fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
        if checkpoint == self.0 {
            Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "injected batch SQL cut",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn capture_batch_native_sql_cuts_reopen_exact_all_member_owners_without_rewrites() {
    for committed in [false, true] {
        let fixture = Fixture::new(|_| {});
        let plans = fixture.plans(4);
        let targets = plans
            .iter()
            .map(|plan| (plan.source_id.clone(), plan.revision_id.clone()))
            .collect::<Vec<_>>();
        let old = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let faulty = Catalog::with_options(
            fixture.fs.clone(),
            id("vault_capture"),
            CatalogOptions {
                busy_timeout_ms: 1000,
                fault: Some(std::sync::Arc::new(BatchSqlFault(if committed {
                    PublicationCheckpoint::AfterCommit
                } else {
                    PublicationCheckpoint::AfterPointer
                }))),
            },
        );
        let projected = fixture.project_batch(plans).unwrap();
        let mut session =
            IndexedRefreshSession::prepare_write(&faulty, &fixture.writer, projected).unwrap();
        let proof = session.proof().clone();
        let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
        assert_eq!(
            engine
                .apply_indexed_refresh(&fixture.writer, &mut session)
                .err()
                .unwrap()
                .code,
            ErrorCode::RecoveryRequired
        );
        drop(session);
        drop(faulty);
        let immutable = proof
            .after
            .iter()
            .filter(|dependency| dependency.path.as_str().starts_with("sources/"))
            .map(|dependency| {
                let absolute = fixture.fs.root().path().join(dependency.path.as_str());
                let bytes = fs::read(&absolute).unwrap();
                let modified = fs::metadata(&absolute).unwrap().modified().unwrap();
                (absolute, bytes, modified)
            })
            .collect::<Vec<_>>();
        let mut resumed = IndexedRefreshSession::resume(
            &fixture.catalog,
            &fixture.writer,
            engine
                .load_indexed_refresh_proof(&proof.change)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            resumed.phase(),
            if committed {
                IndexedRefreshPhase::AlreadyPublished
            } else {
                IndexedRefreshPhase::AtBase
            }
        );
        assert_eq!(
            engine
                .apply_indexed_refresh(&fixture.writer, &mut resumed)
                .unwrap()
                .status,
            ChangeStatus::Committed
        );
        drop(resumed);
        for (source, revision) in targets {
            assert!(old.record(&source).unwrap().is_none());
            let reader = fixture
                .catalog
                .query_snapshot(QueryReadLimits::default())
                .unwrap();
            assert_eq!(
                reader
                    .revision_owner(&crate::changes::RevisionTreeKey {
                        source_component: source.to_string(),
                        revision_component: revision.to_string()
                    })
                    .unwrap(),
                Some(proof.change.clone())
            );
        }
        for (absolute, bytes, modified) in immutable {
            assert_eq!(fs::read(&absolute).unwrap(), bytes);
            assert_eq!(
                fs::metadata(absolute).unwrap().modified().unwrap(),
                modified
            );
        }
        fixture.oracle();
    }
}

#[test]
fn capture_batch_tampered_descriptor_and_retained_payload_are_not_replay_authority() {
    for corrupt_payload in [false, true] {
        let fixture = Fixture::new(|_| {});
        let projected = fixture.project_batch(fixture.plans(4)).unwrap();
        let session =
            IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        let mut proof = session.proof().clone();
        drop(session);
        if corrupt_payload {
            let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
            let manifest = engine
                .load_manifest_structure(&proof.change.change_id)
                .unwrap()
                .0;
            let payload = manifest
                .operations
                .iter()
                .find(|operation| operation.target.as_str().ends_with("/original.bin"))
                .unwrap()
                .after_payload
                .as_ref()
                .unwrap();
            fs::write(
                fixture.fs.root().resolve(&payload.path).unwrap(),
                b"corrupt retained payload",
            )
            .unwrap();
        } else {
            let Some(IndexedWriteOperation::SourceCaptureBatch { captures }) = &mut proof.operation
            else {
                panic!("batch descriptor")
            };
            captures[3].revision_id = id("revision_foreign");
        }
        let resumed = IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof);
        if corrupt_payload {
            let mut resumed = resumed.unwrap();
            assert!(
                ChangeEngine::new(fixture.fs.clone())
                    .unwrap()
                    .apply_indexed_refresh(&fixture.writer, &mut resumed)
                    .is_err()
            );
        } else {
            assert!(resumed.is_err());
        }
        assert!(!fixture.fs.root().path().join("sources").exists());
    }
}

#[cfg(unix)]
#[test]
fn capture_batch_symlink_hardlink_nested_marker_and_legacy_alias_preserve_foreign_bytes() {
    for variant in 0..4 {
        let fixture = Fixture::new(|_| {});
        let plan = SourceStore::new(fixture.fs.clone())
            .plan_capture_named(
                request(b"Exact selected bytes."),
                &CaptureAllocation {
                    source_id: id("source_alias_batch"),
                    revision_id: id("revision_alias_batch"),
                    captured_at: "2026-10-04T00:00:00Z".into(),
                },
            )
            .unwrap();
        let root = fixture.fs.root().path().join("sources/source_alias_batch");
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        let foreign = fixture.fs.root().path().join("foreign.dat");
        fs::write(&foreign, b"foreign exact bytes").unwrap();
        match variant {
            0 => std::os::unix::fs::symlink(&foreign, &root).unwrap(),
            1 => fs::hard_link(&foreign, &root).unwrap(),
            2 => {
                fs::create_dir(&root).unwrap();
                fs::write(root.join("WIKI.md"), b"nested vault").unwrap();
            }
            _ => fs::create_dir(root.parent().unwrap().join("SOURCE_ALIAS_BATCH")).unwrap(),
        }
        assert!(fixture.project_batch(vec![plan]).is_err());
        assert_eq!(fs::read(foreign).unwrap(), b"foreign exact bytes");
    }
}

#[test]
fn capture_batch_mixed_content_states_and_reordered_requests_keep_exact_originals() {
    let fixture = Fixture::new(|_| {});
    let mut empty = request(b"");
    empty.title = "Empty exact source".into();
    let mut opaque = request(&[0xff, 0x00, 0x81]);
    opaque.extraction = ExtractionInput::Unsupported {
        extractor: "opaque-test".into(),
        fingerprint: Blake3Hash::digest(b"opaque-test"),
    };
    let mut supplied = request(&[0xff, 0xfe]);
    supplied.extraction = ExtractionInput::Supplied {
        extractor: "supplied-test".into(),
        fingerprint: Blake3Hash::digest(b"supplied-test"),
        content: "Supplied Café 東京 exact bytes.\n".as_bytes().to_vec(),
    };
    let inputs = vec![request(b"Complete bytes.\r\n"), empty, opaque, supplied];
    let plans = inputs
        .iter()
        .cloned()
        .map(|input| fixture.plan(input))
        .collect::<Vec<_>>();
    let expected = plans
        .iter()
        .zip(&inputs)
        .map(|(plan, input)| {
            (
                plan.source_id.clone(),
                plan.revision_id.clone(),
                input.original.clone(),
                plan.capture_state,
            )
        })
        .collect::<Vec<_>>();
    let first = fixture.project_batch(plans.clone()).unwrap().into_parts();
    let mut reversed = plans.clone();
    reversed.reverse();
    let second = fixture.project_batch(reversed).unwrap().into_parts();
    assert_eq!(first.operation, second.operation);
    assert_eq!(first.delta, second.delta);
    assert_eq!(first.draft.allocated_ids, second.draft.allocated_ids);
    let proof = fixture.apply_batch(plans);
    assert_eq!(proof.operation.as_ref().unwrap().capture_targets().len(), 4);
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    for (source, revision, original, state) in expected {
        let root = format!("sources/{source}/revisions/{revision}");
        assert_eq!(
            fs::read(
                fixture
                    .fs
                    .root()
                    .path()
                    .join(format!("{root}/original.bin"))
            )
            .unwrap(),
            original
        );
        let content = reader
            .document(&path(&format!("{root}/content.md")))
            .unwrap();
        assert_eq!(
            content.is_none(),
            state == Some(SourceCaptureState::Unsupported)
        );
        if state == Some(SourceCaptureState::Empty) {
            assert!(content.unwrap().raw_text.is_empty());
        }
    }
}

#[test]
fn capture_batch_selected_dependencies_stay_finite_and_foreign_publication_cannot_prepare() {
    let fixture = Fixture::new(|handle| {
        for index in 0..16 {
            write(
                handle,
                &format!("pages/unrelated{index}.md"),
                &note(
                    "page",
                    &format!("page_batch_unrelated{index}"),
                    json!({"wiki_status":"reviewed"}),
                    b"Unrelated stored text.",
                ),
            );
        }
    });
    write(
        &fixture.fs,
        "pages/unrelated15.md",
        b"Externally edited unselected bytes.",
    );
    let plans = fixture.plans(4);
    let roots = plans
        .iter()
        .map(|plan| format!("sources/{}/", plan.source_id))
        .collect::<Vec<_>>();
    let projected = fixture.project_batch(plans.clone()).unwrap();
    assert!(
        projected
            .draft()
            .read_preconditions
            .iter()
            .all(|dependency| dependency.path.as_str() == "WIKI.md"
                || roots
                    .iter()
                    .any(|root| dependency.path.as_str().starts_with(root)))
    );
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let limits = RefreshProjectionLimits {
        max_rows: 1,
        ..RefreshProjectionLimits::default()
    };
    assert_eq!(
        project_capture_batch(&fixture.fs, &reader, plans, &limits)
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    drop(reader);
    let foreign = Fixture::new(|_| {});
    assert!(
        IndexedRefreshSession::prepare_write(&foreign.catalog, &foreign.writer, projected).is_err()
    );
    assert!(!fixture.fs.root().path().join("sources").exists());
    assert!(!foreign.fs.root().path().join("sources").exists());
}

#[test]
fn capture_batch_corrupt_required_policy_index_refuses_before_retention_or_sources() {
    let fixture = Fixture::new(|_| {});
    let plans = fixture.plans(4);
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let file_id = QueryCatalog::snapshot(&reader)
        .publication()
        .unwrap()
        .file_id
        .clone();
    drop(reader);
    let database = fixture
        .fs
        .root()
        .path()
        .join(format!(".wiki/cache/catalogs/{file_id}.sqlite"));
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute_batch("DROP INDEX policy_facts_owner")
        .unwrap();
    drop(connection);
    assert!(fixture.project_batch(plans).is_err());
    assert!(!fixture.fs.root().path().join("sources").exists());
    assert!(!fixture.fs.root().path().join("changes").exists());
}

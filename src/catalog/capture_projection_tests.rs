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
        CaptureRequest, ExtractionInput, SourceOrigin, SourceStore,
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
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            note("vault", "vault_capture", json!({}), b""),
        )
        .unwrap();
        let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
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
    fn apply(&self, plan: SourcePlan) {
        let projected = self.project(plan).unwrap();
        let mut session =
            IndexedRefreshSession::prepare_write(&self.catalog, &self.writer, projected).unwrap();
        let report = ChangeEngine::new(self.fs.clone())
            .unwrap()
            .apply_indexed_refresh(&self.writer, &mut session)
            .unwrap();
        assert_eq!(report.status, ChangeStatus::Committed);
        drop(session);
        self.oracle();
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
fn capture_complete_empty_unsupported_and_supplied_match_full_oracle() {
    for mode in 0..4 {
        let fixture = Fixture::new(|_| {});
        let mut input = request(if mode == 1 {
            b""
        } else {
            "Café distant fact.\r\n第二行\n".as_bytes()
        });
        if mode == 2 {
            input.original = vec![0xff, 0x00, 0x81];
            input.extraction = ExtractionInput::Unsupported {
                extractor: "opaque-v1".into(),
                fingerprint: Blake3Hash::digest(b"opaque-v1"),
            };
        }
        if mode == 3 {
            input.original = vec![0xff, 0xfe];
            input.extraction = ExtractionInput::Supplied {
                extractor: "supplied-v1".into(),
                fingerprint: Blake3Hash::digest(b"supplied-v1"),
                content: b"Extracted exact bytes.\n".to_vec(),
            };
        }
        let original = input.original.clone();
        let plan = fixture.plan(input);
        let source_id = plan.source_id.clone();
        let revision_id = plan.revision_id.clone();
        fixture.apply(plan);
        let reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let source = reader.record(&source_id).unwrap().unwrap();
        assert_eq!(
            source.record.string("wiki_origin"),
            Some("outside vault/source with spaces.txt")
        );
        assert_eq!(source.record.string("wiki_origin_kind"), Some("local-file"));
        let revision = reader.record(&revision_id).unwrap().unwrap();
        assert_eq!(
            revision.record.string("wiki_media_type"),
            Some("text/plain")
        );
        assert_eq!(
            revision.eligibility,
            if mode == 2 {
                Eligibility::Unsupported
            } else {
                Eligibility::Current
            }
        );
        let root = format!("sources/{source_id}/revisions/{revision_id}");
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
        assert_eq!(
            reader
                .document(&path(&format!("{root}/content.md")))
                .unwrap()
                .is_some(),
            mode != 2
        );
    }
}

#[test]
fn capture_updates_broad_navigation_and_preserves_unrelated_policy() {
    let receipt = format!(
        "```{}\n{{}}\n```\n",
        crate::graph::review_types::GRAPH_REVIEW_FENCE
    );
    let fixture = Fixture::new(|handle| {
        write(
            handle,
            "pages/guide.md",
            &note(
                "page",
                "page_guide",
                json!({"wiki_status":"reviewed"}),
                b"See [[source]].",
            ),
        );
        write(
            handle,
            "pages/preseeded-policy.md",
            &note(
                "page",
                "page_preseeded_policy",
                json!({"wiki_status":"reviewed"}),
                receipt.as_bytes(),
            ),
        );
    });
    let old = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let prior = old.record(&id("page_preseeded_policy")).unwrap().unwrap();
    assert_eq!(prior.eligibility, Eligibility::Invalid);
    let old_link: String = old
        .connection()
        .query_row(
            "SELECT resolution FROM links WHERE from_path='pages/guide.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let plan = fixture.plan(request(b"New source quote."));
    let source = plan.source_id.clone();
    fixture.apply(plan);
    let now = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let new_link: String = now
        .connection()
        .query_row(
            "SELECT resolution FROM links WHERE from_path='pages/guide.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_link, "Missing");
    assert_eq!(
        new_link,
        format!(
            "{:?}",
            crate::catalog::navigation_resolution::NavigationResolution::Resolved {
                id: source.clone(),
                path: path(&format!("sources/{source}/source.md")),
                fragment: None,
                companion_stale: false,
            }
        ),
    );
    let target: (String, String) = now
        .connection()
        .query_row(
            "SELECT target_id,target_path FROM links WHERE from_path='pages/guide.md'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        target,
        (source.to_string(), format!("sources/{source}/source.md"))
    );
    assert_eq!(
        now.record(&id("page_preseeded_policy")).unwrap().unwrap(),
        prior
    );
    // The held reader retains the old unresolved navigation publication.
    assert_eq!(
        old.connection()
            .query_row(
                "SELECT resolution FROM links WHERE from_path='pages/guide.md'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        old_link
    );
}

#[test]
fn capture_preserves_agent_claimed_retrieval_provenance() {
    let fixture = Fixture::new(|_| {});
    let mut input = request(b"Host acquired observation.");
    input.origin_kind = SourceOrigin::AgentReport;
    input.origin = "https://example.test/public-evidence".into();
    let plan = SourceStore::new(fixture.fs.clone())
        .plan_agent_capture(input, Some("2026-10-04T00:00:00Z"))
        .unwrap();
    let revision = plan.revision_id.clone();
    fixture.apply(plan);
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let row = reader.record(&revision).unwrap().unwrap();
    assert_eq!(
        row.record.string("origin_retrieved_at"),
        Some("2026-10-04T00:00:00Z")
    );
    assert_eq!(
        row.record.string("origin_retrieved_at_kind"),
        Some("agent-claimed")
    );
}

#[test]
fn capture_rejects_arbitrary_overlay_envelopes_assets_and_ordering() {
    for variant in 0..12 {
        let fixture = Fixture::new(|_| {});
        let mut plan = fixture.plan(request(b"Quote."));
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
                let op = plan
                    .draft
                    .as_mut()
                    .unwrap()
                    .operations
                    .iter_mut()
                    .find(|op| op.target.as_str().ends_with("/original.bin"))
                    .unwrap();
                op.proposed = Some(b"Tampered original".to_vec());
            }
            6 => {
                let op = plan
                    .draft
                    .as_mut()
                    .unwrap()
                    .operations
                    .iter_mut()
                    .find(|op| op.target.as_str().ends_with("/content.md"))
                    .unwrap();
                op.proposed = Some(b"Tampered content".to_vec());
            }
            7 => change_record(&mut plan, RecordKind::Source, |fields| {
                fields.insert("aliases".into(), json!(["unadmitted"]));
            }),
            8 => change_record(&mut plan, RecordKind::Revision, |fields| {
                fields.insert("wiki_depends_on_ids".into(), json!(["page_unknown"]));
            }),
            9 => plan
                .draft
                .as_mut()
                .unwrap()
                .operations
                .iter_mut()
                .find(|op| op.target.as_str().ends_with("/source.md"))
                .unwrap()
                .apply_after
                .clear(),
            10 => {
                let op = plan
                    .draft
                    .as_mut()
                    .unwrap()
                    .operations
                    .iter_mut()
                    .find(|op| op.target.as_str().ends_with("/content.md"))
                    .unwrap();
                op.target = path("pages/unrelated.md");
            }
            11 => change_record(&mut plan, RecordKind::Revision, |fields| {
                fields.insert("title".into(), json!("Wrong revision title"));
            }),
            _ => unreachable!(),
        }
        assert!(fixture.project(plan).is_err(), "variant {variant}");
        assert!(
            !fixture.fs.root().path().join("sources").exists(),
            "variant {variant}"
        );
        fixture.oracle();
    }
}

#[test]
fn capture_reserves_both_source_and_revision_ids_and_exact_companion_paths() {
    for reserved_source in [false, true] {
        for via_companion in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            fs::write(
                temp.path().join("WIKI.md"),
                note("vault", "vault_seed", json!({}), b""),
            )
            .unwrap();
            let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
            let plan = SourceStore::new(handle)
                .plan_capture(request(b"Fresh generated quote."))
                .unwrap();
            let reserved = if reserved_source {
                plan.source_id.clone()
            } else {
                plan.revision_id.clone()
            };
            let target = if reserved_source {
                format!("sources/{}/source.md", plan.source_id)
            } else {
                format!(
                    "sources/{}/revisions/{}/revision.md",
                    plan.source_id, plan.revision_id
                )
            };
            let fixture = Fixture::new(|fs_handle| {
                let extra = if via_companion {
                    json!({"wiki_status":"accepted","wiki_subject_id":"entity_one","wiki_subject":format!("[[{target}]]"),"wiki_object_id":"entity_one","wiki_predicate":"uses"})
                } else {
                    json!({"wiki_status":"accepted","wiki_subject_id":reserved,"wiki_object_id":"entity_one","wiki_predicate":"uses"})
                };
                write(
                    fs_handle,
                    "entity.md",
                    &note(
                        "entity",
                        "entity_one",
                        json!({"wiki_status":"active","wiki_entity_type":"component"}),
                        b"",
                    ),
                );
                write(
                    fs_handle,
                    "claim.md",
                    &note("assertion", "assertion_one", extra, b""),
                );
            });
            assert_eq!(
                fixture.project(plan).err().unwrap().code,
                ErrorCode::ContentConflict
            );
            assert!(!fixture.fs.root().path().join("sources").exists());
        }
    }
}

#[test]
fn capture_reserves_invalid_identity_claims_and_physical_asset_collisions() {
    for physical in [false, true] {
        let fixture = Fixture::new(|_| {});
        let plan = fixture.plan(request(b"Quote."));
        let source = plan.source_id.clone();
        let revision = plan.revision_id.clone();
        if physical {
            write(
                &fixture.fs,
                &format!("sources/{source}/revisions/{}/original.bin", revision),
                b"Unindexed bytes must survive.",
            );
        } else {
            // Rebuild the fixture with an invalid readable claim of the exact ID.
            let occupied = Fixture::new(|handle| {
                write(handle,"invalid.md",format!("---\nwiki_schema: '1'\nwiki_id: '{source}'\nwiki_kind: page\ntitle: Invalid\n---\n").as_bytes())
            });
            assert_eq!(
                occupied.project(plan).err().unwrap().code,
                ErrorCode::ContentConflict
            );
            assert!(!occupied.fs.root().path().join("sources").exists());
            continue;
        }
        assert_eq!(
            fixture.project(plan).err().unwrap().code,
            ErrorCode::ContentConflict
        );
        assert_eq!(
            fs::read(fixture.fs.root().path().join(format!(
                "sources/{source}/revisions/{revision}/original.bin"
            )))
            .unwrap(),
            b"Unindexed bytes must survive."
        );
    }
}

#[test]
fn capture_refuses_existing_empty_source_root() {
    let fixture = Fixture::new(|_| {});
    let plan = fixture.plan(request(b"Quote."));
    let root = fixture
        .fs
        .root()
        .path()
        .join(format!("sources/{}", plan.source_id));
    fs::create_dir_all(&root).unwrap();
    assert_eq!(
        fixture.project(plan).err().unwrap().code,
        ErrorCode::ContentConflict
    );
    assert!(root.is_dir());
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}

#[test]
fn capture_selected_projection_ignores_unrelated_physical_edits_and_is_finite() {
    let fixture = Fixture::new(|handle| {
        for n in 0..32 {
            write(
                handle,
                &format!("pages/{n}.md"),
                &note(
                    "page",
                    &format!("page_{n}"),
                    json!({"wiki_status":"reviewed"}),
                    b"Unrelated.",
                ),
            );
        }
    });
    write(
        &fixture.fs,
        "pages/31.md",
        b"Externally changed unselected bytes.",
    );
    let plan = fixture.plan(request(b"Fresh capture."));
    let source = plan.source_id.clone();
    let projected = fixture.project(plan).unwrap();
    assert!(
        projected
            .draft()
            .read_preconditions
            .iter()
            .all(|dependency| dependency.path.as_str() == "WIKI.md"
                || dependency
                    .path
                    .as_str()
                    .starts_with(&format!("sources/{source}/")))
    );
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let limits = RefreshProjectionLimits {
        max_rows: 1,
        ..Default::default()
    };
    let plan = fixture.plan(request(b"Bounded refusal."));
    assert_eq!(
        project_capture(&fixture.fs, &reader, plan, &limits)
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert!(!fixture.fs.root().path().join("sources").exists());
}

struct CaptureSqlFault(PublicationCheckpoint);
impl PublicationFault for CaptureSqlFault {
    fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
        if checkpoint == self.0 {
            Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "injected capture SQL interruption",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn sealed_capture_v3_retains_owner_and_recovers_before_and_after_sql_commit() {
    for committed in [false, true] {
        let fixture = Fixture::new(|_| {});
        let plan = fixture.plan(request(b"Immutable recovery quote."));
        let source = plan.source_id.clone();
        let revision = plan.revision_id.clone();
        let old = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let projected = fixture.project(plan).unwrap();
        let faulty = Catalog::with_options(
            fixture.fs.clone(),
            id("vault_capture"),
            CatalogOptions {
                busy_timeout_ms: 1000,
                fault: Some(std::sync::Arc::new(CaptureSqlFault(if committed {
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
            matches!(&proof.operation,Some(IndexedWriteOperation::SourceCapture {source_id,revision_id}) if source_id==&source && revision_id==&revision)
        );
        let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
        assert_eq!(
            engine
                .apply_indexed_refresh(&fixture.writer, &mut session)
                .unwrap_err()
                .code,
            ErrorCode::RecoveryRequired
        );
        drop(session);
        drop(faulty);
        let observations: Vec<_> = proof
            .after
            .iter()
            .filter(|d| d.path.as_str().starts_with("sources/"))
            .map(|d| {
                let absolute = fixture.fs.root().path().join(d.path.as_str());
                let bytes = fs::read(&absolute).unwrap();
                let modified = fs::metadata(&absolute).unwrap().modified().unwrap();
                (absolute, bytes, modified)
            })
            .collect();
        assert!(old.record(&source).unwrap().is_none());
        let retained = engine
            .load_indexed_refresh_proof(&proof.change)
            .unwrap()
            .unwrap();
        assert_eq!(retained, proof);
        let mut resumed =
            IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, retained).unwrap();
        assert_eq!(
            resumed.phase(),
            if committed {
                IndexedRefreshPhase::AlreadyPublished
            } else {
                IndexedRefreshPhase::AtBase
            }
        );
        let report = engine
            .apply_indexed_refresh(&fixture.writer, &mut resumed)
            .unwrap();
        assert_eq!(report.status, ChangeStatus::Committed);
        drop(resumed);
        assert!(old.record(&source).unwrap().is_none());
        let reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let key = crate::changes::RevisionTreeKey {
            source_component: source.to_string(),
            revision_component: revision.to_string(),
        };
        assert_eq!(
            reader.revision_owner(&key).unwrap(),
            Some(proof.change.clone())
        );
        drop(reader);
        fixture.oracle();
        let mut repeated =
            IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof.clone())
                .unwrap();
        assert_eq!(
            engine
                .apply_indexed_refresh(&fixture.writer, &mut repeated)
                .unwrap(),
            report
        );
        drop(repeated);
        for (absolute, bytes, modified) in observations {
            assert_eq!(fs::read(&absolute).unwrap(), bytes);
            assert_eq!(
                fs::metadata(&absolute).unwrap().modified().unwrap(),
                modified
            );
        }
    }
}

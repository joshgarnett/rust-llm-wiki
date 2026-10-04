use super::*;
use crate::{
    catalog::{
        Catalog, CatalogOptions, DocumentRow, GraphRow, IdentityClaimRow, LinkRow,
        PublicationCheckpoint, PublicationFault, RetrievalSink,
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        policy_facts::{PolicyKind, PolicyRow, PolicyState},
        query_types::QueryReadLimits,
        scan, selector,
        source_refresh::IndexedRefreshSession,
    },
    changes::indexed_refresh::IndexedRefreshPhase,
    changes::{ChangeEngine, ChangeStatus, ExpectedWrite},
    domain::{CanonicalRecord, ErrorCode, RecordId},
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
fn note(kind: &str, name: &str, extra: Value, body: &str) -> Vec<u8> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".to_owned(), json!("1")),
        ("wiki_id".to_owned(), json!(name)),
        ("wiki_kind".to_owned(), json!(kind)),
        ("title".to_owned(), json!(name)),
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
        bytes.extend(format!("{key}: {value}\n").bytes());
    }
    bytes.extend(format!("---\n{body}").bytes());
    bytes
}
fn page(name: &str, extra: Value, body: &str) -> Vec<u8> {
    let mut extra = extra.as_object().unwrap().clone();
    extra.insert("wiki_status".into(), json!("reviewed"));
    note("page", name, Value::Object(extra), body)
}
#[derive(Default)]
struct Sink {
    documents: Vec<DocumentRow>,
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
    fn graph(&mut self, _: GraphRow) -> Result<()> {
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
    fn new(notes: &[(&str, Vec<u8>)]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            note("vault", "vault_pages", json!({}), ""),
        )
        .unwrap();
        let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
        for (name, bytes) in notes {
            let target = temp.path().join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        let identity = BuildIdentity {
            selection: CatalogSelection::new(id("vault_pages"), 1).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&fs_handle, &writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default())
                .unwrap();
        let input = scan::scan_input(&fs_handle, &id("vault_pages")).unwrap();
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
            catalog: Catalog::new(fs_handle.clone(), id("vault_pages")),
            fs: fs_handle,
            writer,
        }
    }
    fn draft(&self, updates: &[(&str, Vec<u8>)]) -> ChangeDraft {
        ChangeDraft {
            title: "Edit selected Pages".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: vec![],
            operations: updates
                .iter()
                .map(|(name, bytes)| ExpectedWrite {
                    target: path(name),
                    expected: fs::read(self.fs.root().path().join(name))
                        .ok()
                        .map_or(ExpectedState::Absent, |bytes| {
                            ExpectedState::Hash(Blake3Hash::digest(bytes))
                        }),
                    proposed: Some(bytes.clone()),
                    apply_after: vec![],
                })
                .collect(),
        }
    }
    fn project(&self, draft: ChangeDraft) -> Result<Option<ProjectedWrite>> {
        let reader = self.catalog.query_snapshot(QueryReadLimits::default())?;
        project_pages(
            &self.fs,
            &reader,
            draft,
            &RefreshProjectionLimits::default(),
        )
    }
    fn apply(&self, updates: &[(&str, Vec<u8>)]) {
        let projected = self.project(self.draft(updates)).unwrap().unwrap();
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
        let input = scan::scan_input(&self.fs, &id("vault_pages")).unwrap();
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
        for doc in sink.documents {
            assert_eq!(
                reader.document(&doc.path).unwrap().as_ref(),
                Some(&doc),
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
        // Incremental replacement changes SQL insertion order. Compare the
        // complete diagnostic multiset, including duplicate counts, with the
        // full projection's canonical ordering rather than incidental row IDs.
        let mut actual_diagnostics = reader.diagnostics(&paths).unwrap();
        actual_diagnostics.sort_by(|a, b| {
            (&a.path, format!("{:?}", a.code), a.details.to_string()).cmp(&(
                &b.path,
                format!("{:?}", b.code),
                b.details.to_string(),
            ))
        });
        assert_eq!(actual_diagnostics, projection.validation.diagnostics);
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
        assert_eq!(
            actual_policy, expected_policy,
            "complete receipt memberships/results/traces"
        );
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
                super::super::eligibility_facts::EligibilityEdge {
                    owner_id: id(&owner),
                    target_id: id(&target),
                    role: serde_json::from_str(&role).unwrap(),
                }
            })
            .collect();
        assert_eq!(actual_edges, projection.facts.edges);
        let actual_links:Vec<LinkRow> = reader.connection().prepare("SELECT from_path,byte_start,target_id,target_path,resolution FROM links ORDER BY from_path,byte_start").unwrap().query_map([],|r| Ok(LinkRow {
            from_path:path(&r.get::<_,String>(0)?),byte_start:r.get::<_,i64>(1)? as u64,target_id:r.get::<_,Option<String>>(2)?.map(|s|id(&s)),target_path:r.get::<_,Option<String>>(3)?.map(|s|path(&s)),resolution:r.get(4)?,
        })).unwrap().collect::<std::result::Result<_,_>>().unwrap();
        sink.links
            .sort_by(|a, b| (&a.from_path, a.byte_start).cmp(&(&b.from_path, b.byte_start)));
        assert_eq!(actual_links, sink.links);
    }
}
#[test]
fn page_create_crosslinks_edit_and_noop_match_complete_rebuild() {
    let fixture = Fixture::new(&[]);
    fixture.apply(&[
        ("pages/a.md", page("page_a", json!({}), "See [[b]].")),
        ("pages/b.md", page("page_b", json!({}), "See [[a]].")),
    ]);
    fixture.apply(&[("pages/a.md", page("page_a", json!({}), "New body [[b]]."))]);
    let unchanged = fixture.draft(&[("pages/a.md", page("page_a", json!({}), "New body [[b]]."))]);
    assert!(fixture.project(unchanged).unwrap().is_none());
}
#[test]
fn named_page_adoption_reactivates_decision_and_propagated_dependent() {
    let decision = |name: &str, input: &str, output: &str| {
        note(
            "decision",
            name,
            json!({"wiki_status":"active","wiki_action":"correct","wiki_created_at":"2026-09-28T00:00:00Z","wiki_input_ids":[input],"wiki_output_ids":[output]}),
            "",
        )
    };
    let fixture = Fixture::new(&[
        ("existing.md", page("page_existing", json!({}), "")),
        (
            "second-output.md",
            page("page_second_output", json!({}), ""),
        ),
        (
            "first.md",
            decision("decision_first", "page_existing", "page_named"),
        ),
        (
            "second.md",
            decision("decision_second", "decision_first", "page_second_output"),
        ),
    ]);
    fixture.oracle();
    {
        let reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let first = reader.record(&id("decision_first")).unwrap().unwrap();
        let second = reader.record(&id("decision_second")).unwrap().unwrap();
        assert_eq!(first.eligibility, Eligibility::Invalid);
        assert!(
            first
                .reasons
                .iter()
                .any(|reason| reason == "invalid_reference:wiki_output_ids")
        );
        assert_eq!(second.eligibility, Eligibility::Invalid);
        assert!(
            second
                .reasons
                .iter()
                .any(|reason| reason == "invalid_referenced_record")
        );
    }
    fixture.apply(&[(
        "named.md",
        page("page_named", json!({}), "The missing output now exists."),
    )]);
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_ne!(
        reader
            .record(&id("decision_first"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Invalid
    );
    assert_ne!(
        reader
            .record(&id("decision_second"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Invalid
    );
}
#[test]
fn adoption_that_enables_invalid_decision_outcome_refuses_before_mutation() {
    let fixture = Fixture::new(&[(
        "accept.md",
        note(
            "decision",
            "decision_accept",
            json!({"wiki_status":"active","wiki_action":"accept","wiki_created_at":"2026-09-28T00:00:00Z","wiki_input_ids":[],"wiki_output_ids":["page_named"]}),
            "",
        ),
    )]);
    assert!(
        fixture
            .project(fixture.draft(&[("named.md", page("page_named", json!({}), ""))]))
            .is_err()
    );
    assert!(!fixture.fs.root().path().join("named.md").exists());
    fixture.oracle();
}
#[test]
fn two_alias_removals_from_three_candidates_resolve_the_surviving_owner() {
    let fixture = Fixture::new(&[
        ("a.md", page("page_a", json!({"aliases":["shared"]}), "")),
        ("b.md", page("page_b", json!({"aliases":["shared"]}), "")),
        ("c.md", page("page_c", json!({"aliases":["shared"]}), "")),
        ("nav.md", page("page_nav", json!({}), "[[shared]]")),
    ]);
    fixture.apply(&[
        ("a.md", page("page_a", json!({}), "")),
        ("b.md", page("page_b", json!({}), "")),
    ]);
}
#[test]
fn declared_support_removal_and_restoration_match_rebuild() {
    let fixture = Fixture::new(&[
        (
            "entity.md",
            note(
                "entity",
                "entity_one",
                json!({"wiki_status":"active","wiki_entity_type":"component"}),
                "",
            ),
        ),
        (
            "claim.md",
            note(
                "assertion",
                "assertion_one",
                json!({"wiki_status":"accepted","wiki_subject_id":"entity_one","wiki_object_id":"entity_one","wiki_predicate":"uses"}),
                "",
            ),
        ),
        (
            "page.md",
            page(
                "page_one",
                json!({"wiki_depends_on_ids":["assertion_one"]}),
                "",
            ),
        ),
    ]);
    fixture.apply(&[("page.md", page("page_one", json!({}), ""))]);
    fixture.apply(&[(
        "page.md",
        page(
            "page_one",
            json!({"wiki_depends_on_ids":["assertion_one"]}),
            "",
        ),
    )]);
}
#[test]
fn wrong_guard_invalid_kind_and_duplicate_identity_refuse() {
    let fixture = Fixture::new(&[("one.md", page("page_one", json!({}), "before"))]);
    let mut draft = fixture.draft(&[("one.md", page("page_one", json!({}), "after"))]);
    draft.operations[0].expected = ExpectedState::Hash(Blake3Hash::digest(b"wrong"));
    assert_eq!(
        fixture.project(draft).err().unwrap().code,
        ErrorCode::ContentConflict
    );
    let draft = fixture.draft(&[("other.md", page("page_one", json!({}), "duplicate"))]);
    assert!(fixture.project(draft).is_err());
    let draft = fixture.draft(&[
        (
            "one.md",
            page(
                "page_one",
                json!({"wiki_depends_on_ids":["page_other"]}),
                "",
            ),
        ),
        ("other.md", page("page_other", json!({}), "")),
    ]);
    assert!(fixture.project(draft).is_err());
    fixture.oracle();
}
#[test]
fn unchanged_page_is_a_guard_not_a_manifest_write() {
    let bytes = page("page_one", json!({}), "before");
    let fixture = Fixture::new(&[("one.md", bytes.clone())]);
    let projected = fixture
        .project(fixture.draft(&[
            ("one.md", bytes),
            ("two.md", page("page_two", json!({}), "new")),
        ]))
        .unwrap()
        .unwrap();
    assert_eq!(projected.parts.draft.operations.len(), 1);
    assert!(
        projected
            .parts
            .before
            .iter()
            .any(|d| d.path == path("one.md"))
    );
    assert!(
        matches!(projected.parts.operation,IndexedWriteOperation::PageBatch { pages } if pages.len()==1 && pages[0].id==id("page_two"))
    );
}

#[test]
fn adopting_missing_source_id_refreshes_immutable_integrity_error_details() {
    use crate::sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore};
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        note("vault", "vault_seed", json!({}), ""),
    )
    .unwrap();
    let source_fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let capture = SourceStore::new(source_fs)
        .plan_capture(CaptureRequest {
            title: "Selected immutable text".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.txt".into(),
            original: b"immutable quote".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let source_id = capture.source_id.clone();
    let revision_id = capture.revision_id.clone();
    let files: Vec<_> = capture
        .draft
        .unwrap()
        .operations
        .into_iter()
        .filter_map(|op| {
            let bytes = op.proposed.unwrap();
            if parse_note(&bytes)
                .canonical
                .as_ref()
                .is_some_and(|r| r.kind() == RecordKind::Source)
            {
                None
            } else {
                Some((op.target.to_string(), bytes))
            }
        })
        .collect();
    let fixtures: Vec<_> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.clone()))
        .collect();
    let fixture = Fixture::new(&fixtures);
    fixture.apply(&[(
        "named-source.md",
        page(
            source_id.as_str(),
            json!({}),
            "A Page adopts an absent source identity.",
        ),
    )]);
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let fact = reader.eligibility_fact(&revision_id).unwrap().unwrap();
    let details = fact
        .structural
        .effects
        .iter()
        .find(|e| e.reason == "revision_integrity")
        .unwrap()
        .details
        .as_ref()
        .unwrap()
        .to_string();
    assert!(
        details.contains("kind or companion path disagrees"),
        "{details}"
    );
}

#[test]
fn fixed_page_edit_retains_only_selected_files_and_vault_marker() {
    let files: Vec<_> = (0..64)
        .map(|n| {
            (
                format!("pages/{n}.md"),
                page(&format!("page_{n}"), json!({}), "unrelated body"),
            )
        })
        .collect();
    let fixture = Fixture::new(
        &files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.clone()))
            .collect::<Vec<_>>(),
    );
    let projected = fixture
        .project(fixture.draft(&[("pages/0.md", page("page_0", json!({}), "updated body"))]))
        .unwrap()
        .unwrap();
    assert_eq!(
        projected
            .parts
            .before
            .iter()
            .map(|d| d.path.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([path("WIKI.md"), path("pages/0.md")])
    );
}

#[test]
fn bounded_closure_refuses_before_writing() {
    let fixture = Fixture::new(&[]);
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let limits = RefreshProjectionLimits {
        max_rows: 1,
        ..Default::default()
    };
    let result = project_pages(
        &fixture.fs,
        &reader,
        fixture.draft(&[("new.md", page("page_new", json!({}), "new"))]),
        &limits,
    );
    assert_eq!(result.err().unwrap().code, ErrorCode::BudgetExceeded);
    assert!(!fixture.fs.root().path().join("new.md").exists());
}

#[test]
fn page_then_source_refresh_retains_complete_policy_oracle() {
    use crate::sources::{
        CaptureRequest, ExtractionInput, SourceOrigin, SourceRefreshLimits, SourceStore,
    };
    let request = |text: &[u8]| CaptureRequest {
        title: "Capture and author".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "fixture.txt".into(),
        original: text.to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: None,
    };
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        note("vault", "vault_seed", json!({}), ""),
    )
    .unwrap();
    let source_fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let capture = SourceStore::new(source_fs)
        .plan_capture(request(b"first quote"))
        .unwrap();
    let source_id = capture.source_id.clone();
    let files: Vec<_> = capture
        .draft
        .unwrap()
        .operations
        .into_iter()
        .map(|op| (op.target.to_string(), op.proposed.unwrap()))
        .collect();
    let fixture = Fixture::new(
        &files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.clone()))
            .collect::<Vec<_>>(),
    );
    fixture.apply(&[(
        "authored.md",
        page("page_authored", json!({}), "An authored observation."),
    )]);
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let plan = SourceStore::new(fixture.fs.clone())
        .plan_refresh_indexed(
            &reader,
            &source_id,
            request(b"second quote"),
            None,
            &SourceRefreshLimits::default(),
        )
        .unwrap();
    let projected = source_projection::project_refresh(
        &fixture.fs,
        &reader,
        plan,
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap();
    drop(reader);
    let mut session =
        IndexedRefreshSession::prepare_projected(&fixture.catalog, &fixture.writer, projected)
            .unwrap();
    assert_eq!(
        ChangeEngine::new(fixture.fs.clone())
            .unwrap()
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .unwrap()
            .status,
        ChangeStatus::Committed
    );
    drop(session);
    fixture.oracle();
}

#[test]
fn new_page_at_missing_typed_companion_path_refuses() {
    let fixture = Fixture::new(&[
        (
            "entity.md",
            note(
                "entity",
                "entity_one",
                json!({"wiki_status":"active","wiki_entity_type":"component"}),
                "",
            ),
        ),
        (
            "claim.md",
            note(
                "assertion",
                "assertion_one",
                json!({"wiki_status":"accepted","wiki_subject_id":"entity_one","wiki_subject":"[[future.md]]","wiki_object_id":"entity_one","wiki_predicate":"uses"}),
                "",
            ),
        ),
    ]);
    let result = fixture.project(fixture.draft(&[(
        "future.md",
        page(
            "page_future",
            json!({}),
            "A different identity occupies the companion path.",
        ),
    )]));
    assert!(result.is_err());
    assert!(!fixture.fs.root().path().join("future.md").exists());
    fixture.oracle();
}

#[test]
fn named_adoption_with_overlapping_decision_outcomes_matches_full_oracle_refusal() {
    let decision = |name: &str, input: &str, output: &str| {
        note(
            "decision",
            name,
            json!({"wiki_status":"active","wiki_action":"correct","wiki_created_at":"2026-09-28T00:00:00Z","wiki_input_ids":[input],"wiki_output_ids":[output]}),
            "",
        )
    };
    let fixture = Fixture::new(&[
        ("existing.md", page("page_existing", json!({}), "")),
        (
            "first.md",
            decision("decision_first", "page_existing", "page_named"),
        ),
        (
            "second.md",
            decision("decision_second", "decision_first", "page_existing"),
        ),
    ]);
    let draft = fixture.draft(&[(
        "named.md",
        page("page_named", json!({}), "The missing output now exists."),
    )]);
    let mut input = scan::scan_input(&fixture.fs, &id("vault_pages")).unwrap();
    input.overlay = draft
        .operations
        .iter()
        .map(|op| crate::changes::ProposedTarget {
            path: op.target.clone(),
            bytes: op.proposed.clone(),
        })
        .collect();
    let mut sink = Sink::default();
    let proposed =
        scan::project_normalized_with_sink(&fixture.fs, &input, false, &mut sink).unwrap();
    let first = &proposed.validation.records[&id("decision_first")];
    assert_eq!(first.eligibility, Eligibility::Invalid);
    assert!(
        first
            .reasons
            .iter()
            .any(|reason| reason == "conflicting_active_decisions")
    );
    let error = fixture.project(draft).err().unwrap();
    assert_eq!(error.code, ErrorCode::RecordInvalid);
    assert!(error.message.contains("conflicting_active_decisions"));
    assert!(!fixture.fs.root().path().join("named.md").exists());
    fixture.oracle();
}

struct PageSqlFault(PublicationCheckpoint);
impl PublicationFault for PageSqlFault {
    fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
        if checkpoint == self.0 {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "injected sealed Page publication interruption",
            ));
        }
        Ok(())
    }
}

/// Exercise real semantic admission and retained v3 replay, rather than raw
/// row-action fixtures. Both cuts happen after the canonical files were written.
fn page_policy_native_recovery(committed: bool, remove_receipt: bool) {
    let receipt = format!(
        "```{}\n{{}}\n```\n",
        crate::graph::review_types::GRAPH_REVIEW_FENCE
    );
    let before = page(
        "page_recovery",
        json!({}),
        if remove_receipt { &receipt } else { "Before." },
    );
    let fixture = Fixture::new(&[("pages/one.md", before)]);
    let old = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let old_row = old.record(&id("page_recovery")).unwrap().unwrap();
    let old_document = old.document(&path("pages/one.md")).unwrap().unwrap();
    let old_policy = old.policy_state(PolicyKind::Review).unwrap();
    if remove_receipt {
        assert!(matches!(&old_policy, PolicyState::Failed(_)));
        assert_eq!(old_row.eligibility, Eligibility::Invalid);
        assert!(
            old_row
                .reasons
                .iter()
                .any(|r| r == "review_receipt_invalid")
        );
    } else {
        assert_eq!(old_policy, PolicyState::Absent);
        assert_eq!(old_row.eligibility, Eligibility::Current);
    }

    // Introducing a wrong-record receipt fence would invalidate a Page and is
    // correctly refused. Seed that invalid canonical input externally, then
    // exercise the accepted write that removes the fence and restores the Page.
    // The other case is an ordinary valid edit with no receipt candidates.
    let after_body = if remove_receipt {
        "After removal. [[page_second]]".to_owned()
    } else {
        "After ordinary edit. [[page_second]]".to_owned()
    };
    let after = page("page_recovery", json!({}), &after_body);
    let second = page("page_second", json!({}), "See [[page_recovery]].");
    let projected = fixture
        .project(fixture.draft(&[
            ("pages/one.md", after.clone()),
            ("pages/two.md", second.clone()),
        ]))
        .unwrap()
        .unwrap();
    let faulty = Catalog::with_options(
        fixture.fs.clone(),
        id("vault_pages"),
        CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(PageSqlFault(if committed {
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
    assert!(proof.source_id.is_none());
    assert!(matches!(
        &proof.operation,
        Some(IndexedWriteOperation::PageBatch { pages }) if pages.len() == 2
    ));
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    let error = engine
        .apply_indexed_refresh(&fixture.writer, &mut session)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    let interrupted_phase = if committed {
        IndexedRefreshPhase::AlreadyPublished
    } else {
        IndexedRefreshPhase::AtBase
    };
    assert_eq!(session.phase(), interrupted_phase);
    drop(session);
    drop(faulty);

    let observed_files: Vec<_> = ["pages/one.md", "pages/two.md"]
        .into_iter()
        .zip([&after, &second])
        .map(|(name, expected)| {
            let target = fixture.fs.root().path().join(name);
            assert_eq!(fs::read(&target).unwrap(), *expected);
            let modified = fs::metadata(&target).unwrap().modified().unwrap();
            (target, expected.clone(), modified)
        })
        .collect();
    let interrupted = fixture.catalog.operation_state().unwrap().unwrap();
    assert!(interrupted.active().is_some());
    assert!(
        engine
            .indexed_refresh_terminal_outcome(&proof.change)
            .unwrap()
            .is_none()
    );
    // Native SQLite transactions retain the old complete generation at both
    // cuts; fresh transactions see the base before commit and intended after it.
    assert_eq!(old.snapshot(), &proof.base);
    assert_eq!(
        old.record(&id("page_recovery")).unwrap(),
        Some(old_row.clone())
    );
    assert_eq!(
        old.document(&path("pages/one.md")).unwrap(),
        Some(old_document.clone())
    );
    assert!(old.record(&id("page_second")).unwrap().is_none());
    assert_eq!(old.policy_state(PolicyKind::Review).unwrap(), old_policy);
    {
        let reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert_eq!(
            reader.snapshot(),
            if committed {
                &proof.intended
            } else {
                &proof.base
            }
        );
        assert_eq!(
            reader.record(&id("page_second")).unwrap().is_some(),
            committed
        );
    }

    // Reopen the engine and load its durable proof, not a newly constructed
    // candidate. Recovery must work from the SQL-published state without base.
    drop(engine);
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    let retained = engine
        .load_indexed_refresh_proof(&proof.change)
        .unwrap()
        .unwrap();
    assert_eq!(retained, proof);
    let mut recovered =
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, retained).unwrap();
    assert_eq!(recovered.phase(), interrupted_phase);
    if committed {
        assert!(recovered.starting_ownership_lookup().is_err());
    } else {
        assert_eq!(
            recovered.starting_ownership_lookup().unwrap().snapshot(),
            &proof.base
        );
    }
    let report = engine
        .apply_indexed_refresh(&fixture.writer, &mut recovered)
        .unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    assert_eq!(report.snapshot, Some(proof.intended.clone()));
    drop(recovered);
    let authority = fixture.catalog.operation_state().unwrap().unwrap();
    assert!(authority.active().is_none());
    assert_eq!(authority.publication().epoch, proof.intended.generation);
    {
        let reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert_eq!(reader.snapshot(), &proof.intended);
        assert_eq!(
            reader
                .document(&path("pages/one.md"))
                .unwrap()
                .unwrap()
                .hash,
            Blake3Hash::digest(&after)
        );
        assert!(reader.record(&id("page_second")).unwrap().is_some());
        let policy = reader.policy_state(PolicyKind::Review).unwrap();
        assert_eq!(policy, PolicyState::Absent);
        let row = reader.record(&id("page_recovery")).unwrap().unwrap();
        assert_eq!(row.eligibility, Eligibility::Current);
        assert!(row.reasons.is_empty());
        assert_eq!(old.record(&id("page_recovery")).unwrap(), Some(old_row));
        assert_eq!(
            old.document(&path("pages/one.md")).unwrap(),
            Some(old_document)
        );
        assert_eq!(old.policy_state(PolicyKind::Review).unwrap(), old_policy);
    }
    fixture.oracle();
    let checked = fixture.catalog.check_normalized(&fixture.writer).unwrap();
    assert_eq!(checked.snapshot, proof.intended);

    // Retry terminal replay through its retained proof and through the terminal
    // report path. Neither may rewrite canonical after-images or advance epoch.
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
    assert_eq!(
        engine
            .indexed_refresh_terminal_report(&fixture.writer, &proof.change)
            .unwrap(),
        Some(report)
    );
    assert_eq!(
        fixture.catalog.operation_state().unwrap().unwrap(),
        authority
    );
    for (target, expected, modified) in observed_files {
        assert_eq!(fs::read(&target).unwrap(), expected);
        assert_eq!(fs::metadata(&target).unwrap().modified().unwrap(), modified);
    }
}

#[test]
fn sealed_page_v3_native_precommit_recovery_preserves_policy_and_reader() {
    for remove_receipt in [false, true] {
        page_policy_native_recovery(false, remove_receipt);
    }
}

#[test]
fn sealed_page_v3_native_postcommit_recovery_preserves_policy_and_reader() {
    for remove_receipt in [false, true] {
        page_policy_native_recovery(true, remove_receipt);
    }
}

//! Exact selected-move/reconstruction oracles on disposable native fixtures.
use super::*;
use crate::{
    catalog::{
        Catalog, CatalogOptions, PublicationCheckpoint, PublicationFault,
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        query_types::QueryReadLimits,
        selector,
        source_refresh::IndexedRefreshSession,
    },
    changes::{ChangeEngine, ChangeStatus},
    domain::{ErrorCode, Result},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultRoot, WriterPermit},
};
use rusqlite::types::ValueRef;
use serde_json::{Value, json};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

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
    if kind == "page" {
        fields.insert("wiki_status".into(), json!("reviewed"));
    }
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields {
        bytes.extend(format!("{key}: {value}\n").bytes());
    }
    bytes.extend(format!("---\n{body}").bytes());
    bytes
}
fn page(name: &str, body: &str) -> Vec<u8> {
    note("page", name, json!({"wiki_status":"reviewed"}), body)
}

/// Frozen semantic field mapping. Physical ordinals and publication bookkeeping
/// are excluded; all canonical paths, hashes, claims, facts and terms remain.
pub(crate) fn logical_dump(reader: &QuerySnapshot) -> Value {
    let queries = [
        (
            "records",
            "SELECT id,kind,path,hash,authored_status,eligibility,identity_eligibility,description_eligibility,disputed,row_json FROM records",
        ),
        (
            "documents",
            "SELECT path,record_id,kind,file_hash,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,source_id,owner_revision,eligibility,reasons_json FROM documents",
        ),
        (
            "document_terms",
            "SELECT d.path,v.term,v.col,v.offset FROM documents_vocab v JOIN documents d ON d.doc_row=v.doc",
        ),
        (
            "graph_rows",
            "SELECT target_id,target_kind,name,aliases_json,aliases_text,endpoints,predicate,qualifiers,description FROM graph_rows",
        ),
        (
            "graph_terms",
            "SELECT g.target_id,v.term,v.col,v.offset FROM graph_vocab v JOIN graph_rows g ON g.graph_row=v.doc",
        ),
        (
            "identity_claims",
            "SELECT record_id,path,file_hash,kind FROM identity_claims",
        ),
        (
            "links",
            "SELECT from_path,byte_start,target_id,target_path,resolution FROM links",
        ),
        (
            "diagnostics",
            "SELECT path,record_id,code,details_json FROM diagnostics",
        ),
        (
            "dependencies",
            "SELECT path,expected_hash FROM dependencies",
        ),
        (
            "record_eligibility_facts",
            "SELECT record_id,baseline_json,structural_json FROM record_eligibility_facts",
        ),
        (
            "record_direct_paths",
            "SELECT owner_id,path FROM record_direct_paths",
        ),
        (
            "semantic_edges",
            "SELECT owner_id,target_id,role_json FROM semantic_edges",
        ),
        (
            "link_facts",
            "SELECT from_path,byte_start,raw_destination,typed_id,typed_kind FROM link_facts",
        ),
        (
            "link_match_keys",
            "SELECT kind,value,from_path,byte_start FROM link_match_keys",
        ),
        (
            "registry_match_keys",
            "SELECT kind,value,record_id,path FROM registry_match_keys",
        ),
        (
            "policy_facts",
            "SELECT family,key,owner,value FROM policy_facts",
        ),
        (
            "assertion_navigation_keys",
            "SELECT kind,value,assertion_id FROM assertion_navigation_keys",
        ),
        (
            "opposition_members",
            "SELECT key_json,negated,assertion_id FROM opposition_members",
        ),
        (
            "source_revision_identity",
            "SELECT source_id,revision_id,retained_ordinal,original_hash,content_hash,extractor_fingerprint,extraction_status FROM source_revision_identity",
        ),
        (
            "source_evidence",
            "SELECT source_id,evidence_id,assertion_id FROM source_evidence",
        ),
        (
            "revision_tree_owners",
            "SELECT source_component,revision_component,change_id,manifest_hash FROM revision_tree_owners",
        ),
        (
            "catalog_semantics",
            "SELECT schema_version,vault_id,parser_hash,state,vector_cache_lost,vector_loss_unknown,revision_ownership_version,proof_layout_version FROM catalog_meta",
        ),
    ];
    let mut result = serde_json::Map::new();
    for (name, sql) in queries {
        let mut statement = reader.connection().prepare(sql).unwrap();
        let columns = statement.column_count();
        let json_columns: Vec<_> = statement
            .column_names()
            .into_iter()
            .map(|name| name.ends_with("_json"))
            .collect();
        let mut rows = statement.query([]).unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            reader
                .reserve_experimental_scalar_row(row, columns)
                .unwrap();
            let mut fields = Vec::new();
            for (column, json_column) in json_columns.iter().enumerate() {
                let value = match row.get_ref(column).unwrap() {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => json!(value),
                    ValueRef::Text(raw) => {
                        let text = std::str::from_utf8(raw).unwrap();
                        if *json_column {
                            serde_json::from_str(text).unwrap()
                        } else {
                            json!(text)
                        }
                    }
                    ValueRef::Blob(_) => panic!("unexpected blob in frozen semantic dump"),
                };
                fields.push(value);
            }
            values.push(Value::Array(fields));
        }
        values.sort_by_key(Value::to_string);
        result.insert(name.into(), Value::Array(values));
    }
    Value::Object(result)
}

struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    writer: WriterPermit,
    catalog: Catalog,
    captured: BTreeMap<VaultRelativePath, Vec<u8>>,
}
impl Fixture {
    fn new(notes: &[(&str, Vec<u8>)]) -> Self {
        Self::new_inner(notes, false, false)
    }
    fn with_capture(notes: &[(&str, Vec<u8>)]) -> Self {
        Self::new_inner(notes, true, false)
    }
    fn with_supported_entity(notes: &[(&str, Vec<u8>)]) -> Self {
        Self::new_inner(notes, true, true)
    }
    fn new_inner(notes: &[(&str, Vec<u8>)], capture: bool, support_entity: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            note("vault", "vault_move", json!({}), ""),
        )
        .unwrap();
        for (name, bytes) in notes {
            let target = temp.path().join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
        let mut captured = BTreeMap::new();
        if capture {
            let plan = SourceStore::new(fs.clone())
                .plan_capture(CaptureRequest {
                    title: "Immutable mixed oracle source".into(),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: "disposable mixed oracle input.txt".into(),
                    original: "Captured café 東京 [[pages/guide.md#Anchor]].\n"
                        .as_bytes()
                        .to_vec(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: None,
                })
                .unwrap();
            for operation in plan.draft.unwrap().operations {
                let bytes = operation.proposed.unwrap();
                let target = temp.path().join(operation.target.as_str());
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(&target, &bytes).unwrap();
                captured.insert(operation.target, bytes);
            }
            if support_entity {
                let quote = "Captured café 東京".as_bytes();
                let assertion = note(
                    "assertion",
                    "assertion_route_support",
                    json!({
                        "wiki_status":"accepted", "wiki_subject_id":"entity_route",
                        "wiki_object_id":"entity_route", "wiki_predicate":"uses"
                    }),
                    "Route description support.\n",
                );
                let evidence = note(
                    "evidence",
                    "evidence_route_support",
                    json!({
                        "wiki_status":"active", "wiki_assertion_id":"assertion_route_support",
                        "wiki_source_id":plan.source_id, "wiki_source_revision":plan.revision_id,
                        "wiki_stance":"supports", "wiki_locator_kind":"utf8-bytes",
                        "wiki_span_start":0, "wiki_span_end":quote.len(), "wiki_quote_hash":Blake3Hash::digest(quote)
                    }),
                    &String::from_utf8(
                        crate::sources::evidence::exact_quote_body(quote, "\n", "Route evidence")
                            .unwrap(),
                    )
                    .unwrap(),
                );
                fs::write(temp.path().join("route-support.md"), assertion).unwrap();
                fs::write(temp.path().join("route-evidence.md"), evidence).unwrap();
            }
        }
        let identity = BuildIdentity {
            selection: CatalogSelection::new(id("vault_move"), 1).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&fs, &writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&fs, &writer, identity, BuildLimits::default()).unwrap();
        let input = scan::scan_input(&fs, &id("vault_move")).unwrap();
        let projected =
            scan::project_normalized_with_sink(&fs, &input, false, &mut builder).unwrap();
        let completed = builder.finish_normalized(&projected).unwrap();
        selector::publish(&fs, &writer, &completed.identity.selection, Duration::ZERO).unwrap();
        Self {
            catalog: Catalog::new(fs.clone(), id("vault_move")),
            fs,
            writer,
            _temp: temp,
            captured,
        }
    }
    fn project(&self, to: &str) -> Result<Option<ProjectedWrite>> {
        let reader = self.catalog.query_snapshot(QueryReadLimits::default())?;
        let hash =
            Blake3Hash::digest(fs::read(self.fs.root().path().join("pages/guide.md")).unwrap());
        project_page_rename(
            &self.fs,
            &reader,
            id("page_guide"),
            path(to),
            hash,
            &RefreshProjectionLimits::default(),
        )
    }
    fn apply(&self, projected: ProjectedWrite) {
        let mut session =
            IndexedRefreshSession::prepare_write(&self.catalog, &self.writer, projected).unwrap();
        let result = ChangeEngine::new(self.fs.clone())
            .unwrap()
            .apply_indexed_refresh(&self.writer, &mut session)
            .unwrap();
        assert_eq!(result.status, ChangeStatus::Committed);
    }
    fn oracle(&self) {
        let before = logical_dump(
            &self
                .catalog
                .query_snapshot(QueryReadLimits::default())
                .unwrap(),
        );
        self.catalog.rebuild_normalized(&self.writer).unwrap();
        let rebuilt = logical_dump(
            &self
                .catalog
                .query_snapshot(QueryReadLimits::default())
                .unwrap(),
        );
        for (name, expected) in before.as_object().unwrap() {
            assert_eq!(
                &rebuilt[name], expected,
                "complete semantic relation {name}"
            );
        }
    }
}

#[test]
fn mixed_page_move_matches_exact_fresh_rebuild() {
    let target = note(
        "page",
        "page_guide",
        json!({"aliases":["GuideAlias","SharedAlias"]}),
        "# Anchor\nMoveSignal 茶 [[pages/guide.md#Anchor|Self label]].\n",
    );
    let incoming = page(
        "page_incoming",
        "[[pages/guide.md#Anchor|Retained label]]\n[inline](pages/guide.md#Anchor \"Kept title\")\n[reference][guide]\n\n[guide]: pages/guide.md#Anchor \"Definition title\"\n\n[[GuideAlias]] [[SharedAlias]]\n`[[pages/guide.md]]`\n```text\n[[pages/guide.md]]\n```\n",
    );
    let plain = "Plain café [[pages/guide.md#Anchor|Plain label]]. Unrelated bytes.\n"
        .as_bytes()
        .to_vec();
    // A readable but unadopted Decision has a real document kind, despite no
    // canonical record; this is ordinary mutable incoming Markdown.
    let invalid = b"---\nwiki_schema: '1'\nwiki_id: decision_invalid\nwiki_kind: decision\ntitle: Invalid envelope\n# Preserve this comment\n---\nReadable [[pages/guide.md]].\n".to_vec();
    let untouched = page("page_unrelated", "Exact unrelated body.");
    let fixture = Fixture::with_supported_entity(&[
        ("pages/guide.md", target),
        ("pages/incoming.md", incoming),
        ("plain.md", plain),
        ("invalid.md", invalid),
        (
            "knowledge/entities/route.md",
            note(
                "entity",
                "entity_route",
                json!({"wiki_status":"active","wiki_entity_type":"concept","wiki_depends_on_ids":["assertion_route_support"]}),
                "# Route concept\nEntity body [[pages/guide.md]] café.\n",
            ),
        ),
        (
            "unadopted-extraction.md",
            note(
                "extraction",
                "extraction_invalid",
                json!({}),
                "[[pages/guide.md]]\nRetained extraction text.\n",
            ),
        ),
        (
            "unadopted-run.md",
            note(
                "run",
                "run_invalid",
                json!({}),
                "[[pages/guide.md]]\nRetained run text.\n",
            ),
        ),
        (
            "unadopted-change.md",
            note(
                "change",
                "change_invalid",
                json!({}),
                "[[pages/guide.md]]\nRetained change text.\n",
            ),
        ),
        ("pages/unrelated.md", untouched.clone()),
        (
            "pages/homonym.md",
            note(
                "page",
                "page_homonym",
                json!({"aliases":["SharedAlias"]}),
                "Homonym.",
            ),
        ),
        (
            "elsewhere/Operations Guide.md",
            page("page_destination_name", "Distractor."),
        ),
        (
            "missing.md",
            b"[[handbook/Operations Guide.md]] [[Operations Guide]]".to_vec(),
        ),
    ]);
    let parts = fixture
        .project("handbook/Operations Guide.md")
        .unwrap()
        .unwrap()
        .into_parts();
    assert!(parts.before.iter().any(|d| d.path == path("plain.md")));
    assert!(parts.before.iter().any(|d| d.path == path("invalid.md")));
    let plain_link = &parts
        .delta
        .links
        .iter()
        .find(|owned| owned.path == path("plain.md"))
        .unwrap()
        .rows[0];
    assert_eq!(plain_link.target_id, Some(id("page_guide")));
    assert_eq!(
        plain_link.target_path,
        Some(path("handbook/Operations Guide.md"))
    );
    let entity = parts
        .delta
        .records
        .iter()
        .find(|r| r.record.id() == &id("entity_route"))
        .unwrap();
    assert!(
        entity.record.field("description").is_none(),
        "graph description must be derived from body"
    );
    let graph = parts
        .delta
        .graph
        .iter()
        .find(|r| r.target_id == id("entity_route"))
        .expect("changed Entity body must emit updated graph description");
    assert!(graph.description.contains("handbook/Operations Guide.md"));
    assert!(!graph.description.contains("pages/guide.md"));
    for (name, kind) in [
        ("invalid.md", RecordKind::Decision),
        ("unadopted-extraction.md", RecordKind::Extraction),
        ("unadopted-run.md", RecordKind::Run),
        ("unadopted-change.md", RecordKind::Change),
    ] {
        let DocumentMutation::Put { row } = parts
            .delta
            .documents
            .iter()
            .find(|d| matches!(d, DocumentMutation::Put { row } if row.path == path(name)))
            .unwrap()
        else {
            panic!("unadopted incoming owner must render exact replacement")
        };
        assert!(row.record_id.is_none());
        assert_eq!(row.kind, Some(kind));
        assert!(row.raw_text.contains("[[handbook/Operations Guide.md]]"));
        assert!(
            parts
                .delta
                .facts
                .as_ref()
                .unwrap()
                .policy
                .as_ref()
                .unwrap()
                .memberships
                .iter()
                .any(|m| m.path == path(name) && m.hash == row.hash)
        );
    }
    fixture.apply(ProjectedWrite::from_parts(parts));
    for (path, bytes) in &fixture.captured {
        assert_eq!(
            &fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
            bytes,
            "captured source/revision files and old-path text must remain byte-exact"
        );
    }
    assert!(!fixture.fs.root().path().join("pages/guide.md").exists());
    let after = fs::read_to_string(fixture.fs.root().path().join("pages/incoming.md")).unwrap();
    assert!(after.contains("[[handbook/Operations Guide.md#Anchor|Retained label]]"));
    assert!(after.contains("[inline](<handbook/Operations Guide.md#Anchor> \"Kept title\")"));
    assert!(after.contains("[guide]: <handbook/Operations Guide.md#Anchor> \"Definition title\""));
    assert!(after.contains("[[SharedAlias]]"));
    assert!(after.contains("`[[pages/guide.md]]`"));
    assert!(after.contains("```text\n[[pages/guide.md]]\n```"));
    assert_eq!(
        fs::read(fixture.fs.root().path().join("pages/unrelated.md")).unwrap(),
        untouched
    );
    assert!(
        fs::read_to_string(fixture.fs.root().path().join("invalid.md"))
            .unwrap()
            .contains("# Preserve this comment")
    );
    fixture.oracle();
}

#[test]
fn page_move_many_incoming_owners_loads_affected_policy_certificate() {
    let mut files = vec![("pages/guide.md".to_owned(), page("page_guide", "Guide."))];
    for n in 0..20 {
        files.push((
            format!("pages/incoming-{n:02}.md"),
            page(&format!("page_incoming_{n}"), "[[pages/guide.md]]"),
        ));
    }
    let fence = crate::graph::review_types::GRAPH_REVIEW_FENCE;
    files.push((
        "policy-changed.md".into(),
        format!("[[pages/guide.md]]\n```{fence}\n{{}}\n```\n").into_bytes(),
    ));
    // This untouched nonempty indexed certificate must actually be loaded by
    // the affected review recomputation, with >16 replaced owner exclusions.
    files.push((
        "policy-external.md".into(),
        format!("```{fence}\n{{}}\n```\n").into_bytes(),
    ));
    let fixture = Fixture::new(
        &files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.clone()))
            .collect::<Vec<_>>(),
    );
    let parts = fixture
        .project("handbook/Guide.md")
        .unwrap()
        .unwrap()
        .into_parts();
    assert!(matches!(&parts.operation,
        IndexedWriteOperation::PageRename { rewritten_paths, .. } if rewritten_paths.len() == 21));
    for n in 0..20 {
        let owner = path(&format!("pages/incoming-{n:02}.md"));
        let link = &parts
            .delta
            .links
            .iter()
            .find(|links| links.path == owner)
            .unwrap()
            .rows[0];
        assert_eq!(link.target_id, Some(id("page_guide")));
        assert_eq!(link.target_path, Some(path("handbook/Guide.md")));
    }
    assert!(
        parts
            .before
            .iter()
            .any(|d| d.path == path("policy-external.md")),
        "nonempty indexed policy certificate was not loaded"
    );
    assert!(
        parts
            .delta
            .facts
            .as_ref()
            .unwrap()
            .policy
            .as_ref()
            .unwrap()
            .replacements
            .iter()
            .any(|r| r.kind == super::super::policy_facts::PolicyKind::Review)
    );
    fixture.apply(ProjectedWrite::from_parts(parts));
    fixture.oracle();
}

#[test]
fn page_move_guards_noop_immutable_and_budget_before_staging() {
    let fixture = Fixture::new(&[
        ("pages/guide.md", page("page_guide", "Guide.")),
        ("incoming.md", b"[[pages/guide.md]]".to_vec()),
        ("occupied.md", page("page_occupied", "Occupied.")),
    ]);
    assert!(fixture.project("pages/guide.md").unwrap().is_none());
    assert_eq!(
        fixture.project("occupied.md").err().unwrap().code,
        ErrorCode::ContentConflict
    );
    // A matching sealed move cannot silently omit retirement of any old-owned
    // relation, even when its old relation happened to be empty.
    let exact = fixture
        .project("new.md")
        .unwrap()
        .unwrap()
        .into_parts()
        .delta;
    exact.validate().unwrap();
    for category in ["claims", "diagnostics", "links", "link_facts"] {
        let mut omitted = exact.clone();
        let old = path("pages/guide.md");
        match category {
            "claims" => omitted.claims.retain(|o| o.path != old),
            "diagnostics" => omitted.diagnostics.retain(|o| o.path != old),
            "links" => omitted.links.retain(|o| o.path != old),
            "link_facts" => omitted
                .facts
                .as_mut()
                .unwrap()
                .links
                .retain(|o| o.path != old),
            _ => unreachable!(),
        }
        let failure = omitted
            .validate()
            .err()
            .expect("missing old-owner clear must refuse");
        assert_eq!(failure.code, ErrorCode::IndexCorrupt, "{category}");
        assert!(
            failure.message.contains("retire old owned rows"),
            "{category}: {failure:?}"
        );
    }
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let bad = project_page_rename(
        &fixture.fs,
        &reader,
        id("page_guide"),
        path("new.md"),
        Blake3Hash::digest(b"wrong"),
        &RefreshProjectionLimits::default(),
    );
    assert_eq!(bad.err().unwrap().code, ErrorCode::ContentConflict);
    drop(reader);
    fs::write(
        fixture.fs.root().path().join("incoming.md"),
        b"Unindexed external edit",
    )
    .unwrap();
    assert_eq!(
        fixture.project("new.md").err().unwrap().code,
        ErrorCode::ContentConflict
    );
    assert!(!fixture.fs.root().path().join("new.md").exists());
    let fixture = Fixture::new(&[
        ("pages/guide.md", page("page_guide", "Guide.")),
        (
            "immutable.md",
            b"---\nwiki_kind: run_event\n---\n[[pages/guide.md]]".to_vec(),
        ),
    ]);
    let error = fixture.project("new.md").err().unwrap();
    assert!(error.message.contains("immutable incoming"));
    assert!(!fixture.fs.root().path().join("new.md").exists());
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let hash =
        Blake3Hash::digest(fs::read(fixture.fs.root().path().join("pages/guide.md")).unwrap());
    let limited = project_page_rename(
        &fixture.fs,
        &reader,
        id("page_guide"),
        path("new.md"),
        hash,
        &RefreshProjectionLimits {
            max_rows: 1,
            ..Default::default()
        },
    );
    assert_eq!(limited.err().unwrap().code, ErrorCode::BudgetExceeded);
    assert!(!fixture.fs.root().path().join("changes").exists());
}

struct SqlCut {
    cut: PublicationCheckpoint,
    fired: AtomicBool,
}
impl PublicationFault for SqlCut {
    fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
        if checkpoint == self.cut && !self.fired.swap(true, Ordering::SeqCst) {
            return Err(crate::domain::WikiError::new(
                ErrorCode::RecoveryRequired,
                "Page move SQL cut",
            ));
        }
        Ok(())
    }
}
#[test]
fn page_move_sql_cut_recovery_preserves_old_reader_and_one_identity() {
    for cut in [
        PublicationCheckpoint::AfterPointer,
        PublicationCheckpoint::AfterCommit,
    ] {
        let fixture = Fixture::new(&[
            ("pages/guide.md", page("page_guide", "Guide.")),
            ("incoming.md", b"[[pages/guide.md]]".to_vec()),
        ]);
        let old = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let projected = fixture.project("handbook/Guide.md").unwrap().unwrap();
        let fault = Arc::new(SqlCut {
            cut,
            fired: AtomicBool::new(false),
        });
        let faulty = Catalog::with_options(
            fixture.fs.clone(),
            id("vault_move"),
            CatalogOptions {
                busy_timeout_ms: 1000,
                fault: Some(fault.clone()),
            },
        );
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
        assert!(fault.fired.load(Ordering::SeqCst));
        assert_eq!(
            old.record(&id("page_guide")).unwrap().unwrap().path,
            path("pages/guide.md")
        );
        assert!(old.document(&path("handbook/Guide.md")).unwrap().is_none());
        drop(session);
        drop(faulty);
        let retained = engine
            .load_indexed_refresh_proof(&proof.change)
            .unwrap()
            .unwrap();
        let mut recovered =
            IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, retained).unwrap();
        assert_eq!(
            engine
                .apply_indexed_refresh(&fixture.writer, &mut recovered)
                .unwrap()
                .status,
            ChangeStatus::Committed
        );
        drop(recovered);
        drop(old);
        assert_eq!(
            fixture
                .catalog
                .query_snapshot(QueryReadLimits::default())
                .unwrap()
                .record(&id("page_guide"))
                .unwrap()
                .unwrap()
                .path,
            path("handbook/Guide.md")
        );
        fixture.oracle();
    }
}

/// Public-workflow owner supplies only its disposable vault and evidence path.
/// No labels, target selection or canonical write occurs in this export.
#[test]
#[ignore]
fn export_normalized_page_rename_logical_catalog() {
    let config: Value =
        serde_json::from_str(&std::env::var("LWIKI_PAGE_RENAME_DUMP_JSON").unwrap()).unwrap();
    let root =
        VaultRoot::explicit(std::path::Path::new(config["vault"].as_str().unwrap())).unwrap();
    let fs = VaultFs::new(root);
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let catalog = Catalog::new(fs, engine.vault_id().clone());
    catalog.guard_query().unwrap();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let raw = serde_json::to_vec_pretty(&logical_dump(&reader)).unwrap();
    assert!(raw.len() <= 16 * 1024 * 1024);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(config["output"].as_str().unwrap())
        .unwrap();
    std::io::Write::write_all(&mut file, &raw).unwrap();
}

// Existing native durability hooks at the two Page move filesystem boundaries.
use crate::vault::fs::{DirectorySync, DurableIo, NativeIo};
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug)]
enum MoveFsCut {
    AfterDestinationReplace,
    BeforeOldRemove,
}
struct MoveFaultIo {
    cut: MoveFsCut,
    destination: PathBuf,
    old: PathBuf,
    fired: AtomicBool,
}
impl DurableIo for MoveFaultIo {
    fn create_stage(&self, p: &Path) -> io::Result<fs::File> {
        NativeIo.create_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> io::Result<fs::File> {
        NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> io::Result<()> {
        NativeIo.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> io::Result<fs::File> {
        NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &fs::File, n: u64) -> io::Result<()> {
        NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut fs::File, b: &[u8]) -> io::Result<()> {
        NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &fs::File) -> io::Result<()> {
        NativeIo.sync_file(f)
    }
    fn replace(&self, a: &Path, b: &Path) -> io::Result<()> {
        NativeIo.replace(a, b)?;
        if b == self.destination
            && matches!(self.cut, MoveFsCut::AfterDestinationReplace)
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            return Err(io::Error::other("Page move after destination replace"));
        }
        Ok(())
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        if p == self.old
            && matches!(self.cut, MoveFsCut::BeforeOldRemove)
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            return Err(io::Error::other("Page move before old removal"));
        }
        NativeIo.remove(p)
    }
    fn create_directory(&self, p: &Path) -> io::Result<()> {
        NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
        NativeIo.sync_directory(p)
    }
}

#[test]
fn page_move_filesystem_cuts_recover_through_ordinary_apply() {
    use crate::app::{OfflineApp, OperationOptions};
    for cut in [
        MoveFsCut::AfterDestinationReplace,
        MoveFsCut::BeforeOldRemove,
    ] {
        let incoming_before = b"[[pages/guide.md]]".to_vec();
        let mut fixture = Fixture::with_capture(&[
            ("pages/guide.md", page("page_guide", "Guide.")),
            ("incoming.md", incoming_before.clone()),
        ]);
        let old_reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let projected = fixture.project("handbook/Guide.md").unwrap().unwrap();
        let mut session =
            IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        let proof = session.proof().clone();
        let root = fixture.fs.root().clone();
        let fault = Arc::new(MoveFaultIo {
            cut,
            destination: root.path().join("handbook/Guide.md"),
            old: root.path().join("pages/guide.md"),
            fired: AtomicBool::new(false),
        });
        let engine = ChangeEngine::new(VaultFs::with_io(root.clone(), fault.clone())).unwrap();
        let error = engine
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .err()
            .unwrap();
        assert!(fault.fired.load(Ordering::SeqCst), "{cut:?}: {error:?}");
        // Actual canonical observations distinguish these cuts from an earlier failure.
        assert!(root.path().join("handbook/Guide.md").exists());
        assert!(root.path().join("pages/guide.md").exists());
        let incoming = fs::read(root.path().join("incoming.md")).unwrap();
        match cut {
            MoveFsCut::AfterDestinationReplace => assert_eq!(incoming, incoming_before),
            MoveFsCut::BeforeOldRemove => assert_eq!(incoming, b"[[handbook/Guide.md]]"),
        }
        assert!(
            fixture.catalog.guard_current(None).is_err(),
            "pending move must not advertise current query authority"
        );
        assert_eq!(
            old_reader.record(&id("page_guide")).unwrap().unwrap().path,
            path("pages/guide.md")
        );
        assert!(
            old_reader
                .document(&path("handbook/Guide.md"))
                .unwrap()
                .is_none()
        );
        for (p, b) in &fixture.captured {
            assert_eq!(&fs::read(root.path().join(p.as_str())).unwrap(), b);
        }
        drop(session);
        drop(engine);
        drop(fixture.writer); // ordinary app acquires the production writer itself
        let app = OfflineApp::new(
            VaultFs::new(root.clone()),
            OperationOptions {
                offline: true,
                lock_timeout_ms: 200,
                ..Default::default()
            },
        )
        .unwrap();
        let first = app.changes_apply(proof.change.change_id.clone()).unwrap();
        assert_eq!(first.status, Some(ChangeStatus::Committed));
        let second = app.changes_apply(proof.change.change_id.clone()).unwrap();
        assert_eq!(second.status, Some(ChangeStatus::Committed));
        assert!(second.reused);
        assert_eq!(first.snapshot, second.snapshot);
        assert_eq!(first.snapshot.as_ref(), Some(&proof.intended));
        assert_eq!(
            old_reader.record(&id("page_guide")).unwrap().unwrap().path,
            path("pages/guide.md")
        );
        assert!(!root.path().join("pages/guide.md").exists());
        assert_eq!(
            fs::read(root.path().join("incoming.md")).unwrap(),
            b"[[handbook/Guide.md]]"
        );
        for (p, b) in &fixture.captured {
            assert_eq!(&fs::read(root.path().join(p.as_str())).unwrap(), b);
        }
        let fresh = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert_eq!(
            fresh.record(&id("page_guide")).unwrap().unwrap().path,
            path("handbook/Guide.md")
        );
        assert!(
            fresh
                .unique_identity_claim(&id("page_guide"))
                .unwrap()
                .is_some()
        );
        drop(fresh);
        drop(old_reader);
        drop(app);
        fixture.writer = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        fixture.oracle();
    }
}

//! Connected full projection/build tests for the explicit normalized fact layout.
use super::{
    Catalog, NormalizedValidationProjection,
    eligibility_facts::{EligibilityBaseline, EligibilityEdge, EligibilityRole},
    file_types::{BuildIdentity, CatalogSelection},
    link_facts::{MatchKey, MatchKeyKind},
    normalized_build::{BuildLimits, CompletedCatalog, NormalizedBuilder},
    query_types::{QueryCatalog, QueryReadLimits},
    scan, selector,
};
use crate::{
    changes::{ChangeDraft, ReadDependency},
    domain::{Blake3Hash, Eligibility, ErrorCode, RecordId, VaultRelativePath},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::{Connection, OpenFlags, params};
use std::{collections::BTreeSet, fs, time::Duration};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    writer: WriterPermit,
    source: RecordId,
    historical: RecordId,
    head: RecordId,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_id: vault_fact_build\nwiki_kind: vault\ntitle: Fact build\n---\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
        let store = SourceStore::new(fs.clone());
        let first = store
            .plan_capture(Self::request(b"first immutable capture"))
            .unwrap();
        Self::seed(&fs, first.draft.unwrap());
        let second = store
            .plan_refresh(&first.source_id, Self::request(b"second immutable capture"))
            .unwrap();
        Self::seed(&fs, second.draft.unwrap());
        let result = Self {
            _temp: temp,
            fs,
            writer,
            source: first.source_id,
            historical: first.revision_id,
            head: second.revision_id,
        };
        result.write("unrelated.md", b"---\nwiki_schema: '1'\nwiki_id: page_unrelated\nwiki_kind: page\ntitle: Unrelated\nwiki_status: reviewed\n---\nSee [[revision]] and [[not-yet-present]].\n");
        result
    }
    fn request(bytes: &[u8]) -> CaptureRequest {
        CaptureRequest {
            title: "Captured fixture".into(),
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
    fn catalog(&self) -> Catalog {
        Catalog::new(self.fs.clone(), id("vault_fact_build"))
    }
    fn identity(&self) -> BuildIdentity {
        BuildIdentity {
            selection: CatalogSelection::new(id("vault_fact_build"), 1).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        }
    }
    fn normalized(&self) -> (CompletedCatalog, NormalizedValidationProjection) {
        let identity = self.identity();
        selector::prepare(&self.fs, &self.writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&self.fs, &self.writer, identity, BuildLimits::default())
                .unwrap();
        let input = scan::scan_input(&self.fs, &id("vault_fact_build")).unwrap();
        let projection =
            scan::project_normalized_with_sink(&self.fs, &input, false, &mut builder).unwrap();
        let completed = builder.finish_normalized(&projection).unwrap();
        (completed, projection)
    }
    fn select(&self, completed: &CompletedCatalog) {
        selector::publish(
            &self.fs,
            &self.writer,
            &completed.identity.selection,
            Duration::ZERO,
        )
        .unwrap();
    }
}
fn readonly(completed: &CompletedCatalog) -> Connection {
    Connection::open_with_flags(&completed.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

#[test]
fn full_normalized_build_persists_central_states_baselines_and_local_paths_without_closures() {
    let fixture = Fixture::new();
    let legacy = scan::scan(&fixture.fs, &id("vault_fact_build")).unwrap();
    let (completed, projected) = fixture.normalized();
    let db = readonly(&completed);
    assert_eq!(
        db.query_row("SELECT proof_layout_version FROM catalog_meta", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM record_eligibility_facts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        projected.validation.records.len() as i64
    );
    for (record_id, expected) in &projected.validation.records {
        let json: String = db
            .query_row(
                "SELECT row_json FROM records WHERE id=?1",
                [record_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let stored: super::RecordRow = serde_json::from_str(&json).unwrap();
        assert!(stored.dependencies.is_empty());
        let mut original = legacy.records[record_id].clone();
        original.dependencies.clear();
        assert_eq!(stored, original);
        assert_eq!(&stored, expected);
        let baseline: String = db
            .query_row(
                "SELECT baseline_json FROM record_eligibility_facts WHERE record_id=?1",
                [record_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<EligibilityBaseline>(&baseline).unwrap(),
            projected.facts.records[record_id].baseline
        );
    }
    for (observed_path, state) in &projected.facts.observed {
        let hash: Option<String> = db
            .query_row(
                "SELECT expected_hash FROM dependencies WHERE path=?1",
                [observed_path.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let expected = match state {
            ExpectedState::Absent => None,
            ExpectedState::Hash(hash) => Some(hash.to_string()),
        };
        assert_eq!(hash, expected);
    }
    let local: BTreeSet<String> = db
        .prepare("SELECT path FROM record_direct_paths WHERE owner_id=?1 ORDER BY path")
        .unwrap()
        .query_map([fixture.historical.as_str()], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    let tree = format!(
        "sources/{}/revisions/{}",
        fixture.source, fixture.historical
    );
    assert_eq!(
        local,
        ["revision.md", "original.bin", "content.md"]
            .into_iter()
            .map(|name| format!("{tree}/{name}"))
            .collect()
    );
    assert_eq!(
        projected.facts.records[&fixture.historical]
            .baseline
            .eligibility,
        Eligibility::Current
    );
    assert_eq!(
        projected.validation.records[&fixture.historical].eligibility,
        Eligibility::Historical
    );
    let role = serde_json::to_string(&EligibilityRole::TypedReference {
        field: "wiki_source_id".into(),
    })
    .unwrap();
    let owners: Vec<String> = db.prepare("SELECT owner_id FROM semantic_edges INDEXED BY semantic_dependents WHERE target_id=?1 AND role_json=?2 ORDER BY owner_id").unwrap().query_map(params![fixture.source.as_str(), role], |row| row.get(0)).unwrap().collect::<std::result::Result<_, _>>().unwrap();
    assert_eq!(
        owners.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([fixture.historical.to_string(), fixture.head.to_string()])
    );
    drop(db);
    fixture.select(&completed);
    let reader = fixture
        .catalog()
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert!(
        reader
            .record(&fixture.historical)
            .unwrap()
            .unwrap()
            .dependencies
            .is_empty()
    );
    reader.require_fact_layout().unwrap();
    let fact = reader
        .eligibility_fact(&fixture.historical)
        .unwrap()
        .unwrap();
    assert_eq!(fact, projected.facts.records[&fixture.historical]);
    let paths: Vec<_> = fact.direct_paths.iter().cloned().collect();
    let expected: Vec<_> = paths
        .iter()
        .map(|path| ReadDependency {
            path: path.clone(),
            expected: projected.facts.observed[path].clone(),
        })
        .collect();
    assert_eq!(reader.direct_path_states(&paths).unwrap(), expected);
    let role = EligibilityRole::TypedReference {
        field: "wiki_source_id".into(),
    };
    assert_eq!(
        reader
            .outgoing_edges(&fixture.historical, std::slice::from_ref(&role))
            .unwrap(),
        vec![EligibilityEdge {
            owner_id: fixture.historical.clone(),
            target_id: fixture.source.clone(),
            role: role.clone()
        }]
    );
    let dependents = reader.dependent_edges(&fixture.source, &[role]).unwrap();
    assert_eq!(
        dependents
            .into_iter()
            .map(|edge| edge.owner_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([fixture.historical.clone(), fixture.head.clone()])
    );
}

#[test]
fn normalized_build_indexes_raw_ambiguous_and_missing_links_for_future_membership_changes() {
    let fixture = Fixture::new();
    let (completed, _) = fixture.normalized();
    let db = readonly(&completed);
    let rows: Vec<(String, u64, String)> = db.prepare("SELECT f.raw_destination,f.byte_start,l.resolution FROM link_facts f JOIN links l ON l.from_path=f.from_path AND l.byte_start=f.byte_start WHERE f.from_path='unrelated.md' ORDER BY f.byte_start").unwrap().query_map([], |row| Ok((row.get(0)?,u64::try_from(row.get::<_, i64>(1)?).unwrap(),row.get(2)?))).unwrap().collect::<std::result::Result<_, _>>().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "revision");
    assert!(rows[0].2.starts_with("Ambiguous"));
    assert_eq!(rows[1].0, "not-yet-present");
    assert_eq!(rows[1].2, "Missing");
    assert_eq!(db.query_row("SELECT count(*) FROM link_match_keys WHERE kind='basename' AND value='not-yet-present' AND from_path='unrelated.md'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM registry_match_keys WHERE kind='basename' AND value='revision'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    let bytes = fs::read(fixture.fs.root().path().join("unrelated.md")).unwrap();
    assert_eq!(&bytes[rows[0].1 as usize..][..2], b"[[");
    drop(db);
    fixture.select(&completed);
    let reader = fixture
        .catalog()
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let owner = path("unrelated.md");
    let fact = reader.link_fact(&owner, rows[1].1).unwrap().unwrap();
    assert_eq!(fact.raw_destination, "not-yet-present");
    let absent = MatchKey {
        kind: MatchKeyKind::Basename,
        value: "not-yet-present".into(),
    };
    assert!(fact.keys.contains(&absent));
    assert_eq!(
        reader.affected_links(&[absent.clone()]).unwrap(),
        vec![(owner.clone(), rows[1].1)]
    );
    assert!(
        reader
            .registry_candidates_for_key(&absent)
            .unwrap()
            .is_empty()
    );
    let revision = MatchKey {
        kind: MatchKeyKind::Basename,
        value: "revision".into(),
    };
    let entries = reader.registry_candidates_for_key(&revision).unwrap();
    assert_eq!(
        entries
            .into_iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([fixture.historical.clone(), fixture.head.clone()])
    );
    assert!(
        reader
            .affected_links(&[revision])
            .unwrap()
            .contains(&(owner, rows[0].1))
    );
}

#[test]
fn incomplete_normalized_facts_refuse_completion_and_cannot_be_selected() {
    for variant in 0..6 {
        let fixture = Fixture::new();
        let identity = fixture.identity();
        let selection = identity.selection.clone();
        selector::prepare(&fixture.fs, &fixture.writer, &selection).unwrap();
        let mut builder = NormalizedBuilder::begin(
            &fixture.fs,
            &fixture.writer,
            identity,
            BuildLimits::default(),
        )
        .unwrap();
        let input = scan::scan_input(&fixture.fs, &id("vault_fact_build")).unwrap();
        let mut projection =
            scan::project_normalized_with_sink(&fixture.fs, &input, false, &mut builder).unwrap();
        match variant {
            0 => {
                projection.facts.records.remove(&fixture.source);
            }
            1 => {
                projection.facts.records.insert(
                    id("foreign_fact"),
                    projection.facts.records[&fixture.source].clone(),
                );
            }
            2 => {
                let own = projection.validation.records[&fixture.source].path.clone();
                projection.facts.observed.insert(
                    own,
                    ExpectedState::Hash(Blake3Hash::digest(b"contradictory")),
                );
            }
            3 => {
                projection
                    .facts
                    .records
                    .get_mut(&fixture.source)
                    .unwrap()
                    .direct_paths
                    .insert(path("unobserved.bin"));
            }
            4 => {
                projection
                    .validation
                    .records
                    .get_mut(&fixture.source)
                    .unwrap()
                    .dependencies
                    .push(ReadDependency {
                        path: path("unrelated.md"),
                        expected: ExpectedState::Absent,
                    });
            }
            5 => projection.facts.version = 99,
            _ => unreachable!(),
        }
        assert!(
            builder.finish_normalized(&projection).is_err(),
            "variant {variant}"
        );
        assert!(
            selector::publish(&fixture.fs, &fixture.writer, &selection, Duration::ZERO).is_err(),
            "variant {variant} became selected"
        );
    }
}

#[test]
fn unrelated_growth_does_not_expand_persisted_local_fact_rows() {
    let fixture = Fixture::new();
    let (first, projection) = fixture.normalized();
    let expected = projection.facts.records[&fixture.historical].clone();
    for n in 0..96 {
        fixture.write(&format!("unrelated/{n}.md"), format!("---\nwiki_schema: '1'\nwiki_id: page_extra_{n}\nwiki_kind: page\ntitle: Extra {n}\nwiki_status: reviewed\n---\nPlain unrelated bytes.\n").as_bytes());
    }
    let store = SourceStore::new(fixture.fs.clone());
    for n in 0..4 {
        let plan = store
            .plan_refresh(
                &fixture.source,
                Fixture::request(format!("new immutable {n}").as_bytes()),
            )
            .unwrap();
        Fixture::seed(&fixture.fs, plan.draft.unwrap());
    }
    let (later, projection) = fixture.normalized();
    assert_eq!(expected, projection.facts.records[&fixture.historical]);
    for completed in [&first, &later] {
        let db = readonly(completed);
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM record_direct_paths WHERE owner_id=?1",
                [fixture.historical.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            3
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM record_direct_paths WHERE owner_id='page_unrelated'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}

#[test]
fn compatibility_finish_retains_layout_zero_and_fact_lookup_requires_explicit_rebuild() {
    let fixture = Fixture::new();
    let identity = fixture.identity();
    selector::prepare(&fixture.fs, &fixture.writer, &identity.selection).unwrap();
    let mut builder = NormalizedBuilder::begin(
        &fixture.fs,
        &fixture.writer,
        identity,
        BuildLimits::default(),
    )
    .unwrap();
    let input = scan::scan_input(&fixture.fs, &id("vault_fact_build")).unwrap();
    let projection = scan::project_with_sink(&fixture.fs, &input, false, &mut builder).unwrap();
    let completed = builder.finish(&projection).unwrap();
    let db = readonly(&completed);
    assert_eq!(
        db.query_row("SELECT proof_layout_version FROM catalog_meta", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    drop(db);
    fixture.select(&completed);
    let reader = fixture
        .catalog()
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        reader.require_fact_layout().unwrap_err().code,
        ErrorCode::OfflineUnavailable
    );
    assert_eq!(
        reader.eligibility_fact(&fixture.source).unwrap_err().code,
        ErrorCode::OfflineUnavailable
    );
    // Compatibility serving keeps its original populated closure; it is not
    // silently reinterpreted as a normalized local proof.
    assert!(
        !reader
            .record(&fixture.historical)
            .unwrap()
            .unwrap()
            .dependencies
            .is_empty()
    );
}

#[test]
fn dangling_evidence_preserves_invalid_row_without_inventing_support_owner() {
    let fixture = Fixture::new();
    let quote = b"second immutable capture";
    let mut bytes = format!("---\nwiki_schema: '1'\nwiki_id: evidence_dangling\nwiki_kind: evidence\ntitle: Dangling evidence\nwiki_status: active\nwiki_assertion_id: assertion_absent\nwiki_source_id: {}\nwiki_source_revision: {}\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: 0\nwiki_span_end: {}\nwiki_quote_hash: {}\n---\n", fixture.source, fixture.head, quote.len(), Blake3Hash::digest(quote)).into_bytes();
    bytes.extend(crate::sources::evidence::exact_quote_body(quote, "\n", "Fixture").unwrap());
    fixture.write("evidence.md", &bytes);
    let legacy = scan::scan(&fixture.fs, &id("vault_fact_build")).unwrap();
    let (completed, projected) = fixture.normalized();
    let evidence = id("evidence_dangling");
    let missing = id("assertion_absent");
    let mut original = legacy.records[&evidence].clone();
    original.dependencies.clear();
    assert_eq!(projected.validation.records[&evidence], original);
    assert_eq!(projected.validation.diagnostics, legacy.diagnostics);
    assert_eq!(original.eligibility, Eligibility::Invalid);
    fixture.select(&completed);
    let reader = fixture
        .catalog()
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert!(reader.eligibility_fact(&missing).unwrap().is_none());
    assert!(
        reader
            .outgoing_edges(&missing, &[EligibilityRole::AssertionEvidence])
            .unwrap()
            .is_empty()
    );
    let dangling = reader
        .dependent_edges(
            &missing,
            &[EligibilityRole::TypedReference {
                field: "wiki_assertion_id".into(),
            }],
        )
        .unwrap();
    assert_eq!(dangling.len(), 1);
    assert_eq!(dangling[0].owner_id, evidence);
}

#[test]
fn thousands_of_ambiguous_candidates_do_not_prevent_normalized_build() {
    let fixture = Fixture::new();
    for i in 0..4100 {
        fixture.write(&format!("aliases/page_{i}.md"), format!("---\nwiki_schema: '1'\nwiki_id: alias_{i}\nwiki_kind: page\ntitle: Alias {i}\nwiki_status: reviewed\naliases: [shared-target]\n---\n").as_bytes());
    }
    fixture.write("shared-link.md", b"---\nwiki_schema: '1'\nwiki_id: shared_link\nwiki_kind: page\ntitle: Shared link\nwiki_status: reviewed\n---\n[[shared-target]]\n");
    let (completed, projected) = fixture.normalized();
    assert!(projected.validation.records.len() > 4096);
    fixture.select(&completed);
    let reader = fixture
        .catalog()
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let key = MatchKey {
        kind: MatchKeyKind::Alias,
        value: "shared-target".into(),
    };
    let affected = reader.affected_links(&[key]).unwrap();
    assert_eq!(affected.len(), 1);
    assert_eq!(affected[0].0, path("shared-link.md"));
    let fact = reader
        .link_fact(&affected[0].0, affected[0].1)
        .unwrap()
        .unwrap();
    assert_eq!(fact.keys.len(), 4);
    assert!(fact.keys.iter().all(|key| key.kind != MatchKeyKind::Id));
}

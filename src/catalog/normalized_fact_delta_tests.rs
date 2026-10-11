//! Connected row/fact application over disposable canonical source fixtures.
//! The complete small projector is an oracle only; these tests do not qualify
//! semantic capability admission, epoch publication or change-engine durability.
use super::{
    Catalog,
    eligibility_facts::{EligibilityEdge, EligibilityRole},
    file_types::{BuildIdentity, CatalogSelection},
    link_facts::{self, MatchKey, MatchKeyKind, OwnedLinkFact},
    normalized_build::{BuildLimits, NormalizedBuilder},
    normalized_delta::*,
    normalized_fact_delta::*,
    query_types::{QueryCatalog, QueryReadLimits},
    scan, selector, sql,
    types::*,
};
use crate::{
    changes::ReadDependency,
    domain::{Blake3Hash, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath},
    records::{RegistryEntry, edit_note, parse_note},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourcePlan, SourceStore},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    time::Duration,
};
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn request(text: &str) -> CaptureRequest {
    CaptureRequest {
        title: "Immutable title".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "fixture.md".into(),
        original: text.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/markdown".into()),
    }
}
fn write_plan(fs_handle: &VaultFs, plan: &SourcePlan) {
    for operation in &plan.draft.as_ref().unwrap().operations {
        let absolute = fs_handle.root().path().join(operation.target.as_str());
        fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        fs::write(absolute, operation.proposed.as_ref().unwrap()).unwrap();
    }
}
#[derive(Default)]
struct Rows {
    documents: Vec<DocumentRow>,
    claims: Vec<IdentityClaimRow>,
    links: Vec<LinkRow>,
    raw: Vec<OwnedLinkFact>,
    registry: Vec<OwnedRegistryKeys>,
}
impl RetrievalSink for Rows {
    fn identity_claim(&mut self, row: IdentityClaimRow) -> Result<()> {
        self.claims.push(row);
        Ok(())
    }
    fn document(&mut self, row: DocumentRow) -> Result<()> {
        self.documents.push(row);
        Ok(())
    }
    fn graph(&mut self, _row: GraphRow) -> Result<()> {
        Ok(())
    }
    fn link(&mut self, row: LinkRow) -> Result<()> {
        self.links.push(row);
        Ok(())
    }
    fn link_fact(&mut self, row: OwnedLinkFact) -> Result<()> {
        self.raw.push(row);
        Ok(())
    }
    fn registry_keys(&mut self, entry: &RegistryEntry, keys: &[MatchKey]) -> Result<()> {
        self.registry.push(OwnedRegistryKeys {
            record_id: entry.id.clone(),
            path: entry.path.clone(),
            keys: keys.to_vec(),
        });
        Ok(())
    }
}
fn project(fs_handle: &VaultFs) -> (NormalizedValidationProjection, Rows) {
    let input = scan::scan_input(fs_handle, &id("vault_fact_delta")).unwrap();
    let mut rows = Rows::default();
    let projection =
        scan::project_normalized_with_sink(fs_handle, &input, false, &mut rows).unwrap();
    (projection, rows)
}
struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    catalog: Catalog,
    database: Connection,
    source: RecordId,
    historical: RecordId,
    next: SourcePlan,
    before: NormalizedValidationProjection,
    rows: Rows,
}
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"),"---\nwiki_schema: '1'\nwiki_id: vault_fact_delta\nwiki_kind: vault\ntitle: Fact delta\n---\n").unwrap();
    fs::write(temp.path().join("entity.md"),"---\nwiki_schema: '1'\nwiki_id: entity_fact_delta\nwiki_kind: entity\ntitle: Entity\nwiki_status: active\nwiki_entity_type: component\n---\n").unwrap();
    fs::write(temp.path().join("assertion.md"),"---\nwiki_schema: '1'\nwiki_id: assertion_fact_delta\nwiki_kind: assertion\ntitle: Authored assertion\nwiki_status: accepted\nwiki_subject_id: entity_fact_delta\nwiki_object_id: entity_fact_delta\nwiki_predicate: uses\n---\n").unwrap();
    fs::write(temp.path().join("page.md"),"---\nwiki_schema: '1'\nwiki_id: page_fact_delta\nwiki_kind: page\ntitle: Navigation\nwiki_status: reviewed\n---\n[[revision]]\n").unwrap();
    let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let store = SourceStore::new(fs_handle.clone());
    let first = store.plan_capture(request("firsttoken")).unwrap();
    write_plan(&fs_handle, &first);
    let source = first.source_id.clone();
    let historical = first.revision_id.clone();
    let second = store.plan_refresh(&source, request("secondtoken")).unwrap();
    write_plan(&fs_handle, &second);
    let next = store.plan_refresh(&source, request("thirdtoken")).unwrap();
    let (before, rows) = project(&fs_handle);
    let catalog = Catalog::new(fs_handle.clone(), id("vault_fact_delta"));
    let writer = WriterPermit::acquire(fs_handle.root(), Duration::from_secs(1)).unwrap();
    let identity = BuildIdentity {
        selection: CatalogSelection::new(id("vault_fact_delta"), 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(&fs_handle, &writer, &identity.selection).unwrap();
    let mut builder =
        NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default()).unwrap();
    let input = scan::scan_input(&fs_handle, &id("vault_fact_delta")).unwrap();
    let full = scan::project_normalized_with_sink(&fs_handle, &input, false, &mut builder).unwrap();
    let completed = builder.finish_normalized(&full).unwrap();
    selector::publish(
        &fs_handle,
        &writer,
        &completed.identity.selection,
        Duration::from_secs(1),
    )
    .unwrap();
    let database = Connection::open(completed.path).unwrap();
    drop(writer);
    Fixture {
        _temp: temp,
        fs: fs_handle,
        catalog,
        database,
        source,
        historical,
        next,
        before,
        rows,
    }
}
fn empty() -> CatalogDelta {
    CatalogDelta {
        version: 2,
        facts: Some(FactDelta {
            policy: None,
            records: vec![],
            edge_inserts: vec![],
            edge_deletes: vec![],
            links: vec![],
            registry: vec![],
        }),
        records: vec![],
        documents: vec![],
        graph: vec![],
        links: vec![],
        diagnostics: vec![],
        claims: vec![],
        revisions: vec![],
        dependencies: vec![],
        owners: vec![],
    }
}
fn group<T: Clone>(
    rows: &[T],
    owner: impl Fn(&T) -> &VaultRelativePath,
) -> BTreeMap<VaultRelativePath, Vec<T>> {
    let mut result: BTreeMap<VaultRelativePath, Vec<T>> = BTreeMap::new();
    for row in rows {
        result
            .entry(owner(row).clone())
            .or_default()
            .push(row.clone());
    }
    result
}
fn delta_from_oracle(
    f: &Fixture,
    after: &NormalizedValidationProjection,
    rows: &Rows,
) -> CatalogDelta {
    let mut delta = empty();
    delta.records = after
        .validation
        .records
        .iter()
        .filter(|(id, row)| f.before.validation.records.get(*id) != Some(*row))
        .map(|(_, row)| row.clone())
        .collect();
    for row in &rows.documents {
        let old = f.rows.documents.iter().find(|old| old.path == row.path);
        if old == Some(row) {
            continue;
        }
        let metadata_only = old.is_some_and(|old| {
            let mut copied = old.clone();
            copied.eligibility = row.eligibility;
            copied.reasons = row.reasons.clone();
            copied == *row
        });
        delta.documents.push(if metadata_only {
            DocumentMutation::Metadata {
                path: row.path.clone(),
                eligibility: row.eligibility,
                reasons: row.reasons.clone(),
            }
        } else {
            DocumentMutation::Put { row: row.clone() }
        });
    }
    let old_links = group(&f.rows.links, |row| &row.from_path);
    let new_links = group(&rows.links, |row| &row.from_path);
    let new_raw = group(&rows.raw, |row| &row.from_path);
    let changed_ids: BTreeSet<_> = delta
        .records
        .iter()
        .map(|row| row.record.id().clone())
        .collect();
    for (owner, links) in new_links {
        if old_links.get(&owner) == Some(&links) {
            continue;
        }
        delta.links.push(OwnedLinks {
            path: owner.clone(),
            rows: links,
        });
        delta.facts.as_mut().unwrap().links.push(OwnedLinkFacts {
            path: owner.clone(),
            rows: new_raw.get(&owner).cloned().unwrap_or_default(),
        });
    }
    for claim in &rows.claims {
        if changed_ids.contains(&claim.id) {
            delta.claims.push(OwnedClaims {
                path: claim.path.clone(),
                rows: vec![claim.clone()],
            });
        }
    }
    for (path, state) in &after.facts.observed {
        if f.before.facts.observed.get(path) != Some(state) {
            delta.dependencies.push(ReadDependency {
                path: path.clone(),
                expected: state.clone(),
            });
        }
    }
    let facts = delta.facts.as_mut().unwrap();
    facts.records = after
        .facts
        .records
        .iter()
        .filter(|(id, fact)| f.before.facts.records.get(*id) != Some(*fact))
        .map(|(id, fact)| RecordFactMutation {
            record_id: id.clone(),
            fact: fact.clone(),
        })
        .collect();
    facts.edge_inserts = after
        .facts
        .edges
        .difference(&f.before.facts.edges)
        .cloned()
        .collect();
    facts.edge_deletes = f
        .before
        .facts
        .edges
        .difference(&after.facts.edges)
        .cloned()
        .collect();
    facts.registry = rows
        .registry
        .iter()
        .filter(|entry| !f.rows.registry.contains(entry))
        .cloned()
        .collect();
    for row in &delta.records {
        if row.record.kind() == RecordKind::Revision
            && !f.before.validation.records.contains_key(row.record.id())
        {
            let source = id(row.record.string("wiki_source_id").unwrap());
            let inventory = after.validation.records[&source]
                .record
                .field("wiki_revisions")
                .unwrap()
                .as_array()
                .unwrap();
            delta.revisions.push(RevisionIdentityRow {
                source_id: source,
                revision_id: row.record.id().clone(),
                retained_ordinal: inventory
                    .iter()
                    .position(|id| id.as_str() == Some(row.record.id().as_str()))
                    .unwrap(),
                original_hash: Blake3Hash::new(row.record.string("wiki_original_hash").unwrap())
                    .unwrap(),
                content_hash: row
                    .record
                    .string("wiki_content_hash")
                    .map(|v| Blake3Hash::new(v).unwrap()),
                extractor_fingerprint: Blake3Hash::new(
                    row.record.string("wiki_extractor_fingerprint").unwrap(),
                )
                .unwrap(),
                extraction_status: row.record.string("wiki_extraction_status").unwrap().into(),
            });
        }
    }
    delta
}
fn title_delta(f: &Fixture) -> CatalogDelta {
    let row = &f.before.validation.records[&f.source];
    let absolute = f.fs.root().path().join(row.path.as_str());
    let note = parse_note(&fs::read(&absolute).unwrap());
    let bytes = edit_note(
        &note,
        &BTreeMap::from([("title".into(), json!("Updated source title"))]),
        None,
        &note.source_hash,
    )
    .unwrap();
    fs::write(absolute, bytes).unwrap();
    let (after, rows) = project(&f.fs);
    delta_from_oracle(f, &after, &rows)
}
fn apply(f: &Fixture, delta: &CatalogDelta) -> DeltaStats {
    f.database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let stats = delta.apply(&f.database).unwrap();
    f.database.execute_batch("COMMIT").unwrap();
    stats
}
fn hits(c: &Connection, term: &str) -> i64 {
    c.query_row(
        "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH ?1",
        [term],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn source_title_hash_updates_central_state_without_changing_historical_fact() {
    let f = fixture();
    let held = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let old = held.eligibility_fact(&f.historical).unwrap().unwrap();
    let own = f.before.validation.records[&f.source].path.clone();
    let old_state = held.direct_path_states(std::slice::from_ref(&own)).unwrap();
    let delta = title_delta(&f);
    assert!(
        delta
            .facts
            .as_ref()
            .unwrap()
            .records
            .iter()
            .all(|fact| fact.record_id != f.historical)
    );
    let stats = apply(&f, &delta);
    assert!(stats.old_fact_bytes > 0);
    let fresh = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(fresh.eligibility_fact(&f.historical).unwrap(), Some(old));
    assert_ne!(
        fresh
            .direct_path_states(std::slice::from_ref(&own))
            .unwrap(),
        old_state
    );
    assert_eq!(held.direct_path_states(&[own]).unwrap(), old_state);
    assert_eq!(
        fresh.record(&f.source).unwrap().unwrap().record.title(),
        "Updated source title"
    );
    let content = path(&format!(
        "sources/{}/revisions/{}/content.md",
        f.source, f.historical
    ));
    assert_eq!(
        fresh.document(&content).unwrap().unwrap().title,
        "Immutable title"
    );
}
#[test]
fn new_revision_installs_exact_fact_registry_edges_and_paired_raw_links() {
    let f = fixture();
    write_plan(&f.fs, &f.next);
    let (after, rows) = project(&f.fs);
    let delta = delta_from_oracle(&f, &after, &rows);
    let encoded = serde_json::to_vec(&delta).unwrap();
    assert_eq!(
        serde_json::from_slice::<CatalogDelta>(&encoded).unwrap(),
        delta
    );
    apply(&f, &delta);
    let read = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        read.eligibility_fact(&f.next.revision_id).unwrap(),
        Some(after.facts.records[&f.next.revision_id].clone())
    );
    let candidates = read
        .registry_candidates_for_key(&MatchKey {
            kind: MatchKeyKind::Id,
            value: f.next.revision_id.to_string(),
        })
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, f.next.revision_id);
    let edges = read
        .outgoing_edges(
            &f.source,
            &[
                EligibilityRole::SourceInventory,
                EligibilityRole::TypedReference {
                    field: "wiki_current_revision".into(),
                },
            ],
        )
        .unwrap();
    let expected: Vec<_> = after
        .facts
        .edges
        .iter()
        .filter(|edge| {
            edge.owner_id == f.source
                && (edge.role == EligibilityRole::SourceInventory
                    || edge.role
                        == EligibilityRole::TypedReference {
                            field: "wiki_current_revision".into(),
                        })
        })
        .cloned()
        .collect();
    assert_eq!(edges, expected);
    for owned in &delta.facts.as_ref().unwrap().links {
        assert_eq!(read.owned_link_facts(&owned.path).unwrap(), owned.rows);
    }
    assert_eq!(hits(&f.database, "thirdtoken"), 1);
}
#[test]
fn fact_stage_failure_rolls_back_ordinary_fts_and_prior_fact_actions() {
    let f = fixture();
    let mut delta = title_delta(&f);
    let before = f
        .database
        .query_row(
            "SELECT row_json FROM records WHERE id=?1",
            [f.source.as_str()],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    let mut source_fact = f.before.facts.records[&f.source].clone();
    source_fact
        .baseline
        .reasons
        .push("staged_rollback_witness".into());
    delta.facts.as_mut().unwrap().records = vec![
        RecordFactMutation {
            record_id: f.source.clone(),
            fact: source_fact,
        },
        RecordFactMutation {
            record_id: id("orphan_fact"),
            fact: f.before.facts.records[&f.historical].clone(),
        },
    ];
    f.database.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        delta.apply(&f.database).unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
    assert!(!f.database.is_autocommit());
    assert_eq!(
        f.database
            .query_row(
                "SELECT row_json FROM records WHERE id=?1",
                [f.source.as_str()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        before
    );
    assert_eq!(hits(&f.database, "Updated"), 0);
    let old_source = &f.before.validation.records[&f.source];
    assert_eq!(
        f.database
            .query_row(
                "SELECT expected_hash FROM dependencies WHERE path=?1",
                [old_source.path.as_str()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        old_source.hash.as_str()
    );
    assert_eq!(
        f.database
            .query_row(
                "SELECT file_hash FROM documents WHERE record_id=?1",
                [f.source.as_str()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        old_source.hash.as_str()
    );
    assert_eq!(
        f.database
            .query_row(
                "SELECT count(*) FROM record_eligibility_facts WHERE record_id='orphan_fact'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    f.database.execute_batch("ROLLBACK").unwrap();
    let read = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        read.eligibility_fact(&f.source).unwrap(),
        Some(f.before.facts.records[&f.source].clone())
    );
}

fn refuses(f: &Fixture, delta: &CatalogDelta, code: ErrorCode) {
    f.database.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(delta.apply(&f.database).unwrap_err().code, code);
    if !f.database.is_autocommit() {
        f.database.execute_batch("ROLLBACK").unwrap();
    }
}
#[test]
fn exact_edge_overlap_absent_deletion_and_existing_insertion_refuse() {
    let f = fixture();
    let edge = f
        .before
        .facts
        .edges
        .iter()
        .find(|edge| edge.owner_id == f.source)
        .unwrap()
        .clone();
    let mut overlap = empty();
    overlap.facts.as_mut().unwrap().edge_inserts = vec![edge.clone()];
    overlap.facts.as_mut().unwrap().edge_deletes = vec![edge.clone()];
    assert_eq!(
        overlap.validate().unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
    let mut absent = empty();
    absent.facts.as_mut().unwrap().edge_deletes = vec![EligibilityEdge {
        target_id: id("absent_target"),
        ..edge.clone()
    }];
    refuses(&f, &absent, ErrorCode::ContentConflict);
    let mut existing = empty();
    existing.facts.as_mut().unwrap().edge_inserts = vec![edge];
    refuses(&f, &existing, ErrorCode::IndexCorrupt);
    let read = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        read.outgoing_edges(&f.source, &[EligibilityRole::SourceInventory])
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn new_revision_missing_fact_or_registry_and_orphan_owners_refuse() {
    let f = fixture();
    write_plan(&f.fs, &f.next);
    let (after, rows) = project(&f.fs);
    let delta = delta_from_oracle(&f, &after, &rows);
    for omit_fact in [true, false] {
        let mut bad = delta.clone();
        let facts = bad.facts.as_mut().unwrap();
        if omit_fact {
            facts
                .records
                .retain(|fact| fact.record_id != f.next.revision_id);
        } else {
            facts
                .registry
                .retain(|entry| entry.record_id != f.next.revision_id);
        }
        refuses(&f, &bad, ErrorCode::IndexCorrupt);
    }
    let mut orphan = empty();
    orphan.facts.as_mut().unwrap().records = vec![RecordFactMutation {
        record_id: id("orphan_owner"),
        fact: f.before.facts.records[&f.historical].clone(),
    }];
    refuses(&f, &orphan, ErrorCode::IndexCorrupt);
    let mut orphan = empty();
    orphan.facts.as_mut().unwrap().registry = vec![OwnedRegistryKeys {
        record_id: id("orphan_owner"),
        path: path("orphan.md"),
        keys: vec![],
    }];
    refuses(&f, &orphan, ErrorCode::IndexCorrupt);
    let mut authored = empty();
    let mut record = f.before.validation.records[&id("entity_fact_delta")].clone();
    let mut fields = record.record.fields().clone();
    fields.insert("wiki_id".into(), json!("new_authored_entity"));
    record.record = crate::domain::CanonicalRecord::new(fields).unwrap();
    record.path = path("new-entity.md");
    authored.records = vec![record];
    refuses(&f, &authored, ErrorCode::IndexCorrupt);
}
#[test]
fn source_own_state_and_related_canonical_path_cannot_be_omitted_or_forged() {
    let f = fixture();
    let mut missing = title_delta(&f);
    missing.dependencies.clear();
    refuses(&f, &missing, ErrorCode::IndexCorrupt);
    let mut foreign = empty();
    let mut fact = f.before.facts.records[&f.source].clone();
    fact.direct_paths.insert(path("entity.md"));
    foreign.facts.as_mut().unwrap().records = vec![RecordFactMutation {
        record_id: f.source.clone(),
        fact,
    }];
    refuses(&f, &foreign, ErrorCode::IndexCorrupt);
    let mut missing = empty();
    let mut fact = f.before.facts.records[&f.source].clone();
    fact.direct_paths.clear();
    missing.facts.as_mut().unwrap().records = vec![RecordFactMutation {
        record_id: f.source.clone(),
        fact,
    }];
    refuses(&f, &missing, ErrorCode::IndexCorrupt);
}
fn raw_delta(
    destination: &str,
    target_id: Option<RecordId>,
    target_path: Option<VaultRelativePath>,
) -> CatalogDelta {
    let owner = path("page.md");
    let resolution = if destination.starts_with("https:") {
        crate::records::LinkResolution::External
    } else {
        crate::records::LinkResolution::Missing
    };
    let fact = link_facts::untyped_fact(&owner, 0, destination, &resolution).unwrap();
    let mut delta = empty();
    delta.links = vec![OwnedLinks {
        path: owner.clone(),
        rows: vec![LinkRow {
            from_path: owner.clone(),
            byte_start: 0,
            target_id,
            target_path,
            resolution: format!("{resolution:?}"),
        }],
    }];
    delta.facts.as_mut().unwrap().links = vec![OwnedLinkFacts {
        path: owner,
        rows: vec![fact],
    }];
    delta
}
#[test]
fn raw_links_require_exact_keys_complete_target_pairs_and_external_target_absence() {
    let mut bad = raw_delta("revision", None, None);
    bad.facts.as_mut().unwrap().links[0].rows[0].keys.pop();
    assert_eq!(bad.validate().unwrap_err().code, ErrorCode::IndexCorrupt);
    let bad = raw_delta("revision", Some(id("entity_fact_delta")), None);
    assert_eq!(bad.validate().unwrap_err().code, ErrorCode::IndexCorrupt);
    let mut bad = raw_delta(
        "https://example.invalid/item",
        Some(id("entity_fact_delta")),
        Some(path("entity.md")),
    );
    bad.facts.as_mut().unwrap().links[0].rows[0]
        .keys
        .push(MatchKey {
            kind: MatchKeyKind::Id,
            value: "entity_fact_delta".into(),
        });
    assert_eq!(bad.validate().unwrap_err().code, ErrorCode::IndexCorrupt);
    let bad = raw_delta(
        "revision",
        Some(id("entity_fact_delta")),
        Some(path("entity.md")),
    );
    assert_eq!(
        bad.validate().unwrap_err().code,
        ErrorCode::IndexCorrupt,
        "resolved target ID must be represented in raw match keys"
    );
    let mut bad = raw_delta("revision", None, None);
    bad.facts.as_mut().unwrap().links[0].rows[0].from_path = path("cross-owned.md");
    assert_eq!(bad.validate().unwrap_err().code, ErrorCode::IndexCorrupt);
}
#[test]
fn nested_keys_and_replaced_old_rows_and_bytes_are_cumulatively_bounded() {
    let mut excessive = empty();
    excessive.facts.as_mut().unwrap().registry = vec![OwnedRegistryKeys {
        record_id: id("source_owner"),
        path: path("source.md"),
        keys: (0..4096)
            .map(|n| MatchKey {
                kind: MatchKeyKind::Alias,
                value: format!("alias_{n}"),
            })
            .collect(),
    }];
    assert_eq!(
        excessive.validate().unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    for oversized_bytes in [false, true] {
        let f = fixture();
        let mut delta = title_delta(&f);
        let entry = f
            .rows
            .registry
            .iter()
            .find(|entry| entry.record_id == f.source)
            .unwrap()
            .clone();
        delta.facts.as_mut().unwrap().registry = vec![entry.clone()];
        if oversized_bytes {
            f.database
                .execute(
                    "INSERT INTO registry_match_keys VALUES('alias',?1,?2,?3)",
                    params![
                        "x".repeat(8 * 1024 * 1024 + 1),
                        f.source.as_str(),
                        entry.path.as_str()
                    ],
                )
                .unwrap();
        } else {
            f.database.execute_batch("BEGIN IMMEDIATE").unwrap();
            for n in 0..4097 {
                f.database
                    .execute(
                        "INSERT INTO registry_match_keys VALUES('alias',?1,?2,?3)",
                        params![format!("extra_{n}"), f.source.as_str(), entry.path.as_str()],
                    )
                    .unwrap();
            }
            f.database.execute_batch("COMMIT").unwrap();
        }
        refuses(&f, &delta, ErrorCode::BudgetExceeded);
        assert_eq!(hits(&f.database, "Updated"), 0);
        assert_eq!(
            f.database
                .query_row(
                    "SELECT title FROM documents WHERE record_id=?1",
                    [f.source.as_str()],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "Immutable title"
        );
    }
}
#[test]
fn delta_version_must_match_selected_fact_layout() {
    let f = fixture();
    let mut legacy = empty();
    legacy.version = 1;
    legacy.facts = None;
    refuses(&f, &legacy, ErrorCode::IndexCorrupt);
    f.database
        .execute("UPDATE catalog_meta SET proof_layout_version=0", [])
        .unwrap();
    refuses(&f, &empty(), ErrorCode::IndexCorrupt);
    let mut mismatch = empty();
    mismatch.version = 1;
    assert_eq!(
        mismatch.validate().unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
}
#[test]
fn authored_assertion_canonical_change_is_rejected_and_opposition_index_stays_exact() {
    let f = fixture();
    let snapshot = || {
        f.database.prepare("SELECT key_json,negated,assertion_id FROM opposition_members ORDER BY key_json,negated,assertion_id").unwrap().query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?))).unwrap().collect::<std::result::Result<Vec<_>,_>>().unwrap()
    };
    let before = snapshot();
    assert_eq!(before.len(), 1);
    let mut row = f.before.validation.records[&id("assertion_fact_delta")].clone();
    let mut fields = row.record.fields().clone();
    fields.insert("wiki_predicate".into(), json!("maintains"));
    row.record = crate::domain::CanonicalRecord::new(fields).unwrap();
    row.hash = Blake3Hash::digest(sql::json(&row.record).unwrap());
    let mut delta = empty();
    delta.records = vec![row];
    refuses(&f, &delta, ErrorCode::ContentConflict);
    assert_eq!(snapshot(), before);
}
#[test]
fn selected_old_record_json_identity_corruption_refuses_before_replacement() {
    let f = fixture();
    let delta = title_delta(&f);
    let mut corrupted = f.before.validation.records[&f.source].clone();
    corrupted.path = path("forged-source.md");
    f.database
        .execute(
            "UPDATE records SET row_json=?1 WHERE id=?2",
            params![sql::json(&corrupted).unwrap(), f.source.as_str()],
        )
        .unwrap();
    refuses(&f, &delta, ErrorCode::IndexCorrupt);
    assert_eq!(hits(&f.database, "Updated"), 0);
    assert_eq!(
        f.database
            .query_row(
                "SELECT path FROM records WHERE id=?1",
                [f.source.as_str()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        f.before.validation.records[&f.source].path.as_str()
    );
}

// This matrix validates the structural membership domain independently of the
// guarded operation kind. Sealed projectors still own write authorization.
fn author_membership_delta() -> CatalogDelta {
    use super::policy_delta::{PolicyDelta, PolicyMembershipUpdate};
    let ordinary_path = path("ordinary.md");
    let ordinary = parse_note(b"---\nauthor: reader\n---\n# Ordinary\n");
    let adopted_path = path("adopted.md");
    let adopted = parse_note(b"---\nwiki_schema: '1'\nwiki_id: page_membership\nwiki_kind: page\ntitle: Adopted\nwiki_status: reviewed\n---\nBody\n");
    let record = RecordRow {
        record: adopted.canonical.clone().unwrap(),
        path: adopted_path.clone(),
        hash: adopted.source_hash.clone(),
        authored_status: Some("reviewed".into()),
        eligibility: crate::domain::Eligibility::Current,
        reasons: vec![],
        identity_eligibility: None,
        description_eligibility: None,
        disputed: false,
        dependencies: vec![],
    };
    let ordinary_row =
        super::row_projection::canonical_document(&ordinary_path, &ordinary, None).unwrap();
    let adopted_row =
        super::row_projection::canonical_document(&adopted_path, &adopted, Some(&record)).unwrap();
    let mut captured_row = ordinary_row.clone();
    captured_row.path = path("sources/source_membership/revisions/revision_membership/content.md");
    captured_row.source_id = Some(id("source_membership"));
    captured_row.owner_revision = Some(id("revision_membership"));
    let mut delta = empty();
    delta.version = 3;
    delta.records.push(record);
    delta.documents = vec![
        DocumentMutation::Put { row: ordinary_row },
        DocumentMutation::Put { row: adopted_row },
        DocumentMutation::Put { row: captured_row },
        DocumentMutation::Metadata {
            path: path("metadata-only.md"),
            eligibility: crate::domain::Eligibility::Stale,
            reasons: vec!["fixture".into()],
        },
    ];
    delta.claims = vec![OwnedClaims {
        path: ordinary_path.clone(),
        rows: vec![],
    }];
    delta.facts.as_mut().unwrap().policy = Some(PolicyDelta {
        retired_owners: vec![],
        replacements: vec![],
        memberships: [(ordinary_path, ordinary), (adopted_path, adopted)]
            .into_iter()
            .map(|(path, note)| PolicyMembershipUpdate {
                keys: crate::graph::policy_inputs::policy_membership_keys(&path, &note).unwrap(),
                path,
                hash: note.source_hash,
            })
            .collect(),
    });
    delta
}

#[test]
fn v3_author_note_membership_domain_is_exact_for_ordinary_and_adopted_puts() {
    let delta = author_membership_delta();
    delta.validate().unwrap();
    for omitted in ["ordinary.md", "adopted.md"] {
        let mut missing = delta.clone();
        missing
            .facts
            .as_mut()
            .unwrap()
            .policy
            .as_mut()
            .unwrap()
            .memberships
            .retain(|member| member.path != path(omitted));
        let error = missing.validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::IndexCorrupt);
        assert!(
            error.message.contains("exact policy membership"),
            "{error:?}"
        );
    }
    for extra in [
        "sources/source_membership/revisions/revision_membership/content.md",
        "metadata-only.md",
        "unwritten.md",
    ] {
        let mut bad = delta.clone();
        bad.facts
            .as_mut()
            .unwrap()
            .policy
            .as_mut()
            .unwrap()
            .memberships
            .push(super::policy_delta::PolicyMembershipUpdate {
                path: path(extra),
                hash: Blake3Hash::digest(b"unowned membership"),
                keys: BTreeSet::new(),
            });
        let error = bad.validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::IndexCorrupt);
        assert!(
            error.message.contains("exact policy membership"),
            "{error:?}"
        );
    }
    // Ordinary zero-key membership is a required replacement certificate.
    assert!(
        delta
            .facts
            .as_ref()
            .unwrap()
            .policy
            .as_ref()
            .unwrap()
            .memberships
            .iter()
            .find(|member| member.path == path("ordinary.md"))
            .unwrap()
            .keys
            .is_empty()
    );
}

#[test]
fn author_note_domain_excludes_every_capture_owner_and_metadata_combination() {
    let delta = author_membership_delta();
    let DocumentMutation::Put { row } = &delta.documents[0] else {
        panic!("ordinary fixture")
    };
    for moved in [false, true] {
        for source in [false, true] {
            for revision in [false, true] {
                let mut row = row.clone();
                row.source_id = source.then(|| id("source_membership"));
                row.owner_revision = revision.then(|| id("revision_membership"));
                let mutation = if moved {
                    DocumentMutation::MovePage {
                        from: path("before.md"),
                        from_hash: Blake3Hash::digest(b"before"),
                        row,
                    }
                } else {
                    DocumentMutation::Put { row }
                };
                assert_eq!(mutation.author_note_row().is_some(), !source && !revision);
            }
        }
    }
    assert!(delta.documents[3].author_note_row().is_none());
    assert!(
        DocumentMutation::DeletePage {
            path: path("deleted.md"),
            expected_hash: Blake3Hash::digest(b"old")
        }
        .author_note_row()
        .is_none()
    );
}

#[test]
fn v3_author_note_writes_cannot_omit_policy_maintenance() {
    let mut delta = author_membership_delta();
    delta.facts.as_mut().unwrap().policy = None;
    assert_eq!(delta.validate().unwrap_err().code, ErrorCode::IndexCorrupt);
    assert_eq!(
        delta
            .facts
            .as_ref()
            .unwrap()
            .validate(&delta, &mut 0)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    // Historical v2 fact deltas keep their separate compatibility contract.
    delta.version = 2;
    delta.validate().unwrap();
}

#[test]
fn page_move_and_ordinary_put_share_membership_coverage_without_operation_exceptions() {
    let mut delta = author_membership_delta();
    let DocumentMutation::Put { row } = delta.documents[1].clone() else {
        panic!("adopted fixture")
    };
    let from = path("old-page.md");
    delta.documents[1] = DocumentMutation::MovePage {
        from: from.clone(),
        from_hash: Blake3Hash::digest(b"old Page"),
        row,
    };
    delta.links.push(OwnedLinks {
        path: from.clone(),
        rows: vec![],
    });
    delta.claims.push(OwnedClaims {
        path: from.clone(),
        rows: vec![],
    });
    delta.diagnostics.push(OwnedDiagnostics {
        path: from.clone(),
        rows: vec![],
    });
    let facts = delta.facts.as_mut().unwrap();
    facts.links.push(OwnedLinkFacts {
        path: from.clone(),
        rows: vec![],
    });
    facts
        .policy
        .as_mut()
        .unwrap()
        .retired_owners
        .push(from.clone());
    delta.validate().unwrap();
    for omitted in ["ordinary.md", "adopted.md"] {
        let mut missing = delta.clone();
        missing
            .facts
            .as_mut()
            .unwrap()
            .policy
            .as_mut()
            .unwrap()
            .memberships
            .retain(|member| member.path != path(omitted));
        assert!(
            missing
                .validate()
                .unwrap_err()
                .message
                .contains("exact policy membership")
        );
    }
    for duplicate in [0, 1] {
        let mut extra = delta.clone();
        let members = &mut extra
            .facts
            .as_mut()
            .unwrap()
            .policy
            .as_mut()
            .unwrap()
            .memberships;
        members.push(members[duplicate].clone());
        assert_eq!(extra.validate().unwrap_err().code, ErrorCode::IndexCorrupt);
    }
    let mut extra = delta;
    extra
        .facts
        .as_mut()
        .unwrap()
        .policy
        .as_mut()
        .unwrap()
        .memberships
        .push(super::policy_delta::PolicyMembershipUpdate {
            path: from,
            hash: Blake3Hash::digest(b"old Page"),
            keys: BTreeSet::new(),
        });
    assert!(
        extra
            .validate()
            .unwrap_err()
            .message
            .contains("exact policy membership")
    );
}

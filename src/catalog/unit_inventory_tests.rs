//! Mechanical invalidation boundary tests. Minimal in-memory SQL stands in for
//! central row replacement; these do not exercise sealed publication/recovery.
use super::*;
use crate::catalog::{
    eligibility_facts::{EligibilityBaseline, EligibilityEdge, EligibilityFact, EligibilityRole},
    normalized_fact_delta::{FactDelta, RecordFactMutation},
    structural_rules::{StructuralEffect, StructuralFact, StructuralStage},
    types::RecordRow,
};
use serde_json::json;

const EPOCH: i64 = 7;
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn empty_delta() -> CatalogDelta {
    CatalogDelta {
        version: 2,
        records: vec![],
        documents: vec![],
        graph: vec![],
        links: vec![],
        diagnostics: vec![],
        claims: vec![],
        revisions: vec![],
        dependencies: vec![],
        owners: vec![],
        facts: Some(FactDelta {
            policy: None,
            records: vec![],
            edge_inserts: vec![],
            edge_deletes: vec![],
            links: vec![],
            registry: vec![],
        }),
    }
}
fn dependency(value: &str, expected: ExpectedState) -> ReadDependency {
    ReadDependency {
        path: path(value),
        expected,
    }
}
fn page(value: &str, record_id: &str, body: &str) -> (RecordRow, DocumentRow) {
    let raw = format!(
        "---\nwiki_schema: '1'\nwiki_id: {record_id}\nwiki_kind: page\ntitle: {record_id}\nwiki_status: reviewed\n---\n# {record_id}\n\n{body}\n"
    );
    let hash = Blake3Hash::digest(raw.as_bytes());
    let record = crate::records::parse_note(raw.as_bytes())
        .canonical
        .unwrap();
    let row = RecordRow {
        record,
        path: path(value),
        hash: hash.clone(),
        authored_status: Some("reviewed".into()),
        eligibility: Eligibility::Current,
        reasons: vec![],
        identity_eligibility: None,
        description_eligibility: None,
        disputed: false,
        dependencies: vec![],
    };
    let document = DocumentRow {
        path: row.path.clone(),
        hash,
        record_id: Some(id(record_id)),
        kind: Some(RecordKind::Page),
        title: record_id.into(),
        aliases: vec![],
        headings: record_id.into(),
        tags: vec![],
        body: body.into(),
        raw_text: raw,
        source_id: None,
        owner_revision: None,
        eligibility: Eligibility::Current,
        reasons: vec![],
    };
    (row, document)
}
fn producer(value: &str, record_id: &str, target_id: &RecordId) -> RecordRow {
    let raw = format!(
        "---\nwiki_schema: '1'\nwiki_id: {record_id}\nwiki_kind: decision\ntitle: Selected decision\nwiki_status: active\nwiki_action: accept\nwiki_input_ids: [{target_id}]\nwiki_output_ids: [{target_id}]\nwiki_created_at: '2026-10-06T00:00:00Z'\n---\nAccepted producer.\n"
    );
    let record = crate::records::parse_note(raw.as_bytes())
        .canonical
        .unwrap();
    RecordRow {
        record,
        path: path(value),
        hash: Blake3Hash::digest(raw.as_bytes()),
        authored_status: Some("active".into()),
        eligibility: Eligibility::Current,
        reasons: vec![],
        identity_eligibility: None,
        description_eligibility: None,
        disputed: false,
        dependencies: vec![],
    }
}
fn insert_record(c: &Connection, row: &RecordRow) {
    c.execute(
        "INSERT OR REPLACE INTO records(id,path,hash,row_json) VALUES(?1,?2,?3,?4)",
        params![
            row.record.id().as_str(),
            row.path.as_str(),
            row.hash.as_str(),
            sql::json(row).unwrap()
        ],
    )
    .unwrap();
}
fn insert_document(c: &Connection, row: &DocumentRow) {
    c.execute(&format!("INSERT OR REPLACE INTO documents({}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", normalized_schema::DOCUMENT_COLUMNS),
        params![row.path.as_str(), row.hash.as_str(), row.record_id.as_ref().map(RecordId::as_str),
            row.kind.map(|kind| kind.as_str()), row.title, sql::json(&row.aliases).unwrap(), row.headings,
            sql::json(&row.tags).unwrap(), row.body, row.raw_text,
            row.source_id.as_ref().map(RecordId::as_str), row.owner_revision.as_ref().map(RecordId::as_str),
            sql::json(&row.eligibility).unwrap().trim_matches('"'), sql::json(&row.reasons).unwrap()]).unwrap();
}
fn insert_dependency(c: &Connection, dep: &ReadDependency) {
    let hash = match &dep.expected {
        ExpectedState::Absent => None,
        ExpectedState::Hash(hash) => Some(hash.as_str()),
    };
    c.execute(
        "INSERT OR REPLACE INTO dependencies(path,expected_hash) VALUES(?1,?2)",
        params![dep.path.as_str(), hash],
    )
    .unwrap();
}
fn fact(row: &RecordRow) -> EligibilityFact {
    EligibilityFact {
        baseline: EligibilityBaseline {
            eligibility: row.eligibility,
            reasons: row.reasons.clone(),
            identity_eligibility: row.identity_eligibility,
            description_eligibility: row.description_eligibility,
            disputed: row.disputed,
        },
        structural: StructuralFact::default(),
        direct_paths: BTreeSet::from([row.path.clone()]),
    }
}
fn insert_fact(c: &Connection, record_id: &RecordId, fact: &EligibilityFact) {
    c.execute(
        "INSERT OR REPLACE INTO record_eligibility_facts VALUES(?1,?2,?3)",
        params![
            record_id.as_str(),
            sql::json(&fact.baseline).unwrap(),
            sql::json(&fact.structural).unwrap()
        ],
    )
    .unwrap();
    c.execute(
        "DELETE FROM record_direct_paths WHERE owner_id=?1",
        [record_id.as_str()],
    )
    .unwrap();
    for path in &fact.direct_paths {
        c.execute(
            "INSERT INTO record_direct_paths VALUES(?1,?2)",
            params![record_id.as_str(), path.as_str()],
        )
        .unwrap();
    }
}
#[derive(Debug, PartialEq, Eq)]
struct OwnerState {
    hash: String,
    token: String,
    proof: i64,
    modified: i64,
    tombstone: bool,
    units: i64,
}
fn owner_state(c: &Connection, policy: &RenderPolicyId, owner: &VaultRelativePath) -> OwnerState {
    c.query_row("SELECT source_hash,render_token,proof_version,modified_seq,tombstone,unit_count FROM unit_owners WHERE policy=?1 AND owner=?2", params![policy.as_str(),owner.as_str()], |row| {
        Ok(OwnerState { hash: row.get(0)?, token: row.get(1)?, proof: row.get(2)?, modified: row.get(3)?, tombstone: row.get(4)?, units: row.get(5)? })
    }).unwrap()
}
fn units(c: &Connection, policy: &RenderPolicyId, owner: &VaultRelativePath) -> Vec<String> {
    c.prepare(
        "SELECT descriptor_json FROM retrieval_units WHERE policy=?1 AND owner=?2 ORDER BY unit_id",
    )
    .unwrap()
    .query_map(params![policy.as_str(), owner.as_str()], |row| row.get(0))
    .unwrap()
    .collect::<std::result::Result<_, _>>()
    .unwrap()
}
fn maps(c: &Connection) -> Vec<(String, String, String)> {
    c.prepare("SELECT policy,owner,path FROM unit_owner_dependencies ORDER BY policy,owner,path")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}
struct Fixture {
    c: Connection,
    policy: RenderPolicyId,
    owner: DocumentRow,
    support: RecordRow,
    support_doc: DocumentRow,
    unrelated: DocumentRow,
}
impl Fixture {
    fn new() -> Self {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE catalog_meta(singleton INTEGER PRIMARY KEY,epoch INTEGER NOT NULL,publication_hash TEXT NOT NULL,state TEXT NOT NULL); CREATE TABLE records(id TEXT PRIMARY KEY,path TEXT NOT NULL,hash TEXT NOT NULL,row_json TEXT NOT NULL); CREATE TABLE dependencies(path TEXT PRIMARY KEY,expected_hash TEXT); CREATE TABLE record_eligibility_facts(record_id TEXT PRIMARY KEY,baseline_json TEXT NOT NULL,structural_json TEXT NOT NULL); CREATE TABLE record_direct_paths(owner_id TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(owner_id,path)); CREATE TABLE semantic_edges(owner_id TEXT NOT NULL,target_id TEXT NOT NULL,role_json TEXT NOT NULL,PRIMARY KEY(owner_id,target_id,role_json)); CREATE TABLE documents(path TEXT PRIMARY KEY,file_hash TEXT NOT NULL,record_id TEXT,kind TEXT,title TEXT NOT NULL,aliases_json TEXT NOT NULL,headings TEXT NOT NULL,tags_json TEXT NOT NULL,body TEXT NOT NULL,raw_text TEXT NOT NULL,source_id TEXT,owner_revision TEXT,eligibility TEXT NOT NULL,reasons_json TEXT NOT NULL); CREATE TABLE publication_attempts(value TEXT PRIMARY KEY);").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute(
            "INSERT INTO catalog_meta VALUES(1,?1,?2,'complete')",
            params![EPOCH, Blake3Hash::digest(b"coherent old header").as_str()],
        )
        .unwrap();
        let parser = Blake3Hash::digest(b"in-memory parser");
        let settings = EmbeddingSettings::default();
        let policy = RenderPolicyId::for_settings(&parser, &settings).unwrap();
        c.execute("INSERT INTO unit_policies(policy,version,parser_hash,settings_json,complete,after_owner) VALUES(?1,1,?2,?3,1,NULL)", params![policy.as_str(),parser.as_str(),sql::json(&settings).unwrap()]).unwrap();
        let (owner_row, owner) = page(
            "pages/owner.md",
            "page_owner",
            "Answer depends on the reviewed support.",
        );
        let (support, support_doc) = page(
            "pages/support.md",
            "page_support",
            "The support remains current.",
        );
        let (unrelated_row, unrelated) = page(
            "pages/unrelated.md",
            "page_unrelated",
            "Unrelated stable facts.",
        );
        for (record, document) in [
            (&owner_row, &owner),
            (&support, &support_doc),
            (&unrelated_row, &unrelated),
        ] {
            insert_record(&c, record);
            insert_document(&c, document);
            insert_fact(&c, record.record.id(), &fact(record));
            insert_dependency(
                &c,
                &dependency(
                    record.path.as_str(),
                    ExpectedState::Hash(record.hash.clone()),
                ),
            );
        }
        insert_dependency(
            &c,
            &dependency(
                "WIKI.md",
                ExpectedState::Hash(Blake3Hash::digest(b"fixed wiki witness")),
            ),
        );
        let budget = UnitBudget::new(UnitLimits::default()).unwrap();
        for document in [&owner, &unrelated] {
            replace_owner(
                &c,
                &policy,
                &settings,
                Some(document),
                &document.path,
                EPOCH as u64,
                &budget,
            )
            .unwrap();
            for dependency in [&document.path, &path("WIKI.md")] {
                c.execute(
                    "INSERT INTO unit_owner_dependencies VALUES(?1,?2,?3)",
                    params![policy.as_str(), document.path.as_str(), dependency.as_str()],
                )
                .unwrap();
            }
        }
        c.execute(
            "INSERT INTO unit_owner_dependencies VALUES(?1,?2,?3)",
            params![policy.as_str(), owner.path.as_str(), support.path.as_str()],
        )
        .unwrap();
        assert!(!units(&c, &policy, &owner.path).is_empty());
        Self {
            c,
            policy,
            owner,
            support,
            support_doc,
            unrelated,
        }
    }
    fn one_owner(&self, affected: &AffectedUnits) {
        assert_eq!(
            affected.owners,
            BTreeSet::from([(self.policy.clone(), self.owner.path.clone())])
        );
        assert!(affected.documents.is_empty());
    }
    fn expect_proof_only(&self, delta: &CatalogDelta) {
        let before = owner_state(&self.c, &self.policy, &self.owner.path);
        let unrelated = owner_state(&self.c, &self.policy, &self.unrelated.path);
        let descriptors = units(&self.c, &self.policy, &self.owner.path);
        let reverse = maps(&self.c);
        let affected = affected_before(&self.c, delta).unwrap();
        self.one_owner(&affected);
        apply_after(&self.c, delta, &affected).unwrap();
        let after = owner_state(&self.c, &self.policy, &self.owner.path);
        assert_eq!((after.proof, after.modified), (EPOCH + 1, EPOCH + 1));
        assert_eq!(
            (after.hash, after.token, after.tombstone, after.units),
            (before.hash, before.token, before.tombstone, before.units)
        );
        assert_eq!(units(&self.c, &self.policy, &self.owner.path), descriptors);
        assert_eq!(
            owner_state(&self.c, &self.policy, &self.unrelated.path),
            unrelated
        );
        assert_eq!(maps(&self.c), reverse); // dirty proofs keep the old reverse map
    }
}

#[test]
fn complete_base_and_building_successor_use_the_same_next_unit_sequence() {
    for building in [false, true] {
        let fixture = Fixture::new();
        if building {
            // A copied candidate binds the successor epoch before selected SQL;
            // a complete selected base still carries the predecessor epoch.
            fixture
                .c
                .execute("UPDATE catalog_meta SET state='building',epoch=epoch+1", [])
                .unwrap();
        }
        let mut delta = empty_delta();
        delta.dependencies.push(dependency(
            fixture.support.path.as_str(),
            ExpectedState::Hash(Blake3Hash::digest(b"changed support bytes")),
        ));
        // Both contexts publish at EPOCH+1. Dirty proof invalidation preserves
        // descriptors, reverse witnesses and unrelated registered owners.
        fixture.expect_proof_only(&delta);
    }
}

#[test]
fn present_null_dependency_is_unchanged_but_absent_sql_observation_is_new() {
    let fixture = Fixture::new();
    let missing = dependency("assets/optional.txt", ExpectedState::Absent);
    fixture
        .c
        .execute(
            "INSERT INTO unit_owner_dependencies VALUES(?1,?2,?3)",
            params![
                fixture.policy.as_str(),
                fixture.owner.path.as_str(),
                missing.path.as_str()
            ],
        )
        .unwrap();
    let mut delta = empty_delta();
    delta.dependencies.push(missing.clone());
    // No SQL row differs from a durable observed absence, despite both carrying
    // no hash. The new observation must invalidate its registered reader.
    fixture.one_owner(&affected_before(&fixture.c, &delta).unwrap());
    insert_dependency(&fixture.c, &missing);
    let state = owner_state(&fixture.c, &fixture.policy, &fixture.owner.path);
    let affected = affected_before(&fixture.c, &delta).unwrap();
    assert!(affected.owners.is_empty());
    assert!(affected.documents.is_empty());
    apply_after(&fixture.c, &delta, &affected).unwrap();
    assert_eq!(
        owner_state(&fixture.c, &fixture.policy, &fixture.owner.path),
        state
    );
    delta.dependencies[0].expected = ExpectedState::Hash(Blake3Hash::digest(b"asset arrived"));
    fixture.expect_proof_only(&delta);
}

#[test]
fn unchanged_whole_closure_does_not_invalidate_any_owner_or_descriptor() {
    let fixture = Fixture::new();
    let mut delta = empty_delta();
    delta.records.push(fixture.support.clone());
    delta.documents.push(DocumentMutation::Put {
        row: fixture.support_doc.clone(),
    });
    delta.documents.push(DocumentMutation::Metadata {
        path: fixture.owner.path.clone(),
        eligibility: Eligibility::Current,
        reasons: vec![],
    });
    delta.dependencies = vec![
        dependency(
            fixture.support.path.as_str(),
            ExpectedState::Hash(fixture.support.hash.clone()),
        ),
        dependency(
            "WIKI.md",
            ExpectedState::Hash(Blake3Hash::digest(b"fixed wiki witness")),
        ),
    ];
    delta
        .facts
        .as_mut()
        .unwrap()
        .records
        .push(RecordFactMutation {
            record_id: fixture.support.record.id().clone(),
            fact: fact(&fixture.support),
        });
    let owner = owner_state(&fixture.c, &fixture.policy, &fixture.owner.path);
    let unrelated = owner_state(&fixture.c, &fixture.policy, &fixture.unrelated.path);
    let descriptors = units(&fixture.c, &fixture.policy, &fixture.owner.path);
    let reverse = maps(&fixture.c);
    let affected = affected_before(&fixture.c, &delta).unwrap();
    assert!(affected.owners.is_empty());
    assert!(affected.documents.is_empty());
    apply_after(&fixture.c, &delta, &affected).unwrap();
    assert_eq!(
        owner_state(&fixture.c, &fixture.policy, &fixture.owner.path),
        owner
    );
    assert_eq!(
        owner_state(&fixture.c, &fixture.policy, &fixture.unrelated.path),
        unrelated
    );
    assert_eq!(
        units(&fixture.c, &fixture.policy, &fixture.owner.path),
        descriptors
    );
    assert_eq!(maps(&fixture.c), reverse);
}

#[test]
fn new_reverse_decision_producer_invalidates_unchanged_endpoint_without_old_producer_guard() {
    for role in [
        EligibilityRole::DecisionInput,
        EligibilityRole::DecisionOutput,
        EligibilityRole::PolicySupersession,
        EligibilityRole::TypedReference {
            field: "wiki_supersedes_id".into(),
        },
        EligibilityRole::TypedReference {
            field: "wiki_superseded_by_id".into(),
        },
        EligibilityRole::TypedReference {
            field: "wiki_assertion_id".into(),
        },
    ] {
        let fixture = Fixture::new();
        let mut delta = empty_delta();
        let producer = producer(
            "decisions/new.md",
            "decision_new",
            fixture.support.record.id(),
        );
        assert_eq!(
            fixture
                .c
                .query_row(
                    "SELECT count(*) FROM records WHERE id=?1",
                    [producer.record.id().as_str()],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert!(
            !maps(&fixture.c)
                .iter()
                .any(|(_, _, p)| p == producer.path.as_str())
        );
        delta
            .facts
            .as_mut()
            .unwrap()
            .edge_inserts
            .push(EligibilityEdge {
                owner_id: producer.record.id().clone(),
                target_id: fixture.support.record.id().clone(),
                role,
            });
        delta.records.push(producer);
        let endpoint_hash = fixture.support.hash.clone();
        fixture.expect_proof_only(&delta);
        let actual: String = fixture
            .c
            .query_row(
                "SELECT hash FROM records WHERE id=?1",
                [fixture.support.record.id().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual, endpoint_hash.as_str());
        assert_eq!(
            load_document(&fixture.c, &fixture.support.path, &mut 0).unwrap(),
            Some(fixture.support_doc)
        );
    }
}

#[test]
fn removing_reverse_membership_invalidates_endpoint_without_changed_document() {
    let fixture = Fixture::new();
    let producer = producer(
        "decisions/existing.md",
        "decision_existing",
        fixture.support.record.id(),
    );
    insert_record(&fixture.c, &producer);
    let edge = EligibilityEdge {
        owner_id: producer.record.id().clone(),
        target_id: fixture.support.record.id().clone(),
        role: EligibilityRole::DecisionOutput,
    };
    fixture
        .c
        .execute(
            "INSERT INTO semantic_edges VALUES(?1,?2,?3)",
            params![
                edge.owner_id.as_str(),
                edge.target_id.as_str(),
                sql::json(&edge.role).unwrap()
            ],
        )
        .unwrap();
    assert!(
        !maps(&fixture.c)
            .iter()
            .any(|(_, _, p)| p == producer.path.as_str())
    );
    let mut delta = empty_delta();
    delta.facts.as_mut().unwrap().edge_deletes.push(edge);
    fixture.expect_proof_only(&delta);
}

#[test]
fn source_inventory_edges_are_irrelevant_to_selected_proof_invalidation() {
    for inserted in [true, false] {
        let fixture = Fixture::new();
        let mut delta = empty_delta();
        let edge = EligibilityEdge {
            owner_id: fixture.support.record.id().clone(),
            target_id: id("revision_retained"),
            role: EligibilityRole::SourceInventory,
        };
        if inserted {
            delta.facts.as_mut().unwrap().edge_inserts.push(edge);
        } else {
            delta.facts.as_mut().unwrap().edge_deletes.push(edge);
        }
        let before = owner_state(&fixture.c, &fixture.policy, &fixture.owner.path);
        let affected = affected_before(&fixture.c, &delta).unwrap();
        assert!(affected.owners.is_empty());
        assert!(affected.documents.is_empty());
        apply_after(&fixture.c, &delta, &affected).unwrap();
        assert_eq!(
            owner_state(&fixture.c, &fixture.policy, &fixture.owner.path),
            before
        );
    }
}

#[test]
fn structural_producer_and_direct_path_changes_invalidate_without_changed_baseline_or_bytes() {
    for structural_change in [true, false] {
        let fixture = Fixture::new();
        let mut original = fact(&fixture.support);
        original.structural.effects.push(StructuralEffect {
            stage: StructuralStage::Decision,
            producer: Some(id("decision_old")),
            eligibility: Eligibility::Current,
            reason: "accepted_support".into(),
            details: Some(json!({"verified":true})),
        });
        original.baseline = original.structural.baseline();
        insert_fact(&fixture.c, fixture.support.record.id(), &original);
        let mut changed = original.clone();
        if structural_change {
            changed.structural.effects[0].producer = Some(id("decision_new"));
        } else {
            changed.direct_paths.insert(path("assets/new-receipt.json"));
        }
        assert_eq!(changed.baseline, original.baseline);
        assert_eq!(
            changed.structural.baseline(),
            original.structural.baseline()
        );
        let mut delta = empty_delta();
        delta
            .facts
            .as_mut()
            .unwrap()
            .records
            .push(RecordFactMutation {
                record_id: fixture.support.record.id().clone(),
                fact: changed.clone(),
            });
        fixture.expect_proof_only(&delta);
        // Once the exact replacement is central, retaining it in a later
        // closure must not create another invalidation.
        insert_fact(&fixture.c, fixture.support.record.id(), &changed);
        let affected = affected_before(&fixture.c, &delta).unwrap();
        assert!(affected.owners.is_empty());
        assert!(affected.documents.is_empty());
    }
}

#[test]
fn duplicate_changed_seeds_bump_once_and_unrelated_corpus_growth_has_fixed_fanout() {
    for unrelated_count in [0, 1024] {
        let fixture = Fixture::new();
        for ordinal in 0..unrelated_count {
            let owner = path(&format!("unrelated/{ordinal:04}.md"));
            fixture
                .c
                .execute(
                    "INSERT INTO unit_owners VALUES(?1,?2,?3,?3,?4,?4,0,0)",
                    params![
                        fixture.policy.as_str(),
                        owner.as_str(),
                        Blake3Hash::digest(owner.as_str()).as_str(),
                        EPOCH
                    ],
                )
                .unwrap();
            fixture
                .c
                .execute(
                    "INSERT INTO unit_owner_dependencies VALUES(?1,?2,?2)",
                    params![fixture.policy.as_str(), owner.as_str()],
                )
                .unwrap();
        }
        let before: i64 = fixture
            .c
            .query_row(
                "SELECT count(*) FROM unit_owners WHERE proof_version=?1 AND modified_seq=?1",
                [EPOCH],
                |row| row.get(0),
            )
            .unwrap();
        let mut delta = empty_delta();
        let mut changed_row = fixture.support.clone();
        changed_row.hash = Blake3Hash::digest(b"changed support bytes, same eligibility");
        delta.dependencies.push(dependency(
            changed_row.path.as_str(),
            ExpectedState::Hash(changed_row.hash.clone()),
        ));
        let mut changed_fact = fact(&fixture.support);
        changed_fact.direct_paths.insert(path("assets/changed.txt"));
        delta
            .facts
            .as_mut()
            .unwrap()
            .records
            .push(RecordFactMutation {
                record_id: changed_row.record.id().clone(),
                fact: changed_fact,
            });
        delta.records.push(changed_row);
        fixture.expect_proof_only(&delta);
        let old: i64 = fixture
            .c
            .query_row(
                "SELECT count(*) FROM unit_owners WHERE proof_version=?1 AND modified_seq=?1",
                [EPOCH],
                |row| row.get(0),
            )
            .unwrap();
        let new: i64 = fixture
            .c
            .query_row(
                "SELECT count(*) FROM unit_owners WHERE proof_version=?1 AND modified_seq=?1",
                [EPOCH + 1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((old, new), (before - 1, 1));
    }
}

/// Mirror only the caller's delta savepoint boundary around the two hooks. The
/// attempt marker proves rollback cleans earlier savepoint-local work. This
/// deliberately does not stand in for CatalogDelta::apply_for_operation.
fn mock_delta_savepoint(c: &Connection, delta: &CatalogDelta) -> Result<()> {
    c.execute_batch(
        "SAVEPOINT lwiki_catalog_delta; INSERT INTO publication_attempts VALUES('inside delta');",
    )
    .map_err(sql::sql_error)?;
    let result = (|| {
        let affected = affected_before(c, delta)?;
        for dep in &delta.dependencies {
            insert_dependency(c, dep);
        }
        apply_after(c, delta, &affected)?;
        c.execute(
            "UPDATE catalog_meta SET epoch=epoch+1,publication_hash=?1",
            [Blake3Hash::digest(sql::json(delta)?).as_str()],
        )
        .map_err(sql::sql_error)?;
        Ok(())
    })();
    match result {
        Ok(()) => c
            .execute_batch("RELEASE lwiki_catalog_delta")
            .map_err(sql::sql_error),
        Err(error) => {
            c.execute_batch("ROLLBACK TO lwiki_catalog_delta; RELEASE lwiki_catalog_delta")
                .map_err(sql::sql_error)?;
            Err(error)
        }
    }
}
fn snapshot(c: &Connection) -> Vec<String> {
    // Each row's JSON is computed by SQLite only for this bounded fixture. No
    // snapshot serializer participates in the implementation under test.
    let mut out = Vec::new();
    for query in [
        "SELECT json_array(singleton,epoch,publication_hash,state) FROM catalog_meta ORDER BY singleton",
        "SELECT json_array(path,file_hash,eligibility,raw_text) FROM documents ORDER BY path",
        "SELECT json_array(id,path,hash,row_json) FROM records ORDER BY id",
        "SELECT json_array(path,expected_hash) FROM dependencies ORDER BY path",
        "SELECT json_array(policy,version,parser_hash,settings_json,complete,after_owner,unit_count) FROM unit_policies ORDER BY policy",
        "SELECT json_array(policy,owner,source_hash,render_token,proof_version,modified_seq,tombstone,unit_count) FROM unit_owners ORDER BY policy,owner",
        "SELECT json_array(policy,owner,unit_id,input_hash,descriptor_json) FROM retrieval_units ORDER BY policy,owner,unit_id",
        "SELECT json_array(policy,owner,path) FROM unit_owner_dependencies ORDER BY policy,owner,path",
        "SELECT json_array(record_id,baseline_json,structural_json) FROM record_eligibility_facts ORDER BY record_id",
        "SELECT json_array(owner_id,path) FROM record_direct_paths ORDER BY owner_id,path",
        "SELECT json_array(value) FROM publication_attempts ORDER BY value",
    ] {
        out.extend(
            c.prepare(query)
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap(),
        );
    }
    out
}

#[test]
fn affected_owner_pair_limit_refuses_in_delta_savepoint_without_partial_publication() {
    let fixture = Fixture::new();
    // 2048 paths in two registered policies reach 4096 distinct pairs.
    // The limit must count policy/owner bindings rather than owner paths.
    let parser = Blake3Hash::digest(b"in-memory parser");
    let mut settings = EmbeddingSettings::default();
    settings.quality_target_bytes = Some(1000);
    let second_policy = RenderPolicyId::for_settings(&parser, &settings).unwrap();
    fixture.c.execute("INSERT INTO unit_policies(policy,version,parser_hash,settings_json,complete,after_owner) VALUES(?1,1,?2,?3,1,NULL)", params![second_policy.as_str(), parser.as_str(), sql::json(&settings).unwrap()]).unwrap();
    for policy in [&fixture.policy, &second_policy] {
        if policy == &second_policy {
            fixture
                .c
                .execute(
                    "INSERT INTO unit_owners VALUES(?1,?2,?3,?3,?4,?4,0,0)",
                    params![
                        policy.as_str(),
                        fixture.owner.path.as_str(),
                        fixture.owner.hash.as_str(),
                        EPOCH
                    ],
                )
                .unwrap();
            fixture
                .c
                .execute(
                    "INSERT INTO unit_owner_dependencies VALUES(?1,?2,?3)",
                    params![
                        policy.as_str(),
                        fixture.owner.path.as_str(),
                        fixture.support.path.as_str()
                    ],
                )
                .unwrap();
        }
        for ordinal in 0..2047 {
            let owner = path(&format!("fanout/{ordinal:04}.md"));
            fixture
                .c
                .execute(
                    "INSERT INTO unit_owners VALUES(?1,?2,?3,?3,?4,?4,0,0)",
                    params![
                        policy.as_str(),
                        owner.as_str(),
                        Blake3Hash::digest(owner.as_str()).as_str(),
                        EPOCH
                    ],
                )
                .unwrap();
            fixture
                .c
                .execute(
                    "INSERT INTO unit_owner_dependencies VALUES(?1,?2,?3)",
                    params![
                        policy.as_str(),
                        owner.as_str(),
                        fixture.support.path.as_str()
                    ],
                )
                .unwrap();
        }
    }
    let mut delta = empty_delta();
    delta.dependencies.push(dependency(
        fixture.support.path.as_str(),
        ExpectedState::Hash(Blake3Hash::digest(b"one real changed dependency")),
    ));
    assert_eq!(
        affected_before(&fixture.c, &delta).unwrap().owners.len(),
        4096
    );
    fixture
        .c
        .execute(
            "INSERT INTO unit_owners VALUES(?1,'fanout/overflow.md',?2,?2,?3,?3,0,0)",
            params![
                fixture.policy.as_str(),
                Blake3Hash::digest(b"overflow").as_str(),
                EPOCH
            ],
        )
        .unwrap();
    fixture
        .c
        .execute(
            "INSERT INTO unit_owner_dependencies VALUES(?1,'fanout/overflow.md',?2)",
            params![fixture.policy.as_str(), fixture.support.path.as_str()],
        )
        .unwrap();
    let before = snapshot(&fixture.c);
    fixture.c.execute_batch("BEGIN IMMEDIATE").unwrap();
    let error = mock_delta_savepoint(&fixture.c, &delta).unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(snapshot(&fixture.c), before);
    assert!(!fixture.c.is_autocommit());
    // The caller can still perform orderly outer-transaction recovery/cleanup.
    fixture
        .c
        .execute(
            "INSERT INTO publication_attempts VALUES('outer remains usable')",
            [],
        )
        .unwrap();
    fixture.c.execute_batch("COMMIT").unwrap();
    assert_eq!(
        fixture
            .c
            .query_row("SELECT count(*) FROM publication_attempts", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
}

//! Graph writes start with normalized authority; rebuild is a test oracle only.
use crate::{
    app::{OfflineApp, OperationOptions},
    catalog::{
        Catalog,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    changes::{ChangeEngine, ChangeStatus, PreparedChange},
    domain::*,
    graph::{self, ExportRequest, ExtractionLimits, GraphPlan},
    retrieval::VerificationBudget,
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceView},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use serde_json::{Value, json};
use std::{fs, time::Duration};

fn intended_publication(
    snapshot: &ReadSnapshot,
) -> crate::changes::operation_authority::Publication {
    let binding = snapshot
        .publication()
        .expect("test fixture has a published catalog");
    crate::changes::operation_authority::Publication {
        file_id: binding.file_id.clone(),
        epoch: snapshot.generation,
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    app: OfflineApp,
    catalog: Catalog,
    text: String,
    packet: graph::ExtractionPacket,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"),"---\nwiki_schema: '1'\nwiki_id: vault_graph_write\nwiki_kind: vault\ntitle: Selected graph writes\n---\n").unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let app = OfflineApp::new(
            handle.clone(),
            OperationOptions {
                offline: true,
                ..Default::default()
            },
        )
        .unwrap();
        app.index_rebuild_normalized().unwrap();
        let text = format!(
            "# Café\n\n{}",
            (0..5)
                .map(|i| format!("P{i} uses T{i}.\n"))
                .collect::<String>()
        );
        let captured = app
            .source_add(CaptureRequest {
                title: "Five relationships".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "selected note.txt".into(),
                original: text.as_bytes().to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            })
            .unwrap();
        let packet = app
            .graph_extract_agent(&ExportRequest {
                source_id: captured.allocated_ids["source"].clone(),
                revision_id: None,
                windows: vec![],
                limits: ExtractionLimits::default(),
                candidate_context: vec![],
            })
            .unwrap()
            .packet;
        let catalog = Catalog::new(handle.clone(), RecordId::new("vault_graph_write").unwrap());
        Self {
            _temp: temp,
            fs: handle,
            app,
            catalog,
            text,
            packet,
        }
    }
    fn imported(&self) -> Value {
        self.imported_count(5, false)
    }
    fn imported_count(&self, count: usize, new_extraction: bool) -> Value {
        let mut mentions = vec![];
        let mut assertions = vec![];
        for i in 0..count {
            for (prefix, local) in [("P", format!("p{i}")), ("T", format!("t{i}"))] {
                let label = format!("{prefix}{i}");
                let start = self.text.find(&label).unwrap();
                mentions.push(json!({"id":local,"window_id":"w1","label":label,"type":"component","quote":label,"span":{"start":start,"end":start+label.len()}}));
            }
            assertions.push(json!({"id":format!("a{i}"),"subject":format!("p{i}"),"predicate":"uses","object":{"kind":"mention","mention_id":format!("t{i}")},"negated":false,"modality":"asserted","evidence":[{"window_id":"w1","stance":"supports","quote":format!("P{i} uses T{i}.")}]}));
        }
        self.app.graph_import(&serde_json::to_vec(&json!({"schema":graph::EXTRACTION_SCHEMA,"packet_id":self.packet.packet_id,"packet_fingerprint":self.packet.packet_fingerprint,"mentions":mentions,"assertions":assertions,"unresolved":[]})).unwrap(),new_extraction).unwrap()
    }
    fn prepared(outcome: &Value) -> PreparedChange {
        serde_json::from_value(outcome["prepared"].clone()).unwrap()
    }
    fn apply(&self, outcome: &Value) {
        assert_eq!(
            self.app
                .changes_apply(Self::prepared(outcome).change_id)
                .unwrap()
                .status,
            Some(ChangeStatus::Committed)
        );
    }
    fn hash(&self, id: &RecordId, kind: RecordKind) -> Blake3Hash {
        let view = SourceView::from_fs_bounded(&self.fs, 64 * 1024 * 1024, 4096).unwrap();
        view.resolve(id, kind, None).unwrap().1.source_hash.clone()
    }
    fn resolved(&self, imported: &Value) -> Value {
        let id: RecordId =
            serde_json::from_value(imported["extraction"]["record"]["record_id"].clone()).unwrap();
        let mappings=(0..5).flat_map(|i|[(format!("p{i}"),format!("P{i}")),(format!("t{i}"),format!("T{i}"))]).map(|(local,title)|json!({"mention_id":local,"operation":"CreateEntity","title":title,"entity_type":"component","reason":"Distinct explicit component."})).collect::<Vec<_>>();
        self.app.graph_resolve(&serde_json::to_vec(&json!({"schema":graph::RESOLUTION_SCHEMA,"extraction_id":id,"expected_hash":self.hash(&id,RecordKind::Extraction),"mappings":mappings})).unwrap()).unwrap()
    }
    fn reviewed(&self, imported: &Value) -> Value {
        let decisions=(0..5).map(|i|{
            let assertion:RecordId=serde_json::from_value(imported["allocations"]["assertions"][format!("a{i}")].clone()).unwrap();
            let evidence:RecordId=serde_json::from_value(imported["allocations"]["evidence"][format!("a{i}")][0].clone()).unwrap();
            json!({"assertion_id":assertion,"expected_hash":self.hash(&assertion,RecordKind::Assertion),"decision":"accept","reason":"The exact note states this relation.","evidence_checks":[{"evidence_id":evidence,"expected_hash":self.hash(&evidence,RecordKind::Evidence),"assessment":"supports"}]})
        }).collect::<Vec<_>>();
        self.app
            .graph_review(
                &serde_json::to_vec(
                    &json!({"schema":graph::GRAPH_REVIEW_SCHEMA,"decisions":decisions}),
                )
                .unwrap(),
            )
            .unwrap()
    }
    fn semantic_rows(&self) -> Value {
        use crate::catalog::{eligibility_facts::EligibilityRole, policy_facts::PolicyKind};
        let view = SourceView::from_fs_bounded(&self.fs, 64 * 1024 * 1024, 4096).unwrap();
        let reader = self
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let mut roles = vec![
            EligibilityRole::DeclaredSupport,
            EligibilityRole::SourceInventory,
            EligibilityRole::ExtractionSource,
            EligibilityRole::ExtractionRevision,
            EligibilityRole::DecisionInput,
            EligibilityRole::DecisionOutput,
        ];
        roles.extend(
            [
                "wiki_superseded_by_id",
                "wiki_subject_id",
                "wiki_object_id",
                "wiki_assertion_id",
                "wiki_source_id",
                "wiki_source_revision",
                "wiki_supersedes_id",
                "wiki_extraction_id",
                "wiki_current_revision",
                "wiki_packet_id",
                "wiki_checkpoint_event_id",
                "wiki_run_id",
            ]
            .map(|field| EligibilityRole::TypedReference {
                field: field.into(),
            }),
        );
        roles.extend([
            EligibilityRole::AssertionEvidence,
            EligibilityRole::GenerationPacket,
            EligibilityRole::PolicySupersession,
        ]);
        let mut rows = vec![];
        for note in view.notes.values() {
            if let Some(record) = &note.canonical
                && let Some(row) = reader.record(record.id()).unwrap()
            {
                rows.push(json!({"row":row,"fact":reader.eligibility_fact(record.id()).unwrap(),"edges":reader.outgoing_edges(record.id(),&roles).unwrap()}));
            }
        }
        rows.sort_by_key(|r| r.to_string());
        json!({"rows":rows,"review":reader.policy_state(PolicyKind::Review).unwrap(),"remap":reader.policy_state(PolicyKind::Remap).unwrap()})
    }
}
#[test]
fn normalized_graph_large_resolution_matches_rebuild_and_returns_exact_citations() {
    let f = Fixture::new();
    let imported = f.imported();
    f.apply(&imported);
    let resolved = f.resolved(&imported);
    let change = Fixture::prepared(&resolved);
    let engine = ChangeEngine::new(f.fs.clone()).unwrap();
    assert!(
        engine
            .inspect(&change.change_id)
            .unwrap()
            .manifest
            .operations
            .len()
            > 16
    );
    f.apply(&resolved);
    let reviewed = f.reviewed(&imported);
    f.apply(&reviewed);
    for i in 0..5 {
        let id: RecordId =
            serde_json::from_value(resolved["allocations"]["entities"][format!("p{i}")].clone())
                .unwrap();
        let out = graph::indexed_neighbors::neighbors(
            &f.catalog,
            &id,
            &GraphPlan::default(),
            &VerificationBudget::default(),
            true,
        )
        .unwrap();
        assert_eq!(out.assertions.len(), 1);
        for evidence in &out.assertions[0].support {
            let view = SourceView::from_fs_bounded(&f.fs, 64 * 1024 * 1024, 4096).unwrap();
            assert_eq!(
                view.verify(
                    evidence.citation.as_ref().unwrap(),
                    crate::sources::CitationScope::Current
                )
                .unwrap()
                .quote,
                format!("P{i} uses T{i}.").as_bytes()
            );
        }
    }
    let before = f.semantic_rows();
    // Every Evidence fact must retain the immutable assets checked for its
    // citation, in addition to its own canonical path.
    for row in before["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["row"]["record"]["wiki_kind"] == "evidence")
    {
        let paths = row["fact"]["direct_paths"].as_array().unwrap();
        assert_eq!(paths.len(), 3, "Evidence direct dependencies: {row}");
        assert!(
            paths
                .iter()
                .any(|path| path.as_str().unwrap().ends_with("/content.md"))
        );
        assert!(
            paths
                .iter()
                .any(|path| path.as_str().unwrap().ends_with("/original.bin"))
        );
    }
    f.app.index_rebuild_normalized().unwrap();
    let rebuilt = f.semantic_rows();
    for (selected, rebuilt) in before["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(rebuilt["rows"].as_array().unwrap())
    {
        assert_eq!(
            selected, rebuilt,
            "selected/rebuilt record differs: {}",
            selected["row"]["record"]["wiki_id"]
        );
    }
    assert_eq!(rebuilt, before);
}
#[test]
fn normalized_graph_first_admission_is_staged_and_preserves_manifest() {
    let f = Fixture::new();
    let imported = f.imported();
    let change = Fixture::prepared(&imported);
    let engine = ChangeEngine::new(f.fs.clone()).unwrap();
    let before = engine.load_manifest_structure(&change.change_id).unwrap();
    let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
    let session = crate::catalog::source_refresh::IndexedRefreshSession::adopt_graph(
        &f.catalog, &writer, &change,
    )
    .unwrap();
    assert_eq!(session.proof().version, 5);
    drop(session);
    drop(writer);
    assert_eq!(
        engine.load_manifest_structure(&change.change_id).unwrap(),
        before
    );
    let recovery = f.app.recover().unwrap();
    let _ = recovery;
    assert!(
        !f.fs
            .root()
            .path()
            .join(imported["extraction"]["path"].as_str().unwrap())
            .exists()
    );
    f.apply(&imported);
    f.apply(&imported);
    assert_eq!(
        engine.load_manifest_structure(&change.change_id).unwrap(),
        before
    );
}
#[test]
fn normalized_graph_review_stale_guard_preserves_external_bytes() {
    let f = Fixture::new();
    let imported = f.imported();
    f.apply(&imported);
    let resolved = f.resolved(&imported);
    f.apply(&resolved);
    let review = f.reviewed(&imported);
    let id: RecordId =
        serde_json::from_value(imported["allocations"]["assertions"]["a0"].clone()).unwrap();
    let view = SourceView::from_fs_bounded(&f.fs, 64 * 1024 * 1024, 4096).unwrap();
    let path = view
        .resolve(&id, RecordKind::Assertion, None)
        .unwrap()
        .0
        .clone();
    let physical = f.fs.root().path().join(path.as_str());
    let mut bytes = fs::read(&physical).unwrap();
    bytes.extend_from_slice(b"\nOwner clarification.\n");
    fs::write(&physical, &bytes).unwrap();
    assert!(
        f.app
            .changes_apply(Fixture::prepared(&review).change_id)
            .is_err()
    );
    assert_eq!(fs::read(physical).unwrap(), bytes);
}
struct StopSql(crate::catalog::PublicationCheckpoint);
impl crate::catalog::PublicationFault for StopSql {
    fn check(&self, point: crate::catalog::PublicationCheckpoint) -> Result<()> {
        if point == self.0 {
            Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "injected graph publication interruption",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn normalized_graph_recovers_before_and_after_sql_commit_without_reallocation() {
    use crate::catalog::{
        CatalogOptions, PublicationCheckpoint, source_refresh::IndexedRefreshSession,
    };
    for cut in [
        PublicationCheckpoint::AfterPointer,
        PublicationCheckpoint::AfterCommit,
    ] {
        let f = Fixture::new();
        let imported = f.imported();
        let change = Fixture::prepared(&imported);
        let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
        let faulty = Catalog::with_options(
            f.fs.clone(),
            RecordId::new("vault_graph_write").unwrap(),
            CatalogOptions {
                busy_timeout_ms: 1000,
                fault: Some(std::sync::Arc::new(StopSql(cut))),
            },
        );
        let engine = ChangeEngine::new(f.fs.clone()).unwrap();
        let before = engine.load_manifest_structure(&change.change_id).unwrap();
        let mut session = IndexedRefreshSession::adopt_graph(&faulty, &writer, &change).unwrap();
        assert!(engine.apply_indexed_refresh(&writer, &mut session).is_err());
        drop(session);
        drop(writer);
        f.apply(&imported);
        assert_eq!(
            engine.load_manifest_structure(&change.change_id).unwrap(),
            before
        );
        let resolved = f.resolved(&imported);
        f.apply(&resolved);
        let review = f.reviewed(&imported);
        f.apply(&review);
    }
}
#[test]
fn normalized_graph_missing_active_proof_cannot_be_readmitted() {
    use crate::catalog::{
        CatalogOptions, PublicationCheckpoint, source_refresh::IndexedRefreshSession,
    };
    let f = Fixture::new();
    let imported = f.imported();
    let change = Fixture::prepared(&imported);
    let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
    let faulty = Catalog::with_options(
        f.fs.clone(),
        RecordId::new("vault_graph_write").unwrap(),
        CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(StopSql(
                PublicationCheckpoint::AfterPointer,
            ))),
        },
    );
    let engine = ChangeEngine::new(f.fs.clone()).unwrap();
    let mut session = IndexedRefreshSession::adopt_graph(&faulty, &writer, &change).unwrap();
    assert!(engine.apply_indexed_refresh(&writer, &mut session).is_err());
    drop(session);
    drop(writer);
    fs::remove_file(
        f.fs.root().path().join(
            crate::changes::indexed_refresh::baseline_path(&change)
                .unwrap()
                .as_str(),
        ),
    )
    .unwrap();
    assert_eq!(
        f.app.changes_apply(change.change_id).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
}

#[test]
fn normalized_graph_review_rejects_forged_origin_allocations_and_extra_writes() {
    for variant in 0..3 {
        let f = Fixture::new();
        let imported = f.imported();
        f.apply(&imported);
        let resolved = f.resolved(&imported);
        f.apply(&resolved);
        let review = f.reviewed(&imported);
        let engine = ChangeEngine::new(f.fs.clone()).unwrap();
        let (_, mut draft, _) =
            crate::catalog::graph_projection::retained_draft(&engine, &Fixture::prepared(&review))
                .unwrap();
        let before = f.semantic_rows();
        match variant {
            0 => {
                draft.origin.as_mut().unwrap().response_hash =
                    Blake3Hash::digest(b"forged review request")
            }
            1 => {
                draft.allocated_ids.insert(
                    "extra".into(),
                    RecordId::new("decision_unauthorized").unwrap(),
                );
            }
            _ => {
                let mut extra = draft.operations[0].clone();
                extra.target =
                    VaultRelativePath::new("knowledge/assertions/unauthorized.md").unwrap();
                extra.expected = crate::vault::ExpectedState::Absent;
                draft.operations.push(extra);
            }
        }
        // prepare() intentionally reuses a same-origin proposal before visiting
        // its draft. Retain a distinct proposal, then forge its authenticated
        // origin/manifest/prefix so every variant reaches graph admission.
        let forged_origin = draft.origin.take().unwrap();
        let expected_allocations = draft.allocated_ids.clone();
        let mut expected_targets: Vec<_> = draft
            .operations
            .iter()
            .map(|op| op.target.clone())
            .collect();
        expected_targets.sort();
        let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
        let forged = engine.prepare(&writer, draft).unwrap();
        drop(writer);
        assert_ne!(
            forged.prepared.change_id,
            Fixture::prepared(&review).change_id
        );
        let mut manifest = forged.manifest;
        manifest.origin = Some(forged_origin.clone());
        let hash = Blake3Hash::digest(serde_json::to_vec(&manifest).unwrap());
        fs::write(
            f.fs.root().path().join(
                crate::changes::prepare::manifest_path(&manifest.change_id)
                    .unwrap()
                    .as_str(),
            ),
            crate::changes::prepare::render_prepared_note(&manifest, &hash).unwrap(),
        )
        .unwrap();
        fs::write(
            f.fs.root().path().join(
                crate::changes::journal::journal_path(&manifest.change_id)
                    .unwrap()
                    .as_str(),
            ),
            crate::changes::journal::encode_frame(&crate::changes::JournalFrame {
                version: 1,
                sequence: 0,
                change_id: manifest.change_id.clone(),
                manifest_hash: hash,
                event: crate::changes::ChangeEvent::Prepared,
            })
            .unwrap(),
        )
        .unwrap();
        let actual = engine.inspect(&manifest.change_id).unwrap();
        assert_eq!(actual.manifest.origin, Some(forged_origin));
        assert_eq!(actual.manifest.allocated_ids, expected_allocations);
        assert_eq!(
            actual
                .manifest
                .operations
                .iter()
                .map(|op| op.target.clone())
                .collect::<Vec<_>>(),
            expected_targets
        );
        let original_targets = target_bytes(&f, &actual.prepared);
        assert!(
            f.app
                .changes_apply(actual.prepared.change_id.clone())
                .is_err(),
            "forged review variant {variant} must be refused"
        );
        assert_eq!(target_bytes(&f, &actual.prepared), original_targets);
        assert_eq!(f.semantic_rows(), before);
        assert!(
            !f.fs
                .root()
                .path()
                .join("knowledge/assertions/unauthorized.md")
                .exists()
        );
    }
}

/// Recompute every self-authenticated checksum so the admission/active authority
/// must reject the semantic change, rather than a broken JSON checksum.
fn rehash_graph_delta(f: &Fixture, change: &PreparedChange, mutation: usize) {
    use crate::catalog::source_refresh::{RetainedDelta, intended};
    use crate::changes::indexed_refresh::{IndexedRefreshProof, baseline_path};
    let path =
        f.fs.root()
            .path()
            .join(baseline_path(change).unwrap().as_str());
    let mut receipt: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let facts = &mut receipt["embedded_delta"]["rows"]["facts"];
    let edges = facts["edge_inserts"].as_array_mut().unwrap();
    let position = edges
        .iter()
        .position(|edge| edge["role"]["role"] == "assertion_evidence")
        .expect("resolution includes assertion/evidence membership");
    let mut relation = edges[position].clone();
    match mutation {
        0 => {
            edges.remove(position);
        }
        1 => {
            relation["role"] = json!({"role":"policy_supersession"});
            facts["edge_deletes"].as_array_mut().unwrap().push(relation);
        }
        2 | 3 => {
            relation["role"] =
                json!({"role":if mutation==2 {"policy_supersession"} else {"generation_packet"}});
            edges.push(relation);
        }
        _ => unreachable!(),
    }
    let delta: RetainedDelta = serde_json::from_value(receipt["embedded_delta"].clone()).unwrap();
    let mut proof: IndexedRefreshProof = serde_json::from_value(receipt["proof"].clone()).unwrap();
    proof.delta_hash = Blake3Hash::digest(serde_json::to_vec(&delta).unwrap());
    proof.intended = intended(&proof.base, &proof.change, &proof.delta_hash, 5).unwrap();
    let checksum = Blake3Hash::digest(serde_json::to_vec(&(&proof, Some(&delta))).unwrap());
    fs::write(
        &path,
        serde_json::to_vec(&json!({"proof":proof,"embedded_delta":delta,"checksum":checksum}))
            .unwrap(),
    )
    .unwrap();
    // This proves the tampering survived the ordinary envelope codec.
    ChangeEngine::new(f.fs.clone())
        .unwrap()
        .load_indexed_refresh_proof(change)
        .unwrap()
        .unwrap();
}
fn target_bytes(f: &Fixture, change: &PreparedChange) -> Vec<Option<Vec<u8>>> {
    let (manifest, _) = ChangeEngine::new(f.fs.clone())
        .unwrap()
        .load_manifest_structure(&change.change_id)
        .unwrap();
    manifest
        .operations
        .iter()
        .map(|op| fs::read(f.fs.root().path().join(op.target.as_str())).ok())
        .collect()
}

#[test]
fn normalized_graph_prepared_rehash_cannot_remove_or_forge_relations() {
    use crate::catalog::source_refresh::IndexedRefreshSession;
    for mutation in 0..4 {
        let f = Fixture::new();
        let imported = f.imported();
        f.apply(&imported);
        let resolved = f.resolved(&imported);
        let change = Fixture::prepared(&resolved);
        let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
        drop(IndexedRefreshSession::adopt_graph(&f.catalog, &writer, &change).unwrap());
        drop(writer);
        let before = target_bytes(&f, &change);
        rehash_graph_delta(&f, &change, mutation);
        assert!(f.app.changes_apply(change.change_id.clone()).is_err());
        assert_eq!(target_bytes(&f, &change), before);
        assert!(
            f.catalog
                .operation_state()
                .unwrap()
                .unwrap()
                .active()
                .is_none()
        );
    }
}

#[test]
fn normalized_graph_rehash_is_refused_at_applying_files_applied_and_published_cuts() {
    use crate::{
        catalog::{CatalogOptions, PublicationCheckpoint, source_refresh::IndexedRefreshSession},
        changes::{ChangeEvent, journal, operation_authority},
    };
    for cut in 0..3 {
        let f = Fixture::new();
        let imported = f.imported();
        f.apply(&imported);
        let resolved = f.resolved(&imported);
        let change = Fixture::prepared(&resolved);
        let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
        let engine = ChangeEngine::new(f.fs.clone()).unwrap();
        if cut == 0 {
            // Exact first-activation boundary, before the executor's first file.
            let session = IndexedRefreshSession::adopt_graph(&f.catalog, &writer, &change).unwrap();
            session.validate_before_files().unwrap();
            let proof = session.proof().clone();
            let authority = f.catalog.operation_state().unwrap().unwrap();
            operation_authority::begin_with_intent(
                &f.fs,
                &writer,
                &authority,
                change.clone(),
                intended_publication(&proof.intended),
                proof.activation_binding().unwrap(),
            )
            .unwrap();
            let (manifest, hash) = engine.load_manifest_structure(&change.change_id).unwrap();
            journal::append_event(&f.fs, &writer, &manifest, &hash, ChangeEvent::Applying).unwrap();
        } else {
            let faulty = Catalog::with_options(
                f.fs.clone(),
                RecordId::new("vault_graph_write").unwrap(),
                CatalogOptions {
                    busy_timeout_ms: 1000,
                    fault: Some(std::sync::Arc::new(StopSql(if cut == 1 {
                        PublicationCheckpoint::AfterPointer
                    } else {
                        PublicationCheckpoint::AfterCommit
                    }))),
                },
            );
            let mut session =
                IndexedRefreshSession::adopt_graph(&faulty, &writer, &change).unwrap();
            assert!(engine.apply_indexed_refresh(&writer, &mut session).is_err());
        }
        drop(writer);
        let authority = f.catalog.operation_state().unwrap().unwrap();
        let original_binding = authority.active().unwrap().intent_hash.clone();
        assert!(original_binding.is_some());
        let before = target_bytes(&f, &change);
        rehash_graph_delta(&f, &change, 0);
        assert_eq!(
            f.app
                .changes_apply(change.change_id.clone())
                .unwrap_err()
                .code,
            ErrorCode::RecoveryRequired
        );
        assert_eq!(target_bytes(&f, &change), before);
        assert_eq!(
            f.catalog
                .operation_state()
                .unwrap()
                .unwrap()
                .active()
                .unwrap()
                .intent_hash,
            original_binding
        );
    }
}

#[test]
fn normalized_graph_active_missing_intent_binding_is_never_upgraded() {
    use crate::{
        catalog::source_refresh::IndexedRefreshSession,
        changes::{ChangeEvent, journal, operation_authority},
    };
    let f = Fixture::new();
    let imported = f.imported();
    let change = Fixture::prepared(&imported);
    let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
    let session = IndexedRefreshSession::adopt_graph(&f.catalog, &writer, &change).unwrap();
    session.validate_before_files().unwrap();
    let proof = session.proof().clone();
    let authority = f.catalog.operation_state().unwrap().unwrap();
    operation_authority::begin(
        &f.fs,
        &writer,
        &authority,
        change.clone(),
        intended_publication(&proof.intended),
    )
    .unwrap();
    let engine = ChangeEngine::new(f.fs.clone()).unwrap();
    let (manifest, hash) = engine.load_manifest_structure(&change.change_id).unwrap();
    journal::append_event(&f.fs, &writer, &manifest, &hash, ChangeEvent::Applying).unwrap();
    drop(session);
    drop(writer);
    let before = target_bytes(&f, &change);
    assert_eq!(
        f.app
            .changes_apply(change.change_id.clone())
            .unwrap_err()
            .code,
        ErrorCode::RecoveryRequired
    );
    assert_eq!(target_bytes(&f, &change), before);
    assert!(
        f.catalog
            .operation_state()
            .unwrap()
            .unwrap()
            .active()
            .unwrap()
            .intent_hash
            .is_none()
    );
}

#[test]
fn normalized_graph_rejects_count_and_combined_payload_bound_before_body_reads() {
    use crate::changes::{
        ChangeEvent, JournalFrame, OriginOperation, PayloadRef, journal, prepare,
    };
    for over_count in [true, false] {
        let f = Fixture::new();
        let imported = f.imported();
        let change = Fixture::prepared(&imported);
        let engine = ChangeEngine::new(f.fs.clone()).unwrap();
        let (mut manifest, _) = engine.load_manifest_structure(&change.change_id).unwrap();
        manifest.origin.as_mut().unwrap().operation = OriginOperation::GraphResolve;
        // Bodies deliberately do not exist. A premature generic validation
        // reports a missing payload instead of the bounded graph shape error.
        if over_count {
            let original = manifest.operations[0].clone();
            manifest.operations = (0..770)
                .map(|index| {
                    let mut op = original.clone();
                    op.target = VaultRelativePath::new(format!(
                        "knowledge/assertions/bounded_{index:05}.md"
                    ))
                    .unwrap();
                    op.after_payload.as_mut().unwrap().path = VaultRelativePath::new(format!(
                        "changes/{}/proposed/{index:05}.md",
                        change.change_id
                    ))
                    .unwrap();
                    op
                })
                .collect();
        } else {
            let op = &mut manifest.operations[0];
            let hash = Blake3Hash::digest(b"before body deliberately absent");
            op.before = crate::vault::ExpectedState::Hash(hash.clone());
            op.before_payload = Some(PayloadRef {
                path: VaultRelativePath::new(format!(
                    "changes/{}/before/00000.md",
                    change.change_id
                ))
                .unwrap(),
                hash,
                byte_len: 33 * 1024 * 1024,
            });
            op.after_payload.as_mut().unwrap().byte_len = 33 * 1024 * 1024;
        }
        // Remove the one original body too, including index 00000.
        fs::remove_file(
            f.fs.root()
                .path()
                .join(format!("changes/{}/proposed/00000.md", change.change_id)),
        )
        .unwrap();
        let hash = Blake3Hash::digest(serde_json::to_vec(&manifest).unwrap());
        fs::write(
            f.fs.root()
                .path()
                .join(prepare::manifest_path(&change.change_id).unwrap().as_str()),
            prepare::render_prepared_note(&manifest, &hash).unwrap(),
        )
        .unwrap();
        fs::write(
            f.fs.root()
                .path()
                .join(journal::journal_path(&change.change_id).unwrap().as_str()),
            journal::encode_frame(&JournalFrame {
                version: 1,
                sequence: 0,
                change_id: change.change_id.clone(),
                manifest_hash: hash,
                event: ChangeEvent::Prepared,
            })
            .unwrap(),
        )
        .unwrap();
        let error = f.app.changes_apply(change.change_id).unwrap_err();
        if over_count {
            assert!(error.message.contains("operation shape"), "{error:?}");
        } else {
            assert_eq!(error.code, ErrorCode::BudgetExceeded, "{error:?}");
        }
        assert!(
            f.catalog
                .operation_state()
                .unwrap()
                .unwrap()
                .active()
                .is_none()
        );
    }
}

#[test]
fn normalized_graph_terminal_retains_full_v5_after_later_publication_and_payload_expiry() {
    use crate::catalog::source_refresh::IndexedRefreshSession;
    use crate::changes::indexed_refresh::baseline_path;
    let f = Fixture::new();
    let imported = f.imported();
    let change = Fixture::prepared(&imported);
    let engine = ChangeEngine::new(f.fs.clone()).unwrap();
    let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
    let mut session = IndexedRefreshSession::adopt_graph(&f.catalog, &writer, &change).unwrap();
    let proof = session.proof().clone();
    let retained_path =
        f.fs.root()
            .path()
            .join(baseline_path(&change).unwrap().as_str());
    let original = fs::read(&retained_path).unwrap();
    let report = engine.apply_indexed_refresh(&writer, &mut session).unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    assert_eq!(report.snapshot.as_ref(), Some(&proof.intended));
    assert_eq!(fs::read(&retained_path).unwrap(), original);
    // Cover the idle terminal session dispatch in addition to fresh completion.
    assert_eq!(
        engine.apply_indexed_refresh(&writer, &mut session).unwrap(),
        report
    );
    drop(session);
    drop(writer);
    assert_eq!(
        f.app
            .changes_apply(change.change_id.clone())
            .unwrap()
            .snapshot,
        report.snapshot
    );
    let resolved = f.resolved(&imported);
    f.apply(&resolved);
    let after_resolution = f.semantic_rows();
    let (manifest, _) = engine.load_manifest_structure(&change.change_id).unwrap();
    for payload in manifest
        .operations
        .iter()
        .flat_map(|op| [&op.before_payload, &op.after_payload])
        .flatten()
    {
        fs::remove_file(f.fs.root().path().join(payload.path.as_str())).unwrap();
    }
    assert!(
        engine.inspect(&change.change_id).is_err(),
        "fresh payload verification must still refuse expired retained bodies"
    );
    let retry = f.app.changes_apply(change.change_id.clone()).unwrap();
    assert_eq!(retry.status, Some(ChangeStatus::Committed));
    assert!(retry.reused);
    assert_eq!(retry.snapshot, report.snapshot);
    f.app.recover().unwrap();
    assert_eq!(f.semantic_rows(), after_resolution);
    assert_eq!(fs::read(&retained_path).unwrap(), original);
    let receipt: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(receipt["proof"]["version"], 5);
    assert!(receipt["embedded_delta"].is_object());
    assert!(
        !f.fs
            .root()
            .path()
            .join(format!(
                "changes/{}/legacy-page-validation-v4.json",
                change.change_id
            ))
            .exists()
    );
}

#[test]
fn normalized_graph_recover_staged_v5_checks_combined_payload_bound_before_bodies() {
    use crate::{
        catalog::source_refresh::{IndexedRefreshSession, RetainedDelta, intended},
        changes::{
            ChangeEvent, JournalFrame,
            indexed_refresh::{IndexedRefreshProof, baseline_path},
            journal, prepare,
        },
    };
    let f = Fixture::new();
    let imported = f.imported();
    f.apply(&imported);
    let resolved = f.resolved(&imported);
    f.apply(&resolved);
    let review = f.reviewed(&imported);
    let original_change = Fixture::prepared(&review);
    let engine = ChangeEngine::new(f.fs.clone()).unwrap();
    let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
    drop(IndexedRefreshSession::adopt_graph(&f.catalog, &writer, &original_change).unwrap());
    drop(writer);
    let before = f.semantic_rows();
    let (mut manifest, _) = engine
        .load_manifest_structure(&original_change.change_id)
        .unwrap();
    let replacement = manifest
        .operations
        .iter_mut()
        .find(|op| op.before_payload.is_some() && op.after_payload.is_some())
        .unwrap();
    for payload in [
        &mut replacement.before_payload,
        &mut replacement.after_payload,
    ]
    .into_iter()
    .flatten()
    {
        payload.byte_len = 33 * 1024 * 1024;
        fs::remove_file(f.fs.root().path().join(payload.path.as_str())).unwrap();
    }
    let change = PreparedChange {
        change_id: original_change.change_id.clone(),
        manifest_hash: Blake3Hash::digest(serde_json::to_vec(&manifest).unwrap()),
    };
    fs::write(
        f.fs.root()
            .path()
            .join(prepare::manifest_path(&change.change_id).unwrap().as_str()),
        prepare::render_prepared_note(&manifest, &change.manifest_hash).unwrap(),
    )
    .unwrap();
    let prefix = journal::encode_frame(&JournalFrame {
        version: 1,
        sequence: 0,
        change_id: change.change_id.clone(),
        manifest_hash: change.manifest_hash.clone(),
        event: ChangeEvent::Prepared,
    })
    .unwrap();
    fs::write(
        f.fs.root()
            .path()
            .join(journal::journal_path(&change.change_id).unwrap().as_str()),
        &prefix,
    )
    .unwrap();
    let path =
        f.fs.root()
            .path()
            .join(baseline_path(&change).unwrap().as_str());
    let mut receipt: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    receipt["embedded_delta"]["change"] = serde_json::to_value(&change).unwrap();
    receipt["embedded_delta"]["graph_admission"]["prepared_journal_hash"] =
        serde_json::to_value(Blake3Hash::digest(&prefix)).unwrap();
    let delta: RetainedDelta = serde_json::from_value(receipt["embedded_delta"].clone()).unwrap();
    let mut proof: IndexedRefreshProof = serde_json::from_value(receipt["proof"].clone()).unwrap();
    proof.change = change.clone();
    proof.delta_hash = Blake3Hash::digest(serde_json::to_vec(&delta).unwrap());
    proof.intended = intended(&proof.base, &change, &proof.delta_hash, 5).unwrap();
    let checksum = Blake3Hash::digest(serde_json::to_vec(&(&proof, Some(&delta))).unwrap());
    fs::write(
        path,
        serde_json::to_vec(&json!({"proof":proof,"embedded_delta":delta,"checksum":checksum}))
            .unwrap(),
    )
    .unwrap();
    engine.load_indexed_refresh_proof(&change).unwrap().unwrap();
    let error = f.app.recover().unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded, "{error:?}");
    assert!(
        f.catalog
            .operation_state()
            .unwrap()
            .unwrap()
            .active()
            .is_none()
    );
    assert_eq!(f.semantic_rows(), before);
}

#[test]
fn normalized_graph_staged_old_base_does_not_block_recovery_or_new_graph_commands() {
    use crate::catalog::source_refresh::IndexedRefreshSession;
    let f = Fixture::new();
    let staged = f.imported();
    let change = Fixture::prepared(&staged);
    let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
    let session = IndexedRefreshSession::adopt_graph(&f.catalog, &writer, &change).unwrap();
    let proof = session.proof().clone();
    drop(session);
    drop(writer);
    let original_targets = target_bytes(&f, &change);
    let other = f.imported_count(4, true);
    f.apply(&other);
    assert_ne!(
        f.catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap()
            .snapshot(),
        &proof.base
    );
    let recovered = f.app.recover().unwrap().report.unwrap();
    assert!(recovered.staged.contains(&change));
    assert_eq!(target_bytes(&f, &change), original_targets);
    // Actual apply/preview readiness stays strict for the obsolete base.
    assert!(IndexedRefreshSession::check_preview(&f.catalog, &proof).is_err());
    assert!(f.app.changes_apply(change.change_id.clone()).is_err());
    // Ordinary graph entry performs recovery before staging its own work.
    let next = f.imported_count(3, true);
    assert_ne!(Fixture::prepared(&next).change_id, change.change_id);
    assert_eq!(target_bytes(&f, &change), original_targets);
}

#[test]
fn normalized_graph_staged_change_does_not_block_unrelated_active_recovery() {
    use crate::catalog::{
        CatalogOptions, PublicationCheckpoint, source_refresh::IndexedRefreshSession,
    };
    let f = Fixture::new();
    let staged = f.imported();
    let change = Fixture::prepared(&staged);
    let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
    drop(IndexedRefreshSession::adopt_graph(&f.catalog, &writer, &change).unwrap());
    drop(writer);
    let original_targets = target_bytes(&f, &change);
    let other = f.imported_count(4, true);
    let other_change = Fixture::prepared(&other);
    let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
    let faulty = Catalog::with_options(
        f.fs.clone(),
        RecordId::new("vault_graph_write").unwrap(),
        CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(StopSql(
                PublicationCheckpoint::AfterPointer,
            ))),
        },
    );
    let engine = ChangeEngine::new(f.fs.clone()).unwrap();
    let mut session = IndexedRefreshSession::adopt_graph(&faulty, &writer, &other_change).unwrap();
    assert!(engine.apply_indexed_refresh(&writer, &mut session).is_err());
    drop(session);
    drop(writer);
    assert_eq!(
        f.catalog
            .operation_state()
            .unwrap()
            .unwrap()
            .active()
            .unwrap()
            .change,
        other_change
    );
    let recovered = f.app.recover().unwrap().report.unwrap();
    assert!(recovered.staged.contains(&change));
    assert!(recovered.changes.iter().any(|report| {
        report.change == other_change && report.status == ChangeStatus::Committed
    }));
    assert!(
        f.catalog
            .operation_state()
            .unwrap()
            .unwrap()
            .active()
            .is_none()
    );
    assert_eq!(target_bytes(&f, &change), original_targets);
    assert_eq!(
        engine.inspect(&change.change_id).unwrap().status,
        ChangeStatus::Prepared
    );
}

#[test]
fn normalized_graph_recover_metadata_classification_does_not_skip_missing_staged_payload() {
    use crate::catalog::source_refresh::IndexedRefreshSession;
    let f = Fixture::new();
    let imported = f.imported();
    let change = Fixture::prepared(&imported);
    let engine = ChangeEngine::new(f.fs.clone()).unwrap();
    let writer = WriterPermit::acquire(f.fs.root(), Duration::ZERO).unwrap();
    drop(IndexedRefreshSession::adopt_graph(&f.catalog, &writer, &change).unwrap());
    drop(writer);
    let before = f.semantic_rows();
    let targets = target_bytes(&f, &change);
    let (manifest, _) = engine.load_manifest_structure(&change.change_id).unwrap();
    let payload = manifest.operations[0].after_payload.as_ref().unwrap();
    fs::remove_file(f.fs.root().path().join(payload.path.as_str())).unwrap();
    let error = f.app.recover().unwrap_err();
    assert_eq!(error.code, ErrorCode::RecordInvalid, "{error:?}");
    assert!(
        error.message.contains("missing retained payload"),
        "{error:?}"
    );
    assert_eq!(target_bytes(&f, &change), targets);
    assert_eq!(f.semantic_rows(), before);
    assert!(
        f.catalog
            .operation_state()
            .unwrap()
            .unwrap()
            .active()
            .is_none()
    );
}

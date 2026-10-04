//! Retained baseline and exact replay-scope tests; SQL session integration is
//! included below uses the real catalog session with disposable source fixtures.
use super::*;
use crate::vault::{VaultFs, VaultRoot};
use std::{fs, time::Duration};

fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn fixture() -> (
    tempfile::TempDir,
    ChangeEngine,
    WriterPermit,
    ChangeInspection,
    IndexedRefreshProof,
) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_refresh_apply\nwiki_kind: vault\ntitle: Indexed apply fixture\n---\n").unwrap();
    fs::write(temp.path().join("page.md"), b"before").unwrap();
    fs::write(temp.path().join("context.md"), b"selected unchanged").unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    let writer = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root)).unwrap();
    let change = engine
        .prepare(
            &writer,
            ChangeDraft {
                title: "refresh fixture".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: BTreeMap::new(),
                read_preconditions: vec![ReadDependency {
                    path: path("context.md"),
                    expected: ExpectedState::Hash(Blake3Hash::digest(b"selected unchanged")),
                }],
                operations: vec![ExpectedWrite {
                    target: path("page.md"),
                    expected: ExpectedState::Hash(Blake3Hash::digest(b"before")),
                    proposed: Some(b"after".to_vec()),
                    apply_after: vec![],
                }],
            },
        )
        .unwrap();
    let file = "0123456789abcdef0123456789abcdef".to_string();
    let parser = Blake3Hash::digest(b"fixture parser");
    let before = vec![
        change.manifest.read_preconditions[0].clone(),
        ReadDependency {
            path: path("page.md"),
            expected: ExpectedState::Hash(Blake3Hash::digest(b"before")),
        },
    ];
    let mut after = before.clone();
    after[1].expected = ExpectedState::Hash(Blake3Hash::digest(b"after"));
    let proof = IndexedRefreshProof {
        version: 2,
        vault_id: engine.vault_id.clone(),
        source_id: id("source_fixture"),
        change: change.prepared.clone(),
        base: ReadSnapshot::published(
            10,
            parser.clone(),
            file.clone(),
            Blake3Hash::digest(b"base"),
        )
        .unwrap(),
        intended: ReadSnapshot::published(11, parser, file, Blake3Hash::digest(b"intended"))
            .unwrap(),
        delta_hash: Blake3Hash::digest(b"retained exact planned delta"),
        before,
        after,
    };
    (temp, engine, writer, change, proof)
}

#[test]
fn baseline_rejects_wrong_publication_and_nonunique_or_incomplete_dependency_scope() {
    let (_temp, engine, _writer, change, proof) = fixture();
    proof.validate_manifest(&change.manifest).unwrap();
    for variant in 0..9 {
        let mut invalid = proof.clone();
        match variant {
            0 => invalid.version = 1,
            1 => invalid.vault_id = id("vault_foreign"),
            2 => invalid.intended.generation = 12,
            3 => invalid.intended.parser_fingerprint = Blake3Hash::digest(b"other parser"),
            4 => invalid.before.push(invalid.before[0].clone()),
            5 => {
                invalid.before.remove(1);
                invalid.after.remove(1);
            }
            6 => {
                invalid.before[0].expected = ExpectedState::Absent;
                invalid.after[0].expected = ExpectedState::Absent;
            }
            7 => invalid.after[0].expected = ExpectedState::Absent,
            8 => {
                invalid.base = ReadSnapshot::canonical(
                    10,
                    Blake3Hash::digest(b"parser"),
                    Blake3Hash::digest(b"manifest"),
                )
            }
            _ => unreachable!(),
        }
        assert!(
            invalid
                .validate(&engine.vault_id, &change.prepared)
                .and_then(|_| invalid.validate_manifest(&change.manifest))
                .is_err(),
            "variant {variant}"
        );
    }
}

#[test]
fn retained_baseline_is_exact_checked_and_cannot_be_recreated_for_replay() {
    let (_temp, engine, writer, change, proof) = fixture();
    assert!(
        engine
            .retain_indexed_refresh_proof(&writer, &proof, false)
            .is_err()
    );
    engine
        .retain_indexed_refresh_proof(&writer, &proof, true)
        .unwrap();
    assert_eq!(
        engine.load_indexed_refresh_proof(&change.prepared).unwrap(),
        Some(proof.clone())
    );
    engine
        .retain_indexed_refresh_proof(&writer, &proof, false)
        .unwrap();
    let name = engine
        .fs
        .root()
        .resolve(&baseline_path(&change.prepared).unwrap())
        .unwrap();
    let original = fs::read(&name).unwrap();
    let mut changed = proof.clone();
    changed.delta_hash = Blake3Hash::digest(b"replanned delta");
    assert!(
        engine
            .retain_indexed_refresh_proof(&writer, &changed, true)
            .is_err()
    );
    assert_eq!(fs::read(&name).unwrap(), original);
    let mut bytes: serde_json::Value = serde_json::from_slice(&original).unwrap();
    bytes["proof"]["source_id"] = serde_json::json!("source_changed");
    fs::write(&name, serde_json::to_vec(&bytes).unwrap()).unwrap();
    assert!(engine.load_indexed_refresh_proof(&change.prepared).is_err());
    fs::remove_file(&name).unwrap();
    assert!(
        engine
            .retain_indexed_refresh_proof(&writer, &proof, false)
            .is_err()
    );
    assert!(!name.exists());
}

#[test]
fn replay_accepts_only_exact_before_or_after_targets_and_unchanged_selected_reads() {
    let (temp, engine, _writer, change, proof) = fixture();
    engine
        .verify_indexed_dependencies(&proof, &change.manifest, false, false)
        .unwrap();
    assert!(
        engine
            .verify_indexed_dependencies(&proof, &change.manifest, true, false)
            .is_err()
    );
    fs::write(temp.path().join("page.md"), b"after").unwrap();
    assert!(
        engine
            .verify_indexed_dependencies(&proof, &change.manifest, false, false)
            .is_err()
    );
    engine
        .verify_indexed_dependencies(&proof, &change.manifest, false, true)
        .unwrap();
    engine
        .verify_indexed_dependencies(&proof, &change.manifest, true, false)
        .unwrap();
    fs::write(
        temp.path().join("unrelated.md"),
        b"external unrelated bytes",
    )
    .unwrap();
    engine
        .verify_indexed_dependencies(&proof, &change.manifest, true, false)
        .unwrap();
    fs::write(temp.path().join("context.md"), b"changed selected read").unwrap();
    assert!(
        engine
            .verify_indexed_dependencies(&proof, &change.manifest, false, true)
            .is_err()
    );
    fs::write(temp.path().join("context.md"), b"selected unchanged").unwrap();
    fs::write(temp.path().join("page.md"), b"foreign third state").unwrap();
    assert!(
        engine
            .verify_indexed_dependencies(&proof, &change.manifest, false, true)
            .is_err()
    );
}

#[test]
fn intended_finalization_seal_is_bound_to_retained_proof_and_exact_active_operation() {
    let (_temp, engine, writer, change, proof) = fixture();
    engine
        .retain_indexed_refresh_proof(&writer, &proof, true)
        .unwrap();
    let idle = operations::activate(
        &engine.fs,
        &writer,
        &engine.vault_id,
        publication(&proof.base).unwrap(),
        Presence::LegacyMayBeAbsent,
    )
    .unwrap();
    // Unit-only construction probes seal validation. Production constructs this
    // private type solely after the session verifies committed SQL identity.
    let seal = VerifiedIndexedRefreshPublication {
        proof: proof.clone(),
    };
    assert!(seal.require_for(&engine, &writer).is_err());
    let active = operations::begin(
        &engine.fs,
        &writer,
        &idle,
        change.prepared.clone(),
        publication(&proof.intended).unwrap(),
    )
    .unwrap();
    seal.require_for(&engine, &writer).unwrap();
    let mut different = proof.clone();
    different.delta_hash = Blake3Hash::digest(b"another plan");
    assert!(
        VerifiedIndexedRefreshPublication { proof: different }
            .require_for(&engine, &writer)
            .is_err()
    );
    operations::cancel(&engine.fs, &writer, &active, &change.prepared).unwrap();
    assert!(seal.require_for(&engine, &writer).is_err());
}

#[test]
fn proof_only_unchanged_boundary_must_be_in_retained_manifest() {
    let (_temp, _engine, _writer, change, mut proof) = fixture();
    let extra = ReadDependency {
        path: path("unretained.md"),
        expected: ExpectedState::Absent,
    };
    proof.before.push(extra.clone());
    proof.after.push(extra);
    assert!(proof.validate_manifest(&change.manifest).is_err());
}

use crate::{
    catalog::{
        Catalog, CatalogOptions, IdentityClaimRow, PublicationCheckpoint, PublicationFault,
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        normalized_delta::{CatalogDelta, DocumentMutation, OwnedClaims},
        query_types::{QueryCatalog, QueryReadLimits},
        row_projection, scan, selector,
    },
    domain::RecordKind,
    records::parse_note,
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceRefreshLimits, SourceStore},
};

struct ConnectedFixture {
    _temp: tempfile::TempDir,
    engine: ChangeEngine,
    writer: WriterPermit,
    catalog: Catalog,
    source: RecordId,
    normalized_facts: bool,
}
impl ConnectedFixture {
    fn request() -> CaptureRequest {
        CaptureRequest {
            title: "Original source title".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "disposable.md".into(),
            original: b"Immutable source content for connected refresh.\n".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: Some("text/markdown".into()),
        }
    }
    fn new() -> Self {
        Self::new_layout(false)
    }
    fn new_layout(normalized_facts: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_connected_refresh\nwiki_kind: vault\ntitle: Connected refresh fixture\n---\n").unwrap();
        let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
        let engine = ChangeEngine::new(fs_handle.clone()).unwrap();
        let plan = SourceStore::new(fs_handle.clone())
            .plan_capture(Self::request())
            .unwrap();
        let source = plan.source_id;
        // Seed canonical fixture bytes before the initial real full projection.
        for operation in plan.draft.unwrap().operations {
            let absolute = temp.path().join(operation.target.as_str());
            fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            fs::write(absolute, operation.proposed.unwrap()).unwrap();
        }
        let identity = BuildIdentity {
            selection: CatalogSelection::new(engine.vault_id.clone(), 1).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&fs_handle, &writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default())
                .unwrap();
        let input = scan::scan_input(&fs_handle, &engine.vault_id).unwrap();
        let completed = if normalized_facts {
            let projection =
                scan::project_normalized_with_sink(&fs_handle, &input, false, &mut builder)
                    .unwrap();
            builder.finish_normalized(&projection).unwrap()
        } else {
            let projection =
                scan::project_with_sink(&fs_handle, &input, false, &mut builder).unwrap();
            builder.finish(&projection).unwrap()
        };
        selector::publish(
            &fs_handle,
            &writer,
            &completed.identity.selection,
            Duration::ZERO,
        )
        .unwrap();
        let catalog = Catalog::new(fs_handle, engine.vault_id.clone());
        Self {
            _temp: temp,
            engine,
            writer,
            catalog,
            source,
            normalized_facts,
        }
    }
    fn title_session<'a>(&'a self, catalog: &Catalog, title: &str) -> IndexedRefreshSession<'a> {
        let reader = self
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let plan = SourceStore::new(self.engine.fs.clone())
            .plan_refresh_indexed(
                &reader,
                &self.source,
                Self::request(),
                Some(title),
                &SourceRefreshLimits::default(),
            )
            .unwrap();
        let mut row = reader.record(&self.source).unwrap().unwrap();
        let draft = plan.plan.draft.unwrap();
        assert_eq!(
            draft.operations.len(),
            1,
            "fixture changes only source title"
        );
        let proposed = draft.operations[0].proposed.as_ref().unwrap();
        let note = parse_note(proposed);
        row.record = note.canonical.clone().unwrap();
        row.hash = note.source_hash.clone();
        for dependency in &mut row.dependencies {
            if dependency.path == row.path {
                dependency.expected = ExpectedState::Hash(row.hash.clone());
            }
        }
        let inspection = self.engine.prepare(&self.writer, draft).unwrap();
        let mut before: BTreeMap<_, _> = plan
            .plan
            .dependencies
            .into_iter()
            .map(|d| (d.path, d.expected))
            .collect();
        for operation in &inspection.manifest.operations {
            before.insert(operation.target.clone(), operation.before.clone());
        }
        let mut after = before.clone();
        for operation in &inspection.manifest.operations {
            after.insert(operation.target.clone(), operation.after.clone());
        }
        let dependencies = |map: BTreeMap<_, _>| {
            map.into_iter()
                .map(|(path, expected)| ReadDependency { path, expected })
                .collect::<Vec<_>>()
        };
        let before = dependencies(before);
        let after = dependencies(after);
        let document = row_projection::canonical_document(&row.path, &note, Some(&row));
        let claim = IdentityClaimRow {
            id: self.source.clone(),
            path: row.path.clone(),
            hash: row.hash.clone(),
            kind: Some(RecordKind::Source),
        };
        let delta = CatalogDelta {
            facts: self.normalized_facts.then(|| {
                crate::catalog::normalized_fact_delta::FactDelta {
                    records: vec![],
                    edge_inserts: vec![],
                    edge_deletes: vec![],
                    links: vec![],
                    registry: vec![],
                }
            }),
            version: if self.normalized_facts { 2 } else { 1 },
            records: vec![row.clone()],
            documents: vec![DocumentMutation::Put { row: document }],
            graph: vec![],
            links: vec![],
            diagnostics: vec![],
            claims: vec![OwnedClaims {
                path: row.path,
                rows: vec![claim],
            }],
            revisions: vec![],
            dependencies: after.clone(),
            owners: vec![],
        };
        IndexedRefreshSession::retain(
            catalog,
            &self.writer,
            self.source.clone(),
            inspection.prepared,
            plan.base_snapshot,
            before,
            after,
            delta,
        )
        .unwrap()
    }
    fn assert_idle(&self, epoch: u64) {
        let authority = required_authority(&self.engine).unwrap();
        assert!(authority.active().is_none());
        assert_eq!(authority.publication().epoch, epoch);
    }
}

#[test]
fn connected_title_apply_keeps_old_reader_and_terminal_retry_survives_later_epoch() {
    let fixture = ConnectedFixture::new();
    let old = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut session = fixture.title_session(&fixture.catalog, "First updated title");
    let first_proof = session.proof().clone();
    let report = fixture
        .engine
        .apply_indexed_refresh(&fixture.writer, &mut session)
        .unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    assert_eq!(report.snapshot, Some(first_proof.intended.clone()));
    fixture.assert_idle(2);
    assert_eq!(
        old.record(&fixture.source).unwrap().unwrap().record.title(),
        "Original source title"
    );
    let current = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        current
            .record(&fixture.source)
            .unwrap()
            .unwrap()
            .record
            .title(),
        "First updated title"
    );
    drop(current);
    drop(session);
    let mut replay =
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, first_proof.clone())
            .unwrap();
    assert_eq!(replay.phase(), IndexedRefreshPhase::AlreadyPublished);
    assert!(replay.starting_ownership_lookup().is_err());
    assert_eq!(
        fixture
            .engine
            .apply_indexed_refresh(&fixture.writer, &mut replay)
            .unwrap(),
        report
    );
    drop(replay);
    let mut second = fixture.title_session(&fixture.catalog, "Second updated title");
    fixture
        .engine
        .apply_indexed_refresh(&fixture.writer, &mut second)
        .unwrap();
    fixture.assert_idle(3);
    drop(second);
    assert!(
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, first_proof.clone())
            .is_err()
    );
    assert_eq!(
        fixture
            .engine
            .indexed_refresh_terminal_report(&fixture.writer, &first_proof.change)
            .unwrap(),
        Some(report)
    );
    fixture.assert_idle(3);
}

struct FailAfterSqlCommit;
impl PublicationFault for FailAfterSqlCommit {
    fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
        if checkpoint == PublicationCheckpoint::AfterCommit {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "injected after SQL commit",
            ));
        }
        Ok(())
    }
}

#[test]
fn connected_committed_delta_resumes_without_base_and_missing_replay_payload_refuses() {
    let fixture = ConnectedFixture::new();
    let faulty = Catalog::with_options(
        fixture.engine.fs.clone(),
        fixture.engine.vault_id.clone(),
        CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(FailAfterSqlCommit)),
        },
    );
    let mut session = fixture.title_session(&faulty, "Committed before interruption");
    let proof = session.proof().clone();
    assert!(
        fixture
            .engine
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .is_err()
    );
    assert_eq!(session.phase(), IndexedRefreshPhase::AlreadyPublished);
    assert!(
        required_authority(&fixture.engine)
            .unwrap()
            .active()
            .is_some()
    );
    assert!(
        fixture
            .engine
            .indexed_refresh_terminal_report(&fixture.writer, &proof.change)
            .unwrap()
            .is_none()
    );
    drop(session);
    let source_path = fixture
        .engine
        .fs
        .root()
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let bytes = fs::read(&source_path).unwrap();
    let modified = fs::metadata(&source_path).unwrap().modified().unwrap();
    let delta_path = fixture.engine.fs.root().path().join(format!(
        "changes/{}/indexed-delta.json",
        proof.change.change_id
    ));
    let delta = fs::read(&delta_path).unwrap();
    fs::remove_file(&delta_path).unwrap();
    assert!(
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof.clone()).is_err()
    );
    assert!(
        required_authority(&fixture.engine)
            .unwrap()
            .active()
            .is_some()
    );
    assert_eq!(fs::read(&source_path).unwrap(), bytes);
    fs::write(&delta_path, delta).unwrap();
    let mut recovered =
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof.clone()).unwrap();
    assert_eq!(recovered.phase(), IndexedRefreshPhase::AlreadyPublished);
    assert!(recovered.starting_ownership_lookup().is_err());
    let report = fixture
        .engine
        .apply_indexed_refresh(&fixture.writer, &mut recovered)
        .unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    assert_eq!(report.snapshot, Some(proof.intended));
    assert_eq!(fs::read(&source_path).unwrap(), bytes);
    assert_eq!(
        fs::metadata(&source_path).unwrap().modified().unwrap(),
        modified
    );
    fixture.assert_idle(2);
}

struct FailBeforeSqlCommit;
impl PublicationFault for FailBeforeSqlCommit {
    fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
        if checkpoint == PublicationCheckpoint::AfterPointer {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "injected before SQL commit",
            ));
        }
        Ok(())
    }
}

#[test]
fn connected_precommit_failure_keeps_base_and_replay_publishes_exactly_once() {
    let fixture = ConnectedFixture::new();
    let faulty = Catalog::with_options(
        fixture.engine.fs.clone(),
        fixture.engine.vault_id.clone(),
        CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(FailBeforeSqlCommit)),
        },
    );
    let mut session = fixture.title_session(&faulty, "Files applied before SQL commit");
    let proof = session.proof().clone();
    assert!(
        fixture
            .engine
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .is_err()
    );
    assert_eq!(session.phase(), IndexedRefreshPhase::AtBase);
    let (manifest, hash) = fixture
        .engine
        .load_manifest_structure(&proof.change.change_id)
        .unwrap();
    assert_eq!(
        journal::load_journal(&fixture.engine.fs, &manifest, &hash)
            .unwrap()
            .status,
        Some(ChangeStatus::FilesApplied)
    );
    fixture
        .engine
        .require_all_after(&fixture.writer, &manifest, &hash)
        .unwrap();
    let source_path = fixture
        .engine
        .fs
        .root()
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let bytes = fs::read(&source_path).unwrap();
    let modified = fs::metadata(&source_path).unwrap().modified().unwrap();
    drop(session);
    let mut recovered =
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof.clone()).unwrap();
    assert_eq!(recovered.phase(), IndexedRefreshPhase::AtBase);
    assert_eq!(
        recovered.starting_ownership_lookup().unwrap().snapshot(),
        &proof.base
    );
    let report = fixture
        .engine
        .apply_indexed_refresh(&fixture.writer, &mut recovered)
        .unwrap();
    assert_eq!(report.snapshot, Some(proof.intended.clone()));
    assert_eq!(fs::read(&source_path).unwrap(), bytes);
    assert_eq!(
        fs::metadata(&source_path).unwrap().modified().unwrap(),
        modified
    );
    fixture.assert_idle(2);
    drop(recovered);
    let mut repeated =
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof).unwrap();
    assert_eq!(
        fixture
            .engine
            .apply_indexed_refresh(&fixture.writer, &mut repeated)
            .unwrap(),
        report
    );
    fixture.assert_idle(2);
}

/// Fail the actual authority replacement after terminal receipt retention. The
/// reservation remains naturally active; no fixture rewrites authority state.
struct FailAcknowledgement;
impl crate::vault::DurableIo for FailAcknowledgement {
    fn create_stage(&self, p: &std::path::Path) -> std::io::Result<fs::File> {
        crate::vault::NativeIo.create_stage(p)
    }
    fn create_private_stage(&self, p: &std::path::Path) -> std::io::Result<fs::File> {
        crate::vault::NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::NativeIo.create_private_directory(p)
    }
    fn open_append(&self, p: &std::path::Path) -> std::io::Result<fs::File> {
        crate::vault::NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &fs::File, n: u64) -> std::io::Result<()> {
        crate::vault::NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut fs::File, b: &[u8]) -> std::io::Result<()> {
        crate::vault::NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &fs::File) -> std::io::Result<()> {
        crate::vault::NativeIo.sync_file(f)
    }
    fn replace(&self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
        if b.file_name().is_some_and(|name| name == "operations.json") {
            let next: serde_json::Value = serde_json::from_slice(&fs::read(a)?).unwrap();
            if next["active"].is_null()
                && next["publication"]["epoch"].as_u64().is_some_and(|n| n > 1)
            {
                return Err(std::io::Error::other(
                    "injected before authority acknowledgement replace",
                ));
            }
        }
        crate::vault::NativeIo.replace(a, b)
    }
    fn remove(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::NativeIo.remove(p)
    }
    fn create_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &std::path::Path) -> std::io::Result<crate::vault::DirectorySync> {
        crate::vault::NativeIo.sync_directory(p)
    }
}

#[test]
fn connected_terminal_receipt_with_prepared_journal_prefix_finishes_active_acknowledgement() {
    let fixture = ConnectedFixture::new();
    let faulty_fs = VaultFs::with_io(
        fixture.engine.fs.root().clone(),
        std::sync::Arc::new(FailAcknowledgement),
    );
    let faulty_engine = ChangeEngine::new(faulty_fs).unwrap();
    let mut session = fixture.title_session(&fixture.catalog, "Terminal receipt survives prefix");
    let proof = session.proof().clone();
    assert!(
        faulty_engine
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .is_err()
    );
    let (manifest, hash) = fixture
        .engine
        .load_manifest_structure(&proof.change.change_id)
        .unwrap();
    let report = outcome::terminal_report(&fixture.engine.fs, &manifest, &hash)
        .unwrap()
        .unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    assert!(
        required_authority(&fixture.engine)
            .unwrap()
            .active()
            .is_some()
    );
    drop(session);
    let state = journal::load_journal(&fixture.engine.fs, &manifest, &hash).unwrap();
    let first = state.frames.first().unwrap();
    let journal_path = fixture
        .engine
        .fs
        .root()
        .resolve(&journal::journal_path(&proof.change.change_id).unwrap())
        .unwrap();
    fs::write(&journal_path, journal::encode_frame(first).unwrap()).unwrap();
    assert_eq!(
        journal::load_journal(&fixture.engine.fs, &manifest, &hash)
            .unwrap()
            .status,
        Some(ChangeStatus::Prepared)
    );
    assert_eq!(
        outcome::terminal_report(&fixture.engine.fs, &manifest, &hash).unwrap(),
        Some(report.clone())
    );
    let mut resumed =
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof).unwrap();
    assert_eq!(resumed.phase(), IndexedRefreshPhase::AlreadyPublished);
    assert_eq!(
        fixture
            .engine
            .apply_indexed_refresh(&fixture.writer, &mut resumed)
            .unwrap(),
        report
    );
    fixture.assert_idle(2);
}

#[test]
fn connected_new_revision_commits_and_recovers_exact_immutable_owner() {
    use crate::catalog::normalized_delta::{OwnedDiagnostics, OwnedLinks, RevisionIdentityRow};
    let fixture = ConnectedFixture::new();
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut request = ConnectedFixture::request();
    request.original = b"New immutable captured revision with changed content.\n".to_vec();
    // This fixture deliberately tests raw v1 delta/ownership recovery on the
    // old proof layout. Production indexed allocation requires current facts;
    // the sealed v2 projector recovery has its own connected tests.
    let base_snapshot = QueryCatalog::snapshot(&reader).clone();
    let plan = SourceStore::new(fixture.engine.fs.clone())
        .plan_refresh_with_title(&fixture.source, request, None)
        .unwrap();
    assert!(!plan.reused);
    let revision_id = plan.revision_id.clone();
    let mut draft = plan.draft.unwrap();
    // This test oracle projects the COMPLETE small fixture plus its proposed
    // overlay. Production admission may not use a synthetic partial graph.
    let mut input = scan::scan_input(&fixture.engine.fs, &fixture.engine.vault_id).unwrap();
    input.overlay = draft
        .operations
        .iter()
        .map(|operation| ProposedTarget {
            path: operation.target.clone(),
            bytes: operation.proposed.clone(),
        })
        .collect();
    let projected = scan::project(&fixture.engine.fs, &input).unwrap();
    let mut before: BTreeMap<_, _> = plan
        .dependencies
        .into_iter()
        .map(|d| (d.path, d.expected))
        .collect();
    for dependency in &projected.dependencies {
        before.insert(
            dependency.path.clone(),
            fixture.engine.target_state(&dependency.path).unwrap(),
        );
    }
    for operation in &draft.operations {
        before.insert(operation.target.clone(), operation.expected.clone());
    }
    draft.read_preconditions = before
        .iter()
        .map(|(path, expected)| ReadDependency {
            path: path.clone(),
            expected: expected.clone(),
        })
        .collect();
    let inspection = fixture.engine.prepare(&fixture.writer, draft).unwrap();
    let mut after = before.clone();
    for operation in &inspection.manifest.operations {
        after.insert(operation.target.clone(), operation.after.clone());
    }
    let deps = |map: BTreeMap<_, _>| {
        map.into_iter()
            .map(|(path, expected)| ReadDependency { path, expected })
            .collect::<Vec<_>>()
    };
    let before = deps(before);
    let after = deps(after);
    let revision = &projected.records[&revision_id].record;
    let hash = |field| Blake3Hash::new(revision.string(field).unwrap()).unwrap();
    let identity = RevisionIdentityRow {
        source_id: fixture.source.clone(),
        revision_id: revision_id.clone(),
        retained_ordinal: 1,
        original_hash: hash("wiki_original_hash"),
        content_hash: revision
            .string("wiki_content_hash")
            .map(|value| Blake3Hash::new(value).unwrap()),
        extractor_fingerprint: hash("wiki_extractor_fingerprint"),
        extraction_status: revision.string("wiki_extraction_status").unwrap().into(),
    };
    let owners = fixture
        .engine
        .manifest_revision_owners(&inspection.prepared)
        .unwrap();
    assert_eq!(owners.len(), 1);
    let records: Vec<_> = projected.records.values().cloned().collect();
    let claims = records
        .iter()
        .map(|row| OwnedClaims {
            path: row.path.clone(),
            rows: vec![IdentityClaimRow {
                id: row.record.id().clone(),
                path: row.path.clone(),
                hash: row.hash.clone(),
                kind: Some(row.record.kind()),
            }],
        })
        .collect();
    let links = records
        .iter()
        .map(|row| OwnedLinks {
            path: row.path.clone(),
            rows: projected
                .links
                .iter()
                .filter(|link| link.from_path == row.path)
                .cloned()
                .collect(),
        })
        .collect();
    let diagnostics = records
        .iter()
        .map(|row| OwnedDiagnostics {
            path: row.path.clone(),
            rows: projected
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.path == row.path)
                .cloned()
                .collect(),
        })
        .collect();
    let delta = CatalogDelta {
        facts: None,
        version: 1,
        records,
        documents: projected
            .documents
            .into_iter()
            .map(|row| DocumentMutation::Put { row })
            .collect(),
        graph: projected.graph,
        links,
        diagnostics,
        claims,
        revisions: vec![identity],
        dependencies: after.clone(),
        owners: owners.clone(),
    };
    let faulty = Catalog::with_options(
        fixture.engine.fs.clone(),
        fixture.engine.vault_id.clone(),
        CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(FailAfterSqlCommit)),
        },
    );
    let mut session = IndexedRefreshSession::retain(
        &faulty,
        &fixture.writer,
        fixture.source.clone(),
        inspection.prepared.clone(),
        base_snapshot,
        before,
        after,
        delta,
    )
    .unwrap();
    let proof = session.proof().clone();
    assert!(
        fixture
            .engine
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .is_err()
    );
    assert_eq!(session.phase(), IndexedRefreshPhase::AlreadyPublished);
    let tree_receipt = fixture.engine.fs.root().path().join(format!(
        "changes/{}/revision-trees.json",
        inspection.prepared.change_id
    ));
    assert!(tree_receipt.is_file());
    let tree = fixture.engine.fs.root().path().join(format!(
        "sources/{}/revisions/{revision_id}",
        fixture.source
    ));
    let original = fs::read(tree.join("original.bin")).unwrap();
    let modified = fs::metadata(tree.join("original.bin"))
        .unwrap()
        .modified()
        .unwrap();
    drop(session);
    let mut recovered =
        IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof.clone()).unwrap();
    assert_eq!(recovered.phase(), IndexedRefreshPhase::AlreadyPublished);
    let report = fixture
        .engine
        .apply_indexed_refresh(&fixture.writer, &mut recovered)
        .unwrap();
    assert_eq!(report.snapshot, Some(proof.intended));
    fixture.assert_idle(2);
    let current = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        crate::changes::RevisionOwnershipLookup::revision_owner(&current, &owners[0].key).unwrap(),
        Some(inspection.prepared)
    );
    assert_eq!(
        current
            .record(&fixture.source)
            .unwrap()
            .unwrap()
            .record
            .string("wiki_current_revision"),
        Some(revision_id.as_str())
    );
    assert_eq!(fs::read(tree.join("original.bin")).unwrap(), original);
    assert_eq!(
        fs::metadata(tree.join("original.bin"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
}

#[test]
fn normalized_fact_layout_survives_retained_precommit_and_postcommit_recovery() {
    for committed in [false, true] {
        let fixture = ConnectedFixture::new_layout(true);
        let old = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let old_row = old.record(&fixture.source).unwrap().unwrap();
        let old_fact = old.eligibility_fact(&fixture.source).unwrap().unwrap();
        let faulty = Catalog::with_options(
            fixture.engine.fs.clone(),
            fixture.engine.vault_id.clone(),
            CatalogOptions {
                busy_timeout_ms: 1000,
                fault: Some(if committed {
                    std::sync::Arc::new(FailAfterSqlCommit)
                } else {
                    std::sync::Arc::new(FailBeforeSqlCommit)
                }),
            },
        );
        // Same byte length preserves source-owned link offsets in this narrow
        // retained-plan recovery fixture. Actual projector handles moved offsets.
        let title = "Updated source title!";
        assert_eq!(title.len(), old_row.record.title().len());
        let mut session = fixture.title_session(&faulty, title);
        let proof = session.proof().clone();
        assert!(
            fixture
                .engine
                .apply_indexed_refresh(&fixture.writer, &mut session)
                .is_err()
        );
        assert_eq!(
            session.phase(),
            if committed {
                IndexedRefreshPhase::AlreadyPublished
            } else {
                IndexedRefreshPhase::AtBase
            }
        );
        drop(session);
        let mut recovered =
            IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof.clone())
                .unwrap();
        let report = fixture
            .engine
            .apply_indexed_refresh(&fixture.writer, &mut recovered)
            .unwrap();
        assert_eq!(report.status, ChangeStatus::Committed);
        fixture.assert_idle(2);
        let current = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let row = current.record(&fixture.source).unwrap().unwrap();
        assert_eq!(row.record.title(), title);
        assert_eq!(
            current.eligibility_fact(&fixture.source).unwrap().unwrap(),
            old_fact
        );
        assert_eq!(
            current
                .direct_path_states(std::slice::from_ref(&row.path))
                .unwrap()[0]
                .expected,
            ExpectedState::Hash(row.hash)
        );
        assert_eq!(
            old.direct_path_states(std::slice::from_ref(&old_row.path))
                .unwrap()[0]
                .expected,
            ExpectedState::Hash(old_row.hash)
        );
    }
}

#[test]
fn normalized_replay_refuses_missing_fact_index_before_canonical_writes() {
    let fixture = ConnectedFixture::new_layout(true);
    let source_path = fixture
        .engine
        .fs
        .root()
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let before = fs::read(&source_path).unwrap();
    let session = fixture.title_session(&fixture.catalog, "Updated source title!");
    let proof = session.proof().clone();
    drop(session);
    let binding = proof.base.publication().unwrap();
    let db = rusqlite::Connection::open(
        fixture
            .engine
            .fs
            .root()
            .path()
            .join(format!(".wiki/cache/catalogs/{}.sqlite", binding.file_id)),
    )
    .unwrap();
    db.execute_batch("DROP INDEX registry_match_owners")
        .unwrap();
    assert!(IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof).is_err());
    assert_eq!(fs::read(source_path).unwrap(), before);
}

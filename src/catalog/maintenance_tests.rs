use super::*;
use crate::{
    catalog::{
        normalized_build::{BuildCheckpoint, BuildFault},
        query_types::{QueryCatalog, QueryReadLimits},
    },
    domain::{RecordId, VaultRelativePath},
    vault::{VaultFs, VaultRoot},
};
use std::{fs, sync::Arc};

fn fixture() -> (tempfile::TempDir, Catalog) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_maintenance_rebuild\nwiki_kind: vault\ntitle: Maintenance\n---\n").unwrap();
    fs::write(temp.path().join("page.md"), "# First\n\noriginal content\n").unwrap();
    let catalog = Catalog::new(
        VaultFs::new(VaultRoot::explicit(temp.path()).unwrap()),
        RecordId::new("vault_maintenance_rebuild").unwrap(),
    );
    (temp, catalog)
}

#[test]
fn maintenance_rebuild_publishes_real_rows_and_preserves_held_old_reader() {
    let (temp, catalog) = fixture();
    let writer = WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
    let first = catalog.rebuild_normalized(&writer).unwrap();
    assert_eq!(first.report.snapshot.generation, 1);
    assert!(first.input.io_bytes > 0);
    assert!(first.input.layout_io_bytes > 0);
    assert!(first.build.as_ref().unwrap().documents >= 2);
    let old = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let path = VaultRelativePath::new("page.md").unwrap();
    assert!(
        old.document(&path)
            .unwrap()
            .unwrap()
            .body
            .contains("original")
    );
    fs::write(temp.path().join("page.md"), "# Second\n\nchanged content\n").unwrap();
    let second = catalog.rebuild_normalized(&writer).unwrap();
    assert_eq!(second.report.snapshot.generation, 2);
    assert_ne!(first.report.snapshot, second.report.snapshot);
    let current = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert!(
        current
            .document(&path)
            .unwrap()
            .unwrap()
            .body
            .contains("changed")
    );
    assert!(
        old.document(&path)
            .unwrap()
            .unwrap()
            .body
            .contains("original")
    );
    assert_eq!(old.snapshot(), &first.report.snapshot);
}

struct EditDuringBuild(std::path::PathBuf);

#[test]
fn maintenance_sync_reuses_current_commitments_and_discovers_added_removed_invalid_notes() {
    let (temp, catalog) = fixture();
    let writer = WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
    let first = catalog.rebuild_normalized(&writer).unwrap();
    let same = catalog.sync_normalized(&writer).unwrap();
    assert!(same.report.reused);
    assert!(same.build.is_none());
    assert_eq!(same.report.snapshot, first.report.snapshot);
    fs::write(
        temp.path().join("invalid.md"),
        b"---\nwiki_id: [broken\n---\nnew note\n",
    )
    .unwrap();
    let added = catalog.sync_normalized(&writer).unwrap();
    assert!(!added.report.reused);
    assert_eq!(added.report.snapshot.generation, 2);
    let same = catalog.sync_normalized(&writer).unwrap();
    assert!(same.report.reused);
    assert_eq!(same.report.snapshot, added.report.snapshot);
    fs::remove_file(temp.path().join("invalid.md")).unwrap();
    let removed = catalog.sync_normalized(&writer).unwrap();
    assert!(!removed.report.reused);
    assert_eq!(removed.report.snapshot.generation, 3);
    assert!(catalog.sync_normalized(&writer).unwrap().report.reused);
}

#[test]
fn maintenance_explicit_rebuild_repairs_missing_owned_database_from_authority_floor() {
    let (temp, catalog) = fixture();
    let writer = WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
    let first = catalog.rebuild_normalized(&writer).unwrap();
    let (selected, _) =
        selector::maintenance_header(&catalog.fs, &catalog.vault_id, Duration::from_secs(1))
            .unwrap()
            .unwrap();
    fs::remove_file(
        temp.path()
            .join(format!(".wiki/cache/catalogs/{}.sqlite", selected.file_id)),
    )
    .unwrap();
    assert!(catalog.sync_normalized(&writer).is_err());
    let repaired = catalog.rebuild_normalized(&writer).unwrap();
    assert_eq!(
        repaired.report.snapshot.generation,
        first.report.snapshot.generation + 1
    );
    assert!(repaired.report.vector_loss_unknown);
    assert!(repaired.retirement_deferred);
    assert!(!repaired.cleanup_errors.is_empty());
    assert!(
        temp.path()
            .join(format!(
                ".wiki/cache/catalogs/{}.sqlite-wal",
                selected.file_id
            ))
            .exists()
    );
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert!(
        reader
            .document(&VaultRelativePath::new("page.md").unwrap())
            .unwrap()
            .unwrap()
            .body
            .contains("original")
    );
}

#[test]
fn maintenance_sync_rebuilds_incompatible_layout_without_input_changes() {
    for mutation in [
        "UPDATE catalog_meta SET proof_layout_version=1",
        "UPDATE catalog_meta SET revision_ownership_version=0",
        "DROP INDEX document_candidate_metadata",
        "DROP INDEX document_titles",
    ] {
        let (temp, catalog) = fixture();
        let writer = WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
        catalog.rebuild_normalized(&writer).unwrap();
        let (selected, _) =
            selector::maintenance_header(&catalog.fs, &catalog.vault_id, Duration::ZERO)
                .unwrap()
                .unwrap();
        let database = rusqlite::Connection::open(
            temp.path()
                .join(format!(".wiki/cache/catalogs/{}.sqlite", selected.file_id)),
        )
        .unwrap();
        selector::configure_wal(&database).unwrap();
        database.execute_batch(mutation).unwrap();
        drop(database);
        let synced = catalog.sync_normalized(&writer).unwrap();
        assert!(!synced.report.reused, "{mutation}");
        assert_eq!(synced.report.snapshot.generation, 2);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        reader.require_fact_layout().unwrap();
        crate::changes::RevisionOwnershipLookup::require_ready(&reader).unwrap();
        let hits = crate::retrieval::lexical::search_catalog(
            &reader,
            "original",
            &crate::retrieval::QueryPlan::default(),
        )
        .unwrap();
        assert_eq!(hits.hits.len(), 1);
    }
}

#[test]
fn maintenance_interrupted_rebuild_reuses_exact_fresh_candidate_or_rebuilds_external_edit() {
    for external_edit in [false, true] {
        let (temp, catalog) = fixture();
        let writer = WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
        catalog.rebuild_normalized(&writer).unwrap();
        let input =
            MaintenanceInput::capture(&catalog.fs, &catalog.vault_id, MaintenanceLimits::rebuild())
                .unwrap();
        let identity = BuildIdentity {
            selection: CatalogSelection::new(catalog.vault_id.clone(), 2).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&catalog.fs, &writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&catalog.fs, &writer, identity, BuildLimits::default())
                .unwrap();
        let projection = scan::project_maintenance_with_sink(&input, &mut builder).unwrap();
        let completed = builder.finish_normalized(&projection).unwrap();
        input.final_recheck().unwrap();
        assert!(
            selector::publish_with_faults(
                &catalog.fs,
                &writer,
                &completed.identity.selection,
                Duration::ZERO,
                &mut |point| {
                    if point == selector::PublishPoint::BeforeMarker {
                        Err(WikiError::new(
                            ErrorCode::Internal,
                            "interrupted acknowledged switch",
                        ))
                    } else {
                        Ok(())
                    }
                }
            )
            .is_err()
        );
        if external_edit {
            fs::write(
                temp.path().join("page.md"),
                "# Changed while stopped\n\nnew external text\n",
            )
            .unwrap();
        }
        let result = catalog.rebuild_normalized(&writer).unwrap();
        assert!(result.resumed);
        if external_edit {
            assert!(!result.report.reused);
            assert!(result.build.is_some());
            assert_eq!(result.report.snapshot.generation, 3);
        } else {
            assert!(result.report.reused);
            assert!(result.build.is_none());
            assert_eq!(result.report.snapshot, completed.snapshot);
        }
    }
}

impl BuildFault for EditDuringBuild {
    fn check(&self, point: BuildCheckpoint) -> Result<()> {
        if point == BuildCheckpoint::BeforeComplete {
            fs::write(&self.0, "# External edit\n\nintervening bytes\n").unwrap();
        }
        Ok(())
    }
}

#[test]
fn maintenance_rebuild_refuses_intervening_canonical_edit_without_switching() {
    let (temp, catalog) = fixture();
    let writer = WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
    let first = catalog.rebuild_normalized(&writer).unwrap();
    let limits = BuildLimits {
        fault: Some(Arc::new(EditDuringBuild(temp.path().join("page.md")))),
        ..BuildLimits::default()
    };
    let error = catalog
        .rebuild_normalized_with_limits(&writer, MaintenanceLimits::rebuild(), limits)
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(reader.snapshot(), &first.report.snapshot);
    assert!(
        reader
            .document(&VaultRelativePath::new("page.md").unwrap())
            .unwrap()
            .unwrap()
            .body
            .contains("original")
    );
}

#[cfg(unix)]
#[test]
fn maintenance_whole_cache_loss_uses_new_inodes_and_preserves_old_transaction() {
    let (temp, catalog) = fixture();
    let writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap();
    let first = catalog.rebuild_normalized(&writer).unwrap();
    let old = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let page = VaultRelativePath::new("page.md").unwrap();
    let old_document = old.document(&page).unwrap().unwrap();
    let mut selected = crate::retrieval::selected_documents::authenticate(
        &catalog,
        &old,
        std::slice::from_ref(&page),
        &crate::retrieval::VerificationBudget::default(),
    )
    .unwrap();
    fn cache_files(
        root: &std::path::Path,
    ) -> std::collections::BTreeMap<std::path::PathBuf, (Vec<u8>, std::time::SystemTime)> {
        fn visit(
            root: &std::path::Path,
            dir: &std::path::Path,
            result: &mut std::collections::BTreeMap<
                std::path::PathBuf,
                (Vec<u8>, std::time::SystemTime),
            >,
        ) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    visit(root, &entry.path(), result);
                } else {
                    result.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        (
                            fs::read(entry.path()).unwrap(),
                            entry.metadata().unwrap().modified().unwrap(),
                        ),
                    );
                }
            }
        }
        let mut result = std::collections::BTreeMap::new();
        visit(root, root, &mut result);
        result
    }
    let backup = temp.path().join("preserved-cache");
    fs::rename(temp.path().join(".wiki/cache"), &backup).unwrap();
    // The moved cache is outside canonical scan input in this test.
    let preserved = fs::read(backup.join(format!(
        "catalogs/{}.sqlite",
        first.report.snapshot.publication().unwrap().file_id
    )))
    .unwrap();
    let all_preserved = cache_files(&backup);
    let second = catalog.rebuild_normalized(&writer).unwrap();
    selected.recheck(&catalog, &old).unwrap();
    assert_eq!(cache_files(&backup), all_preserved);
    assert_eq!(
        second.report.snapshot.generation,
        first.report.snapshot.generation + 1
    );
    assert_ne!(
        first.report.snapshot.publication().unwrap().file_id,
        second.report.snapshot.publication().unwrap().file_id
    );
    assert_eq!(old.document(&page).unwrap().unwrap(), old_document);
    old.verify_operations(&catalog).unwrap();
    assert_eq!(
        fs::read(backup.join(format!(
            "catalogs/{}.sqlite",
            first.report.snapshot.publication().unwrap().file_id
        )))
        .unwrap(),
        preserved
    );
    let new = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        new.document(&page).unwrap().unwrap().body,
        old_document.body
    );
    assert_eq!(new.snapshot(), &second.report.snapshot);
    assert!(
        !temp
            .path()
            .join(".wiki/state/catalog-rebuild.json")
            .exists()
    );
    fs::write(
        temp.path().join("page.md"),
        "changed selected canonical bytes",
    )
    .unwrap();
    assert_eq!(
        selected.recheck(&catalog, &old).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
}
struct FailReconstructionAt(BuildCheckpoint);
impl BuildFault for FailReconstructionAt {
    fn check(&self, point: BuildCheckpoint) -> Result<()> {
        if point == self.0 {
            Err(WikiError::new(
                ErrorCode::Internal,
                "injected reconstruction build cut",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn maintenance_whole_cache_loss_retries_real_header_build_and_seal_cuts() {
    for point in [
        BuildCheckpoint::AfterHeader,
        BuildCheckpoint::AfterOrdinaryRow,
        BuildCheckpoint::BeforeComplete,
        BuildCheckpoint::AfterComplete,
        BuildCheckpoint::BeforeSync,
        BuildCheckpoint::AfterSync,
    ] {
        let (temp, catalog) = fixture();
        let writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap();
        let first = catalog.rebuild_normalized(&writer).unwrap();
        fs::rename(
            temp.path().join(".wiki/cache"),
            temp.path().join("preserved-cache"),
        )
        .unwrap();
        let error = catalog
            .rebuild_normalized_with_limits(
                &writer,
                MaintenanceLimits::rebuild(),
                BuildLimits {
                    fault: Some(Arc::new(FailReconstructionAt(point))),
                    ..BuildLimits::default()
                },
            )
            .err()
            .unwrap();
        assert!(
            error.message.contains("injected reconstruction"),
            "{point:?}: {error:?}"
        );
        let record = selector::read_rebuild_record(&catalog.fs).unwrap().unwrap();
        let old_candidate: serde_json::Value = serde_json::from_slice(&record).unwrap();
        let retried = catalog.rebuild_normalized(&writer).unwrap();
        assert_eq!(
            retried.report.snapshot.generation,
            first.report.snapshot.generation + 1
        );
        assert_ne!(
            retried.report.snapshot.publication().unwrap().file_id,
            old_candidate["candidate"]["file_id"].as_str().unwrap()
        );
        assert!(retried.abandoned_rebuild_candidates.is_empty());
        assert!(
            !temp
                .path()
                .join(".wiki/state/catalog-rebuild.json")
                .exists()
        );
        catalog.check_normalized(&writer).unwrap();
    }
}

struct ReconstructionIoCut {
    point: u8,
    fired: std::sync::atomic::AtomicBool,
}
impl ReconstructionIoCut {
    fn fail(&self) -> std::io::Result<()> {
        self.fired.store(true, std::sync::atomic::Ordering::SeqCst);
        Err(std::io::Error::other("injected native reconstruction cut"))
    }
}
impl crate::vault::DurableIo for ReconstructionIoCut {
    fn create_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        let file = crate::vault::NativeIo.create_stage(p)?;
        if self.point == 1 && p.extension().is_some_and(|extension| extension == "sqlite") {
            crate::vault::NativeIo.sync_file(&file)?;
            crate::vault::NativeIo.sync_directory(p.parent().unwrap())?;
            self.fail()?;
        }
        Ok(file)
    }
    fn create_private_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        crate::vault::DurableIo::create_private_stage(&crate::vault::NativeIo, p)
    }
    fn create_private_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::DurableIo::create_private_directory(&crate::vault::NativeIo, p)
    }
    fn open_append(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        crate::vault::DurableIo::open_append(&crate::vault::NativeIo, p)
    }
    fn truncate_file(&self, f: &std::fs::File, n: u64) -> std::io::Result<()> {
        crate::vault::DurableIo::truncate_file(&crate::vault::NativeIo, f, n)
    }
    fn write_stage(&self, f: &mut std::fs::File, b: &[u8]) -> std::io::Result<()> {
        crate::vault::DurableIo::write_stage(&crate::vault::NativeIo, f, b)
    }
    fn sync_file(&self, f: &std::fs::File) -> std::io::Result<()> {
        crate::vault::DurableIo::sync_file(&crate::vault::NativeIo, f)
    }
    fn replace(&self, s: &std::path::Path, t: &std::path::Path) -> std::io::Result<()> {
        crate::vault::DurableIo::replace(&crate::vault::NativeIo, s, t)?;
        if (self.point == 2 && t.ends_with("catalog-rebuild.json"))
            || (self.point == 4 && t.ends_with("operations.json"))
            || (self.point == 5 && t.ends_with("catalog-current.json"))
        {
            self.fail()?;
        }
        Ok(())
    }
    fn remove(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::DurableIo::remove(&crate::vault::NativeIo, p)?;
        if (self.point == 3 && p.extension().is_some_and(|extension| extension == "sqlite"))
            || (self.point == 6 && p.ends_with("catalog-rebuild.json"))
        {
            self.fail()?;
        }
        Ok(())
    }
    fn create_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::DurableIo::create_directory(&crate::vault::NativeIo, p)
    }
    fn sync_directory(&self, p: &std::path::Path) -> std::io::Result<crate::vault::DirectorySync> {
        crate::vault::DurableIo::sync_directory(&crate::vault::NativeIo, p)
    }
}
#[test]
fn maintenance_whole_cache_loss_retries_native_creation_rotation_retirement_and_ack_cuts() {
    for point in 1..=6 {
        let (temp, catalog) = fixture();
        let writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap();
        let first = catalog.rebuild_normalized(&writer).unwrap();
        fs::rename(
            temp.path().join(".wiki/cache"),
            temp.path().join("preserved-cache"),
        )
        .unwrap();
        let mut reserved_before = None;
        if matches!(point, 2 | 3) {
            let admission = super::super::missing_cache::prepare(
                &catalog.fs,
                &writer,
                &catalog.vault_id,
                true,
                Duration::ZERO,
            )
            .unwrap();
            let candidate = admission.candidate.unwrap();
            selector::prepare(&catalog.fs, &writer, &candidate).unwrap();
            if point == 2 {
                fs::write(
                    temp.path()
                        .join(format!(".wiki/cache/catalogs/{}.sqlite", candidate.file_id)),
                    b"",
                )
                .unwrap();
            } else {
                let builder = NormalizedBuilder::begin(
                    &catalog.fs,
                    &writer,
                    BuildIdentity {
                        selection: candidate.clone(),
                        origin: None,
                        vector_cache_lost: false,
                        vector_loss_unknown: true,
                    },
                    BuildLimits::default(),
                )
                .unwrap();
                drop(builder);
            }
            reserved_before = Some(candidate.file_id);
        }
        let fault = Arc::new(ReconstructionIoCut {
            point,
            fired: std::sync::atomic::AtomicBool::new(false),
        });
        let faulty = Catalog::new(
            VaultFs::with_io(catalog.fs.root().clone(), fault.clone()),
            catalog.vault_id.clone(),
        );
        let error = faulty
            .rebuild_normalized(&writer)
            .err()
            .expect("native cut must fail");
        assert!(
            fault.fired.load(std::sync::atomic::Ordering::SeqCst),
            "cut {point} not reached: {error:?}"
        );
        let witness = selector::read_rebuild_record(&catalog.fs)
            .unwrap()
            .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).unwrap());
        let retried = catalog.rebuild_normalized(&writer).unwrap();
        let new_id = &retried.report.snapshot.publication().unwrap().file_id;
        assert_ne!(
            new_id,
            &first.report.snapshot.publication().unwrap().file_id
        );
        if matches!(point, 4 | 5) {
            assert!(retried.resumed);
            assert!(retried.report.reused);
            assert_eq!(
                new_id,
                witness.as_ref().unwrap()["candidate"]["file_id"]
                    .as_str()
                    .unwrap()
            );
            assert_eq!(
                retried.report.snapshot.generation,
                first.report.snapshot.generation + 1
            );
        } else if let Some(witness) = witness {
            assert_ne!(new_id, witness["candidate"]["file_id"].as_str().unwrap());
        }
        if matches!(point, 1 | 2) {
            assert_eq!(retried.abandoned_rebuild_candidates.len(), 1);
            let abandoned = &retried.abandoned_rebuild_candidates[0];
            assert_eq!(abandoned.bytes, 0);
            if let Some(id) = &reserved_before {
                assert_eq!(&abandoned.candidate.file_id, id);
            }
            let file = temp.path().join(format!(
                ".wiki/cache/catalogs/{}.sqlite",
                abandoned.candidate.file_id
            ));
            assert_eq!(fs::read(file).unwrap(), b"");
        } else {
            assert!(retried.abandoned_rebuild_candidates.is_empty());
        }
        assert!(
            !temp
                .path()
                .join(".wiki/state/catalog-rebuild.json")
                .exists()
        );
        catalog.check_normalized(&writer).unwrap();
    }
}

#[test]
fn maintenance_witness_publication_refuses_restored_predecessor_controls() {
    let (temp, catalog) = fixture();
    let writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap();
    catalog.rebuild_normalized(&writer).unwrap();
    let original_authority = catalog.operation_state().unwrap().unwrap();
    let backup = temp.path().join("preserved-cache");
    fs::rename(temp.path().join(".wiki/cache"), &backup).unwrap();
    let admitted = super::super::missing_cache::prepare(
        &catalog.fs,
        &writer,
        &catalog.vault_id,
        true,
        Duration::ZERO,
    )
    .unwrap();
    let candidate = admitted.candidate.unwrap();
    selector::prepare(&catalog.fs, &writer, &candidate).unwrap();
    let input =
        MaintenanceInput::capture(&catalog.fs, &catalog.vault_id, MaintenanceLimits::rebuild())
            .unwrap();
    let mut builder = NormalizedBuilder::begin(
        &catalog.fs,
        &writer,
        BuildIdentity {
            selection: candidate.clone(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: true,
        },
        BuildLimits::default(),
    )
    .unwrap();
    let projection = scan::project_maintenance_with_sink(&input, &mut builder).unwrap();
    builder.finish_normalized(&projection).unwrap();
    for name in ["catalog-current.json", "catalog-v2-active.json"] {
        fs::copy(
            backup.join(name),
            temp.path().join(".wiki/cache").join(name),
        )
        .unwrap();
    }
    let witness = selector::read_rebuild_record(&catalog.fs).unwrap().unwrap();
    let error = selector::publish(&catalog.fs, &writer, &candidate, Duration::ZERO).unwrap_err();
    assert_eq!(error.code, ErrorCode::IndexCorrupt);
    assert_eq!(
        selector::read_rebuild_record(&catalog.fs).unwrap().unwrap(),
        witness
    );
    assert!(
        catalog
            .operation_state()
            .unwrap()
            .unwrap()
            .same_revision(&original_authority)
    );
    for name in ["catalog-current.json", "catalog-v2-active.json"] {
        assert_eq!(
            fs::read(backup.join(name)).unwrap(),
            fs::read(temp.path().join(".wiki/cache").join(name)).unwrap()
        );
    }
}

#[test]
fn maintenance_witness_publication_requires_outside_authority() {
    let (temp, catalog) = fixture();
    let writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap();
    catalog.rebuild_normalized(&writer).unwrap();
    let original_authority = catalog.operation_state().unwrap().unwrap();
    let backup = temp.path().join("preserved-cache");
    fs::rename(temp.path().join(".wiki/cache"), &backup).unwrap();
    let admitted = super::super::missing_cache::prepare(
        &catalog.fs,
        &writer,
        &catalog.vault_id,
        true,
        Duration::ZERO,
    )
    .unwrap();
    let candidate = admitted.candidate.unwrap();
    selector::prepare(&catalog.fs, &writer, &candidate).unwrap();
    let input =
        MaintenanceInput::capture(&catalog.fs, &catalog.vault_id, MaintenanceLimits::rebuild())
            .unwrap();
    let mut builder = NormalizedBuilder::begin(
        &catalog.fs,
        &writer,
        BuildIdentity {
            selection: candidate.clone(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: true,
        },
        BuildLimits::default(),
    )
    .unwrap();
    let projection = scan::project_maintenance_with_sink(&input, &mut builder).unwrap();
    builder.finish_normalized(&projection).unwrap();
    fs::remove_file(temp.path().join(".wiki/state/operations.json")).unwrap();
    let witness = selector::read_rebuild_record(&catalog.fs).unwrap().unwrap();
    let error = selector::publish(&catalog.fs, &writer, &candidate, Duration::ZERO).unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    assert_eq!(
        selector::read_rebuild_record(&catalog.fs).unwrap().unwrap(),
        witness
    );
    assert!(!temp.path().join(".wiki/state/operations.json").exists());
    assert!(
        !temp
            .path()
            .join(".wiki/cache/catalog-current.json")
            .exists()
    );
    assert!(
        !temp
            .path()
            .join(".wiki/cache/catalog-v2-active.json")
            .exists()
    );
    assert_eq!(
        catalog.operation_state().unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    // Outside authority was revisioned before loss; it must never be regenerated.
    assert!(original_authority.revision() > 0);
}

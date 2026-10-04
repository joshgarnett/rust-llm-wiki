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

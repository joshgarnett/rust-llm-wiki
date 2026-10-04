use super::*;
use crate::changes::operation_authority::{self as operations, Presence, Publication};

fn authority(fs: &VaultFs, vault: &RecordId) -> operations::Authority {
    operations::load(fs, vault, Presence::Required)
        .unwrap()
        .unwrap()
}

fn interrupt(
    fs: &VaultFs,
    writer: &WriterPermit,
    candidate: &CatalogSelection,
    point: PublishPoint,
) {
    let result = publish_with_faults(fs, writer, candidate, Duration::ZERO, &mut |seen| {
        if seen == point {
            Err(corrupt("frozen maintenance selector fault"))
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
}

#[test]
fn maintenance_without_activation_needs_no_catalog() {
    let (_temp, fs, writer, vault) = fixture();
    assert!(
        maintenance_header(&fs, &vault, Duration::ZERO)
            .unwrap()
            .is_none()
    );
    assert!(!resume_acknowledged_rebuild(&fs, &writer, &vault, Duration::ZERO).unwrap());
}

#[test]
fn maintenance_header_reports_mutable_epoch_and_preserves_old_reader() {
    let (_temp, fs, writer, vault) = fixture();
    let first = build(&fs, &writer, &vault, "old content");
    make_wal(&fs, &first);
    put(&fs, &writer, &first);
    let old = get(&fs, &vault).unwrap().unwrap();
    let delta = open_delta(&fs, &writer, &first, Duration::ZERO).unwrap();
    delta.connection().execute_batch("UPDATE catalog_meta SET epoch=epoch+1,control_hash=NULL,dependency_hash=NULL,audit_epoch=NULL;").unwrap();
    drop(delta);
    let (selected, header) = maintenance_header(&fs, &vault, Duration::ZERO)
        .unwrap()
        .unwrap();
    assert_eq!(selected, first);
    assert_eq!(header.snapshot.generation, first.creation_epoch + 1);
    assert_eq!(
        super::super::super::normalized_read::header(old.value(), &first)
            .unwrap()
            .snapshot
            .generation,
        first.creation_epoch
    );
}

#[test]
fn exact_acknowledged_recovery_covers_every_publish_boundary_and_held_predecessor() {
    for initial in [true, false] {
        for point in [
            PublishPoint::BeforeMarker,
            PublishPoint::MarkerStaged,
            PublishPoint::MarkerDurable,
            PublishPoint::SelectorStaged,
            PublishPoint::SelectorDurable,
        ] {
            if !initial && point == PublishPoint::MarkerStaged {
                continue; // An existing activation marker is not staged again.
            }
            let (_temp, fs, writer, vault) = fixture();
            let old = if initial {
                None
            } else {
                let first = build(&fs, &writer, &vault, "held predecessor");
                put(&fs, &writer, &first);
                Some(get(&fs, &vault).unwrap().unwrap())
            };
            let candidate = build(&fs, &writer, &vault, "acknowledged content");
            interrupt(&fs, &writer, &candidate, point);
            let acknowledged = authority(&fs, &vault);
            assert_eq!(acknowledged.publication().file_id, candidate.file_id);
            assert_eq!(acknowledged.publication().epoch, candidate.creation_epoch);
            assert_eq!(
                resume_acknowledged_rebuild(&fs, &writer, &vault, Duration::ZERO).unwrap(),
                point != PublishPoint::SelectorDurable
            );
            assert!(acknowledged.same_revision(&authority(&fs, &vault)));
            let (selected, header) = maintenance_header(&fs, &vault, Duration::ZERO)
                .unwrap()
                .unwrap();
            assert_eq!(selected, candidate);
            assert_eq!(header.snapshot.generation, candidate.creation_epoch);
            assert_eq!(
                text(&get(&fs, &vault).unwrap().unwrap()),
                "acknowledged content"
            );
            if let Some(old) = old {
                assert_eq!(text(&old), "held predecessor");
            }
            assert!(!resume_acknowledged_rebuild(&fs, &writer, &vault, Duration::ZERO).unwrap());
        }
    }
}

#[test]
fn acknowledged_recovery_refuses_invalid_or_missing_exact_candidate() {
    for variant in 0..7 {
        let (_temp, fs, writer, vault) = fixture();
        let first = build(&fs, &writer, &vault, "predecessor");
        put(&fs, &writer, &first);
        let candidate = build(&fs, &writer, &vault, "exact candidate");
        interrupt(&fs, &writer, &candidate, PublishPoint::BeforeMarker);
        let before = read(&fs, CURRENT).unwrap();
        let acknowledged = authority(&fs, &vault);
        let name = path(&fs, database_name(&candidate)).unwrap();
        if variant == 6 {
            fs::remove_file(name).unwrap();
        } else {
            let connection = Connection::open(name).unwrap();
            let update = match variant {
                0 => "UPDATE catalog_meta SET state='building'",
                1 => "UPDATE catalog_meta SET epoch=epoch+1",
                2 => "UPDATE catalog_meta SET file_id='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'",
                3 => "UPDATE catalog_meta SET creation_header_hash='invalid'",
                4 => "UPDATE catalog_meta SET vault_id='vault_foreign'",
                _ => {
                    "UPDATE catalog_meta SET origin_change_id='change_foreign',origin_manifest_hash=parser_hash"
                }
            };
            connection.execute_batch(update).unwrap();
        }
        // An unrelated complete sibling is never used to replace missing or
        // damaged authority-named state.
        let _unrelated = build(&fs, &writer, &vault, "unrelated newer sibling");
        assert!(resume_acknowledged_rebuild(&fs, &writer, &vault, Duration::ZERO).is_err());
        assert_eq!(read(&fs, CURRENT).unwrap(), before);
        assert!(acknowledged.same_revision(&authority(&fs, &vault)));
        assert_eq!(text(&get(&fs, &vault).unwrap().unwrap()), "predecessor");
    }
}

#[test]
fn maintenance_resume_refuses_active_canonical_operation() {
    let (_temp, fs, writer, vault) = fixture();
    let first = build(&fs, &writer, &vault, "selected");
    put(&fs, &writer, &first);
    let idle = authority(&fs, &vault);
    let active = operations::begin(
        &fs,
        &writer,
        &idle,
        crate::changes::PreparedChange {
            change_id: RecordId::new("change_maintenance_test").unwrap(),
            manifest_hash: Blake3Hash::digest(b"maintenance test"),
        },
        Publication {
            file_id: first.file_id.clone(),
            epoch: first.creation_epoch + 1,
        },
    )
    .unwrap();
    let before = read(&fs, CURRENT).unwrap();
    assert_eq!(
        resume_acknowledged_rebuild(&fs, &writer, &vault, Duration::ZERO)
            .unwrap_err()
            .code,
        ErrorCode::RecoveryRequired
    );
    assert_eq!(
        validate_rebuild_predecessor(&fs, &writer, &vault, Duration::ZERO)
            .unwrap_err()
            .code,
        ErrorCode::RecoveryRequired
    );
    assert!(active.same_revision(&authority(&fs, &vault)));
    assert_eq!(read(&fs, CURRENT).unwrap(), before);
}

#[test]
fn coherent_pointer_allows_rebuild_admission_but_malformed_selector_refuses() {
    let (_temp, fs, writer, vault) = fixture();
    let first = build(&fs, &writer, &vault, "selected");
    put(&fs, &writer, &first);
    // Recovery does not require an intact selected cache to admit the separate
    // canonical reconstruction coordinator.
    let connection = Connection::open(path(&fs, database_name(&first)).unwrap()).unwrap();
    connection.execute_batch("DROP TABLE catalog_meta").unwrap();
    drop(connection);
    assert!(!resume_acknowledged_rebuild(&fs, &writer, &vault, Duration::ZERO).unwrap());
    assert!(maintenance_header(&fs, &vault, Duration::ZERO).is_err());
    fs::write(path(&fs, CURRENT).unwrap(), b"malformed").unwrap();
    assert!(resume_acknowledged_rebuild(&fs, &writer, &vault, Duration::ZERO).is_err());
}

#[test]
fn rebuild_predecessor_admits_corrupt_or_missing_content_without_sqlite() {
    let (_temp, fs, writer, vault) = fixture();
    let first = build(&fs, &writer, &vault, "selected");
    put(&fs, &writer, &first);
    let name = path(&fs, database_name(&first)).unwrap();
    fs::write(&name, b"corrupt sqlite contents").unwrap();
    fs::write(
        path(&fs, CachePath::File(&first.file_id, FileKind::Wal)).unwrap(),
        b"corrupt wal contents",
    )
    .unwrap();
    validate_rebuild_predecessor(&fs, &writer, &vault, Duration::ZERO).unwrap();
    fs::remove_file(name).unwrap();
    validate_rebuild_predecessor(&fs, &writer, &vault, Duration::ZERO).unwrap();
    let acknowledged = authority(&fs, &vault);
    let unrelated = build(&fs, &writer, &vault, "unpublished");
    fs::write(
        path(&fs, CURRENT).unwrap(),
        serde_json::to_vec(&unrelated).unwrap(),
    )
    .unwrap();
    assert!(validate_rebuild_predecessor(&fs, &writer, &vault, Duration::ZERO).is_err());
    assert!(acknowledged.same_revision(&authority(&fs, &vault)));
}

#[test]
fn rebuild_predecessor_refuses_unsafe_primary_and_sidecar_bindings() {
    use std::os::unix::fs::symlink;
    for kind in [
        FileKind::Database,
        FileKind::Wal,
        FileKind::Shm,
        FileKind::Journal,
    ] {
        for variant in 0..3 {
            let (temp, fs, writer, vault) = fixture();
            let first = build(&fs, &writer, &vault, "selected");
            put(&fs, &writer, &first);
            let name = path(&fs, CachePath::File(&first.file_id, kind)).unwrap();
            if name.exists() {
                fs::remove_file(&name).unwrap();
            }
            let target = temp.path().join("unsafe-link-target");
            fs::write(&target, b"untouched").unwrap();
            match variant {
                0 => symlink(&target, &name).unwrap(),
                1 => fs::hard_link(&target, &name).unwrap(),
                _ => fs::create_dir(&name).unwrap(),
            }
            assert!(validate_rebuild_predecessor(&fs, &writer, &vault, Duration::ZERO).is_err());
            assert_eq!(fs::read(target).unwrap(), b"untouched");
        }
    }
}

fn begun_candidate(fs: &VaultFs, writer: &WriterPermit, vault: &RecordId) -> CatalogSelection {
    use crate::catalog::{
        RetrievalSink,
        file_types::BuildIdentity,
        normalized_build::{BuildLimits, NormalizedBuilder},
    };
    let candidate = CatalogSelection::new(vault.clone(), 1).unwrap();
    prepare(fs, writer, &candidate).unwrap();
    let mut limits = BuildLimits::default();
    limits.max_batch_rows = 1;
    let mut builder = NormalizedBuilder::begin(
        fs,
        writer,
        BuildIdentity {
            selection: candidate.clone(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        },
        limits,
    )
    .unwrap();
    for index in 0..3 {
        builder
            .document(crate::catalog::DocumentRow {
                path: VaultRelativePath::new(format!("partial-{index}.md")).unwrap(),
                hash: Blake3Hash::digest(format!("partial {index}")),
                record_id: None,
                kind: None,
                title: format!("Partial {index}"),
                aliases: vec![],
                headings: String::new(),
                tags: vec![],
                body: "provisional body".into(),
                raw_text: "provisional body".into(),
                source_id: None,
                owner_revision: None,
                eligibility: crate::domain::Eligibility::Current,
                reasons: vec![],
            })
            .unwrap();
    }
    drop(builder); // Last open batch rolls back; earlier batches remain owned.
    candidate
}

#[test]
fn retire_unpublished_removes_partial_batched_builder_and_retains_lock_inode() {
    let (_temp, fs, writer, vault) = fixture();
    let candidate = begun_candidate(&fs, &writer, &vault);
    let name = path(&fs, database_name(&candidate)).unwrap();
    let connection = Connection::open(&name).unwrap();
    let state: String = connection
        .query_row("SELECT state FROM catalog_meta", [], |r| r.get(0))
        .unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "building");
    assert_eq!(count, 2);
    drop(connection);
    assert!(retire(&fs, &writer, &vault, &candidate, Duration::ZERO).is_err());
    assert!(retire_unpublished(&fs, &writer, &vault, &candidate, Duration::ZERO).unwrap());
    assert!(!name.exists());
    no_sidecars(&fs, &candidate).unwrap();
    assert!(path(&fs, lease_name(&candidate)).unwrap().exists());
}

#[test]
fn retire_unpublished_preserves_selected_and_acknowledged_candidates() {
    let (_temp, fs, writer, vault) = fixture();
    let first = build(&fs, &writer, &vault, "selected");
    put(&fs, &writer, &first);
    assert!(!retire_unpublished(&fs, &writer, &vault, &first, Duration::ZERO).unwrap());
    let candidate = build(&fs, &writer, &vault, "acknowledged");
    interrupt(&fs, &writer, &candidate, PublishPoint::BeforeMarker);
    assert!(!retire_unpublished(&fs, &writer, &vault, &candidate, Duration::ZERO).unwrap());
    assert!(path(&fs, database_name(&candidate)).unwrap().exists());
    assert!(resume_acknowledged_rebuild(&fs, &writer, &vault, Duration::ZERO).unwrap());
    assert_eq!(text(&get(&fs, &vault).unwrap().unwrap()), "acknowledged");
    assert!(retire_unpublished(&fs, &writer, &vault, &first, Duration::ZERO).unwrap());
}

#[test]
fn retire_unpublished_refuses_wrong_identity_malformed_header_and_unsafe_bindings() {
    use std::os::unix::fs::symlink;
    for variant in 0..7 {
        let (temp, fs, writer, vault) = fixture();
        let candidate = begun_candidate(&fs, &writer, &vault);
        let primary = path(&fs, database_name(&candidate)).unwrap();
        if variant < 3 {
            let connection = Connection::open(&primary).unwrap();
            let statement = match variant {
                0 => "UPDATE catalog_meta SET vault_id='vault_foreign'",
                1 => "UPDATE catalog_meta SET creation_header_hash='invalid'",
                _ => "DROP TABLE catalog_meta",
            };
            connection.execute_batch(statement).unwrap();
        } else {
            let target = temp.path().join("untouched-target");
            fs::write(&target, b"untouched bytes").unwrap();
            let name = if variant < 5 {
                primary.clone()
            } else {
                path(&fs, CachePath::File(&candidate.file_id, FileKind::Wal)).unwrap()
            };
            if name.exists() {
                fs::remove_file(&name).unwrap();
            }
            match variant {
                3 | 6 => symlink(&target, &name).unwrap(),
                4 => fs::hard_link(&target, &name).unwrap(),
                _ => fs::create_dir(&name).unwrap(),
            }
        }
        assert!(retire_unpublished(&fs, &writer, &vault, &candidate, Duration::ZERO).is_err());
        assert!(fs::symlink_metadata(primary).is_ok());
        if variant >= 3 {
            assert_eq!(
                fs::read(temp.path().join("untouched-target")).unwrap(),
                b"untouched bytes"
            );
        }
    }
}

#[test]
fn retire_unpublished_refuses_held_candidate_lease() {
    let (_temp, fs, writer, vault) = fixture();
    let candidate = begun_candidate(&fs, &writer, &vault);
    let held = required_lock(
        Checked::open(&path(&fs, lease_name(&candidate)).unwrap(), false, true).unwrap(),
        false,
        Duration::ZERO,
    )
    .unwrap();
    assert!(!retire_unpublished(&fs, &writer, &vault, &candidate, Duration::ZERO).unwrap());
    assert!(path(&fs, database_name(&candidate)).unwrap().exists());
    drop(held);
    assert!(retire_unpublished(&fs, &writer, &vault, &candidate, Duration::ZERO).unwrap());
}

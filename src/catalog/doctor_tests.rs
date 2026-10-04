use super::*;
use crate::{
    catalog::{query_types::QueryReadLimits, sql},
    changes::{
        PreparedChange,
        operation_authority::{self as operations, Presence, Publication},
    },
    domain::{Blake3Hash, RecordId},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::{Connection, params};
use std::{fs, path::PathBuf};

fn fixture() -> (tempfile::TempDir, Catalog) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        "---\nwiki_schema: '1'\nwiki_id: vault_doctor\nwiki_kind: vault\ntitle: Doctor\n---\n",
    )
    .unwrap();
    let catalog = Catalog::new(
        VaultFs::new(VaultRoot::explicit(temp.path()).unwrap()),
        RecordId::new("vault_doctor").unwrap(),
    );
    (temp, catalog)
}
fn legacy(catalog: &Catalog, projection: &str) -> PathBuf {
    let _writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap();
    let name = catalog.fs.root().path().join(".wiki/cache/index.sqlite");
    fs::create_dir_all(name.parent().unwrap()).unwrap();
    let mut connection = Connection::open(&name).unwrap();
    sql::initialize(&mut connection).unwrap();
    connection.execute("INSERT INTO generations(gen,state,manifest_hash,parser_hash,projection_json) VALUES(1,'complete',?1,?2,?3)", params![Blake3Hash::digest(b"recorded manifest").as_str(), scan::parser_fingerprint().as_str(), projection]).unwrap();
    connection
        .execute("UPDATE index_meta SET published_gen=1", [])
        .unwrap();
    name
}
fn normalized(catalog: &Catalog) -> (WriterPermit, crate::domain::ReadSnapshot, PathBuf) {
    let writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap();
    let rebuilt = catalog.rebuild_normalized(&writer).unwrap();
    let name = catalog.fs.root().path().join(format!(
        ".wiki/cache/catalogs/{}.sqlite",
        rebuilt.report.snapshot.publication().unwrap().file_id
    ));
    (writer, rebuilt.report.snapshot, name)
}
fn poison_unrelated(catalog: &Catalog) {
    let root = catalog.fs.root().path();
    fs::write(root.join("invalid.md"), b"---\nwiki_id: [broken\n---\n").unwrap();
    fs::create_dir_all(root.join("changes/not-a-valid-change")).unwrap();
    fs::write(
        root.join("changes/not-a-valid-change/manifest.json"),
        b"malformed history",
    )
    .unwrap();
    // This unrelated path must not matter to a physical header observation.
    fs::write(root.join("sources"), b"not a directory").unwrap();
}

#[test]
fn doctor_legacy_absent_and_large_projection_headers_do_not_scan() {
    let (_temp, catalog) = fixture();
    let absent = catalog.doctor_cache_metadata();
    assert_eq!(absent.layout, "legacy");
    assert_eq!(absent.state, "absent");
    assert_eq!(absent.operation_state, "legacy_without_slot");
    assert!(!absent.header_check_performed);
    assert!(absent.header_snapshot.is_none());
    assert!(!catalog.fs.root().path().join(".wiki").exists());
    let projection = format!(
        "{{\"large_projection\":\"{}\"}}",
        "x".repeat(16 * 1024 * 1024)
    );
    let database = legacy(&catalog, &projection);
    let before = Blake3Hash::digest(fs::read(&database).unwrap());
    poison_unrelated(&catalog);
    let observed = catalog.doctor_cache_metadata();
    assert_eq!(observed.state, "header_available", "{:?}", observed.error);
    assert_eq!(observed.parser_compatible, Some(true));
    assert!(observed.header_check_performed);
    assert_eq!(observed.header_snapshot.unwrap().generation, 1);
    assert_eq!(Blake3Hash::digest(fs::read(&database).unwrap()), before);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!PathBuf::from(format!("{}{suffix}", database.display())).exists());
    }
    let scanned = scan::scan(&catalog.fs, &catalog.vault_id).unwrap();
    assert!(
        scanned
            .diagnostics
            .iter()
            .any(|d| d.path.as_str() == "invalid.md"),
        "explicit canonical checking must discover the malformed note"
    );
}

#[test]
fn doctor_legacy_scalar_errors_and_parser_compatibility_remain_bounded() {
    for mutation in [
        "UPDATE generations SET state='building'",
        "UPDATE generations SET parser_hash='bad-hash'",
        "UPDATE generations SET parser_hash=printf('%017000d',1)",
        "UPDATE index_meta SET published_gen=NULL",
        "PRAGMA user_version=2",
    ] {
        let (_temp, catalog) = fixture();
        let database = legacy(&catalog, "{invalid projection deliberately not decoded}");
        let connection = Connection::open(&database).unwrap();
        connection.execute_batch(mutation).unwrap();
        drop(connection);
        let observed = catalog.doctor_cache_metadata();
        assert_eq!(observed.state, "unavailable", "{mutation}");
        assert!(observed.error.is_some(), "{mutation}");
        assert!(observed.header_snapshot.is_none());
    }
    let (_temp, catalog) = fixture();
    let database = legacy(&catalog, "{invalid projection deliberately not decoded}");
    let connection = Connection::open(database).unwrap();
    connection
        .execute(
            "UPDATE generations SET parser_hash=?1",
            [Blake3Hash::digest(b"old parser").as_str()],
        )
        .unwrap();
    drop(connection);
    let observed = catalog.doctor_cache_metadata();
    assert_eq!(observed.state, "header_available");
    assert_eq!(observed.parser_compatible, Some(false));
}

#[test]
fn doctor_legacy_complete_wal_is_read_without_database_or_wal_writes() {
    let (_temp, catalog) = fixture();
    let database = legacy(&catalog, "{}");
    let _writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap();
    let connection = Connection::open(&database).unwrap();
    selector::configure_wal(&connection).unwrap();
    connection
        .execute("UPDATE index_meta SET vector_loss_unknown=0", [])
        .unwrap();
    let wal = PathBuf::from(format!("{}-wal", database.display()));
    let shm = PathBuf::from(format!("{}-shm", database.display()));
    drop(connection);
    drop(_writer);
    assert!(wal.exists() && shm.exists());
    let before = (fs::read(&database).unwrap(), fs::read(&wal).unwrap());
    let observed = catalog.doctor_cache_metadata();
    assert_eq!(observed.state, "header_available", "{:?}", observed.error);
    assert!(observed.header_check_performed);
    assert_eq!(
        (fs::read(&database).unwrap(), fs::read(&wal).unwrap()),
        before
    );
    assert!(shm.exists());
}

#[test]
fn doctor_legacy_wal_missing_pair_is_ambiguous_and_creates_no_sidecars() {
    for missing in ["both", "wal", "shm"] {
        let (_temp, catalog) = fixture();
        let database = legacy(&catalog, "{}");
        let connection = Connection::open(&database).unwrap();
        selector::configure_wal(&connection).unwrap();
        drop(connection);
        let wal = PathBuf::from(format!("{}-wal", database.display()));
        let shm = PathBuf::from(format!("{}-shm", database.display()));
        if missing != "shm" {
            fs::remove_file(&wal).unwrap();
        }
        if missing != "wal" {
            fs::remove_file(&shm).unwrap();
        }
        let before = fs::read(&database).unwrap();
        let observed = catalog.doctor_cache_metadata();
        assert_eq!(
            observed.state, "present_uninspected",
            "{missing}: {:?}",
            observed.error
        );
        assert!(!observed.header_check_performed);
        assert!(observed.header_snapshot.is_none());
        assert!(observed.error.is_none());
        assert!(observed.note.unwrap().contains("clean close"));
        assert_eq!(wal.exists(), missing == "shm");
        assert_eq!(shm.exists(), missing == "wal");
        assert_eq!(fs::read(database).unwrap(), before);
    }
}

#[cfg(unix)]
#[test]
fn doctor_legacy_inspects_unsafe_present_sidecars_before_uninspected_return() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for unsafe_kind in ["symlink", "directory", "hardlink", "permissions"] {
        let (temp, catalog) = fixture();
        let database = legacy(&catalog, "{}");
        let connection = Connection::open(&database).unwrap();
        selector::configure_wal(&connection).unwrap();
        drop(connection);
        fs::remove_file(format!("{}-wal", database.display())).unwrap();
        let shm = PathBuf::from(format!("{}-shm", database.display()));
        fs::remove_file(&shm).unwrap();
        match unsafe_kind {
            "symlink" => symlink(temp.path().join("WIKI.md"), &shm).unwrap(),
            "directory" => fs::create_dir(&shm).unwrap(),
            "hardlink" => fs::hard_link(temp.path().join("WIKI.md"), &shm).unwrap(),
            "permissions" => {
                fs::write(&shm, b"sidecar").unwrap();
                fs::set_permissions(&shm, fs::Permissions::from_mode(0o666)).unwrap();
            }
            _ => unreachable!(),
        }
        let observed = catalog.doctor_cache_metadata();
        assert_eq!(observed.state, "unavailable", "{unsafe_kind}");
        assert!(observed.error.is_some());
        assert!(!observed.header_check_performed);
    }
}

#[test]
fn doctor_delete_with_regular_sidecar_is_uninspected() {
    let (_temp, catalog) = fixture();
    let database = legacy(&catalog, "{}");
    fs::write(
        format!("{}-journal", database.display()),
        b"uninterpreted journal",
    )
    .unwrap();
    let observed = catalog.doctor_cache_metadata();
    assert_eq!(observed.state, "present_uninspected");
    assert!(!observed.header_check_performed);
    assert!(observed.error.is_none());
}

#[test]
fn doctor_normalized_header_and_active_slot_do_not_audit_unrelated_inputs() {
    let (_temp, catalog) = fixture();
    let (writer, snapshot, _database) = normalized(&catalog);
    let before = catalog.doctor_cache_metadata();
    assert_eq!(before.layout, "normalized");
    assert_eq!(before.state, "header_available", "{:?}", before.error);
    assert_eq!(before.header_snapshot.as_ref(), Some(&snapshot));
    assert_eq!(before.operation_state, "idle");
    assert_eq!(before.parser_compatible, Some(true));
    poison_unrelated(&catalog);
    let unchanged = catalog.doctor_cache_metadata();
    assert_eq!(unchanged.header_snapshot.as_ref(), Some(&snapshot));
    let authority = operations::load(&catalog.fs, &catalog.vault_id, Presence::Required)
        .unwrap()
        .unwrap();
    let change = PreparedChange {
        change_id: RecordId::new("change_doctor_active").unwrap(),
        manifest_hash: Blake3Hash::digest(b"active doctor fixture"),
    };
    operations::begin(
        &catalog.fs,
        &writer,
        &authority,
        change.clone(),
        Publication {
            file_id: snapshot.publication().unwrap().file_id.clone(),
            epoch: snapshot.generation + 1,
        },
    )
    .unwrap();
    let active = catalog.doctor_cache_metadata();
    assert_eq!(active.state, "header_available", "{:?}", active.error);
    assert_eq!(active.operation_state, "active");
    assert_eq!(active.active_change, Some(change.change_id));
    assert_eq!(active.header_snapshot.as_ref(), Some(&snapshot));
    assert!(active.note.unwrap().contains("does not establish"));
}

#[test]
fn doctor_normalized_missing_or_corrupt_authority_never_falls_back_to_legacy() {
    for corruption in [
        "missing_authority",
        "bad_authority",
        "bad_selector",
        "missing_database",
        "missing_shm",
    ] {
        let (temp, catalog) = fixture();
        let (writer, _snapshot, database) = normalized(&catalog);
        drop(writer);
        legacy(&catalog, "{}");
        match corruption {
            "missing_authority" => {
                fs::remove_file(temp.path().join(".wiki/state/operations.json")).unwrap()
            }
            "bad_authority" => {
                fs::write(temp.path().join(".wiki/state/operations.json"), b"broken").unwrap()
            }
            "bad_selector" => fs::write(
                temp.path().join(".wiki/cache/catalog-current.json"),
                b"broken",
            )
            .unwrap(),
            "missing_database" => fs::remove_file(database).unwrap(),
            "missing_shm" => fs::remove_file(format!("{}-shm", database.display())).unwrap(),
            _ => unreachable!(),
        }
        let observed = catalog.doctor_cache_metadata();
        assert_eq!(observed.state, "unavailable", "{corruption}");
        assert!(observed.error.is_some());
        assert_eq!(observed.layout, "normalized");
        assert!(observed.header_snapshot.is_none());
    }
}

#[test]
fn doctor_normalized_parser_mismatch_is_metadata_and_header_corruption_is_error() {
    let (_temp, catalog) = fixture();
    let (_writer, _snapshot, database) = normalized(&catalog);
    let connection = Connection::open(&database).unwrap();
    selector::configure_wal(&connection).unwrap();
    connection
        .execute(
            "UPDATE catalog_meta SET parser_hash=?1",
            [Blake3Hash::digest(b"old parser").as_str()],
        )
        .unwrap();
    let mismatched = catalog.doctor_cache_metadata();
    assert_eq!(mismatched.state, "header_available");
    assert_eq!(mismatched.parser_compatible, Some(false));
    connection
        .execute("UPDATE catalog_meta SET state='building'", [])
        .unwrap();
    let corrupt = catalog.doctor_cache_metadata();
    assert_eq!(corrupt.state, "unavailable");
    assert!(corrupt.error.is_some());
    assert!(corrupt.header_snapshot.is_none());
    drop(connection);
    // Header inspection is not query admission; this test deliberately corrupts
    // a disposable cache and does not attempt to heal it.
    assert!(catalog.query_snapshot(QueryReadLimits::default()).is_err());
}

fn fixture_bytes(root: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(
        root: &std::path::Path,
        directory: &std::path::Path,
        out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), out);
            } else {
                out.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut result = std::collections::BTreeMap::new();
    visit(root, root, &mut result);
    result
}
fn make_legacy_wal(catalog: &Catalog, database: &std::path::Path) {
    let _writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap();
    let connection = Connection::open(database).unwrap();
    selector::configure_wal(&connection).unwrap();
}

#[test]
fn doctor_shared_writer_guard_prevents_wal_and_delete_mutation_at_open_boundary() {
    for wal in [false, true] {
        let (_temp, catalog) = fixture();
        let database = legacy(&catalog, "{}");
        if wal {
            make_legacy_wal(&catalog, &database);
        }
        let before = fixture_bytes(catalog.fs.root().path());
        let root = catalog.fs.root().clone();
        let attempted_database = database.clone();
        let mut attempts = 0;
        let header =
            selector::legacy_doctor_header_with_probe(&catalog.fs, &mut || Ok(()), &mut || {
                attempts += 1;
                let root = root.clone();
                let database = attempted_database.clone();
                let attempt = std::thread::spawn(move || -> Result<()> {
                    let _writer = WriterPermit::acquire(&root, Duration::ZERO)?;
                    let connection = Connection::open(database).map_err(sql::sql_error)?;
                    if wal {
                        connection
                            .execute_batch(
                                "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;",
                            )
                            .map_err(sql::sql_error)?;
                    } else {
                        selector::configure_wal(&connection)?;
                    }
                    Ok(())
                })
                .join()
                .unwrap();
                assert_eq!(
                    attempt.unwrap_err().code,
                    ErrorCode::LockTimeout,
                    "ordinary writer must not alter the preflighted journal/sidecars"
                );
                Ok(())
            })
            .unwrap();
        assert!(matches!(header, selector::LegacyDoctorHeader::Header(_)));
        assert_eq!(attempts, 1);
        let after = fixture_bytes(catalog.fs.root().path());
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>()
        );
        for (name, bytes) in &before {
            if !name.to_string_lossy().ends_with("-shm") {
                assert_eq!(after[name], *bytes, "{}", name.display());
            }
        }
        // The guard covers SQLite close, then releases rather than becoming a
        // persistent reader veto on later ordinary writes.
        drop(WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap());
    }
}

#[test]
fn doctor_missing_or_busy_writer_guard_never_opens_sqlite_or_changes_fixture() {
    for wal in [false, true] {
        for busy in [false, true] {
            let (_temp, catalog) = fixture();
            let database = legacy(&catalog, "{}");
            if wal {
                make_legacy_wal(&catalog, &database);
            }
            let held = if busy {
                Some(WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap())
            } else {
                fs::remove_file(catalog.fs.root().path().join(".wiki/state/writer.lock")).unwrap();
                None
            };
            let before = fixture_bytes(catalog.fs.root().path());
            let mut opened = false;
            let header =
                selector::legacy_doctor_header_with_probe(&catalog.fs, &mut || Ok(()), &mut || {
                    opened = true;
                    Ok(())
                })
                .unwrap();
            assert!(matches!(
                header,
                selector::LegacyDoctorHeader::Uninspected(_)
            ));
            assert!(!opened);
            let observed = catalog.doctor_cache_metadata();
            assert_eq!(observed.state, "present_uninspected");
            assert!(!observed.header_check_performed);
            assert!(observed.error.is_none());
            assert!(
                observed
                    .note
                    .unwrap()
                    .contains(if busy { "busy" } else { "absent" })
            );
            assert_eq!(fixture_bytes(catalog.fs.root().path()), before);
            drop(held);
        }
    }
}

#[cfg(unix)]
#[test]
fn doctor_unsafe_sidecar_cannot_hide_behind_missing_or_busy_writer_guard() {
    use std::os::unix::fs::symlink;
    for busy in [false, true] {
        let (temp, catalog) = fixture();
        let database = legacy(&catalog, "{}");
        make_legacy_wal(&catalog, &database);
        fs::remove_file(format!("{}-wal", database.display())).unwrap();
        let shm = format!("{}-shm", database.display());
        fs::remove_file(&shm).unwrap();
        symlink(temp.path().join("WIKI.md"), &shm).unwrap();
        let held = if busy {
            Some(WriterPermit::acquire(catalog.fs.root(), Duration::ZERO).unwrap())
        } else {
            fs::remove_file(temp.path().join(".wiki/state/writer.lock")).unwrap();
            None
        };
        let observed = catalog.doctor_cache_metadata();
        assert_eq!(observed.state, "unavailable");
        assert!(observed.error.is_some());
        assert!(!observed.header_check_performed);
        drop(held);
    }
}

#[cfg(unix)]
#[test]
fn doctor_revalidates_main_handle_after_obtaining_shared_writer_guard() {
    let (_temp, catalog) = fixture();
    let database = legacy(&catalog, "{}");
    let replacement = database.with_file_name("replacement.sqlite");
    let mut opened = false;
    let result = selector::legacy_doctor_header_with_probe(
        &catalog.fs,
        &mut || {
            let _writer = WriterPermit::acquire(catalog.fs.root(), Duration::ZERO)?;
            let mut connection = Connection::open(&replacement).unwrap();
            sql::initialize(&mut connection)?;
            selector::configure_wal(&connection)?;
            drop(connection);
            fs::remove_file(format!("{}-wal", replacement.display())).unwrap();
            fs::remove_file(format!("{}-shm", replacement.display())).unwrap();
            fs::rename(&replacement, &database).unwrap();
            Ok(())
        },
        &mut || {
            opened = true;
            Ok(())
        },
    );
    assert!(matches!(result, Err(ref error) if error.code == ErrorCode::IndexCorrupt));
    assert!(
        !opened,
        "old DELETE handle must not admit a replaced WAL-mode main file"
    );
    assert!(!PathBuf::from(format!("{}-shm", database.display())).exists());
    assert!(!PathBuf::from(format!("{}-wal", database.display())).exists());
}

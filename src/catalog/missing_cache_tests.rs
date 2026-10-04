use super::*;
use crate::vault::VaultRoot;
use rusqlite::{Connection, params};
use std::{fs, path::PathBuf};

struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    writer: WriterPermit,
    vault: RecordId,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: vault_missing_cache\nwiki_kind: vault\ntitle: Missing cache fixture\n---\nCanonical bytes stay intact.\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
        let vault = RecordId::new("vault_missing_cache").unwrap();
        let previous = CatalogSelection::new(vault.clone(), 7).unwrap();
        operations::activate(
            &fs,
            &writer,
            &vault,
            Publication {
                file_id: previous.file_id,
                epoch: 7,
            },
            Presence::LegacyMayBeAbsent,
        )
        .unwrap();
        Self {
            _temp: temp,
            fs,
            writer,
            vault,
        }
    }
    fn admit(&self) -> Admission {
        prepare(&self.fs, &self.writer, &self.vault, true, Duration::ZERO).unwrap()
    }
    fn candidate(&self) -> CatalogSelection {
        self.admit().candidate.unwrap()
    }
    fn record(&self) -> Vec<u8> {
        selector::read_rebuild_record(&self.fs).unwrap().unwrap()
    }
    fn witness(&self) -> Witness {
        load(&self.fs, &self.vault).unwrap().unwrap().0
    }
    fn write_witness(&self, witness: &Witness) {
        selector::store_rebuild_record(
            &self.fs,
            &self.writer,
            &ExpectedState::Hash(Blake3Hash::digest(self.record())),
            Some(&witness.bytes().unwrap()),
        )
        .unwrap();
    }
    fn database(&self, candidate: &CatalogSelection) -> PathBuf {
        self.fs
            .root()
            .path()
            .join(format!(".wiki/cache/catalogs/{}.sqlite", candidate.file_id))
    }
    fn partial(&self, candidate: &CatalogSelection, bytes: &[u8]) -> PathBuf {
        let name = selector::prepare(&self.fs, &self.writer, candidate).unwrap();
        fs::write(&name, bytes).unwrap();
        name
    }
    fn complete(&self, candidate: &CatalogSelection, large: bool) -> PathBuf {
        let name = selector::prepare(&self.fs, &self.writer, candidate).unwrap();
        let connection = Connection::open(&name).unwrap();
        connection
            .execute_batch(super::super::normalized_schema::SCHEMA)
            .unwrap();
        let hash = Blake3Hash::digest(b"missing-cache native fixture");
        connection.execute(
            "INSERT INTO catalog_meta(singleton,schema_version,vault_id,file_id,creation_epoch,creation_header_hash,epoch,parser_hash,publication_hash,control_hash,dependency_hash,audit_epoch,state,vector_cache_lost,vector_loss_unknown) VALUES(1,3,?1,?2,?3,?4,?3,?5,?5,?5,?5,?3,'complete',0,0)",
            params![self.vault.as_str(), candidate.file_id, candidate.creation_epoch as i64, candidate.creation_header_hash.as_str(), hash.as_str()],
        ).unwrap();
        if large {
            connection.execute_batch("CREATE TABLE native_padding(data BLOB); INSERT INTO native_padding VALUES(zeroblob(1100000));").unwrap();
        }
        connection
            .execute_batch("PRAGMA journal_mode=DELETE;")
            .unwrap();
        drop(connection);
        name
    }
}

#[test]
fn first_force_reserves_before_cache_creation_and_nonforce_has_no_effects() {
    let fixture = Fixture::new();
    let canonical = fixture.fs.root().path().join("WIKI.md");
    let bytes = fs::read(&canonical).unwrap();
    let modified = fs::metadata(&canonical).unwrap().modified().unwrap();
    let before = authority(&fixture.fs, &fixture.vault).unwrap();
    let ordinary = prepare(
        &fixture.fs,
        &fixture.writer,
        &fixture.vault,
        false,
        Duration::ZERO,
    )
    .unwrap();
    assert!(ordinary.candidate.is_none());
    assert!(
        selector::read_rebuild_record(&fixture.fs)
            .unwrap()
            .is_none()
    );
    assert!(selector::cache_root_absent(&fixture.fs).unwrap());
    let candidate = fixture.candidate();
    assert_eq!(candidate.creation_epoch, 8);
    assert_ne!(candidate.file_id, before.publication().file_id);
    assert!(fixture.record().len() <= MAX_RECORD_BYTES);
    assert!(selector::cache_root_absent(&fixture.fs).unwrap());
    assert_eq!(authority(&fixture.fs, &fixture.vault).unwrap(), before);
    assert_eq!(fs::read(&canonical).unwrap(), bytes);
    assert_eq!(
        fs::metadata(&canonical).unwrap().modified().unwrap(),
        modified
    );
    assert!(authorize_publication(&fixture.fs, &fixture.vault, &candidate, &before).unwrap());
    let record = fixture.record();
    assert_eq!(
        prepare(
            &fixture.fs,
            &fixture.writer,
            &fixture.vault,
            false,
            Duration::ZERO
        )
        .unwrap_err()
        .code,
        ErrorCode::RecoveryRequired
    );
    assert_eq!(fixture.record(), record);
}

#[test]
fn an_existing_empty_cache_is_not_adopted_as_whole_cache_loss() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.fs.root().path().join(".wiki/cache")).unwrap();
    assert!(fixture.admit().candidate.is_none());
    assert!(
        selector::read_rebuild_record(&fixture.fs)
            .unwrap()
            .is_none()
    );
}

#[test]
fn first_activation_without_outside_authority_does_not_create_rebuild_witness() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.fs.root().path().join(".wiki/state/operations.json")).unwrap();
    let admitted = fixture.admit();
    assert!(admitted.candidate.is_none());
    assert!(!admitted.resumed);
    assert!(
        selector::read_rebuild_record(&fixture.fs)
            .unwrap()
            .is_none()
    );
    assert!(selector::cache_root_absent(&fixture.fs).unwrap());
}

#[test]
fn interrupted_reservation_without_cache_rotates_one_fresh_identity() {
    let fixture = Fixture::new();
    let first = fixture.candidate();
    let second = fixture.admit();
    assert!(second.abandoned.is_empty());
    assert!(!second.resumed);
    assert_ne!(second.candidate.as_ref().unwrap().file_id, first.file_id);
    assert!(selector::cache_root_absent(&fixture.fs).unwrap());
    assert_eq!(fixture.witness().candidate, second.candidate.unwrap());
}

#[test]
fn unknown_small_and_zero_byte_candidates_are_preserved_without_reuse() {
    for bytes in [&b"partial initialization"[..], &b""[..]] {
        let fixture = Fixture::new();
        let first = fixture.candidate();
        let name = fixture.partial(&first, bytes);
        let modified = fs::metadata(&name).unwrap().modified().unwrap();
        let next = fixture.admit();
        assert_eq!(
            next.abandoned,
            vec![Abandoned {
                candidate: first.clone(),
                bytes: bytes.len() as u64
            }]
        );
        assert_ne!(next.candidate.unwrap().file_id, first.file_id);
        assert_eq!(fs::read(&name).unwrap(), bytes);
        assert_eq!(fs::metadata(&name).unwrap().modified().unwrap(), modified);
        let subsequent = fixture.admit();
        assert_eq!(subsequent.abandoned[0].candidate, first);
        assert_eq!(fs::read(&name).unwrap(), bytes);
    }
}

#[test]
fn abandoned_sizes_are_remeasured_and_sidecar_only_bytes_count() {
    let fixture = Fixture::new();
    let first = fixture.candidate();
    selector::prepare(&fixture.fs, &fixture.writer, &first).unwrap();
    let wal = fixture.database(&first).with_extension("sqlite-wal");
    let shm = fixture.database(&first).with_extension("sqlite-shm");
    let journal = fixture.database(&first).with_extension("sqlite-journal");
    fs::write(&wal, b"sidecar-only WAL").unwrap();
    fs::write(&shm, b"SHM").unwrap();
    fs::write(&journal, b"journal").unwrap();
    let actual = b"sidecar-only WAL".len() as u64 + b"SHM".len() as u64 + b"journal".len() as u64;
    let next = fixture.admit();
    assert_eq!(next.abandoned[0].bytes, actual);
    assert!(!fixture.database(&first).exists());
    let mut witness = fixture.witness();
    witness.abandoned[0].bytes = u64::MAX;
    fixture.write_witness(&witness);
    let next = fixture.admit();
    assert_eq!(next.abandoned[0].bytes, actual);
    assert_eq!(fs::read(&wal).unwrap(), b"sidecar-only WAL");
    assert_eq!(fs::read(&shm).unwrap(), b"SHM");
    assert_eq!(fs::read(&journal).unwrap(), b"journal");
}

#[test]
fn actual_one_mebibyte_boundary_and_external_growth_are_enforced() {
    let fixture = Fixture::new();
    let first = fixture.candidate();
    let name = fixture.partial(&first, b"");
    fs::OpenOptions::new()
        .write(true)
        .open(&name)
        .unwrap()
        .set_len(MAX_ABANDONED_BYTES)
        .unwrap();
    let next = fixture.admit();
    assert_eq!(next.abandoned[0].bytes, MAX_ABANDONED_BYTES);
    let saved = fixture.record();
    fs::OpenOptions::new()
        .write(true)
        .open(&name)
        .unwrap()
        .set_len(MAX_ABANDONED_BYTES + 1)
        .unwrap();
    let grown = fs::read(&name).unwrap();
    assert_eq!(
        prepare(
            &fixture.fs,
            &fixture.writer,
            &fixture.vault,
            true,
            Duration::ZERO
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(fixture.record(), saved);
    assert_eq!(fs::read(&name).unwrap(), grown);
    assert_eq!(
        authority(&fixture.fs, &fixture.vault)
            .unwrap()
            .publication()
            .epoch,
        7
    );
}

#[test]
fn eight_preserved_entries_fit_and_ninth_refuses_without_rotation() {
    let fixture = Fixture::new();
    let mut current = fixture.candidate();
    for count in 1..=MAX_ABANDONED {
        fixture.partial(&current, b"small interrupted build");
        let next = fixture.admit();
        assert_eq!(next.abandoned.len(), count);
        assert!(fixture.record().len() <= MAX_RECORD_BYTES);
        current = next.candidate.unwrap();
    }
    let ninth = fixture.partial(&current, b"ninth");
    let saved = fixture.record();
    assert_eq!(
        prepare(
            &fixture.fs,
            &fixture.writer,
            &fixture.vault,
            true,
            Duration::ZERO
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(fixture.record(), saved);
    assert_eq!(fs::read(ninth).unwrap(), b"ninth");
}

#[test]
fn authenticated_large_catalog_is_retired_and_lease_identity_remains() {
    let fixture = Fixture::new();
    let first = fixture.candidate();
    let name = fixture.complete(&first, true);
    assert!(fs::metadata(&name).unwrap().len() > MAX_ABANDONED_BYTES);
    let lease = name.with_extension("lock");
    let modified = fs::metadata(&lease).unwrap().modified().unwrap();
    let next = fixture.admit();
    assert!(next.abandoned.is_empty());
    assert!(!name.exists());
    assert_eq!(fs::metadata(&lease).unwrap().modified().unwrap(), modified);
    assert_ne!(next.candidate.unwrap().file_id, first.file_id);
    assert_eq!(
        authority(&fixture.fs, &fixture.vault)
            .unwrap()
            .publication()
            .epoch,
        7
    );
}

#[test]
fn foreign_header_and_unreserved_namespace_are_refused_before_rotation() {
    for foreign in [false, true] {
        let fixture = Fixture::new();
        let first = fixture.candidate();
        let name = fixture.complete(&first, false);
        if foreign {
            let connection = Connection::open(&name).unwrap();
            connection
                .execute("UPDATE catalog_meta SET vault_id='vault_foreign'", [])
                .unwrap();
        } else {
            fs::write(name.parent().unwrap().join("unreserved.sqlite"), b"unknown").unwrap();
        }
        let saved = fixture.record();
        let bytes = fs::read(&name).unwrap();
        assert!(
            prepare(
                &fixture.fs,
                &fixture.writer,
                &fixture.vault,
                true,
                Duration::ZERO
            )
            .is_err()
        );
        assert_eq!(fixture.record(), saved);
        assert_eq!(fs::read(&name).unwrap(), bytes);
    }
}

#[test]
fn stale_authority_and_wrong_candidate_never_authorize_publication() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate();
    let initial = authority(&fixture.fs, &fixture.vault).unwrap();
    let unrelated = CatalogSelection::new(fixture.vault.clone(), 8).unwrap();
    assert!(authorize_publication(&fixture.fs, &fixture.vault, &unrelated, &initial).is_err());
    let saved = fixture.record();
    operations::publish_rebuild(
        &fixture.fs,
        &fixture.writer,
        &initial,
        Publication {
            file_id: unrelated.file_id,
            epoch: 8,
        },
    )
    .unwrap();
    let now = authority(&fixture.fs, &fixture.vault).unwrap();
    assert!(authorize_publication(&fixture.fs, &fixture.vault, &candidate, &now).is_err());
    assert!(
        prepare(
            &fixture.fs,
            &fixture.writer,
            &fixture.vault,
            true,
            Duration::ZERO
        )
        .is_err()
    );
    assert_eq!(fixture.record(), saved);
    assert!(selector::cache_root_absent(&fixture.fs).unwrap());
}

#[test]
fn witness_retry_requires_present_idle_outside_authority() {
    for missing in [false, true] {
        let fixture = Fixture::new();
        let candidate = fixture.candidate();
        let saved = fixture.record();
        if missing {
            fs::remove_file(fixture.fs.root().path().join(".wiki/state/operations.json")).unwrap();
        } else {
            let initial = authority(&fixture.fs, &fixture.vault).unwrap();
            operations::begin(
                &fixture.fs,
                &fixture.writer,
                &initial,
                crate::changes::PreparedChange {
                    change_id: RecordId::new("change_active_fixture").unwrap(),
                    manifest_hash: Blake3Hash::digest(b"active operation"),
                },
                Publication {
                    file_id: initial.publication().file_id.clone(),
                    epoch: 8,
                },
            )
            .unwrap();
        }
        assert!(
            prepare(
                &fixture.fs,
                &fixture.writer,
                &fixture.vault,
                true,
                Duration::ZERO
            )
            .is_err()
        );
        assert_eq!(fixture.record(), saved);
        assert!(selector::cache_root_absent(&fixture.fs).unwrap());
        assert!(!fixture.database(&candidate).exists());
    }
}

#[test]
fn malformed_and_oversized_witnesses_are_not_replaced() {
    for bytes in [
        b"{\"version\":1,\"unknown\":true}".to_vec(),
        vec![b' '; MAX_RECORD_BYTES + 1],
    ] {
        let fixture = Fixture::new();
        let name = fixture
            .fs
            .root()
            .path()
            .join(".wiki/state/catalog-rebuild.json");
        fs::write(&name, &bytes).unwrap();
        assert!(
            prepare(
                &fixture.fs,
                &fixture.writer,
                &fixture.vault,
                true,
                Duration::ZERO
            )
            .is_err()
        );
        assert_eq!(fs::read(&name).unwrap(), bytes);
        assert!(selector::cache_root_absent(&fixture.fs).unwrap());
    }
}

#[test]
fn exact_acknowledged_candidate_resumes_then_clears_once_with_inventory() {
    let fixture = Fixture::new();
    let interrupted = fixture.candidate();
    let abandoned_name = fixture.partial(&interrupted, b"preserved interruption");
    let admitted = fixture.admit();
    let candidate = admitted.candidate.unwrap();
    fixture.complete(&candidate, false);
    let canonical = fixture.fs.root().path().join("WIKI.md");
    let canonical_bytes = fs::read(&canonical).unwrap();
    let modified = fs::metadata(&canonical).unwrap().modified().unwrap();
    let failed = selector::publish_with_faults(
        &fixture.fs,
        &fixture.writer,
        &candidate,
        Duration::ZERO,
        &mut |point| {
            if matches!(point, selector::PublishPoint::BeforeMarker) {
                Err(recovery("native post-ack cut"))
            } else {
                Ok(())
            }
        },
    );
    assert!(failed.is_err());
    assert_eq!(
        authority(&fixture.fs, &fixture.vault)
            .unwrap()
            .publication()
            .file_id,
        candidate.file_id
    );
    let resumed = fixture.admit();
    assert!(resumed.resumed);
    assert!(resumed.candidate.is_none());
    assert_eq!(resumed.abandoned, admitted.abandoned);
    assert!(
        selector::read_rebuild_record(&fixture.fs)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fs::read(&abandoned_name).unwrap(),
        b"preserved interruption"
    );
    let (selected, header) =
        selector::maintenance_header(&fixture.fs, &fixture.vault, Duration::ZERO)
            .unwrap()
            .unwrap();
    assert_eq!(selected, candidate);
    assert_eq!(header.snapshot.generation, 8);
    let acknowledged = authority(&fixture.fs, &fixture.vault).unwrap();
    assert_eq!(acknowledged.revision(), 2);
    assert!(
        finish(&fixture.fs, &fixture.writer, &fixture.vault)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        authority(&fixture.fs, &fixture.vault).unwrap(),
        acknowledged
    );
    assert_eq!(fs::read(&canonical).unwrap(), canonical_bytes);
    assert_eq!(
        fs::metadata(&canonical).unwrap().modified().unwrap(),
        modified
    );
}

#[cfg(unix)]
#[test]
fn symlink_and_hardlink_candidate_artifacts_are_not_abandoned() {
    use std::os::unix::fs::symlink;
    for hardlink in [false, true] {
        let fixture = Fixture::new();
        let candidate = fixture.candidate();
        let name = selector::prepare(&fixture.fs, &fixture.writer, &candidate).unwrap();
        let outside = fixture.fs.root().path().join("foreign-bytes");
        fs::write(&outside, b"foreign").unwrap();
        if hardlink {
            fs::hard_link(&outside, &name).unwrap();
        } else {
            symlink(&outside, &name).unwrap();
        }
        let saved = fixture.record();
        assert!(
            prepare(
                &fixture.fs,
                &fixture.writer,
                &fixture.vault,
                true,
                Duration::ZERO
            )
            .is_err()
        );
        assert_eq!(fixture.record(), saved);
        assert_eq!(fs::read(&outside).unwrap(), b"foreign");
    }
}

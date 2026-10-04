use super::*;
use crate::{
    catalog::CatalogGraphValidator,
    changes::{ChangeDraft, ChangeEngine, ExpectedWrite},
    domain::{Blake3Hash, RecordId, VaultRelativePath},
    vault::{ExpectedState, VaultFs, VaultRoot},
};
use rusqlite::{params_from_iter, types::Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const VAULT: &str = "vault_00000000-0000-7000-8000-00000000001b";
fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy(&path, &target)
        } else {
            fs::copy(path, target).unwrap();
        }
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    catalog: Catalog,
    writer: WriterPermit,
    database: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bootstrap/vault"),
            temp.path(),
        );
        let forward = temp.path().join("knowledge/assertions/forward.md");
        fs::write(
            &forward,
            fs::read_to_string(&forward).unwrap().replace(
                "wiki_status: \"accepted\"\n",
                "wiki_status: \"accepted\"\nwiki_evidence: ['[[revision]]', '[[absent.md]]']\n",
            ),
        )
        .unwrap();
        fs::write(
            temp.path().join("invalid.md"),
            b"---\nwiki_id: [broken\n---\nbroken canonical note\n",
        )
        .unwrap();
        fs::write(temp.path().join("linked.md"),
            "---\nwiki_schema: '1'\nwiki_id: page_audit_links\nwiki_kind: page\ntitle: Linked fixture\nwiki_status: reviewed\n---\nSee [[missing-target]] and [[knowledge/entities/north_lab.md]].\n").unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(handle.root(), Duration::ZERO).unwrap();
        let catalog = Catalog::new(handle.clone(), RecordId::new(VAULT).unwrap());
        // Real retained owner authority, including a tree no longer in canonical storage.
        let engine = ChangeEngine::new(handle).unwrap();
        let draft = ChangeDraft {
            title: "Retained audit fixture".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: vec![],
            operations: vec![ExpectedWrite {
                target: VaultRelativePath::new(
                    "sources/legacy owner/revisions/retained/original.bin",
                )
                .unwrap(),
                expected: ExpectedState::Absent,
                proposed: Some(b"retained immutable bytes".to_vec()),
                apply_after: vec![],
            }],
        };
        let prepared = engine.prepare(&writer, draft).unwrap().prepared;
        engine
            .apply(&writer, &prepared, &CatalogGraphValidator, &catalog)
            .unwrap();
        fs::remove_dir_all(temp.path().join("sources/legacy owner")).unwrap();
        catalog.rebuild_normalized(&writer).unwrap();
        // Keep layout ownership with the selector rather than relying on a filename convention.
        let database = selector::acquire(
            catalog.fs(),
            catalog.vault_id(),
            Duration::ZERO,
            |path, _| Ok(path.to_path_buf()),
        )
        .unwrap()
        .unwrap()
        .value()
        .clone();
        Self {
            _temp: temp,
            catalog,
            writer,
            database,
        }
    }
    fn check(&self) -> CheckReport {
        self.catalog.check_normalized(&self.writer).unwrap()
    }
}

#[test]
fn full_check_reconciles_real_bootstrap_and_deleted_owner_without_source_writes() {
    let fixture = Fixture::new();
    let before = Blake3Hash::digest(fs::read(&fixture.database).unwrap());
    let report = fixture.check();
    assert!(
        !report.diagnostics.is_empty(),
        "faithful invalid knowledge remains diagnostics"
    );
    assert!(report.work.rows > 100);
    assert!(report.work.postings > 0);
    assert!(report.scratch_bytes > 0);
    assert_eq!(
        before,
        Blake3Hash::digest(fs::read(&fixture.database).unwrap())
    );
    let again = fixture.check();
    assert_eq!(report.snapshot, again.snapshot);
}

#[test]
fn full_check_all_nineteen_families_reject_changed_missing_and_extra_rows() {
    let fixture = Fixture::new();
    fixture.check();
    let db = Connection::open(&fixture.database).unwrap();
    let families = [
        "documents",
        "graph_rows",
        "records",
        "identity_claims",
        "source_revision_identity",
        "source_evidence",
        "revision_tree_owners",
        "links",
        "link_facts",
        "link_match_keys",
        "registry_match_keys",
        "assertion_navigation_keys",
        "opposition_members",
        "record_eligibility_facts",
        "record_direct_paths",
        "semantic_edges",
        "dependencies",
        "diagnostics",
        "policy_facts",
    ];
    for family in families {
        let mut stmt = db
            .prepare(&format!("SELECT rowid,* FROM {family} LIMIT 1"))
            .unwrap();
        let columns: Vec<String> = stmt.column_names()[1..]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let n = stmt.column_count();
        let original: Vec<Value> = stmt
            .query_row([], |r| (0..n).map(|i| r.get(i)).collect())
            .unwrap_or_else(|e| panic!("fixture must populate {family}: {e}"));
        drop(stmt);
        let Value::Integer(rid) = original[0] else {
            panic!("rowid")
        };
        let text_column = (1..original.len())
            .find(|i| {
                matches!(&original[*i], Value::Text(_))
                    && !(family == "policy_facts" && columns[*i - 1] == "family")
            })
            .unwrap();
        let all_columns = std::iter::once("rowid".to_owned())
            .chain(columns.clone())
            .collect::<Vec<_>>()
            .join(",");
        let placeholders = (1..=original.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let insert = format!("INSERT INTO {family}({all_columns}) VALUES({placeholders})");
        for mutation in ["changed", "missing", "extra"] {
            match mutation {
                "changed" => {
                    db.execute(
                        &format!(
                            "UPDATE {family} SET {}=CAST(?1 AS TEXT) WHERE rowid=?2",
                            columns[text_column - 1]
                        ),
                        rusqlite::params!["audit changed field", rid],
                    )
                    .unwrap();
                }
                "missing" => {
                    db.execute(&format!("DELETE FROM {family} WHERE rowid=?1"), [rid])
                        .unwrap();
                }
                "extra" => {
                    let mut extra = original.clone();
                    for (i, value) in extra.iter_mut().enumerate() {
                        // Keep the closed family discriminator valid so the
                        // explicit audit, rather than SQLite's CHECK, detects
                        // the extra policy row.
                        if family == "policy_facts" && i == 1 {
                            continue;
                        }
                        match value {
                            Value::Text(s) => s.push_str(" audit extra"),
                            Value::Integer(n)
                                if i == 0
                                    || columns.get(i - 1).is_some_and(|s| {
                                        matches!(
                                            s.as_str(),
                                            "doc_row"
                                                | "graph_row"
                                                | "link_row"
                                                | "diagnostic_row"
                                                | "dependency_row"
                                                | "retained_ordinal"
                                        )
                                    }) =>
                            {
                                *n += 1_000_000
                            }
                            _ => {}
                        }
                    }
                    db.execute(&insert, params_from_iter(extra.iter())).unwrap();
                }
                _ => unreachable!(),
            }
            let error = fixture
                .catalog
                .check_normalized(&fixture.writer)
                .err()
                .unwrap_or_else(|| panic!("accepted {mutation} {family}"));
            assert_eq!(
                error.code,
                ErrorCode::IndexCorrupt,
                "{mutation} {family}: {error:?}"
            );
            assert_eq!(error.details["complete"], false);
            if mutation == "extra" {
                db.execute(
                    &format!("DELETE FROM {family} WHERE rowid=?1"),
                    [rid + 1_000_000],
                )
                .unwrap();
            } else {
                db.execute(&format!("DELETE FROM {family} WHERE rowid=?1"), [rid])
                    .unwrap();
                db.execute(&insert, params_from_iter(original.iter()))
                    .unwrap();
            }
            fixture.check();
        }
    }
}

#[test]
fn full_check_resource_refusals_are_incomplete_and_retry_uses_fresh_scratch() {
    let fixture = Fixture::new();
    let limits = [
        CheckLimits {
            max_rows: 1,
            ..CheckLimits::default()
        },
        CheckLimits {
            max_postings: 1,
            ..CheckLimits::default()
        },
        CheckLimits {
            max_vm_steps: 1000,
            ..CheckLimits::default()
        },
        CheckLimits {
            max_source_bytes: 4096,
            ..CheckLimits::default()
        },
        CheckLimits {
            max_scratch_bytes: 4096,
            ..CheckLimits::default()
        },
        CheckLimits {
            max_history_steps: 1,
            ..CheckLimits::default()
        },
        CheckLimits {
            max_diagnostic_bytes: 1,
            ..CheckLimits::default()
        },
        CheckLimits {
            max_elapsed: Duration::from_nanos(1),
            ..CheckLimits::default()
        },
    ];
    let before = Blake3Hash::digest(fs::read(&fixture.database).unwrap());
    for limits in limits {
        let error = fixture
            .catalog
            .check_normalized_with_limits(&fixture.writer, limits)
            .err()
            .expect("bounded refusal");
        assert_eq!(error.code, ErrorCode::BudgetExceeded, "{error:?}");
        assert_eq!(error.details["complete"], false);
        fixture.check();
    }
    assert_eq!(
        before,
        Blake3Hash::digest(fs::read(&fixture.database).unwrap())
    );
}

#[test]
fn full_check_duplicate_multiplicity_matters_but_surrogate_rowids_do_not() {
    let fixture = Fixture::new();
    fixture.check();
    let db = Connection::open(&fixture.database).unwrap();
    for (family, columns) in [
        (
            "links",
            "from_path,byte_start,target_id,target_path,resolution",
        ),
        ("diagnostics", "path,record_id,code,details_json"),
    ] {
        let rid: i64 = db
            .query_row(&format!("SELECT rowid FROM {family} LIMIT 1"), [], |r| {
                r.get(0)
            })
            .unwrap();
        db.execute(&format!("INSERT INTO {family}(rowid,{columns}) SELECT rowid+1000000,{columns} FROM {family} WHERE rowid=?1"),[rid]).unwrap();
        let error = fixture
            .catalog
            .check_normalized(&fixture.writer)
            .err()
            .expect("extra identical row must fail");
        assert_eq!(error.code, ErrorCode::IndexCorrupt);
        assert!(error.message.contains(family), "{error:?}");
        db.execute(&format!("DELETE FROM {family} WHERE rowid=?1"), [rid])
            .unwrap();
        // Same logical multiset, now with a different physical surrogate.
        fixture.check();
    }
}

#[test]
fn full_check_preserves_current_and_held_predecessor_files_and_readers() {
    use crate::catalog::query_types::{QueryCatalog, QueryReadLimits};
    fn files(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
        fn walk(at: &Path, result: &mut BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>) {
            for entry in fs::read_dir(at).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    walk(&path, result)
                } else if !path.to_string_lossy().ends_with("-shm") {
                    result.insert(
                        path.clone(),
                        (fs::read(path).unwrap(), meta.modified().unwrap()),
                    );
                }
            }
        }
        let mut result = BTreeMap::new();
        walk(root, &mut result);
        result
    }
    let fixture = Fixture::new();
    let held = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let path = VaultRelativePath::new("linked.md").unwrap();
    let old = held.document(&path).unwrap().unwrap();
    let old_snapshot = held.snapshot().clone();
    let rebuilt = fixture.catalog.rebuild_normalized(&fixture.writer).unwrap();
    assert!(rebuilt.retirement_deferred);
    let before = files(fixture.catalog.fs().root().path());
    let report = fixture.check();
    assert_eq!(report.snapshot, rebuilt.report.snapshot);
    assert_ne!(report.snapshot, old_snapshot);
    assert_eq!(held.snapshot(), &old_snapshot);
    assert_eq!(held.document(&path).unwrap().unwrap(), old);
    assert_eq!(
        files(fixture.catalog.fs().root().path()),
        before,
        "all canonical/control/index/WAL bytes and mtimes unchanged; SHM read marks excluded"
    );
}

//! Owner reuse checks use real normalized SQL and disposable canonical pages.
use super::*;
#[path = "../../tests/fixtures/p17/common.rs"]
mod common;
use common::Fixture;

fn fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.page("owner", "Signalneedle original owner text.\n");
    fixture.app.index_rebuild_normalized().unwrap();
    fixture
}
fn descriptor(reader: &QuerySnapshot) -> UnitDescriptor {
    let path = VaultRelativePath::new("knowledge/pages/owner.md").unwrap();
    let document = reader.document(&path).unwrap().unwrap();
    UnitDescriptor {
        policy: super::super::unit_inventory_types::RenderPolicyId(Blake3Hash::digest(b"policy")),
        owner: path,
        target: render::TargetKind::Document,
        target_id: document.record_id.or(document.owner_revision),
        source_hash: document.hash,
        source_span: None,
        unit_id: Blake3Hash::digest(b"unit"),
        input_hash: Blake3Hash::digest(b"input"),
    }
}
#[test]
fn repeated_owner_reuses_one_charged_projection_and_checks_each_descriptor() {
    let fixture = fixture();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let unit = descriptor(&reader);
    for excluded in [false, true] {
        let filter = SearchFilters {
            path_prefix: excluded.then(|| "other/".into()),
            ..Default::default()
        };
        let before = reader.usage();
        let mut resolver = OwnerResolver::new(&reader, &filter, true);
        assert_eq!(resolver.allowed(&unit).unwrap(), !excluded);
        let charged = reader.usage();
        assert_eq!(charged.rows, before.rows + 1);
        assert!(charged.bytes > before.bytes);
        for _ in 0..100 {
            assert_eq!(resolver.allowed(&unit).unwrap(), !excluded);
        }
        assert_eq!(reader.usage(), charged);
        for target in [0, 1, 2] {
            let mut malformed = unit.clone();
            if target == 1 {
                malformed.target_id = Some(RecordId::new("page_other").unwrap());
            } else if target == 2 {
                malformed.target_id = None;
            } else {
                malformed.source_hash = Blake3Hash::digest(b"wrong hash");
            }
            assert_eq!(
                resolver.allowed(&malformed).unwrap_err().code,
                ErrorCode::FreshnessConflict
            );
            assert_eq!(
                reader.usage(),
                charged,
                "cache never rereads or skips a false binding"
            );
        }
        assert_eq!(resolver.owners.len(), 1);
    }
}
#[test]
fn owner_resolvers_do_not_reuse_filters_context_policy_or_new_publication() {
    let fixture = fixture();
    // Draft Pages may be searched, but do not supply current context authority.
    let path = fixture.fs.root().path().join("knowledge/pages/owner.md");
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, original.replace("reviewed", "draft")).unwrap();
    fixture.app.index_rebuild_normalized().unwrap();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let unit = descriptor(&reader);
    let filter = SearchFilters::default();
    assert!(
        OwnerResolver::new(&reader, &filter, false)
            .allowed(&unit)
            .unwrap()
    );
    assert!(
        !OwnerResolver::new(&reader, &filter, true)
            .allowed(&unit)
            .unwrap()
    );
    let excluded = SearchFilters {
        path_prefix: Some("other/".into()),
        ..Default::default()
    };
    assert!(
        !OwnerResolver::new(&reader, &excluded, false)
            .allowed(&unit)
            .unwrap()
    );
    let old_snapshot = reader.snapshot().clone();
    drop(reader);
    std::fs::write(&path, original.replace("original", "refreshed")).unwrap();
    fixture.app.index_rebuild_normalized().unwrap();
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_ne!(reader.snapshot(), &old_snapshot);
    let mut resolver = OwnerResolver::new(&reader, &filter, true);
    assert_eq!(
        resolver.allowed(&unit).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
    let current = descriptor(&reader);
    assert!(resolver.allowed(&current).unwrap());
}
#[test]
fn owner_projection_preserves_row_byte_retention_and_cache_hit_deadline_limits() {
    let fixture = fixture();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let unit = descriptor(&reader);
    drop(reader);
    let filter = SearchFilters::default();
    // First owner consumes the sole row; replay remains possible without resetting it.
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits {
            max_rows: 1,
            ..Default::default()
        })
        .unwrap();
    let mut resolver = OwnerResolver::new(&reader, &filter, true);
    assert!(resolver.allowed(&unit).unwrap());
    assert!(resolver.allowed(&unit).unwrap());
    assert_eq!(reader.usage().rows, 1);
    // Direct second charged projection exercises the cumulative reservation boundary.
    assert_eq!(
        resolve_owner(&reader, &unit.owner, &filter, true, usize::MAX)
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    drop(resolver);
    drop(reader);
    for limits in [
        QueryReadLimits {
            max_row_bytes: 1,
            ..Default::default()
        },
        QueryReadLimits {
            max_bytes: 1,
            ..Default::default()
        },
    ] {
        let reader = catalog.cached_query_snapshot(limits).unwrap();
        let mut resolver = OwnerResolver::new(&reader, &filter, true);
        assert_eq!(
            resolver.allowed(&unit).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        assert!(resolver.owners.is_empty());
    }
    let measurement = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    resolve_owner(&measurement, &unit.owner, &filter, true, usize::MAX).unwrap();
    let bytes = measurement.usage().bytes;
    drop(measurement);
    for admitted in [false, true] {
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits {
                max_row_bytes: bytes - usize::from(!admitted),
                max_bytes: bytes - usize::from(!admitted),
                ..Default::default()
            })
            .unwrap();
        let mut resolver = OwnerResolver::new(&reader, &filter, true);
        if admitted {
            assert!(resolver.allowed(&unit).unwrap());
            assert!(resolver.allowed(&unit).unwrap());
            assert_eq!(reader.usage().bytes, bytes);
        } else {
            assert_eq!(
                resolver.allowed(&unit).unwrap_err().code,
                ErrorCode::BudgetExceeded
            );
            assert!(resolver.owners.is_empty());
        }
    }
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut resolver = OwnerResolver::new(&reader, &filter, true);
    resolver.retained_bytes = QueryReadLimits::default().max_bytes;
    assert_eq!(
        resolver.allowed(&unit).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert!(
        resolver.owners.is_empty(),
        "retention refuses before cloning fields/path"
    );
    drop(resolver);
    drop(reader);
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits {
            max_elapsed_ms: 100,
            ..Default::default()
        })
        .unwrap();
    let mut resolver = OwnerResolver::new(&reader, &filter, true);
    assert!(resolver.allowed(&unit).unwrap());
    let usage = reader.usage();
    std::thread::sleep(Duration::from_millis(110));
    assert_eq!(
        resolver.allowed(&unit).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(
        reader.usage(),
        usage,
        "cache hit checks elapsed work without extra SQL"
    );
}

#[test]
fn malformed_sql_owner_identities_refuse_first_projection_and_cached_reuse() {
    for (sql, excluded) in [
        (
            "UPDATE documents SET file_hash='not-a-hash' WHERE path='knowledge/pages/owner.md'",
            false,
        ),
        (
            "UPDATE documents SET file_hash='not-a-hash' WHERE path='knowledge/pages/owner.md'",
            true,
        ),
        (
            "UPDATE documents SET record_id='not a record id' WHERE path='knowledge/pages/owner.md'",
            false,
        ),
        (
            "UPDATE documents SET record_id='not a record id' WHERE path='knowledge/pages/owner.md'",
            true,
        ),
    ] {
        let fixture = fixture();
        let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let unit = descriptor(&reader);
        let path = fixture.fs.root().path().join(format!(
            ".wiki/cache/catalogs/{}.sqlite",
            reader.publication_id().unwrap()
        ));
        drop(reader);
        // Corrupt only the disposable normalized cache, never canonical bytes.
        let writer = fixture.writer();
        let database = rusqlite::Connection::open(path).unwrap();
        crate::catalog::selector::configure_wal(&database).unwrap();
        database.execute(sql, []).unwrap();
        drop(database);
        drop(writer);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let filter = SearchFilters {
            path_prefix: excluded.then(|| "other/".into()),
            ..Default::default()
        };
        let mut resolver = OwnerResolver::new(&reader, &filter, true);
        assert_eq!(
            resolver.allowed(&unit).unwrap_err().code,
            ErrorCode::FreshnessConflict
        );
        assert_eq!(reader.usage().rows, 1);
        let usage = reader.usage();
        assert_eq!(
            resolver.allowed(&unit).unwrap_err().code,
            ErrorCode::FreshnessConflict
        );
        assert_eq!(
            reader.usage(),
            usage,
            "raw cached malformed identity never bypasses comparison"
        );
        assert_eq!(resolver.owners.len(), 1);
    }
}

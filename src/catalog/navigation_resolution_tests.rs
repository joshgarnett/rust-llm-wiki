use super::{
    Catalog,
    file_types::{BuildIdentity, CatalogSelection},
    link_facts::{self, MatchKey, MatchKeyKind},
    navigation_resolution::{
        self as navigation, NavigationResolution, RegistryCandidate, RegistryProbe,
    },
    normalized_build::{BuildLimits, NormalizedBuilder},
    query_types::QueryReadLimits,
    scan, selector,
};
use crate::{
    domain::{ErrorCode, RecordId, RecordKind, VaultRelativePath},
    records::{LinkResolution, RegistryEntry, links::IndexedRegistry},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::{Connection, params};
use std::{fs, time::Duration};

fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn path(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn key(kind: MatchKeyKind, value: &str) -> MatchKey {
    MatchKey {
        kind,
        value: value.into(),
    }
}
fn entry(name: &str, location: &str, kind: RecordKind, aliases: &[&str]) -> RegistryEntry {
    RegistryEntry {
        id: id(name),
        path: path(location),
        kind,
        aliases: aliases.iter().map(|s| (*s).into()).collect(),
    }
}
fn memory_probe(entries: &[RegistryEntry], key: &MatchKey) -> RegistryProbe {
    let mut result = RegistryProbe::Zero;
    for entry in entries {
        if link_facts::registry_keys(entry).unwrap().contains(key) {
            result.insert(RegistryCandidate::from(entry)).unwrap();
        }
    }
    result
}

#[test]
fn bounded_resolution_matches_public_resolver_on_ordered_small_registries() {
    let choices = [
        entry(
            "page_alpha",
            "alpha.md",
            RecordKind::Page,
            &["Éclair", "alias", "alias"],
        ),
        entry(
            "page_beta",
            "folder/alpha.md",
            RecordKind::Entity,
            &["alias", "folder/alpha"],
        ),
        entry("page_alpha", "other.md", RecordKind::Page, &["Éclair"]),
        entry("page_gamma", "folder/alpha", RecordKind::Page, &["alpha"]),
    ];
    // Normalized registry keys identify adopted entries by (ID,path). Identical
    // duplicate entries cannot be stored; duplicate IDs at different paths are
    // included here to exercise the resolver's additional ID ambiguity check.
    let mut catalogs = vec![vec![]];
    for length in 1..=3 {
        for encoded in 0..4usize.pow(length) {
            let mut n = encoded;
            let mut entries = vec![];
            for _ in 0..length {
                entries.push(choices[n % 4].clone());
                n /= 4;
            }
            if entries
                .iter()
                .enumerate()
                .any(|(i, e)| entries[..i].contains(e))
            {
                continue;
            }
            catalogs.push(entries);
        }
    }
    for entries in catalogs {
        let legacy = IndexedRegistry::new(entries.clone());
        let mut probe = |key: &MatchKey| Ok(memory_probe(&entries, key));
        for destination in [
            "alpha",
            "alpha.md",
            "folder/alpha",
            "folder/alpha.md",
            "other",
            "alias",
            "Éclair#section",
            "[[folder/alpha#^block|Label]]",
            "ALPHA",
            "./alpha",
            "absent",
            "../alpha",
            "",
            "https://host/alpha#part",
            "//host/a",
        ] {
            assert_eq!(
                navigation::resolve_untyped(destination, &mut probe).unwrap(),
                NavigationResolution::from(&legacy.resolve_untyped(destination)),
                "{entries:?} {destination:?}"
            );
        }
        for target in [id("page_alpha"), id("page_beta"), id("page_missing")] {
            for kind in [RecordKind::Page, RecordKind::Entity] {
                for companion in [
                    None,
                    Some("alpha"),
                    Some("other#part"),
                    Some("[[folder/alpha#^block|Label]]"),
                    Some("https://host/a#part"),
                    Some("../invalid#part"),
                    Some("absent"),
                    Some(""),
                ] {
                    assert_eq!(
                        navigation::resolve_typed(&target, kind, companion, &mut probe).unwrap(),
                        NavigationResolution::from(&legacy.resolve_typed(&target, kind, companion)),
                        "{entries:?} {target} {companion:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn precedence_is_lazy_and_alias_union_deduplicates_entry_witnesses() {
    let entries = vec![entry(
        "page_target",
        "folder/alpha.md",
        RecordKind::Page,
        &["folder/alpha", "alpha"],
    )];
    let mut visited = vec![];
    let mut probe = |key: &MatchKey| {
        visited.push(key.clone());
        Ok(memory_probe(&entries, key))
    };
    assert!(matches!(
        navigation::resolve_untyped("folder/alpha#part", &mut probe).unwrap(),
        NavigationResolution::Resolved { .. }
    ));
    assert_eq!(
        visited,
        vec![
            key(MatchKeyKind::Path, "folder/alpha"),
            key(MatchKeyKind::Path, "folder/alpha.md"),
            key(MatchKeyKind::Id, "page_target")
        ]
    );
    visited.clear();
    let mut probe = |key: &MatchKey| {
        visited.push(key.clone());
        Ok(memory_probe(&entries, key))
    };
    assert!(matches!(
        navigation::resolve_untyped("alpha", &mut probe).unwrap(),
        NavigationResolution::Resolved { .. }
    ));
    assert_eq!(
        visited,
        vec![
            key(MatchKeyKind::Path, "alpha"),
            key(MatchKeyKind::Path, "alpha.md"),
            key(MatchKeyKind::Basename, "alpha"),
            key(MatchKeyKind::Alias, "alpha"),
            key(MatchKeyKind::Id, "page_target")
        ]
    );
    visited.clear();
    let mut probe = |key: &MatchKey| {
        visited.push(key.clone());
        Ok(memory_probe(&entries, key))
    };
    assert_eq!(
        navigation::resolve_typed(
            &id("page_target"),
            RecordKind::Evidence,
            Some("alpha"),
            &mut probe
        )
        .unwrap(),
        NavigationResolution::WrongKind {
            actual: RecordKind::Page
        }
    );
    assert_eq!(visited, vec![key(MatchKeyKind::Id, "page_target")]);
}

#[test]
fn append_overlay_saturates_without_changing_public_complete_candidates() {
    let a = entry("page_a", "one/revision.md", RecordKind::Page, &[]);
    let b = entry("page_b", "two/revision.md", RecordKind::Page, &[]);
    let c = entry("page_c", "three/revision.md", RecordKind::Page, &[]);
    let mut saturated = RegistryProbe::One(RegistryCandidate::from(&a));
    saturated.insert(RegistryCandidate::from(&a)).unwrap();
    assert!(matches!(saturated, RegistryProbe::One(_)));
    saturated.insert(RegistryCandidate::from(&b)).unwrap();
    let before = saturated.clone();
    saturated.insert(RegistryCandidate::from(&c)).unwrap();
    assert_eq!(before, saturated);
    let legacy = IndexedRegistry::new(vec![a.clone(), b.clone(), c.clone()]);
    assert_eq!(
        legacy.resolve_untyped("revision"),
        LinkResolution::Ambiguous {
            ids: vec![a.id.clone(), b.id.clone(), c.id.clone()]
        }
    );
    let mut visited = vec![];
    let mut probe = |key: &MatchKey| {
        visited.push(key.kind);
        Ok(memory_probe(&[a.clone(), b.clone(), c.clone()], key))
    };
    assert_eq!(
        navigation::resolve_untyped("revision", &mut probe).unwrap(),
        NavigationResolution::Ambiguous
    );
    assert_eq!(
        visited,
        vec![
            MatchKeyKind::Path,
            MatchKeyKind::Path,
            MatchKeyKind::Basename
        ]
    );
    let fact = link_facts::untyped_navigation_fact(
        &path("owner.md"),
        0,
        "revision",
        &NavigationResolution::Ambiguous,
    )
    .unwrap();
    assert!(fact.keys.iter().all(|key| key.kind != MatchKeyKind::Id));
    let mut forged = RegistryCandidate::from(&a);
    forged.kind = RecordKind::Evidence;
    assert_eq!(
        saturated.insert(forged).unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
}

#[test]
fn unique_candidate_without_consistent_id_witness_refuses() {
    let candidate = RegistryCandidate::from(&entry("page_a", "a.md", RecordKind::Page, &[]));
    let mut probe = |key: &MatchKey| {
        Ok(if key.kind == MatchKeyKind::Path {
            RegistryProbe::One(candidate.clone())
        } else {
            RegistryProbe::Zero
        })
    };
    assert_eq!(
        navigation::resolve_untyped("a.md", &mut probe)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
}

fn fixture() -> (tempfile::TempDir, Catalog, Connection) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_id: vault_navigation\nwiki_kind: vault\ntitle: Navigation\n---\n").unwrap();
    fs::write(temp.path().join("revision.md"), b"---\nwiki_schema: '1'\nwiki_id: page_exact\nwiki_kind: page\ntitle: Exact\nwiki_status: reviewed\naliases: [ExactAlias]\n---\nBody [[revision]].\n").unwrap();
    let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let catalog = Catalog::new(fs_handle.clone(), id("vault_navigation"));
    let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
    let identity = BuildIdentity {
        selection: CatalogSelection::new(id("vault_navigation"), 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(&fs_handle, &writer, &identity.selection).unwrap();
    let mut builder =
        NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default()).unwrap();
    let input = scan::scan_input(&fs_handle, &id("vault_navigation")).unwrap();
    let projection =
        scan::project_normalized_with_sink(&fs_handle, &input, false, &mut builder).unwrap();
    let completed = builder.finish_normalized(&projection).unwrap();
    selector::publish(
        &fs_handle,
        &writer,
        &completed.identity.selection,
        Duration::ZERO,
    )
    .unwrap();
    let database = Connection::open(completed.path).unwrap();
    (temp, catalog, database)
}

#[test]
fn selected_sql_probe_stays_bounded_above_full_candidate_ceiling_and_exact_shadows_it() {
    let (_temp, catalog, database) = fixture();
    let measure = || {
        let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        assert!(matches!(
            navigation::resolve_untyped("revision", &mut |key| reader.registry_probe_for_key(key))
                .unwrap(),
            NavigationResolution::Resolved { .. }
        ));
        reader.usage()
    };
    let baseline = measure();
    // Scalar candidate fixtures isolate SQL admission from full builder cost.
    // Root's connected refresh regression creates >4096 actual canonical notes.
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    for n in 0..4100 {
        let name = format!("page_noise_{n:04}");
        let location = format!("noise/{n:04}/revision.md");
        database.execute("INSERT INTO records VALUES(?1,'page',?2,'unused',NULL,'current',NULL,NULL,0,'unread candidate payload')", params![name, location]).unwrap();
        database
            .execute(
                "INSERT INTO registry_match_keys VALUES('basename','revision',?1,?2)",
                params![name, location],
            )
            .unwrap();
    }
    database.execute_batch("COMMIT").unwrap();
    assert_eq!(measure(), baseline);
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    assert!(matches!(
        reader
            .registry_probe_for_key(&key(MatchKeyKind::Basename, "revision"))
            .unwrap(),
        RegistryProbe::Many(_)
    ));
    assert_eq!(reader.usage().rows, 3); // header + exactly two witnesses
    let limits = QueryReadLimits {
        max_rows: 2,
        ..QueryReadLimits::default()
    };
    let reader = catalog.query_snapshot(limits).unwrap();
    assert_eq!(
        reader
            .registry_probe_for_key(&key(MatchKeyKind::Basename, "revision"))
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
}

#[test]
fn selected_witness_binding_alias_membership_layout_and_bytes_are_checked() {
    let (_temp, catalog, database) = fixture();
    let probe = |value: &str| {
        let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        reader.registry_probe_for_key(&key(MatchKeyKind::Alias, value))
    };
    assert!(matches!(
        probe("ExactAlias").unwrap(),
        RegistryProbe::One(_)
    ));
    database.execute("INSERT INTO registry_match_keys VALUES('alias','ForgedAlias','page_exact','revision.md')", []).unwrap();
    assert_eq!(
        probe("ForgedAlias").unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
    database
        .execute(
            "UPDATE records SET path='wrong.md' WHERE id='page_exact'",
            [],
        )
        .unwrap();
    assert_eq!(
        probe("ExactAlias").unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
    database
        .execute(
            "UPDATE records SET path='revision.md' WHERE id='page_exact'",
            [],
        )
        .unwrap();
    database
        .execute(
            "UPDATE records SET kind='not_a_kind' WHERE id='page_exact'",
            [],
        )
        .unwrap();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    assert_eq!(
        reader
            .registry_probe_for_key(&key(MatchKeyKind::Path, "revision.md"))
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    drop(reader);
    database
        .execute("UPDATE records SET kind='page' WHERE id='page_exact'", [])
        .unwrap();
    let reader = catalog
        .query_snapshot(QueryReadLimits {
            max_row_bytes: 4,
            ..QueryReadLimits::default()
        })
        .unwrap();
    assert_eq!(
        reader
            .registry_probe_for_key(&key(MatchKeyKind::Path, "revision.md"))
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    database
        .execute("UPDATE catalog_meta SET proof_layout_version=1", [])
        .unwrap();
    assert_eq!(
        probe("ExactAlias").unwrap_err().code,
        ErrorCode::OfflineUnavailable
    );
}

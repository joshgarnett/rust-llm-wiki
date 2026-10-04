use super::*;
use clap::Parser;
use std::collections::BTreeMap;

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_search_adapter\nwiki_kind: vault\ntitle: Search adapter\n---\n").unwrap();
    temp
}

fn files(root: &Path) -> BTreeMap<std::path::PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut result = BTreeMap::new();
    let mut dirs = vec![root.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                result.insert(
                    path.clone(),
                    (
                        std::fs::read(&path).unwrap(),
                        path.metadata().unwrap().modified().unwrap(),
                    ),
                );
            }
        }
    }
    result
}

#[test]
fn verification_flag_is_search_only_and_compatible_with_no_sync() {
    let args = Arguments::try_parse_from([
        "lwiki",
        "search",
        "needle",
        "--verify-selected",
        "--no-sync",
    ])
    .unwrap();
    let Command::Search(command) = args.command else {
        panic!("search required")
    };
    assert!(command.verify_selected && command.search.no_sync);
    assert!(
        Arguments::try_parse_from(["lwiki", "context", "needle", "--verify-selected"]).is_err()
    );
}

#[test]
fn verified_preview_never_resolves_corrupt_cache_and_preserves_files() {
    let fixture = fixture();
    let cache = fixture.path().join(".wiki/cache");
    std::fs::create_dir_all(&cache).unwrap();
    for name in [
        "catalog-current.json",
        "catalog-v2-active.json",
        "catalog.sqlite",
        "catalog.sqlite-wal",
        "catalog.sqlite-shm",
    ] {
        std::fs::write(cache.join(name), b"invalid index: must not open").unwrap();
    }
    let before = files(fixture.path());
    let args = Arguments::try_parse_from([
        "lwiki",
        "--wiki",
        fixture.path().to_str().unwrap(),
        "--offline",
        "--dry-run",
        "search",
        "needle",
        "--verify-selected",
        "--no-sync",
    ])
    .unwrap();
    let (output, code) = execute(&args);
    assert_eq!(code, 0, "{output:?}");
    assert_eq!(output.data["verification_performed"], false);
    assert!(output.data["hits"].is_null());
    assert!(output.meta.freshness.is_none() && output.meta.verified_at.is_none());
    assert!(!output.meta.network_used);
    assert_eq!(before, files(fixture.path()));
}

#[test]
fn verification_refuses_legacy_and_unsupported_modes_without_remote_setup() {
    let fixture = fixture();
    for mode in ["lexical", "literal", "semantic", "hybrid"] {
        let args = Arguments::try_parse_from([
            "lwiki",
            "--wiki",
            fixture.path().to_str().unwrap(),
            "--offline",
            "search",
            "needle",
            "--verify-selected",
            "--mode",
            mode,
        ])
        .unwrap();
        let (output, _) = execute(&args);
        assert!(!output.ok && output.data.is_null());
        assert_eq!(output.error.unwrap().code, "CAPABILITY_UNAVAILABLE");
        assert!(!output.meta.network_used);
    }
}

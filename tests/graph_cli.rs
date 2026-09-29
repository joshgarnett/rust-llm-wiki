#[path = "../test_support/paths.rs"]
mod test_paths;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
const FORWARD: &str = "assertion_00000000-0000-7000-8000-00000000000a";
const NORTH: &str = "entity_00000000-0000-7000-8000-000000000003";
const SOUTH: &str = "entity_00000000-0000-7000-8000-000000000004";
const PAGE: &str = "page_00000000-0000-7000-8000-000000000011";
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
fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    copy(
        &test_paths::fixture(env!("CARGO_MANIFEST_DIR"), "tests/fixtures/bootstrap/vault"),
        temp.path(),
    );
    temp
}
fn invoke(root: &Path, args: &[&str]) -> (i32, Value) {
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(root)
        .args(["--json", "--offline"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.stderr.is_empty());
    assert!(!output.stdout.contains(&0x1b));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["meta"]["network_used"], false);
    let schema: Value = serde_json::from_str(include_str!("../schemas/output-v1.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&envelope)
        .unwrap();
    (output.status.code().unwrap(), envelope)
}
fn ok(root: &Path, args: &[&str]) -> Value {
    let (exit, value) = invoke(root, args);
    assert_eq!(exit, 0, "{args:?}: {value}");
    value
}
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            out.insert(
                path.strip_prefix(root).unwrap().into(),
                (
                    if meta.is_file() {
                        fs::read(&path).unwrap()
                    } else {
                        vec![]
                    },
                    meta.modified().unwrap(),
                ),
            );
            if meta.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
#[test]
fn graph_cli_preserves_directed_qualifiers_disputes_and_snapshot_labels() {
    let temp = fixture();
    let root = temp.path();
    let query = ok(
        root,
        &["graph", "query", FORWARD, "--strategy", "relationship"],
    );
    assert_eq!(query["meta"]["freshness"], "verified_snapshot");
    let assertion = query["data"]["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["record_ref"]["record_id"] == FORWARD)
        .unwrap();
    assert_eq!(assertion["subject"]["record_id"], NORTH);
    assert_eq!(assertion["object"]["record_ref"]["record_id"], SOUTH);
    assert_eq!(assertion["predicate"], "uses");
    assert_eq!(assertion["qualifiers"]["negated"], false);
    assert_eq!(assertion["disputed"], true);
    assert!(!assertion["support"].as_array().unwrap().is_empty());
    assert!(!assertion["contradictions"].as_array().unwrap().is_empty());
    assert!(assertion["support"][0]["citation"].is_object());
    let snapshot = ok(
        root,
        &[
            "graph",
            "query",
            FORWARD,
            "--strategy",
            "relationship",
            "--no-sync",
        ],
    );
    assert_eq!(snapshot["meta"]["freshness"], "index_snapshot");
    for assertion in snapshot["data"]["assertions"].as_array().unwrap() {
        for leg in ["support", "contradictions"] {
            for evidence in assertion[leg].as_array().unwrap() {
                assert!(evidence["citation"].is_null());
            }
        }
    }
    let homonyms = ok(
        root,
        &["graph", "query", "Alex Kim", "--strategy", "entity"],
    );
    let alex: Vec<_> = homonyms["data"]["seeds"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["title"] == "Alex Kim")
        .collect();
    assert_eq!(alex.len(), 2);
    assert_ne!(alex[0]["record_ref"], alex[1]["record_ref"]);
    let page = ok(root, &["graph", "neighbors", PAGE]);
    assert!(!page["data"]["navigation"].as_array().unwrap().is_empty());
    for edge in page["data"]["navigation"].as_array().unwrap() {
        assert!(edge.get("predicate").is_none());
        assert!(["page_link", "provenance"].contains(&edge["reason"].as_str().unwrap()));
    }
}
#[test]
fn graph_cli_dry_run_is_pure_and_invalid_limits_are_usage() {
    let temp = fixture();
    let root = temp.path();
    let before = tree(root);
    for args in [
        vec!["--dry-run", "graph", "query", "North Lab"],
        vec!["--dry-run", "graph", "neighbors", NORTH],
    ] {
        let plan = ok(root, &args);
        assert_eq!(plan["data"]["cache_state_unknown"], true);
        assert!(plan["data"]["results"].is_null());
        assert!(plan["meta"]["freshness"].is_null());
        assert_eq!(before, tree(root));
    }
    for args in [
        vec!["--dry-run", "graph", "query", "uses", "--depth", "3"],
        vec!["graph", "query", "uses", "--seed", "unknown"],
        vec!["graph", "query", "uses", "--seeds", "13"],
        vec!["graph", "query", "uses", "--limit", "51"],
    ] {
        assert_eq!(invoke(root, &args).0, 2);
        assert_eq!(before, tree(root));
    }
    assert_eq!(invoke(root, &["graph", "neighbors", "missing-ID"]).0, 3);
}
#[test]
fn graph_cli_rebuild_preserves_canonical_rows_and_cursor_scope() {
    let temp = fixture();
    let root = temp.path();
    let first = ok(
        root,
        &[
            "graph",
            "query",
            "uses",
            "--strategy",
            "relationship",
            "--limit",
            "1",
        ],
    );
    assert_eq!(first["data"]["assertions"].as_array().unwrap().len(), 1);
    if let Some(cursor) = first["data"]["next_cursor"].as_str() {
        ok(
            root,
            &[
                "graph",
                "query",
                "uses",
                "--strategy",
                "relationship",
                "--limit",
                "1",
                "--cursor",
                cursor,
            ],
        );
        assert_eq!(
            invoke(
                root,
                &[
                    "graph",
                    "query",
                    "works_for",
                    "--strategy",
                    "relationship",
                    "--limit",
                    "1",
                    "--cursor",
                    cursor
                ]
            )
            .0,
            4
        );
        ok(root, &["index", "rebuild"]);
        assert_eq!(
            invoke(
                root,
                &[
                    "graph",
                    "query",
                    "uses",
                    "--strategy",
                    "relationship",
                    "--limit",
                    "1",
                    "--cursor",
                    cursor
                ]
            )
            .0,
            4
        );
    } else {
        panic!("multiple explicit uses assertions require a continuation");
    }
    let rebuilt = ok(
        root,
        &[
            "graph",
            "query",
            "uses",
            "--strategy",
            "relationship",
            "--limit",
            "1",
        ],
    );
    assert_eq!(first["data"]["assertions"], rebuilt["data"]["assertions"]);
}

#[test]
fn relationship_seed_paths_connect_through_either_endpoint() {
    let f = fixture();
    let result = ok(
        f.path(),
        &[
            "graph",
            "query",
            FORWARD,
            "--strategy",
            "relationship",
            "--depth",
            "2",
            "--limit",
            "50",
        ],
    );
    let edges = result["data"]["assertions"].as_array().unwrap();
    assert!(edges.len() > 1);
    assert!(
        edges
            .iter()
            .any(|edge| !edge["direct_seed"].as_bool().unwrap()
                && edge["path"][0]["traversal"] == "incoming")
    );
    let endpoint = |step: &Value, start: bool| -> Value {
        let incoming = step["traversal"] == "incoming";
        if start != incoming {
            step["subject"]["record_id"].clone()
        } else if step["object"]["kind"] == "entity" {
            step["object"]["record_ref"]["record_id"].clone()
        } else {
            step["object"].clone()
        }
    };
    for edge in edges {
        let path = edge["path"].as_array().unwrap();
        assert_eq!(
            path.last().unwrap()["assertion"]["record_id"],
            edge["record_ref"]["record_id"]
        );
        for pair in path.windows(2) {
            assert_eq!(
                endpoint(&pair[0], false),
                endpoint(&pair[1], true),
                "disconnected path: {path:?}"
            );
        }
    }
    let direct = edges
        .iter()
        .find(|edge| edge["record_ref"]["record_id"] == FORWARD)
        .unwrap();
    assert_eq!(direct["path"][0]["traversal"], "outgoing");
    assert_eq!(direct["subject"]["record_id"], NORTH);
    assert_eq!(direct["object"]["record_ref"]["record_id"], SOUTH);
}

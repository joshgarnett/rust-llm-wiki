//! Normalized public-command mechanics. Native denied-write enforcement is a separate gate.
#[path = "../test_support/paths.rs"]
mod test_paths;

use lwiki::domain::Blake3Hash;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

const SOURCE: &str = "sources/source_00000000-0000-7000-8000-000000000005/source.md";
const CONTENT: &str = "sources/source_00000000-0000-7000-8000-000000000005/revisions/revision_00000000-0000-7000-8000-000000000008/content.md";

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy(&path, &target);
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
    ok(temp.path(), &["index", "rebuild", "--normalized"]);
    temp
}

fn invoke(root: &Path, arguments: &[&str]) -> (i32, Value) {
    let unrelated = tempfile::tempdir().unwrap();
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .current_dir(unrelated.path())
        .args(["--offline", "--json", "--no-user-config", "--wiki"])
        .arg(root)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.stderr.is_empty(),
        "{arguments:?}: {:?}",
        output.stderr
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["meta"]["network_used"], false);
    (output.status.code().unwrap(), result)
}

fn ok(root: &Path, arguments: &[&str]) -> Value {
    let (exit, result) = invoke(root, arguments);
    assert_eq!(exit, 0, "{arguments:?}: {result}");
    assert_eq!(result["ok"], true);
    result
}

fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, at: &Path, result: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            result.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (
                    if metadata.is_file() {
                        fs::read(&path).unwrap()
                    } else {
                        Vec::new()
                    },
                    metadata.modified().unwrap(),
                ),
            );
            if metadata.is_dir() {
                walk(root, &path, result);
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

#[test]
fn cache_loss_preserves_exact_cited_source_read_and_lexical_context() {
    let temp = fixture();
    fs::remove_dir_all(temp.path().join(".wiki/cache")).unwrap();
    let before = tree(temp.path());
    let search = ok(temp.path(), &["search", "North Lab", "--mode", "lexical"]);
    assert!(!search["data"]["hits"].as_array().unwrap().is_empty());
    assert!(search["meta"]["index_generation"].is_null());
    let read = ok(temp.path(), &["read", "--path", CONTENT]);
    let actual = fs::read_to_string(temp.path().join(CONTENT)).unwrap();
    assert_eq!(read["data"]["body"], actual);
    assert_eq!(read["data"]["source_citation"]["eligibility"], "current");
    assert_eq!(
        read["data"]["source_citation"]["citation"]["reference"]["quote_hash"],
        Blake3Hash::digest(actual.as_bytes()).as_str()
    );
    let context = ok(temp.path(), &["context", "North Lab", "--mode", "lexical"]);
    assert!(!context["data"]["passages"].as_array().unwrap().is_empty());
    assert!(context["meta"]["index_generation"].is_null());
    assert_eq!(tree(temp.path()), before);
}

#[test]
fn current_source_head_and_withdrawal_survive_cache_loss() {
    let temp = fixture();
    let source = fs::read_to_string(temp.path().join(SOURCE)).unwrap();
    fs::remove_dir_all(temp.path().join(".wiki/cache")).unwrap();
    fs::write(
        temp.path().join(SOURCE),
        source.replace(
            "wiki_current_revision: \"revision_00000000-0000-7000-8000-000000000008\"",
            "wiki_current_revision: \"revision_00000000-0000-7000-8000-000000000007\"",
        ),
    )
    .unwrap();
    let before = tree(temp.path());
    assert_eq!(
        ok(temp.path(), &["read", "--path", CONTENT])["data"]["source_citation"]["eligibility"],
        "historical"
    );
    assert_eq!(tree(temp.path()), before);
    fs::write(
        temp.path().join(SOURCE),
        source.replace("wiki_status: \"active\"", "wiki_status: \"withdrawn\""),
    )
    .unwrap();
    let before = tree(temp.path());
    assert_eq!(
        ok(temp.path(), &["read", "--path", CONTENT])["data"]["source_citation"]["eligibility"],
        "withdrawn"
    );
    assert_eq!(tree(temp.path()), before);
}

#[test]
fn missing_retained_authority_is_not_treated_as_missing_cache() {
    let temp = fixture();
    fs::remove_file(temp.path().join(".wiki/state/operations.json")).unwrap();
    let before = tree(temp.path());
    let (exit, result) = invoke(temp.path(), &["search", "North Lab", "--mode", "lexical"]);
    assert_ne!(exit, 0, "{result}");
    assert_eq!(result["error"]["code"], "RECOVERY_REQUIRED");
    assert_eq!(tree(temp.path()), before);
}

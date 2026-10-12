//! Local public-command mechanics. Native sandbox denial is a separate gate.
#[path = "../test_support/paths.rs"]
mod test_paths;

use lwiki::{
    domain::Blake3Hash,
    vault::{VaultRoot, WriterPermit},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
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

fn assert_local_workflow(root: &Path) {
    let search = ok(root, &["search", "North Lab", "--mode", "lexical"]);
    assert!(!search["data"]["hits"].as_array().unwrap().is_empty());
    assert_eq!(search["meta"]["freshness"], "verified_snapshot");
    assert!(search["meta"]["index_generation"].is_null());
    assert!(search["data"]["snapshot"]["generation"].is_null());
    let read = ok(root, &["read", "--path", CONTENT]);
    let actual = fs::read_to_string(root.join(CONTENT)).unwrap();
    assert_eq!(read["data"]["body"], actual);
    assert_eq!(read["data"]["source_citation"]["eligibility"], "current");
    assert_eq!(
        read["data"]["source_citation"]["citation"]["reference"]["quote_hash"],
        Blake3Hash::digest(actual.as_bytes()).as_str()
    );
    let context = ok(root, &["context", "North Lab", "--mode", "lexical"]);
    assert_eq!(context["meta"]["freshness"], "verified_snapshot");
    assert!(!context["data"]["passages"].as_array().unwrap().is_empty());
    assert!(
        context["data"]["text"]
            .as_str()
            .unwrap()
            .contains("North Lab")
    );
    assert!(context["meta"]["index_generation"].is_null());
    assert!(context["data"]["snapshot"]["generation"].is_null());
}

#[test]
fn ordinary_lookup_without_cache_creates_no_vault_entries() {
    let temp = fixture();
    let before = tree(temp.path());
    assert_local_workflow(temp.path());
    assert_eq!(tree(temp.path()), before);
    let absent = ok(
        temp.path(),
        &["search", "unrecordedzirconium", "--mode", "lexical"],
    );
    assert!(absent["data"]["hits"].as_array().unwrap().is_empty());
    assert_eq!(tree(temp.path()), before);
}

#[test]
fn ordinary_lookup_does_not_acquire_or_modify_held_writer() {
    let temp = fixture();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    let _writer = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let before = tree(temp.path());
    assert_local_workflow(temp.path());
    assert_eq!(tree(temp.path()), before);
}

#[test]
fn ordinary_lookup_discovers_external_edits_and_new_notes_without_sync() {
    let temp = fixture();
    ok(temp.path(), &["index", "sync"]);
    fs::write(temp.path().join("knowledge/pages/new external.md"), "---\nwiki_schema: '1'\nwiki_id: page_external_lookup\nwiki_kind: page\ntitle: New external note\nwiki_status: reviewed\n---\nexternalcobalt: keep the cabinet dry.\n").unwrap();
    let before = tree(temp.path());
    let search = ok(
        temp.path(),
        &["search", "externalcobalt", "--mode", "lexical"],
    );
    assert_eq!(
        search["data"]["hits"][0]["path"],
        "knowledge/pages/new external.md"
    );
    assert_eq!(
        ok(temp.path(), &["read", "--id", "page_external_lookup"])["data"]["body"],
        "externalcobalt: keep the cabinet dry.\n"
    );
    assert_eq!(tree(temp.path()), before);
    fs::write(temp.path().join("knowledge/pages/new external.md"), "---\nwiki_schema: '1'\nwiki_id: page_external_lookup\nwiki_kind: page\ntitle: New external note\nwiki_status: reviewed\n---\nexternalnickel: keep both keys.\n").unwrap();
    let edited = tree(temp.path());
    assert!(
        ok(
            temp.path(),
            &["search", "externalcobalt", "--mode", "lexical"]
        )["data"]["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !ok(
            temp.path(),
            &["search", "externalnickel", "--mode", "lexical"]
        )["data"]["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(tree(temp.path()), edited);
}

#[test]
fn ordinary_read_rechecks_source_head_and_withdrawal_state() {
    let temp = fixture();
    let source = fs::read_to_string(temp.path().join(SOURCE)).unwrap();
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
    let withdrawn = tree(temp.path());
    assert_eq!(
        ok(temp.path(), &["read", "--path", CONTENT])["data"]["source_citation"]["eligibility"],
        "withdrawn"
    );
    assert_eq!(tree(temp.path()), withdrawn);
}

#[test]
fn duplicate_identity_remains_a_read_error_without_writes() {
    let temp = fixture();
    fs::copy(
        temp.path().join("knowledge/entities/north_lab.md"),
        temp.path().join("duplicate.md"),
    )
    .unwrap();
    let before = tree(temp.path());
    let (exit, result) = invoke(
        temp.path(),
        &[
            "read",
            "--id",
            "entity_00000000-0000-7000-8000-000000000003",
        ],
    );
    assert_eq!(exit, 9, "{result}");
    assert_eq!(result["error"]["code"], "REFERENCE_AMBIGUOUS");
    assert_eq!(tree(temp.path()), before);
}

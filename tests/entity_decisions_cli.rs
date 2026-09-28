use lwiki::domain::Blake3Hash;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};

fn invoke(root: &Path, args: &[&str], request: Option<&Value>) -> (i32, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lwiki"))
        .arg("--wiki")
        .arg(root)
        .args(["--offline", "--json"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(request) = request {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&serde_json::to_vec(request).unwrap())
            .unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["meta"]["network_used"], false);
    (out.status.code().unwrap(), envelope)
}
fn ok(root: &Path, args: &[&str], request: Option<&Value>) -> Value {
    let (exit, envelope) = invoke(root, args, request);
    assert_eq!(exit, 0, "{args:?}: {envelope}");
    envelope["data"].clone()
}
fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: Vault.Case\nwiki_kind: vault\ntitle: Entity decisions CLI\n---\n").unwrap();
    fs::create_dir(temp.path().join("entities")).unwrap();
    for id in ["Entity.A", "Entity.B"] {
        fs::write(temp.path().join(format!("entities/{id}.md")), format!("---\nwiki_schema: \"1\"\nwiki_id: {id}\nwiki_kind: entity\ntitle: Homonym\nwiki_status: active\nwiki_entity_type: person\n---\nAuthor prose for {id}.\n")).unwrap();
    }
    temp
}
fn expected(root: &Path, id: &str, path: &str) -> Value {
    json!({"record_id":id,"hash":Blake3Hash::digest(fs::read(root.join(path)).unwrap())})
}
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for e in fs::read_dir(path).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            result.insert(
                p.strip_prefix(root).unwrap().to_owned(),
                (
                    if m.is_file() {
                        fs::read(&p).unwrap()
                    } else {
                        vec![]
                    },
                    m.modified().unwrap(),
                ),
            );
            if m.is_dir() {
                walk(root, &p, result);
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

#[test]
fn entity_decide_cli_stages_exhaustive_merge_then_rebuilds_without_redirect_inference() {
    let temp = fixture();
    let root = temp.path();
    fs::create_dir(root.join("assertions")).unwrap();
    let assertion = "---\nwiki_schema: \"1\"\nwiki_id: Assertion.C\nwiki_kind: assertion\ntitle: Directed proposition\nwiki_status: proposed\nwiki_subject_id: Entity.A\nwiki_predicate: depends_on\nwiki_object_id: Entity.B\nwiki_negated: true\nwiki_modality: possible\n---\nKeep author assertion prose.\n";
    fs::write(root.join("assertions/Assertion.C.md"), assertion).unwrap();
    let request = json!({"schema":"lwiki.entity-decisions.v1","decisions":[{"operation":"MergeEntities","reason":"Explicitly reviewed homonym identity","source_ids":["Entity.A"],"target_id":"Entity.B","expected_records":[expected(root,"Entity.A","entities/Entity.A.md"),expected(root,"Entity.B","entities/Entity.B.md"),expected(root,"Assertion.C","assertions/Assertion.C.md")],"remaps":[{"kind":"Assertion","assertion_id":"Assertion.C","field":"subject_id","old_entity_id":"Entity.A","target":{"kind":"ExistingEntity","entity_id":"Entity.B"}}]}]});
    let schema = ok(root, &["schema", "entity-decisions"], None);
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&request)
        .unwrap();
    let staged = ok(root, &["graph", "decide", "--file", "-"], Some(&request));
    assert_eq!(staged["status"], "prepared");
    assert_eq!(
        fs::read_to_string(root.join("assertions/Assertion.C.md")).unwrap(),
        assertion
    );
    ok(
        root,
        &[
            "changes",
            "apply",
            staged["prepared"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    let note =
        lwiki::records::parse_note(&fs::read(root.join("assertions/Assertion.C.md")).unwrap());
    let record = note.canonical.unwrap();
    assert_eq!(record.string("wiki_subject_id"), Some("Entity.B"));
    assert_eq!(record.string("wiki_object_id"), Some("Entity.B"));
    assert_eq!(record.field("wiki_negated"), Some(&json!(true)));
    assert_eq!(record.string("wiki_modality"), Some("possible"));
    let repeat = ok(root, &["graph", "decide", "--file", "-"], Some(&request));
    assert_eq!(repeat["allocations"], staged["allocations"]);
    assert_eq!(repeat["reused"], true);
    fs::remove_dir_all(root.join(".wiki/cache")).unwrap();
    ok(root, &["index", "rebuild"], None);
    assert_eq!(
        fs::read(root.join("assertions/Assertion.C.md")).unwrap(),
        note.raw
    );
    let change_id = staged["prepared"]["change_id"].as_str().unwrap();
    let before_rollback = tree(root);
    ok(root, &["--dry-run", "changes", "rollback", change_id], None);
    assert_eq!(tree(root), before_rollback);
    let inverse = ok(root, &["changes", "rollback", change_id], None);
    assert_eq!(inverse["status"], "prepared");
    assert_eq!(
        fs::read(root.join("assertions/Assertion.C.md")).unwrap(),
        note.raw
    );
    let inverse_id = inverse["change"]["change_id"].as_str().unwrap();
    ok(root, &["changes", "apply", inverse_id], None);
    assert_eq!(
        fs::read_to_string(root.join("assertions/Assertion.C.md")).unwrap(),
        assertion
    );
    let redo = ok(root, &["changes", "rollback", inverse_id], None);
    assert_eq!(redo["status"], "prepared");
    ok(
        root,
        &[
            "changes",
            "apply",
            redo["change"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        fs::read(root.join("assertions/Assertion.C.md")).unwrap(),
        note.raw
    );
    fs::remove_dir_all(root.join(".wiki/cache")).unwrap();
    ok(root, &["index", "rebuild"], None);
    assert_eq!(
        fs::read(root.join("assertions/Assertion.C.md")).unwrap(),
        note.raw
    );
}

#[test]
fn entity_decide_alias_dry_run_is_pure_and_stale_apply_preserves_author_edit() {
    let temp = fixture();
    let root = temp.path();
    let request = json!({"schema":"lwiki.entity-decisions.v1","decisions":[{"operation":"AddAlias","reason":"Explicit alias request","entity_id":"Entity.A","alias":"An explicit alias","expected_records":[expected(root,"Entity.A","entities/Entity.A.md")],"remaps":[]}]});
    let before = tree(root);
    let dry = ok(
        root,
        &["--dry-run", "graph", "decide", "--file", "-"],
        Some(&request),
    );
    assert_eq!(dry["allocated_ids"], Value::Null);
    assert_eq!(tree(root), before);
    let staged = ok(root, &["graph", "decide", "--file", "-"], Some(&request));
    let path = root.join("entities/Entity.A.md");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"Preserve foreign edit.\n");
    fs::write(&path, &bytes).unwrap();
    let (exit, _) = invoke(
        root,
        &[
            "changes",
            "apply",
            staged["prepared"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    assert_eq!(exit, 4);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn entity_decide_malformed_request_fails_before_authority_or_recovery() {
    let temp = fixture();
    let root = temp.path();
    let before = tree(root);
    for request in [
        json!({"schema":"lwiki.entity-decisions.v2","decisions":[]}),
        json!({"schema":"lwiki.entity-decisions.v1","decisions":[]}),
        json!({"schema":"lwiki.entity-decisions.v1","decisions":[{"operation":"AddAlias","reason":"x","entity_id":"Entity.A","alias":"a","expected_records":[],"remaps":[],"extra":true}]}),
    ] {
        let (exit, _) = invoke(root, &["graph", "decide", "--file", "-"], Some(&request));
        assert_eq!(exit, 9);
        assert_eq!(tree(root), before);
    }
}

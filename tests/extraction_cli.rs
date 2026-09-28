use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};
fn invoke(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> (i32, Value) {
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
    if let Some(bytes) = stdin {
        child.stdin.as_mut().unwrap().write_all(bytes).unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["meta"]["network_used"], false);
    (output.status.code().unwrap(), envelope)
}
fn ok(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Value {
    let (exit, value) = invoke(root, args, stdin);
    assert_eq!(exit, 0, "{args:?}: {value}");
    value["data"].clone()
}
fn fixture() -> (tempfile::TempDir, String) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"),b"---\nwiki_schema: \"1\"\nwiki_id: Vault.Case\nwiki_kind: vault\ntitle: Extraction CLI\n---\n").unwrap();
    let added = ok(
        temp.path(),
        &["source", "add", "-", "--title", "CLI source"],
        Some("# People\r\n\r\n東 works for Acme.\r\n".as_bytes()),
    );
    (
        temp,
        added["allocated_ids"]["source"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}
fn response(packet: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema":"lwiki.extraction.v1","packet_id":packet["packet_id"],"packet_fingerprint":packet["packet_fingerprint"],"mentions":[{"id":"m1","window_id":"w1","label":"東","type":"person","quote":"東"},{"id":"m2","window_id":"w1","label":"Acme","type":"organization","quote":"Acme"}],"assertions":[{"id":"a1","subject":"m1","predicate":"works_for","object":{"kind":"mention","mention_id":"m2"},"negated":false,"modality":"asserted","evidence":[{"window_id":"w1","stance":"supports","quote":"東 works for Acme."}]}],"unresolved":[]})).unwrap()
}
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for e in fs::read_dir(path).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            out.insert(
                p.strip_prefix(root).unwrap().into(),
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
                walk(root, &p, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
#[test]
fn cli_packet_persists_then_import_stages_apply_and_markdown_restore_reuses() {
    let (temp, source) = fixture();
    let root = temp.path();
    let export = ok(
        root,
        &[
            "graph",
            "extract",
            "--source-id",
            &source,
            "--executor",
            "agent",
        ],
        None,
    );
    assert_eq!(export["persisted"], true);
    assert_eq!(export["ready_to_import"], true);
    let packet = &export["packet"];
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/extraction-packet-v1.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(packet)
        .unwrap();
    assert!(
        root.join(export["locator"]["path"].as_str().unwrap())
            .is_file()
    );
    let again = ok(root, &["graph", "extract", "--source-id", &source], None);
    assert_eq!(again["packet"], *packet);
    assert_eq!(again["reused"], true);
    let bytes = response(packet);
    let response_schema: Value =
        serde_json::from_str(include_str!("../schemas/extraction-v1.json")).unwrap();
    jsonschema::validator_for(&response_schema)
        .unwrap()
        .validate(&serde_json::from_slice::<Value>(&bytes).unwrap())
        .unwrap();
    let imported = ok(root, &["graph", "import", "--file", "-"], Some(&bytes));
    assert_eq!(imported["status"], "prepared");
    assert_eq!(imported["coverage"]["pending_mentions"], 2);
    let path = imported["extraction"]["path"].as_str().unwrap();
    assert!(!root.join(path).exists());
    let retry = ok(root, &["graph", "import", "--file", "-"], Some(&bytes));
    assert_eq!(retry["prepared"], imported["prepared"]);
    assert_eq!(retry["allocations"], imported["allocations"]);
    ok(
        root,
        &[
            "changes",
            "apply",
            imported["prepared"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    assert!(root.join(path).is_file());
    let note = lwiki::records::parse_note(&fs::read(root.join(path)).unwrap());
    let body = std::str::from_utf8(note.body()).unwrap();
    let raw = body
        .split_once("```lwiki-extraction-state-v1\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    let state: Value = serde_json::from_str(raw).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/extraction-state-v1.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&state)
        .unwrap();
    assert_eq!(state["raw_response"].as_str().unwrap().as_bytes(), bytes);
    fs::remove_dir_all(root.join(".wiki")).unwrap();
    fs::remove_dir_all(root.join("changes")).unwrap();
    let restored = ok(root, &["graph", "import", "--file", "-"], Some(&bytes));
    assert_eq!(restored["disposition"], "canonical_restored");
    assert!(restored["prepared"].is_null());
    assert_eq!(restored["allocations"], imported["allocations"]);
    assert_eq!(restored["extraction"], imported["extraction"]);
}
#[test]
fn cli_dry_export_and_import_preserve_membership_bytes_and_mtimes() {
    let (temp, source) = fixture();
    let root = temp.path();
    fs::remove_dir_all(root.join(".wiki/cache")).unwrap();
    let before = tree(root);
    let preview = ok(
        root,
        &["--dry-run", "graph", "extract", "--source-id", &source],
        None,
    );
    assert_eq!(preview["persisted"], false);
    assert_eq!(preview["ready_to_import"], false);
    assert_eq!(tree(root), before);
    let packet = ok(root, &["graph", "extract", "--source-id", &source], None);
    let bytes = response(&packet["packet"]);
    fs::remove_dir_all(root.join(".wiki/cache")).unwrap();
    let before = tree(root);
    let planned = ok(
        root,
        &["--dry-run", "graph", "import", "--file", "-"],
        Some(&bytes),
    );
    assert_eq!(planned["dry_run"], true);
    assert!(planned["allocated_ids"].is_null());
    assert_eq!(tree(root), before);
    assert!(!root.join(".wiki/cache").exists());
    let (exit, error) = invoke(
        root,
        &["--stage", "graph", "extract", "--source-id", &source],
        None,
    );
    assert_eq!(exit, 2, "{error}");
    assert_eq!(tree(root), before);
}
#[test]
fn cli_invalid_response_stages_nothing_and_conflicting_new_is_explicit() {
    let (temp, source) = fixture();
    let root = temp.path();
    let packet = ok(root, &["graph", "extract", "--source-id", &source], None)["packet"].clone();
    let mut invalid: Value = serde_json::from_slice(&response(&packet)).unwrap();
    invalid["assertions"][0]["accepted"] = json!(true);
    let before = tree(root);
    let (exit, error) = invoke(
        root,
        &["graph", "import", "--file", "-"],
        Some(&serde_json::to_vec(&invalid).unwrap()),
    );
    assert_eq!(exit, 9, "{error}");
    assert_eq!(tree(root), before);
    let first = ok(
        root,
        &["graph", "import", "--file", "-"],
        Some(&response(&packet)),
    );
    let mut changed: Value = serde_json::from_slice(&response(&packet)).unwrap();
    changed["unresolved"] =
        json!([{"window_id":"w1","quote":"Acme","reason":"identity unresolved"}]);
    let bytes = serde_json::to_vec(&changed).unwrap();
    let (exit, error) = invoke(root, &["graph", "import", "--file", "-"], Some(&bytes));
    assert_eq!(exit, 4, "{error}");
    let second = ok(
        root,
        &["graph", "import", "--file", "-", "--new-extraction"],
        Some(&bytes),
    );
    assert_ne!(first["extraction"], second["extraction"]);
    assert_eq!(second["coverage"]["unresolved"], 1);
    let (exit, error) = invoke(
        root,
        &[
            "graph",
            "extract",
            "--source-id",
            &source,
            "--executor",
            "api",
        ],
        None,
    );
    assert_eq!(exit, 6, "{error}");
}

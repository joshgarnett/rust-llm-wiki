#[path = "../test_support/paths.rs"]
mod test_paths;
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
    let mut child = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
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

fn imported(root: &Path, source: &str) -> (Value, Vec<u8>) {
    let exported = ok(root, &["graph", "extract", "--source-id", source], None);
    let raw = response(&exported["packet"]);
    let imported = ok(root, &["graph", "import", "--file", "-"], Some(&raw));
    ok(
        root,
        &[
            "changes",
            "apply",
            imported["prepared"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    (imported, raw)
}
fn request(root: &Path, imported: &Value) -> Vec<u8> {
    let bytes = fs::read(root.join(imported["extraction"]["path"].as_str().unwrap())).unwrap();
    serde_json::to_vec(&json!({"schema":"lwiki.graph-resolution.v1","extraction_id":imported["extraction"]["record"]["record_id"],"expected_hash":lwiki::domain::Blake3Hash::digest(&bytes),"mappings":[{"operation":"CreateEntity","mention_id":"m1","reason":"Explicit identity from this source","title":"東","entity_type":"person"},{"operation":"CreateEntity","mention_id":"m2","reason":"Explicit organization identity","title":"Acme","entity_type":"organization"}]})).unwrap()
}
#[test]
fn resolve_cli_stages_apply_reuses_and_publishes_valid_schemas() {
    let (temp, source) = fixture();
    let root = temp.path();
    let (imported, raw) = imported(root, &source);
    let req = request(root, &imported);
    let schema = ok(root, &["schema", "graph-resolution"], None);
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&serde_json::from_slice::<Value>(&req).unwrap())
        .unwrap();
    let caps = ok(root, &["capabilities"], None);
    assert!(
        caps["commands"]
            .as_array()
            .unwrap()
            .contains(&json!("graph resolve"))
    );
    assert!(
        caps["schemas"]
            .as_array()
            .unwrap()
            .contains(&json!("graph-resolution-receipt"))
    );
    let before = fs::read(root.join(imported["extraction"]["path"].as_str().unwrap())).unwrap();
    let resolved = ok(root, &["graph", "resolve", "--file", "-"], Some(&req));
    assert_eq!(resolved["status"], "prepared");
    assert_eq!(resolved["summary"]["materialize_assertions"], json!(["a1"]));
    assert_eq!(
        fs::read(root.join(imported["extraction"]["path"].as_str().unwrap())).unwrap(),
        before
    );
    let retry = ok(
        root,
        &["--stage", "graph", "resolve", "--file", "-"],
        Some(&req),
    );
    assert_eq!(retry["prepared"], resolved["prepared"]);
    assert_eq!(retry["allocations"], resolved["allocations"]);
    ok(
        root,
        &[
            "changes",
            "apply",
            resolved["prepared"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    let repeated = ok(root, &["graph", "resolve", "--file", "-"], Some(&req));
    assert_eq!(repeated["reused"], true);
    assert_eq!(repeated["status"], "committed");
    let fs = lwiki::vault::VaultFs::new(lwiki::vault::VaultRoot::explicit(root).unwrap());
    let view = lwiki::sources::SourceView::from_fs_bounded(&fs, 64 * 1024 * 1024, 4096).unwrap();
    let extraction = lwiki::graph::import::load_extraction(
        &view,
        &lwiki::domain::RecordId::new(
            imported["extraction"]["record"]["record_id"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(extraction.artifact().raw_response.as_bytes(), raw);
    let assertion = lwiki::graph::extraction_types::PacketLocalId::new("a1").unwrap();
    let assertion_id = &extraction.artifact().allocations.assertions[&assertion];
    let read = ok(root, &["read", "--id", assertion_id.as_str()], None);
    assert!(serde_json::to_string(&read).unwrap().contains("proposed"));
    let candidate_id = resolved["allocations"]["entities"]["m1"].as_str().unwrap();
    let candidate_packet = ok(
        root,
        &[
            "graph",
            "extract",
            "--source-id",
            &source,
            "--candidate-id",
            candidate_id,
        ],
        None,
    );
    assert_eq!(
        candidate_packet["packet"]["candidate_context"][0]["entity_type"],
        "person"
    );
    assert_eq!(
        candidate_packet["packet"]["candidate_context"][0]["reference"]["record_id"],
        candidate_id
    );
    let validated = lwiki::graph::resolution::validate_resolution(&view, &req).unwrap();
    let receipt = lwiki::graph::mention_state::load_resolution_receipt(&view, validated.task_id())
        .unwrap()
        .unwrap();
    let receipt_schema = ok(root, &["schema", "graph-resolution-receipt"], None);
    jsonschema::validator_for(&receipt_schema)
        .unwrap()
        .validate(&serde_json::to_value(receipt.receipt()).unwrap())
        .unwrap();
}
#[test]
fn resolve_dry_run_after_cache_deletion_preserves_every_byte_and_mtime() {
    let (temp, source) = fixture();
    let root = temp.path();
    let (imported, _) = imported(root, &source);
    let req = request(root, &imported);
    fs::remove_dir_all(root.join(".wiki/cache")).unwrap();
    let before = tree(root);
    let dry = ok(
        root,
        &["--dry-run", "graph", "resolve", "--file", "-"],
        Some(&req),
    );
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["allocated_ids"], Value::Null);
    assert_eq!(dry["summary"]["resolved_mentions"], 2);
    assert_eq!(tree(root), before);
    let (exit, _) = invoke(
        root,
        &["--dry-run", "graph", "resolve", "--file", "-"],
        Some(br#"{"schema":"lwiki.graph-resolution.v1","schema":"duplicate"}"#),
    );
    assert_eq!(exit, 9);
    assert_eq!(tree(root), before);
    let huge = vec![b' '; 262145];
    let (exit, _) = invoke(
        root,
        &["--dry-run", "graph", "resolve", "--file", "-"],
        Some(&huge),
    );
    assert_eq!(exit, 9);
    assert_eq!(tree(root), before);
}
#[test]
fn resolve_apply_rechecks_selected_payload_and_preserves_conflicting_bytes() {
    let (temp, source) = fixture();
    let root = temp.path();
    let (imported, _) = imported(root, &source);
    let req = request(root, &imported);
    let resolved = ok(root, &["graph", "resolve", "--file", "-"], Some(&req));
    let original = fs::read_dir(root.join(format!("sources/{source}/revisions")))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("original.bin");
    fs::write(&original, b"changed source bytes").unwrap();
    let (exit, envelope) = invoke(
        root,
        &[
            "changes",
            "apply",
            resolved["prepared"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    assert_eq!(exit, 4, "{envelope}");
    assert_eq!(fs::read(original).unwrap(), b"changed source bytes");
    for entity in resolved["allocations"]["entities"]
        .as_object()
        .unwrap()
        .values()
    {
        assert!(
            !root
                .join(format!(
                    "knowledge/entities/{}.md",
                    entity.as_str().unwrap()
                ))
                .exists()
        );
    }
}

#[test]
fn invalid_resolution_shape_fails_before_normal_writer_or_recovery() {
    let (temp, source) = fixture();
    let root = temp.path();
    let (imported, _) = imported(root, &source);
    let base: Value = serde_json::from_slice(&request(root, &imported)).unwrap();
    let before = tree(root);
    let mut invalid = vec![];
    let mut wrong_schema = base.clone();
    wrong_schema["schema"] = json!("lwiki.graph-resolution.future");
    invalid.push(wrong_schema);
    let mut empty = base.clone();
    empty["mappings"] = json!([]);
    invalid.push(empty);
    let mut duplicate = base.clone();
    duplicate["mappings"][1] = duplicate["mappings"][0].clone();
    invalid.push(duplicate);
    let mut wrong_type = base;
    wrong_type["mappings"][0]["entity_type"] = json!("invented-type");
    invalid.push(wrong_type);
    for request in invalid {
        let (exit, envelope) = invoke(
            root,
            &["graph", "resolve", "--file", "-"],
            Some(&serde_json::to_vec(&request).unwrap()),
        );
        assert_eq!(exit, 9, "{envelope}");
        assert_eq!(tree(root), before);
    }
}

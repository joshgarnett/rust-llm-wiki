#[path = "../test_support/paths.rs"]
mod test_paths;
use lwiki::domain::Blake3Hash;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn invoke(root: Option<&Path>, args: &[&str], stdin: Option<&[u8]>) -> (i32, Value) {
    let mut command = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
    command.args(["--json", "--offline"]);
    if let Some(root) = root {
        command.arg("--wiki").arg(root);
    }
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
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
    assert!(
        !output.stdout.contains(&0x1b),
        "machine output contains ANSI"
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        let stream = String::from_utf8(output.stdout.clone()).unwrap();
        let terminal: Value =
            serde_json::from_str(stream.lines().last().expect("terminal JSONL event")).unwrap();
        terminal["data"].clone()
    });
    let schema: Value = serde_json::from_str(include_str!("../schemas/output-v1.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&envelope)
        .unwrap();
    assert_eq!(envelope["meta"]["network_used"], false);
    (output.status.code().unwrap(), envelope)
}
fn ok(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Value {
    let (exit, envelope) = invoke(Some(root), args, stdin);
    assert_eq!(exit, 0, "{args:?}: {envelope}");
    assert_eq!(envelope["ok"], true);
    envelope
}
fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: \"1\"\nwiki_id: Vault.Case\nwiki_kind: vault\ntitle: CLI fixture\n---\n").unwrap();
    temp
}
fn page(id: &str, body: &str) -> Vec<u8> {
    format!("---\nwiki_schema: \"1\"\nwiki_id: {id}\nwiki_kind: page\ntitle: {id}\nwiki_status: reviewed\n---\n{body}").into_bytes()
}
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            out.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (
                    if metadata.is_file() {
                        fs::read(&path).unwrap()
                    } else {
                        vec![]
                    },
                    metadata.modified().unwrap(),
                ),
            );
            if metadata.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
#[test]
fn executable_put_read_rename_stage_apply_rollback_abort_workflow() {
    let temp = fixture();
    let root = temp.path();
    let bytes = page("Page.Case", "# Overview\nCafé 🦀\n");
    let put = ok(root, &["page", "put", "--file", "-"], Some(&bytes));
    assert_eq!(put["data"]["status"], "committed");
    assert_eq!(fs::read(root.join("pages/Page.Case.md")).unwrap(), bytes);
    let read = ok(
        root,
        &["read", "--id", "Page.Case", "--max-bytes", "5"],
        None,
    );
    assert_eq!(read["data"]["body"], "# Ove");
    assert_eq!(read["meta"]["partial"], true);
    assert_eq!(read["meta"]["freshness"], "verified_snapshot");
    assert_eq!(
        invoke(Some(root), &["read", "--id", "page.case"], None).0,
        3
    );
    assert_eq!(
        invoke(Some(root), &["page", "put", "--file", "-"], Some(&bytes)).0,
        4
    );
    let updated = page("Page.Case", "Updated café 🦀\n");
    let old_hash = Blake3Hash::digest(&bytes).to_string();
    ok(
        root,
        &["page", "put", "--file", "-", "--if-match", &old_hash],
        Some(&updated),
    );
    ok(
        root,
        &["page", "put", "--file", "-"],
        Some(&page(
            "incoming",
            "[[pages/Page.Case.md#Overview|Original label]]\n",
        )),
    );
    let hash = Blake3Hash::digest(&updated).to_string();
    let staged = ok(
        root,
        &[
            "--stage",
            "page",
            "rename",
            "Page.Case",
            "--to",
            "pages/renamed.md",
            "--if-match",
            &hash,
        ],
        None,
    );
    let change = staged["data"]["change"]["change_id"].as_str().unwrap();
    assert!(!root.join("pages/renamed.md").exists());
    let inspected = ok(root, &["changes", "show", change], None);
    assert!(inspected["data"]["payloads"].as_array().unwrap().len() >= 3);
    ok(root, &["changes", "show", change, "--operation", "0"], None);
    ok(root, &["changes", "apply", change], None);
    assert!(!root.join("pages/Page.Case.md").exists());
    assert_eq!(fs::read(root.join("pages/renamed.md")).unwrap(), updated);
    assert!(
        fs::read_to_string(root.join("pages/incoming.md"))
            .unwrap()
            .contains("[[pages/renamed.md#Overview|Original label]]")
    );
    let inverse = ok(root, &["changes", "rollback", change], None);
    assert_eq!(inverse["data"]["status"], "prepared");
    ok(
        root,
        &[
            "changes",
            "apply",
            inverse["data"]["change"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    assert_eq!(fs::read(root.join("pages/Page.Case.md")).unwrap(), updated);
    let proposal = ok(
        root,
        &["--stage", "page", "put", "--file", "-"],
        Some(&page("abort", "Never applied")),
    );
    ok(
        root,
        &[
            "changes",
            "abort",
            proposal["data"]["change"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    assert!(!root.join("pages/abort.md").exists());
}
#[test]
fn no_sync_reads_cached_bytes_and_search_reports_freshness_and_channel() {
    let temp = fixture();
    let root = temp.path();
    ok(
        root,
        &["page", "put", "--file", "-"],
        Some(&page("cached", "Before Café")),
    );
    let before = ok(root, &["search", "Café"], None);
    assert_eq!(before["meta"]["freshness"], "verified_snapshot");
    assert_eq!(before["data"]["hits"][0]["excerpt"]["label"], "note_text");
    assert!(before["data"]["hits"][0]["excerpt"]["citation"].is_null());
    fs::write(root.join("pages/cached.md"), page("cached", "After Tea")).unwrap();
    let stale = ok(root, &["read", "--id", "cached", "--no-sync"], None);
    assert_eq!(stale["data"]["body"], "Before Café");
    assert_eq!(stale["meta"]["freshness"], "index_snapshot");
    let stale_search = ok(root, &["search", "Café", "--no-sync"], None);
    assert_eq!(stale_search["data"]["hits"].as_array().unwrap().len(), 1);
    let current = ok(root, &["read", "--id", "cached"], None);
    assert_eq!(current["data"]["body"], "After Tea");
    assert_eq!(current["meta"]["freshness"], "verified_snapshot");
    assert!(
        ok(root, &["search", "Café"], None)["data"]["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        ok(root, &["search", "cached", "--mode", "literal"], None)["data"]["hits"]
            .as_array()
            .unwrap()
            .len()
            == 1
    );
}

#[test]
fn normalized_maintenance_cli_rebuild_sync_discovery_and_dry_run() {
    let temp = fixture();
    let root = temp.path();
    fs::write(
        root.join("page.md"),
        page("Page.Maintenance", "Original apricot text"),
    )
    .unwrap();
    let before = tree(root);
    let preview = ok(
        root,
        &["--dry-run", "index", "rebuild", "--normalized"],
        None,
    );
    assert_eq!(
        preview["data"]["maintenance"]["canonical_scan_performed"],
        false
    );
    assert_eq!(before, tree(root));
    let first = ok(root, &["index", "rebuild", "--normalized"], None);
    assert_eq!(first["data"]["report"]["reused"], false);
    assert_eq!(first["data"]["maintenance"]["layout"], "normalized");
    let snapshot = first["data"]["report"]["snapshot"].clone();
    let same = ok(root, &["index", "sync"], None);
    assert_eq!(same["data"]["report"]["reused"], true);
    assert_eq!(same["data"]["report"]["snapshot"], snapshot);
    assert!(same["data"]["maintenance"]["build"].is_null());
    assert_eq!(
        ok(root, &["search", "apricot", "--no-sync"], None)["data"]["hits"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fs::write(
        root.join("page.md"),
        page("Page.Maintenance", "Updated blueberry text"),
    )
    .unwrap();
    ok(root, &["index", "sync"], None);
    assert!(
        ok(root, &["search", "apricot", "--no-sync"], None)["data"]["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ok(root, &["search", "blueberry", "--no-sync"], None)["data"]["hits"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let before = tree(root);
    for args in [
        vec!["--dry-run", "index", "sync"],
        vec!["--dry-run", "index", "rebuild"],
    ] {
        ok(root, &args, None);
        assert_eq!(before, tree(root));
    }
    // Once activated, plain rebuild continues the same layout.
    let rebuilt = ok(root, &["index", "rebuild"], None);
    assert_eq!(rebuilt["data"]["maintenance"]["layout"], "normalized");
    assert_eq!(rebuilt["data"]["report"]["reused"], false);
}
#[test]
fn dry_run_cli_leaves_existing_vault_cache_changes_and_directories_exactly_unchanged() {
    let temp = fixture();
    let root = temp.path();
    let bytes = page("existing", "Original source text");
    ok(root, &["page", "put", "--file", "-"], Some(&bytes));
    let staged = ok(
        root,
        &["--stage", "page", "put", "--file", "-"],
        Some(&page("staged", "Later")),
    );
    let change = staged["data"]["change"]["change_id"].as_str().unwrap();
    let hash = Blake3Hash::digest(&bytes).to_string();
    let before = tree(root);
    let commands: Vec<Vec<&str>> = vec![
        vec!["read", "--id", "existing"],
        vec!["search", "Original"],
        vec!["search", "Original", "--no-sync"],
        vec!["index", "sync"],
        vec!["index", "rebuild"],
        vec!["check"],
        vec!["doctor"],
        vec!["recover"],
        vec!["changes", "show", change],
        vec!["changes", "apply", change],
        vec!["changes", "abort", change],
        vec![
            "page",
            "rename",
            "existing",
            "--to",
            "pages/moved.md",
            "--if-match",
            &hash,
        ],
        vec!["migrate", "--id", "existing", "--if-match", &hash],
    ];
    for command in commands {
        let mut args = vec!["--dry-run"];
        args.extend(command);
        let (exit, envelope) = invoke(Some(root), &args, None);
        assert_eq!(exit, 0, "{args:?}: {envelope}");
        assert_eq!(before, tree(root), "{args:?} mutated files");
    }
    ok(
        root,
        &["--dry-run", "page", "put", "--file", "-"],
        Some(&page("dry", "Preview")),
    );
    assert_eq!(before, tree(root));
    ok(
        root,
        &[
            "--dry-run",
            "source",
            "add",
            "-",
            "--title",
            "Preview source",
        ],
        Some(b"Preview source text"),
    );
    assert_eq!(before, tree(root));
    let absent = root.join("uncreated");
    let (exit, _) = invoke(None, &["--dry-run", "init", absent.to_str().unwrap()], None);
    assert_eq!(exit, 0);
    assert_eq!(before, tree(root));
}
#[test]
fn source_cli_capture_refresh_reuse_and_withdraw_preserve_immutable_bytes() {
    let temp = fixture();
    let root = temp.path();
    let added = ok(
        root,
        &["source", "add", "-", "--title", "CLI capture"],
        Some("Exact capture 🦀\n".as_bytes()),
    );
    let source = added["data"]["allocated_ids"]["source"].as_str().unwrap();
    let revision = added["data"]["allocated_ids"]["revision"].as_str().unwrap();
    let source_note = ok(root, &["read", "--id", source], None);
    assert_eq!(
        source_note["data"]["record"]["wiki_current_revision"],
        revision
    );
    let reused = ok(
        root,
        &[
            "source",
            "refresh",
            source,
            "--file",
            "-",
            "--title",
            "CLI capture",
        ],
        Some("Exact capture 🦀\n".as_bytes()),
    );
    assert_eq!(reused["data"]["reused"], true);
    assert_eq!(reused["data"]["allocated_ids"]["revision"], revision);
    let refreshed = ok(
        root,
        &[
            "source",
            "refresh",
            source,
            "--file",
            "-",
            "--title",
            "CLI capture",
        ],
        Some(b"New capture\n"),
    );
    assert_ne!(refreshed["data"]["allocated_ids"]["revision"], revision);
    assert_eq!(
        ok(root, &["read", "--id", revision], None)["data"]["record"]["wiki_id"],
        revision
    );
    ok(
        root,
        &[
            "source",
            "withdraw",
            source,
            "--reason",
            "Superseded source",
        ],
        None,
    );
    assert_eq!(
        ok(root, &["read", "--id", source], None)["data"]["record"]["wiki_status"],
        "withdrawn"
    );
}
#[test]
fn source_cli_reports_original_only_and_empty_capture() {
    let temp = fixture();
    let root = temp.path();
    for (name, bytes) in [
        ("page.html", b"<html>HTML-only-token</html>".as_slice()),
        ("bytes.bin", &[0, 255, 0][..]),
        ("invalid.txt", &[255, 254][..]),
    ] {
        let input = root.join(name);
        fs::write(&input, bytes).unwrap();
        let before = tree(root);
        let preview = ok(
            root,
            &["--dry-run", "source", "add", input.to_str().unwrap()],
            None,
        );
        assert_eq!(preview["data"]["extraction_status"], "unsupported");
        assert_eq!(preview["data"]["citable"], false);
        assert_eq!(before, tree(root), "dry-run wrote canonical state");
        let added = ok(root, &["source", "add", input.to_str().unwrap()], None);
        assert_eq!(added["data"]["extraction_status"], "unsupported");
        assert_eq!(added["data"]["citable"], false);
        assert!(
            added["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("cannot be searched"))
        );
        let source = added["data"]["allocated_ids"]["source"].as_str().unwrap();
        let revision = added["data"]["allocated_ids"]["revision"].as_str().unwrap();
        let parent = root.join(format!("sources/{source}/revisions/{revision}"));
        assert_eq!(fs::read(parent.join("original.bin")).unwrap(), bytes);
        assert!(!parent.join("content.md").exists());
        let record = ok(root, &["read", "--id", revision], None);
        assert_eq!(
            record["data"]["record"]["wiki_extraction_status"],
            "unsupported"
        );
    }
    let empty = root.join("empty.txt");
    fs::write(&empty, b"").unwrap();
    let added = ok(root, &["source", "add", empty.to_str().unwrap()], None);
    assert_eq!(added["data"]["extraction_status"], "complete");
    assert_eq!(added["data"]["citable"], false);
    assert!(
        added["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("Empty source"))
    );
    let source = added["data"]["allocated_ids"]["source"].as_str().unwrap();
    let revision = added["data"]["allocated_ids"]["revision"].as_str().unwrap();
    let parent = root.join(format!("sources/{source}/revisions/{revision}"));
    assert_eq!(fs::read(parent.join("original.bin")).unwrap(), b"");
    assert_eq!(fs::read(parent.join("content.md")).unwrap(), b"");

    let supported = root.join("supported.txt");
    fs::write(&supported, b"Ordinary source text").unwrap();
    let added = ok(root, &["source", "add", supported.to_str().unwrap()], None);
    assert_eq!(added["data"]["extraction_status"], "complete");
    assert_eq!(added["data"]["citable"], true);
    assert!(added["warnings"].as_array().unwrap().is_empty());
}
#[test]
fn source_cli_refresh_preserves_title_and_searches_only_current_payload() {
    let temp = fixture();
    let root = temp.path();
    let original = root.join("original.txt");
    fs::write(&original, b"OLD-UNIQUE-CAPTURE-TOKEN").unwrap();
    let added = ok(
        root,
        &[
            "source",
            "add",
            original.to_str().unwrap(),
            "--title",
            "Stable source title",
        ],
        None,
    );
    let source = added["data"]["allocated_ids"]["source"].as_str().unwrap();
    let old_revision = added["data"]["allocated_ids"]["revision"].as_str().unwrap();
    let replacement = root.join("renamed-file.txt");
    fs::write(&replacement, b"NEW-UNIQUE-CAPTURE-TOKEN").unwrap();
    let refreshed = ok(
        root,
        &[
            "source",
            "refresh",
            source,
            "--file",
            replacement.to_str().unwrap(),
        ],
        None,
    );
    let current_revision = refreshed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    assert_ne!(current_revision, old_revision);
    assert_eq!(
        ok(root, &["read", "--id", source], None)["data"]["record"]["title"],
        "Stable source title"
    );
    assert_eq!(
        ok(root, &["read", "--id", current_revision], None)["data"]["record"]["title"],
        "Stable source title"
    );
    let current = ok(
        root,
        &["search", "NEW-UNIQUE-CAPTURE-TOKEN", "--mode", "literal"],
        None,
    );
    let hits = current["data"]["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0]["path"]
            .as_str()
            .unwrap()
            .ends_with(&format!("revisions/{current_revision}/content.md"))
    );
    assert!(
        ok(
            root,
            &["search", "OLD-UNIQUE-CAPTURE-TOKEN", "--mode", "literal"],
            None
        )["data"]["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let titled = ok(
        root,
        &["search", "Stable source title", "--mode", "literal"],
        None,
    );
    let paths: Vec<_> = titled["data"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|hit| hit["path"].as_str())
        .collect();
    assert!(paths.iter().any(|path| path.ends_with("/source.md")));
    assert!(
        paths
            .iter()
            .any(|path| path.ends_with(&format!("revisions/{current_revision}/revision.md")))
    );
    let explicit = ok(
        root,
        &[
            "source",
            "refresh",
            source,
            "--file",
            replacement.to_str().unwrap(),
            "--title",
            "Explicit source title",
        ],
        None,
    );
    assert_eq!(explicit["data"]["reused"], true);
    assert_eq!(
        explicit["data"]["allocated_ids"]["revision"],
        current_revision
    );
    assert_eq!(
        ok(root, &["read", "--id", source], None)["data"]["record"]["title"],
        "Explicit source title"
    );
    assert_eq!(
        ok(root, &["read", "--id", current_revision], None)["data"]["record"]["title"],
        "Stable source title"
    );
}
#[test]
fn migration_is_lossless_staged_guarded_and_future_versions_remain_readable() {
    let temp = fixture();
    let root = temp.path();
    let legacy = b"\xef\xbb\xbf---\r\n# preserve me\r\nwiki_schema: '0' # legacy\r\nwiki_id: legacy\r\nwiki_kind: page\r\ntitle: Legacy\r\nwiki_status: reviewed\r\ncustom: retained\r\n---\r\nBody bytes\r\n";
    fs::write(root.join("legacy.md"), legacy).unwrap();
    let hash = Blake3Hash::digest(legacy).to_string();
    let staged = ok(
        root,
        &["migrate", "--id", "legacy", "--if-match", &hash],
        None,
    );
    assert_eq!(staged["data"]["status"], "prepared");
    assert_eq!(fs::read(root.join("legacy.md")).unwrap(), legacy);
    ok(
        root,
        &[
            "changes",
            "apply",
            staged["data"]["change"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
    let expected = String::from_utf8(legacy.to_vec())
        .unwrap()
        .replace("wiki_schema: '0'", "wiki_schema: \"1\"");
    assert_eq!(
        fs::read(root.join("legacy.md")).unwrap(),
        expected.as_bytes()
    );
    let future = page("future", "Future body")
        .into_iter()
        .collect::<Vec<_>>();
    let future = String::from_utf8(future)
        .unwrap()
        .replace("wiki_schema: \"1\"", "wiki_schema: \"999\"");
    fs::write(root.join("future.md"), &future).unwrap();
    let read = ok(root, &["read", "--id", "future"], None);
    assert_eq!(read["data"]["body"], "Future body");
    assert!(read["data"]["record"].is_null());
    assert_eq!(
        invoke(
            Some(root),
            &[
                "migrate",
                "--id",
                "future",
                "--if-match",
                Blake3Hash::digest(future.as_bytes()).as_str()
            ],
            None
        )
        .0,
        6
    );
}
#[test]
fn machine_envelope_error_exit_and_no_ansi() {
    let temp = fixture();
    let root = temp.path();
    for args in [
        vec!["--format", "json", "capabilities"],
        vec!["--jsonl", "capabilities"],
        vec!["read", "--id", "a", "--path", "a.md"],
        vec!["search", "q", "--limit", "51"],
        vec!["search", "q", "--mode", "unknown"],
    ] {
        let (exit, envelope) = invoke(Some(root), &args, None);
        assert_eq!(exit, 2, "{args:?}: {envelope}");
        assert_eq!(envelope["error"]["code"], "USAGE");
    }
    let before = tree(root);
    let preview = ok(root, &["--dry-run", "doctor", "--probe"], None);
    assert_eq!(preview["data"]["probe"]["dry_run"], true);
    assert_eq!(preview["meta"]["network_used"], false);
    assert_eq!(tree(root), before);
    assert_eq!(
        invoke(Some(root), &["read", "--path", ".wiki/private.json"], None).0,
        3
    );
    fs::write(
        root.join("invalid.md"),
        b"---\nwiki_schema: \"1\"\nwiki_id: incomplete\nwiki_kind: page\n---\n",
    )
    .unwrap();
    let (exit, envelope) = invoke(Some(root), &["check"], None);
    assert_eq!(exit, 9);
    assert!(
        !envelope["data"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .args(["capabilities", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(serde_json::from_slice::<Value>(&output.stdout).is_ok());
}
#[test]
fn jsonl_started_and_terminal_events_share_invocation_and_sequence() {
    let temp = fixture();
    let schema: Value = serde_json::from_str(include_str!("../schemas/stream-v1.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for (args, expected) in [(vec!["index", "sync"], 0), (vec!["search", "q"], 2)] {
        let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .arg("--wiki")
            .arg(temp.path())
            .args(["--jsonl", "--offline"])
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(expected));
        assert!(output.stderr.is_empty());
        let events: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events.len(), 2);
        for event in &events {
            validator.validate(event).unwrap();
        }
        assert_eq!(events[0]["event"], "started");
        assert_eq!(events[1]["event"], "completed");
        assert_eq!(events[0]["sequence"], 0);
        assert_eq!(events[1]["sequence"], 1);
        assert_eq!(events[0]["invocation_id"], events[1]["invocation_id"]);
        assert_eq!(events[1]["data"]["ok"], json!(expected == 0));
    }
}

#[test]
fn retained_apply_failure_returns_inspectable_change_and_human_snapshot_is_labelled() {
    let temp = fixture();
    let root = temp.path();
    fs::create_dir_all(root.join(".wiki/cache")).unwrap();
    fs::write(
        root.join(".wiki/cache/index.sqlite"),
        b"not a SQLite database",
    )
    .unwrap();
    let (exit, failure) = invoke(
        Some(root),
        &["page", "put", "--file", "-"],
        Some(&page("failed", "Not applied")),
    );
    assert_eq!(exit, 5);
    assert_eq!(failure["error"]["code"], "INDEX_CORRUPT");
    assert_eq!(failure["meta"]["partial"], true);
    let id = failure["data"]["change"]["change_id"].as_str().unwrap();
    assert_eq!(failure["error"]["details"]["change"]["change_id"], id);
    assert!(!root.join("pages/failed.md").exists());
    assert_eq!(
        ok(root, &["changes", "show", id], None)["data"]["status"],
        "prepared"
    );
    ok(root, &["changes", "abort", id], None);
    fs::remove_file(root.join(".wiki/cache/index.sqlite")).unwrap();
    ok(
        root,
        &["page", "put", "--file", "-"],
        Some(&page("view", "Visible")),
    );
    let search = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(root)
        .args(["search", "Visible", "--no-sync"])
        .output()
        .unwrap();
    assert!(search.status.success());
    assert!(
        String::from_utf8(search.stdout)
            .unwrap()
            .contains("index_snapshot (unverified)")
    );
    let read = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(root)
        .args(["read", "--id", "view", "--no-sync"])
        .output()
        .unwrap();
    assert!(read.status.success());
    assert_eq!(read.stdout, b"Visible");
    assert!(
        String::from_utf8(read.stderr)
            .unwrap()
            .contains("index_snapshot (unverified)")
    );
}

#[test]
fn init_subprocess_and_bounded_file_input_are_guarded() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("new");
    let (exit, created) = invoke(
        None,
        &["init", root.to_str().unwrap(), "--title", "Created 🦀"],
        None,
    );
    assert_eq!(exit, 0);
    assert_eq!(created["data"]["created"], true);
    assert!(root.join("WIKI.md").is_file());
    assert_eq!(
        fs::read(root.join(".wiki/.gitignore")).unwrap(),
        b"cache/\n"
    );
    assert_eq!(invoke(None, &["init", root.to_str().unwrap()], None).0, 4);
    let large = temp.path().join("large.md");
    let file = fs::File::create(&large).unwrap();
    file.set_len((lwiki::app::MAX_INPUT_BYTES + 1) as u64)
        .unwrap();
    let before = tree(&root);
    assert_eq!(
        invoke(
            Some(&root),
            &["page", "put", "--file", large.to_str().unwrap()],
            None
        )
        .0,
        2
    );
    assert_eq!(before, tree(&root));
}

#[test]
fn normal_cli_search_recovers_committed_sql_with_incomplete_change_journal() {
    use lwiki::{catalog::*, changes::*, domain::*, vault::*};
    use std::{sync::Arc, time::Duration};
    struct Interrupt;
    impl PublicationFault for Interrupt {
        fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
            if checkpoint == PublicationCheckpoint::AfterCommit {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "simulated interrupted return",
                ))
            } else {
                Ok(())
            }
        }
    }
    let temp = fixture();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    let fs = VaultFs::new(root.clone());
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let writer = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
    let retained = engine
        .prepare(
            &writer,
            ChangeDraft {
                title: "Recovery fixture".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: BTreeMap::new(),
                read_preconditions: vec![],
                operations: vec![ExpectedWrite {
                    target: VaultRelativePath::new("recovered.md").unwrap(),
                    expected: ExpectedState::Absent,
                    proposed: Some(page("recovered", "Recovered lookup")),
                    apply_after: vec![],
                }],
            },
        )
        .unwrap();
    let interrupted = Catalog::with_options(
        fs,
        engine.vault_id().clone(),
        CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(Arc::new(Interrupt)),
        },
    );
    assert_eq!(
        engine
            .apply(
                &writer,
                &retained.prepared,
                &CatalogGraphValidator,
                &interrupted
            )
            .unwrap_err()
            .code,
        ErrorCode::Internal
    );
    assert_eq!(
        engine.inspect(&retained.prepared.change_id).unwrap().status,
        ChangeStatus::FilesApplied
    );
    drop(writer);
    let hits = ok(temp.path(), &["search", "Recovered"], None);
    assert_eq!(hits["meta"]["freshness"], "verified_snapshot");
    assert_eq!(hits["data"]["hits"].as_array().unwrap().len(), 1);
    assert_eq!(
        engine.inspect(&retained.prepared.change_id).unwrap().status,
        ChangeStatus::Committed
    );
}

#[test]
fn tiny_utf8_read_gives_progress_guidance_and_page_errors_give_template() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    fs::write(
        root.join("WIKI.md"),
        b"---\nwiki_schema: \"1\"\nwiki_id: test_vault\nwiki_kind: vault\ntitle: Test\n---\n",
    )
    .unwrap();
    let bytes = page("Unicode", "🦀Later");
    ok(root, &["page", "put", "--file", "-"], Some(&bytes));
    for mode in [
        vec!["read", "--id", "Unicode", "--max-bytes", "1"],
        vec!["read", "--id", "Unicode", "--max-bytes", "1", "--no-sync"],
    ] {
        let (exit, output) = invoke(Some(root), &mode, None);
        assert_eq!(exit, 2, "{output}");
        assert_eq!(output["error"]["details"]["minimum_max_bytes"], 4);
    }
    let complete = ok(root, &["read", "--id", "Unicode", "--max-bytes", "4"], None);
    assert_eq!(complete["data"]["body"], "🦀");
    assert_eq!(complete["data"]["continuation"]["start"], 4);
    assert!(
        complete["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("--start 4"))
    );
    for args in [
        vec!["page", "put", "--file", "-"],
        vec!["page", "put", "--file", "-", "--path", "pages/Plain.md"],
    ] {
        let (exit, output) = invoke(Some(root), &args, Some(b"# Plain Markdown"));
        assert_ne!(exit, 0);
        assert_eq!(output["meta"]["wiki_id"], "test_vault");
        assert!(
            output["error"]["details"]["missing_fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "wiki_id")
        );
        assert!(
            output["error"]["details"]["template"]
                .as_str()
                .unwrap()
                .contains("wiki_kind: page")
        );
    }
    let schema = ok(root, &["schema", "page"], None);
    assert!(schema["data"].is_object());
}

#[test]
fn human_search_pagination_and_input_errors_offer_executable_recovery() {
    let temp = fixture();
    for name in ["first", "second", "third"] {
        ok(
            temp.path(),
            &["page", "put", "--file", "-"],
            Some(&page(name, "Irrigation uses collected rainwater.\n")),
        );
    }
    let human = |args: &[&str]| {
        Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .arg("--wiki")
            .arg(temp.path())
            .arg("--offline")
            .args(args)
            .output()
            .unwrap()
    };
    let first = human(&["search", "irrigation", "--limit", "1"]);
    assert!(first.status.success());
    let text = String::from_utf8(first.stdout).unwrap();
    let cursor = text
        .split("--cursor '")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    let next = human(&["search", "irrigation", "--limit", "1", "--cursor", cursor]);
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stderr)
    );
    let next = String::from_utf8(next.stdout).unwrap();
    assert_ne!(text.lines().nth(1), next.lines().nth(1));
    let bounded = human(&["search", "irrigation", "--candidates", "1", "--limit", "10"]);
    assert!(bounded.status.success());
    let text = String::from_utf8(bounded.stdout).unwrap();
    assert!(text.contains("increase --candidates"), "{text}");
    assert!(!text.contains("--cursor '"));
    let missing = temp.path().join("missing-input.txt");
    let result = human(&["source", "add", missing.to_str().unwrap()]);
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    let error = String::from_utf8(result.stderr).unwrap();
    assert!(
        error.contains("USAGE")
            && error.contains("missing-input.txt")
            && error.contains("readable file"),
        "{error}"
    );
    assert_eq!(ok(temp.path(), &["check"], None)["data"]["error_count"], 0);
}

#[test]
fn human_mutation_summary_distinguishes_preview_prepared_and_committed() {
    let temp = fixture();
    let input = temp.path().join("example.txt");
    fs::write(&input, "Collect rainwater for irrigation.\n").unwrap();
    let run = |extra: &[&str]| {
        let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .arg("--wiki")
            .arg(temp.path())
            .args(extra)
            .args(["source", "add"])
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(run(&["--dry-run"]).contains("source add: preview"));
    let staged = run(&["--stage"]);
    assert!(
        staged.contains("source add: prepared") && staged.contains("changes apply"),
        "{staged}"
    );
    let committed = run(&[]);
    assert!(committed.contains("source add: committed"), "{committed}");
    let source = committed
        .lines()
        .find_map(|line| line.trim().strip_prefix("source: "))
        .expect("committed summary must identify its source");
    assert_eq!(source.len(), 39, "{committed}");
    assert!(
        source.bytes().all(|byte| byte.is_ascii_digit()),
        "{committed}"
    );
    let source_id = lwiki::domain::RecordId::new(source).unwrap();
    let selected = ok(temp.path(), &["read", "--id", source_id.as_str()], None);
    assert_eq!(selected["data"]["record"]["wiki_id"], source);
    assert!(!committed.contains("manifest_hash"));
}

#[cfg(unix)]
#[test]
fn human_staged_commands_keep_vault_selection_and_search_escapes_terminal_controls() {
    let parent = tempfile::Builder::new()
        .prefix("lwiki ' quoted ")
        .tempdir()
        .unwrap();
    let root = parent.path().join("vault space");
    let (exit, _) = invoke(None, &["init", root.to_str().unwrap()], None);
    assert_eq!(exit, 0);
    let input = parent.path().join("notes.txt");
    fs::write(&input, "Orchard safety. \x1b[31mForged color\x1b[0m\n").unwrap();
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(&root)
        .args(["--stage", "source", "add"])
        .arg(&input)
        .args(["--title", "Orchard\nForged heading\t\x1b[31m"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let summary = String::from_utf8(output.stdout).unwrap();
    let command = summary
        .lines()
        .find_map(|line| line.strip_prefix("Apply with: lwiki "))
        .unwrap();
    // The copied command must work outside the vault, including shell quoting.
    let result = Command::new("/bin/sh")
        .args(["-c", &format!("exec \"$1\" {command}"), "lwiki-test"])
        .arg(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .current_dir(parent.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(&root)
        .args(["search", "Orchard"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!output.stdout.contains(&0x1b));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("Orchard\\nForged heading\\t\\u{1b}"),
        "{text}"
    );
    assert!(!text.contains("\nForged heading"));
}

fn doctor_fixture(normalized: bool) -> tempfile::TempDir {
    let temp = fixture();
    fs::write(
        temp.path().join("doctor-page.md"),
        page("Doctor.Page", "Cached apricot content"),
    )
    .unwrap();
    let args = if normalized {
        vec!["index", "rebuild", "--normalized"]
    } else {
        vec!["index", "sync"]
    };
    ok(temp.path(), &args, None);
    temp
}

fn assert_doctor_has_no_audit(doctor: &Value) {
    assert!(doctor["check"].is_null(), "{doctor}");
    for field in [
        "canonical_check_performed",
        "history_check_performed",
        "cache_integrity_check_performed",
        "provider_probe_performed",
    ] {
        assert_eq!(doctor[field], false, "{field}: {doctor}");
    }
    assert_eq!(doctor["canonical_freshness"], "unknown");
    assert_eq!(doctor["unresolved_changes"], json!([]));
    assert_eq!(doctor["incomplete_preparations"], json!([]));
    assert_ne!(doctor["cache_state"], "healthy");
    assert_ne!(doctor["cache_state"], "ready");
}

fn poison_unrelated_doctor_inputs(root: &Path) {
    fs::write(root.join("broken.md"), b"---\nwiki_schema: \"1\"\nwiki_id: Broken.Page\nwiki_kind: page\n---\nMissing required title\n").unwrap();
    fs::create_dir_all(root.join("changes/History.Malformed")).unwrap();
    fs::write(
        root.join("changes/History.Malformed/change.md"),
        b"not a valid retained changeset",
    )
    .unwrap();
    fs::create_dir_all(root.join("changes/Preparation.Malformed")).unwrap();
    fs::write(
        root.join("changes/Preparation.Malformed/outcome.json"),
        b"malformed durable evidence without a manifest",
    )
    .unwrap();
}

#[test]
fn doctor_cli_ignores_unrelated_malformed_inputs_but_explicit_check_refuses() {
    for normalized in [false, true] {
        let temp = doctor_fixture(normalized);
        let root = temp.path();
        poison_unrelated_doctor_inputs(root);
        let inputs = [
            "broken.md",
            "changes/History.Malformed/change.md",
            "changes/Preparation.Malformed/outcome.json",
        ];
        let before: Vec<_> = inputs
            .iter()
            .map(|name| fs::read(root.join(name)).unwrap())
            .collect();
        let doctor = ok(root, &["doctor"], None);
        let doctor = &doctor["data"];
        assert_doctor_has_no_audit(doctor);
        assert_eq!(
            doctor["cache_layout"],
            if normalized { "normalized" } else { "legacy" }
        );
        assert!(doctor["cache_error"].is_null(), "{doctor}");
        assert_eq!(
            doctor["operation_state"],
            if normalized {
                "idle"
            } else {
                "legacy_without_slot"
            }
        );
        if normalized {
            assert_eq!(doctor["cache_state"], "header_available");
            assert_eq!(doctor["header_check_performed"], true);
            assert!(doctor["cache_header_snapshot"].is_object());
            assert_eq!(doctor["parser_compatible"], true);
        }
        for (name, bytes) in inputs.iter().zip(before) {
            assert_eq!(fs::read(root.join(name)).unwrap(), bytes);
        }
        let (exit, checked) = invoke(Some(root), &["check"], None);
        if normalized {
            // The newly added malformed note also makes the selected index
            // stale. Full reconciliation refuses before claiming completeness.
            assert_eq!(exit, 5, "{checked}");
            assert_eq!(checked["error"]["code"], "INDEX_CORRUPT");
            assert_eq!(checked["error"]["details"]["complete"], false);
            assert_eq!(checked["meta"]["partial"], true);
        } else {
            assert_eq!(exit, 9, "{checked}");
            assert!(
                checked["data"]["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|diagnostic| diagnostic["path"] == "broken.md"),
                "{checked}"
            );
        }
    }
}

#[test]
fn doctor_cli_dry_run_probe_preserves_absent_legacy_and_normalized_trees() {
    for layout in ["absent", "legacy", "normalized"] {
        let temp = if layout == "absent" {
            fixture()
        } else {
            doctor_fixture(layout == "normalized")
        };
        let root = temp.path();
        poison_unrelated_doctor_inputs(root);
        let before = tree(root);
        for args in [
            vec!["--dry-run", "doctor"],
            vec!["--dry-run", "doctor", "--probe"],
        ] {
            let response = ok(root, &args, None);
            let doctor = if args.contains(&"--probe") {
                &response["data"]["doctor"]
            } else {
                &response["data"]
            };
            assert_doctor_has_no_audit(doctor);
            assert_eq!(doctor["cache_layout"], "unknown");
            assert_eq!(doctor["cache_state"], "unknown");
            assert_eq!(doctor["operation_state"], "not_checked");
            assert_eq!(doctor["header_check_performed"], false);
            assert!(doctor["cache_header_snapshot"].is_null());
            assert!(doctor["parser_compatible"].is_null());
            if args.contains(&"--probe") {
                assert_eq!(response["data"]["probe"]["dry_run"], true);
            }
            assert_eq!(tree(root), before, "layout={layout}, args={args:?}");
        }
    }
}

#[test]
fn doctor_cli_legacy_wal_without_sidecars_is_present_uninspected_and_read_only() {
    let temp = doctor_fixture(false);
    let root = temp.path();
    let database = root.join(".wiki/cache/index.sqlite");
    let bytes = fs::read(&database).unwrap();
    assert_eq!(&bytes[..16], b"SQLite format 3\0");
    assert_eq!((bytes[18], bytes[19]), (2, 2));
    for suffix in ["-wal", "-shm"] {
        assert!(
            !root
                .join(format!(".wiki/cache/index.sqlite{suffix}"))
                .exists()
        );
    }
    let before = tree(root);
    let response = ok(root, &["doctor"], None);
    let doctor = &response["data"];
    assert_doctor_has_no_audit(doctor);
    assert_eq!(doctor["cache_layout"], "legacy");
    assert_eq!(doctor["cache_state"], "present_uninspected");
    assert_eq!(doctor["header_check_performed"], false);
    assert!(doctor["cache_header_snapshot"].is_null());
    assert!(doctor["parser_compatible"].is_null());
    assert!(doctor["cache_error"].is_null());
    assert!(
        doctor["cache_note"]
            .as_str()
            .is_some_and(|note| !note.is_empty())
    );
    assert_eq!(tree(root), before);
}

#[test]
fn doctor_cli_normalized_missing_or_damaged_authority_is_unavailable_without_legacy_fallback() {
    for missing in [true, false] {
        let temp = doctor_fixture(false);
        let root = temp.path();
        assert!(root.join(".wiki/cache/index.sqlite").exists());
        ok(root, &["index", "rebuild", "--normalized"], None);
        let slot = root.join(".wiki/state/operations.json");
        if missing {
            fs::remove_file(&slot).unwrap();
        } else {
            fs::write(&slot, b"damaged authoritative slot").unwrap();
        }
        let before = tree(root);
        let response = ok(root, &["doctor"], None);
        let doctor = &response["data"];
        assert_doctor_has_no_audit(doctor);
        assert_eq!(doctor["cache_layout"], "normalized");
        assert_eq!(doctor["cache_state"], "unavailable");
        assert_eq!(doctor["operation_state"], "unavailable");
        assert_eq!(doctor["header_check_performed"], false);
        assert!(doctor["cache_header_snapshot"].is_null());
        assert!(doctor["cache_error"].is_object(), "{doctor}");
        assert_eq!(tree(root), before);
    }
}

#[test]
fn doctor_cli_normalized_header_availability_does_not_claim_row_integrity() {
    let temp = doctor_fixture(true);
    let root = temp.path();
    let selected: Value =
        serde_json::from_slice(&fs::read(root.join(".wiki/cache/catalog-current.json")).unwrap())
            .unwrap();
    let database = root.join(format!(
        ".wiki/cache/catalogs/{}.sqlite",
        selected["file_id"].as_str().unwrap()
    ));
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("DELETE FROM documents; PRAGMA journal_mode=DELETE;")
        .unwrap();
    drop(connection);
    let response = ok(root, &["doctor"], None);
    let doctor = &response["data"];
    assert_doctor_has_no_audit(doctor);
    assert_eq!(doctor["cache_layout"], "normalized");
    assert_eq!(doctor["cache_state"], "header_available");
    assert_eq!(doctor["header_check_performed"], true);
    assert_eq!(doctor["parser_compatible"], true);
    assert!(doctor["cache_header_snapshot"].is_object());
    assert!(doctor["cache_error"].is_null());
    assert!(root.join("doctor-page.md").exists());
}

#[test]
fn doctor_cli_reports_active_slot_without_loading_unrelated_change_history() {
    let temp = doctor_fixture(true);
    let root = temp.path();
    poison_unrelated_doctor_inputs(root);
    let slot = root.join(".wiki/state/operations.json");
    let mut authority: Value = serde_json::from_slice(&fs::read(&slot).unwrap()).unwrap();
    let mut intended = authority["publication"].clone();
    intended["epoch"] = json!(intended["epoch"].as_u64().unwrap() + 1);
    authority["revision"] = json!(authority["revision"].as_u64().unwrap() + 1);
    authority["active"] = json!({
        "change": {"change_id":"Change.Doctor.Active", "manifest_hash":Blake3Hash::digest(b"fixed doctor active-slot fixture")},
        "starting":authority["publication"], "intended":intended,
    });
    fs::write(&slot, serde_json::to_vec(&authority).unwrap()).unwrap();
    let before = fs::read(&slot).unwrap();
    let response = ok(root, &["doctor"], None);
    let doctor = &response["data"];
    assert_doctor_has_no_audit(doctor);
    assert_eq!(doctor["cache_layout"], "normalized");
    assert_eq!(doctor["operation_state"], "active");
    assert_eq!(doctor["active_change"], "Change.Doctor.Active");
    assert_eq!(doctor["cache_state"], "header_available");
    assert_eq!(doctor["header_check_performed"], true);
    assert_eq!(fs::read(slot).unwrap(), before);
}

#[test]
fn doctor_cli_human_summary_states_unperformed_checks_and_quotes_selected_vault() {
    let temp = fixture();
    let root = temp.path().join("doctor vault with spaces");
    fs::create_dir(&root).unwrap();
    fs::copy(temp.path().join("WIKI.md"), root.join("WIKI.md")).unwrap();
    fs::write(
        root.join("doctor-page.md"),
        page("Doctor.Human", "Apricot text"),
    )
    .unwrap();
    ok(&root, &["index", "sync"], None);
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .args(["--offline", "--wiki"])
        .arg(&root)
        .arg("doctor")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert!(!rendered.trim_start().starts_with('{'), "{rendered}");
    assert!(
        !rendered.contains("\"canonical_check_performed\":"),
        "{rendered}"
    );
    for expected in [
        "Cache:",
        "legacy",
        "present_uninspected",
        "Canonical, history and cache-integrity audits: not performed.",
        "Canonical freshness: unknown",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?}: {rendered}"
        );
    }
    let check = format!("lwiki --wiki '{}' check", root.display());
    assert!(
        rendered.contains(&check),
        "missing executable selected-vault guidance {check:?}: {rendered}"
    );
}

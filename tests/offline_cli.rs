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

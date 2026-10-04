#[path = "../test_support/paths.rs"]
mod test_paths;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_id: vault_check_cli\nwiki_kind: vault\ntitle: Disposable check CLI fixture\n---\n").unwrap();
    fs::write(
        temp.path().join("page.md"),
        page("Original apricot café 東京 text"),
    )
    .unwrap();
    temp
}
fn page(body: &str) -> String {
    format!(
        "---\nwiki_schema: '1'\nwiki_id: page_check_cli\nwiki_kind: page\ntitle: Check page\nwiki_status: reviewed\naliases: ['Check alias']\ntags: [validation]\n---\n# Overview\n{body}\n"
    )
}
fn invoke(root: &Path, args: &[&str]) -> (i32, Value) {
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(root)
        .args(["--json", "--offline"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.contains(&0x1b));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value = serde_json::from_str(include_str!("../schemas/output-v1.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    assert_eq!(response["meta"]["network_used"], false);
    (output.status.code().unwrap(), response)
}
fn ok(root: &Path, args: &[&str]) -> Value {
    let (exit, response) = invoke(root, args);
    assert_eq!(exit, 0, "{args:?}: {response}");
    assert_eq!(response["ok"], true);
    response
}
fn assert_complete_normalized(data: &Value) {
    assert_eq!(data["canonical_check_performed"], true, "{data}");
    assert_eq!(data["cache_integrity_check_performed"], true, "{data}");
    assert_eq!(data["cache_matches_canonical"], true, "{data}");
    assert_eq!(data["complete"], true, "{data}");
    assert!(data["checked_snapshot"].is_object(), "{data}");
    assert!(data["audit"].is_object(), "{data}");
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
fn normalized_check_cli_detects_external_change_and_passes_after_public_sync() {
    let temp = fixture();
    let root = temp.path();
    let built = ok(root, &["index", "rebuild", "--normalized"]);
    let first = ok(root, &["check"]);
    assert_complete_normalized(&first["data"]);
    assert_eq!(first["data"]["error_count"], 0);
    assert_eq!(first["data"]["diagnostics"], serde_json::json!([]));
    assert_eq!(
        first["data"]["checked_snapshot"],
        built["data"]["report"]["snapshot"]
    );
    let selected = fs::read(root.join(".wiki/cache/catalog-current.json")).unwrap();
    let authority = fs::read(root.join(".wiki/state/operations.json")).unwrap();
    let changed = page("Externally updated blueberry café 東京 text");
    fs::write(root.join("page.md"), &changed).unwrap();
    let (exit, stale) = invoke(root, &["check"]);
    assert_ne!(exit, 0, "stale cached bytes were accepted: {stale}");
    assert_eq!(stale["ok"], false, "{stale}");
    assert_eq!(stale["meta"]["partial"], true, "{stale}");
    assert_eq!(stale["error"]["details"]["complete"], false, "{stale}");
    assert_eq!(
        stale["error"]["details"]["phase"], "canonical_projection",
        "{stale}"
    );
    assert_eq!(
        stale["error"]["details"]["checked_snapshot"], first["data"]["checked_snapshot"],
        "{stale}"
    );
    assert_eq!(
        stale["error"]["details"].get("cache_matches_canonical"),
        Some(&Value::Null),
        "{stale}"
    );
    assert_eq!(fs::read_to_string(root.join("page.md")).unwrap(), changed);
    assert_eq!(
        fs::read(root.join(".wiki/cache/catalog-current.json")).unwrap(),
        selected
    );
    assert_eq!(
        fs::read(root.join(".wiki/state/operations.json")).unwrap(),
        authority
    );
    let synced = ok(root, &["index", "sync"]);
    let current = ok(root, &["check"]);
    assert_complete_normalized(&current["data"]);
    assert_eq!(current["data"]["error_count"], 0);
    assert_eq!(
        current["data"]["checked_snapshot"],
        synced["data"]["report"]["snapshot"]
    );
    assert_ne!(
        current["data"]["checked_snapshot"],
        first["data"]["checked_snapshot"]
    );
}

#[test]
fn normalized_check_cli_separates_faithful_invalid_canonical_diagnostics_from_cache_equality() {
    let temp = fixture();
    let root = temp.path();
    fs::write(root.join("broken.md"), b"---\nwiki_schema: '1'\nwiki_id: page_broken_check\nwiki_kind: page\n---\nMissing required title\n").unwrap();
    ok(root, &["index", "rebuild", "--normalized"]);
    let (exit, checked) = invoke(root, &["check"]);
    assert_eq!(exit, 9, "{checked}");
    assert_eq!(checked["ok"], false);
    // Faithfully indexed semantic diagnostics are a completed failing check.
    assert_eq!(checked["meta"]["partial"], false);
    assert_complete_normalized(&checked["data"]);
    assert!(checked["data"]["error_count"].as_u64().unwrap() > 0);
    assert!(
        checked["data"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["path"] == "broken.md"),
        "{checked}"
    );
}

#[test]
fn legacy_check_cli_keeps_cache_audit_explicitly_unperformed() {
    let temp = fixture();
    let root = temp.path();
    ok(root, &["index", "sync"]);
    let checked = ok(root, &["check"]);
    assert_eq!(checked["data"]["canonical_check_performed"], true);
    assert_eq!(checked["data"]["error_count"], 0);
    assert_eq!(checked["data"]["cache_integrity_check_performed"], false);
    assert!(checked["data"]["cache_matches_canonical"].is_null());
    assert!(checked["data"]["checked_snapshot"].is_null());
    assert!(checked["data"]["audit"].is_null());
}

#[test]
fn check_cli_dry_run_performs_no_scan_audit_or_scratch_and_preserves_all_layouts() {
    for layout in ["absent", "legacy", "normalized"] {
        let temp = fixture();
        let root = temp.path();
        match layout {
            "legacy" => {
                ok(root, &["index", "sync"]);
            }
            "normalized" => {
                ok(root, &["index", "rebuild", "--normalized"]);
            }
            _ => {}
        }
        fs::write(root.join("broken.md"), b"---\nwiki_schema: '1'\nwiki_id: broken_dry_check\nwiki_kind: page\n---\nNo required title\n").unwrap();
        fs::create_dir_all(root.join("changes/Unreadable.Check")).unwrap();
        fs::write(
            root.join("changes/Unreadable.Check/change.md"),
            b"unrelated malformed retained history",
        )
        .unwrap();
        let before = tree(root);
        let preview = ok(root, &["--dry-run", "check"]);
        assert_eq!(preview["meta"]["partial"], false, "{layout}: {preview}");
        let data = &preview["data"];
        assert_eq!(data["canonical_check_performed"], false, "{layout}: {data}");
        assert_eq!(data["cache_integrity_check_performed"], false);
        assert!(data["cache_matches_canonical"].is_null());
        assert_eq!(data["complete"], false);
        assert!(data["checked_snapshot"].is_null());
        assert!(data["audit"].is_null());
        assert_eq!(data["diagnostics"], serde_json::json!([]));
        assert_eq!(data["error_count"], 0);
        assert_eq!(tree(root), before, "dry-run layout {layout}");
    }
}

type SelectedMetadata = (Value, (Option<i64>, Option<String>, Option<String>));
fn selected_metadata(root: &Path) -> SelectedMetadata {
    // Match VaultRoot: macOS temporary roots can use the /var symlink alias.
    let root = fs::canonicalize(root).unwrap();
    let selected: Value =
        serde_json::from_slice(&fs::read(root.join(".wiki/cache/catalog-current.json")).unwrap())
            .unwrap();
    let path = root.join(format!(
        ".wiki/cache/catalogs/{}.sqlite",
        selected["file_id"].as_str().unwrap()
    ));
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    let observations = connection
        .query_row(
            "SELECT audit_epoch,control_hash,dependency_hash FROM catalog_meta WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    (selected, observations)
}

#[test]
fn managed_source_title_and_new_revision_check_current_epoch_without_historical_audit_fields() {
    let temp = fixture();
    let root = temp.path();
    let outside = tempfile::tempdir().unwrap();
    let original = outside.path().join("original source with spaces.txt");
    let original_bytes = b"# Original capture\n\nAmber custody includes 37 apricot folders.\n";
    fs::write(&original, original_bytes).unwrap();
    let added = ok(
        root,
        &[
            "source",
            "add",
            original.to_str().unwrap(),
            "--title",
            "Initial source title",
        ],
    );
    let source = added["data"]["allocated_ids"]["source"].as_str().unwrap();
    let revision = added["data"]["allocated_ids"]["revision"].as_str().unwrap();
    let captured = root.join(format!("sources/{source}/revisions/{revision}/content.md"));
    assert_eq!(fs::read(&captured).unwrap(), original_bytes);
    ok(root, &["index", "rebuild", "--normalized"]);
    let before = ok(root, &["check"]);
    assert_complete_normalized(&before["data"]);
    let (initial_selection, initial_audit) = selected_metadata(root);
    assert!(initial_audit.0.is_some());
    assert!(initial_audit.1.is_some());
    assert!(initial_audit.2.is_some());
    let titled = ok(
        root,
        &[
            "source",
            "refresh",
            source,
            "--file",
            original.to_str().unwrap(),
            "--title",
            "Managed source title",
        ],
    );
    assert_eq!(titled["data"]["reused"], true);
    assert_eq!(titled["data"]["allocated_ids"]["revision"], revision);
    let (after_title, title_audit) = selected_metadata(root);
    assert_eq!(after_title, initial_selection);
    assert_eq!(title_audit, (None, None, None));
    let title_checked = ok(root, &["check"]);
    assert_complete_normalized(&title_checked["data"]);
    assert_eq!(title_checked["data"]["error_count"], 0);
    assert!(
        title_checked["data"]["checked_snapshot"]["generation"]
            .as_u64()
            .unwrap()
            > before["data"]["checked_snapshot"]["generation"]
                .as_u64()
                .unwrap()
    );
    let replacement = outside.path().join("new revision with spaces.txt");
    let new_bytes = b"# Updated capture\n\nViolet custody now includes 19 blueberry folders.\n";
    fs::write(&replacement, new_bytes).unwrap();
    let refreshed = ok(
        root,
        &[
            "source",
            "refresh",
            source,
            "--file",
            replacement.to_str().unwrap(),
        ],
    );
    let new_revision = refreshed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    assert_ne!(new_revision, revision);
    let (after_revision, revision_audit) = selected_metadata(root);
    assert_eq!(after_revision, initial_selection);
    assert_eq!(revision_audit, (None, None, None));
    let checked = ok(root, &["check"]);
    assert_complete_normalized(&checked["data"]);
    assert_eq!(checked["data"]["error_count"], 0);
    let authority: Value =
        serde_json::from_slice(&fs::read(root.join(".wiki/state/operations.json")).unwrap())
            .unwrap();
    assert_eq!(
        checked["data"]["checked_snapshot"]["generation"],
        authority["publication"]["epoch"]
    );
    assert!(
        checked["data"]["checked_snapshot"]["generation"]
            .as_u64()
            .unwrap()
            > title_checked["data"]["checked_snapshot"]["generation"]
                .as_u64()
                .unwrap()
    );
    assert_eq!(fs::read(captured).unwrap(), original_bytes);
    assert_eq!(
        fs::read(root.join(format!(
            "sources/{source}/revisions/{new_revision}/content.md"
        )))
        .unwrap(),
        new_bytes
    );
}

#[test]
fn human_check_summarizes_selected_vault_from_unrelated_cwd_and_labels_dry_run() {
    let temp = fixture();
    let root = temp.path().join("selected check vault with spaces");
    fs::create_dir(&root).unwrap();
    fs::copy(temp.path().join("WIKI.md"), root.join("WIKI.md")).unwrap();
    fs::copy(temp.path().join("page.md"), root.join("page.md")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    // Invalid unrelated cwd input must never become the explicit vault's check.
    fs::write(
        outside.path().join("unrelated.md"),
        b"invalid outside canonical input",
    )
    .unwrap();
    let human = |args: &[&str]| {
        let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .current_dir(outside.path())
            .arg("--wiki")
            .arg(&root)
            .arg("--offline")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.contains(&0x1b));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.trim_start().starts_with('{'), "{text}");
        assert!(!text.contains("\"cache_matches_canonical\":"), "{text}");
        text
    };
    ok(&root, &["index", "sync"]);
    let legacy = human(&["check"]);
    assert!(legacy.contains("Canonical check complete"), "{legacy}");
    assert!(
        legacy.contains("Cache integrity was not checked (legacy catalog)"),
        "{legacy}"
    );
    ok(&root, &["index", "rebuild", "--normalized"]);
    let normalized = human(&["check"]);
    assert!(
        normalized.contains("Check complete: canonical documents and selected index agree"),
        "{normalized}"
    );
    let before = tree(&root);
    let preview = human(&["--dry-run", "check"]);
    assert!(
        preview.contains("Dry run: canonical and cache checks were not performed"),
        "{preview}"
    );
    assert_eq!(tree(&root), before);
}

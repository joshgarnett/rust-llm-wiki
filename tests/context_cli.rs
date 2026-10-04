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
const PAGE: &str = "page_00000000-0000-7000-8000-000000000011";
const SOURCE_A: &str = "source_00000000-0000-7000-8000-000000000005";
const SOURCE_B: &str = "source_00000000-0000-7000-8000-000000000006";
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

// A read-only WAL connection may create SQLite coordination sidecars. This
// allows those exact effects, while keeping canonical/operational files, the
// main database and existing WAL bytes AND timestamps immutable.
fn assert_index_read_unchanged(
    root: &Path,
    before: &BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>,
) {
    let mut after = tree(root);
    let mut expected = before.clone();
    let shm = PathBuf::from(".wiki/cache/index.sqlite-shm");
    if after.contains_key(&shm) {
        assert!(
            fs::symlink_metadata(root.join(&shm))
                .unwrap()
                .file_type()
                .is_file()
        );
    }
    after.remove(&shm);
    expected.remove(&shm);
    let wal = PathBuf::from(".wiki/cache/index.sqlite-wal");
    if !expected.contains_key(&wal)
        && let Some((bytes, _)) = after.remove(&wal)
    {
        assert!(
            fs::symlink_metadata(root.join(&wal))
                .unwrap()
                .file_type()
                .is_file()
        );
        assert!(
            bytes.is_empty(),
            "a read-only connection must not create WAL frames"
        );
    }
    let cache = PathBuf::from(".wiki/cache");
    if let (Some((_, timestamp)), Some((_, old))) = (after.get_mut(&cache), expected.get(&cache)) {
        *timestamp = *old;
    }
    let changed: Vec<_> = after
        .keys()
        .chain(expected.keys())
        .filter(|path| after.get(*path) != expected.get(*path))
        .collect();
    assert!(
        changed.is_empty(),
        "index read changed protected paths: {changed:?}"
    );
}

const INDEXED_BODY: &str = "# Independent capture\n\nindexedprobe: The cedar cabinet contains 37 blue folders.\nThe access phrase is café 東京 🦀.\n";

fn add_context_source(root: &Path, body: &str) -> (String, String) {
    let input = tempfile::NamedTempFile::new().unwrap();
    fs::write(input.path(), body).unwrap();
    let added = ok(
        root,
        &[
            "source",
            "add",
            input.path().to_str().unwrap(),
            "--media-type",
            "text/markdown",
            "--title",
            "Independent capture",
        ],
    );
    (
        added["data"]["allocated_ids"]["source"]
            .as_str()
            .unwrap()
            .into(),
        added["data"]["allocated_ids"]["revision"]
            .as_str()
            .unwrap()
            .into(),
    )
}

#[test]
fn indexed_evidence_requires_an_explicit_cache_and_never_creates_one() {
    let f = fixture();
    let before = tree(f.path());
    for extra in [vec![], vec!["--no-sync"]] {
        let mut args = vec!["context", "uses", "--scope", "indexed-evidence"];
        args.extend(extra);
        let (exit, rejected) = invoke(f.path(), &args);
        assert_ne!(exit, 0, "{rejected}");
        assert_eq!(rejected["error"]["code"], "OFFLINE_UNAVAILABLE");
        assert!(rejected["meta"]["freshness"].is_null());
        assert_eq!(tree(f.path()), before);
    }
    assert!(!f.path().join(".wiki/cache/index.sqlite").exists());
    ok(f.path(), &["index", "sync"]);
    assert_eq!(
        ok(
            f.path(),
            &["context", "uses", "--scope", "indexed-evidence"]
        )["meta"]["freshness"],
        "indexed_evidence"
    );
}

#[test]
fn indexed_evidence_is_read_only_scoped_and_independently_cited() {
    use lwiki::domain::{Blake3Hash, ByteSpan};
    let f = fixture();
    let (source, revision) = add_context_source(f.path(), INDEXED_BODY);
    let synced = ok(f.path(), &["index", "sync"]);
    let prefix = format!("sources/{source}/revisions/{revision}/");
    let before = tree(f.path());
    let result = ok(
        f.path(),
        &[
            "context",
            "indexedprobe",
            "--scope",
            "indexed-evidence",
            "--no-sync",
            "--source-id",
            &source,
            "--path-prefix",
            &prefix,
        ],
    );
    assert_index_read_unchanged(f.path(), &before);
    assert_eq!(result["meta"]["freshness"], "indexed_evidence");
    let verification = &result["data"]["verification"];
    assert_eq!(verification["mode"], "indexed_evidence");
    assert_eq!(verification["evidence_domain"], "captured_sources");
    assert_eq!(verification["global_membership_verified"], false);
    assert_eq!(
        verification["discovery_generation"],
        synced["data"]["report"]["snapshot"]["generation"]
    );
    assert_eq!(
        verification["discovery_generation"],
        result["meta"]["index_generation"]
    );
    assert_eq!(verification["verified_at"], result["meta"]["verified_at"]);
    assert!(verification["verified_at"].is_string());
    assert!(verification["catalog_rows_decoded"].as_u64().unwrap() > 0);
    assert!(verification["catalog_bytes_decoded"].as_u64().unwrap() > 0);
    assert!(result["data"]["bundles"].as_array().unwrap().is_empty());
    let passages = result["data"]["passages"].as_array().unwrap();
    assert!(!passages.is_empty());
    for passage in passages {
        let span: ByteSpan = serde_json::from_value(passage["span"].clone()).unwrap();
        let quote = span.slice(INDEXED_BODY).unwrap();
        assert_eq!(passage["text"], quote);
        assert_eq!(passage["label"], "captured_source");
        assert!(
            passage["locator"]["path"]
                .as_str()
                .unwrap()
                .starts_with(&prefix)
        );
        let citations = passage["citations"].as_array().unwrap();
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0]["kind"], "source");
        let reference = &citations[0]["reference"];
        assert_eq!(reference["source_id"], source);
        assert_eq!(reference["source_revision"], revision);
        assert_eq!(reference["span"], passage["span"]);
        assert_eq!(
            reference["quote_hash"],
            Blake3Hash::digest(quote.as_bytes()).as_str()
        );
    }
    let text = result["data"]["text"].as_str().unwrap();
    assert!(text.contains("37 blue folders") && text.contains("café 東京 🦀"));
    assert!(text.contains("global membership, identity uniqueness and completeness not verified"));
    assert_eq!(result["data"]["usage"]["rendered_bytes"], text.len());
    assert_eq!(
        result["data"]["usage"]["estimated_tokens"],
        text.len().div_ceil(4)
    );
    assert!(text.len() <= 12000);
    let human = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(f.path())
        .args([
            "--offline",
            "context",
            "indexedprobe",
            "--scope",
            "indexed-evidence",
            "--no-sync",
            "--source-id",
            &source,
            "--path-prefix",
            &prefix,
        ])
        .output()
        .unwrap();
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    assert_eq!(human.stdout, text.as_bytes());
    assert!(
        String::from_utf8(human.stderr)
            .unwrap()
            .contains("Freshness: indexed_evidence")
    );
    assert_index_read_unchanged(f.path(), &before);
}

#[test]
fn indexed_evidence_does_not_discover_new_sources_but_default_current_does() {
    let f = fixture();
    ok(f.path(), &["index", "sync"]);
    let other = fixture();
    let (source, _) = add_context_source(other.path(), INDEXED_BODY);
    // Copy canonical captured bytes without changing the tested vault's cache.
    copy(
        &other.path().join(format!("sources/{source}")),
        &f.path().join(format!("sources/{source}")),
    );
    let before = tree(f.path());
    let indexed = ok(
        f.path(),
        &[
            "context",
            "indexedprobe",
            "--scope",
            "indexed-evidence",
            "--source-id",
            &source,
        ],
    );
    assert_index_read_unchanged(f.path(), &before);
    assert_eq!(indexed["meta"]["freshness"], "indexed_evidence");
    assert_eq!(
        indexed["data"]["verification"]["global_membership_verified"],
        false
    );
    assert!(indexed["data"]["passages"].as_array().unwrap().is_empty());
    let current = ok(
        f.path(),
        &["context", "indexedprobe", "--source-id", &source],
    );
    assert_eq!(current["meta"]["freshness"], "verified_snapshot");
    assert!(
        current["data"]["text"]
            .as_str()
            .unwrap()
            .contains("37 blue folders")
    );
}

#[test]
fn indexed_evidence_refuses_changed_selected_capture_without_implicit_sync() {
    for changed in ["source.md", "revision.md", "original.bin", "content.md"] {
        let f = fixture();
        let (source, revision) = add_context_source(f.path(), INDEXED_BODY);
        ok(f.path(), &["index", "sync"]);
        let base = f.path().join(format!("sources/{source}"));
        let path = if changed == "source.md" {
            base.join(changed)
        } else {
            base.join(format!("revisions/{revision}/{changed}"))
        };
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b"\nchanged after index sync\n");
        fs::write(&path, bytes).unwrap();
        let before = tree(f.path());
        for extra in [vec![], vec!["--no-sync"]] {
            let mut args = vec![
                "context",
                "indexedprobe",
                "--scope",
                "indexed-evidence",
                "--source-id",
                &source,
            ];
            args.extend(extra);
            let (exit, rejected) = invoke(f.path(), &args);
            assert_ne!(exit, 0, "{changed}: {rejected}");
            assert!(
                matches!(
                    rejected["error"]["code"].as_str(),
                    Some("FRESHNESS_CONFLICT" | "SOURCE_INTEGRITY")
                ),
                "{changed}: {rejected}"
            );
            assert!(rejected["meta"]["freshness"].is_null());
            assert!(rejected["meta"]["verified_at"].is_null());
            assert_index_read_unchanged(f.path(), &before);
        }
    }
}

#[test]
fn indexed_evidence_rejects_unsupported_requests_without_creating_state() {
    let f = fixture();
    let before = tree(f.path());
    for flags in [
        vec!["--mode", "semantic"],
        vec!["--mode", "hybrid"],
        vec!["--mode", "literal"],
        vec!["--target", "graph"],
        vec!["--target", "combined"],
        vec!["--kind", "source"],
        vec!["--status", "active"],
        vec!["--tag", "test"],
        vec!["--include-proposed"],
        vec!["--include-historical"],
        vec!["--prepare-selection"],
        vec!["--selection", "/nonexistent/indexed-evidence-reply.json"],
    ] {
        let mut args = vec!["context", "uses", "--scope", "indexed-evidence"];
        args.extend(flags);
        let (exit, rejected) = invoke(f.path(), &args);
        assert_eq!(exit, 2, "{args:?}: {rejected}");
        assert_eq!(rejected["error"]["code"], "USAGE");
        assert!(
            rejected["error"]["message"]
                .as_str()
                .unwrap()
                .contains("indexed-evidence"),
            "{rejected}"
        );
        assert!(rejected["meta"]["freshness"].is_null());
        assert_eq!(tree(f.path()), before);
    }
}

#[test]
fn indexed_evidence_budgets_rendered_authority_and_selected_proof() {
    let f = fixture();
    let (source, _) = add_context_source(f.path(), INDEXED_BODY);
    ok(f.path(), &["index", "sync"]);
    let before = tree(f.path());
    let result = ok(
        f.path(),
        &[
            "context",
            "indexedprobe",
            "--scope",
            "indexed-evidence",
            "--source-id",
            &source,
            "--max-bytes",
            "500",
            "--max-tokens",
            "125",
            "--instruction-bytes",
            "40",
            "--instruction-tokens",
            "10",
        ],
    );
    let text = result["data"]["text"].as_str().unwrap();
    assert!(text.len() <= 460);
    assert!(text.len().div_ceil(4) <= 115);
    assert_eq!(result["data"]["usage"]["rendered_bytes"], text.len());
    assert_eq!(result["data"]["usage"]["reserved_bytes"], 40);
    assert_eq!(result["data"]["usage"]["reserved_tokens"], 10);
    let (exit, rejected) = invoke(
        f.path(),
        &[
            "context",
            "indexedprobe",
            "--scope",
            "indexed-evidence",
            "--source-id",
            &source,
            "--verification-max-bytes",
            "1",
        ],
    );
    assert_eq!(exit, 7, "{rejected}");
    assert_eq!(rejected["error"]["code"], "BUDGET_EXCEEDED");
    assert!(rejected["meta"]["freshness"].is_null());
    assert_index_read_unchanged(f.path(), &before);
}

#[test]
fn legacy_cache_missing_bounded_indexes_requires_rebuild_only_for_indexed_evidence() {
    for index in [
        "source_document_ids",
        "source_revision_ids",
        "source_document_titles",
    ] {
        let f = fixture();
        let (source, _) = add_context_source(f.path(), INDEXED_BODY);
        ok(f.path(), &["index", "sync"]);
        let db = rusqlite::Connection::open(f.path().join(".wiki/cache/index.sqlite")).unwrap();
        db.execute_batch(&format!("DROP INDEX {index}")).unwrap();
        drop(db);
        let before = tree(f.path());
        let (exit, rejected) = invoke(
            f.path(),
            &[
                "context",
                "indexedprobe",
                "--scope",
                "indexed-evidence",
                "--source-id",
                &source,
            ],
        );
        assert_ne!(exit, 0, "{index}: {rejected}");
        assert_eq!(
            rejected["error"]["code"], "CAPABILITY_UNAVAILABLE",
            "{index}: {rejected}"
        );
        assert!(
            rejected["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("index rebuild")
        );
        assert!(rejected["meta"]["freshness"].is_null());
        assert_index_read_unchanged(f.path(), &before);
        let strict = ok(
            f.path(),
            &["context", "indexedprobe", "--source-id", &source],
        );
        assert_eq!(strict["meta"]["freshness"], "verified_snapshot");
        assert!(
            strict["data"]["text"]
                .as_str()
                .unwrap()
                .contains("37 blue folders")
        );
        ok(f.path(), &["index", "rebuild"]);
        assert_eq!(
            ok(
                f.path(),
                &["context", "indexedprobe", "--scope", "indexed-evidence"]
            )["meta"]["freshness"],
            "indexed_evidence"
        );
    }
}

#[test]
fn indexed_evidence_reads_uncheckpointed_wal_without_mutating_it() {
    let f = fixture();
    ok(f.path(), &["index", "sync"]);
    let cache = f.path().join(".wiki/cache/index.sqlite");
    let keeper = rusqlite::Connection::open(&cache).unwrap();
    keeper
        .execute_batch("PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    let _: i64 = keeper
        .query_row(
            "SELECT published_gen FROM index_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let body = "# WAL-only capture\n\nwalonlyprobe: The amber drawer contains 19 cards.\n";
    let (source, _) = add_context_source(f.path(), body);
    let wal = f.path().join(".wiki/cache/index.sqlite-wal");
    assert!(fs::metadata(&wal).unwrap().len() > 32);
    // Independently inspect only the main file: the new source is committed in
    // WAL and is not yet present in the checkpointed database.
    let isolated = tempfile::tempdir().unwrap();
    fs::copy(&cache, isolated.path().join("main.sqlite")).unwrap();
    let main_only = rusqlite::Connection::open(isolated.path().join("main.sqlite")).unwrap();
    let count: i64 = main_only
        .query_row(
            "SELECT count(*) FROM records WHERE id=?1",
            [&source],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "fixture must need committed WAL content");
    let before = tree(f.path());
    let result = ok(
        f.path(),
        &[
            "context",
            "walonlyprobe",
            "--scope",
            "indexed-evidence",
            "--source-id",
            &source,
        ],
    );
    assert!(
        result["data"]["text"]
            .as_str()
            .unwrap()
            .contains("19 cards")
    );
    assert_index_read_unchanged(f.path(), &before);
    drop(keeper);
}

#[test]
fn context_cli_preserves_scopes_stances_and_exact_text_budget() {
    let f = fixture();
    let graph = ok(
        f.path(),
        &[
            "context",
            "uses",
            "--target",
            "graph",
            "--strategy",
            "relationship",
        ],
    );
    assert_eq!(graph["meta"]["freshness"], "verified_snapshot");
    assert!(graph["meta"]["verified_at"].is_string());
    let data = &graph["data"];
    let bundles = data["bundles"].as_array().unwrap();
    assert!(
        bundles
            .iter()
            .any(|b| b["assertion"]["record_id"] == FORWARD && b["disputed"] == true)
    );
    let passages = data["passages"].as_array().unwrap();
    assert!(
        passages
            .iter()
            .flat_map(|p| p["citations"].as_array().unwrap())
            .any(|c| c["kind"] == "assertion")
    );
    assert!(
        passages
            .iter()
            .flat_map(|p| p["contributors"].as_array().unwrap())
            .any(|c| c["stance"] == "contradicts")
    );
    let text = data["text"].as_str().unwrap();
    assert!(!text.is_empty());
    assert_eq!(
        data["usage"]["rendered_bytes"].as_u64().unwrap() as usize,
        text.len()
    );
    assert_eq!(
        data["usage"]["estimated_tokens"].as_u64().unwrap() as usize,
        text.len().div_ceil(4)
    );
    assert!(text.len() <= 12000);
    assert!(text.len().div_ceil(4) <= 3000);
    let snapshot = ok(
        f.path(),
        &[
            "context",
            "uses",
            "--target",
            "graph",
            "--strategy",
            "relationship",
            "--scope",
            "snapshot",
            "--no-sync",
        ],
    );
    assert_eq!(snapshot["meta"]["freshness"], "index_snapshot");
    assert!(snapshot["meta"]["verified_at"].is_null());
    assert!(
        snapshot["data"]["passages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["citations"].as_array().unwrap().is_empty())
    );
    let historical = ok(
        f.path(),
        &[
            "context",
            "uses",
            "--target",
            "graph",
            "--strategy",
            "relationship",
            "--scope",
            "historical",
        ],
    );
    assert_eq!(historical["meta"]["freshness"], "verified_snapshot");
    let note = ok(f.path(), &["context", PAGE, "--kind", "page"]);
    let passages = note["data"]["passages"].as_array().unwrap();
    assert!(!passages.is_empty());
    assert!(
        passages
            .iter()
            .all(|p| p["label"] == "note_text" && p["citations"].as_array().unwrap().is_empty())
    );
}

#[test]
fn source_scope_limits_graph_and_context_evidence_with_counterevidence_counts() {
    let f = fixture();
    let graph = ok(
        f.path(),
        &[
            "graph",
            "query",
            "uses",
            "--strategy",
            "relationship",
            "--source-id",
            SOURCE_B,
        ],
    );
    let forward = graph["data"]["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["record_ref"]["record_id"] == FORWARD)
        .unwrap();
    assert!(
        forward["support"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["source"]["source_id"] == SOURCE_B)
    );
    assert!(forward["contradictions"].as_array().unwrap().is_empty());
    assert!(forward["omitted_contradictions"].as_u64().unwrap() >= 1);
    assert!(forward["omitted_support"].as_u64().unwrap() >= 1);
    assert!(
        graph["data"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("outside the selected sources"))
    );

    let context = ok(
        f.path(),
        &[
            "context",
            "uses",
            "--target",
            "graph",
            "--strategy",
            "relationship",
            "--source-id",
            SOURCE_B,
        ],
    );
    let bundle = context["data"]["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["assertion"]["record_id"] == FORWARD)
        .unwrap();
    assert!(bundle["omitted_contradictions"].as_u64().unwrap() >= 1);
    assert!(
        context["data"]["passages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|p| p["citations"].as_array().unwrap())
            .all(|c| c["reference"]["source_id"] == SOURCE_B)
    );
    assert!(!context["data"]["text"].as_str().unwrap().contains(SOURCE_A));
}

#[test]
fn requested_navigation_is_labeled_and_budget_omissions_are_visible() {
    let f = fixture();
    let page = f.path().join("knowledge/entities/north_lab.md");
    let mut bytes = fs::read(&page).unwrap();
    bytes.extend_from_slice(b"\n[[south_lab.md]]\n");
    fs::write(&page, bytes).unwrap();
    let nav = ok(
        f.path(),
        &[
            "context",
            "North Lab",
            "--target",
            "graph",
            "--strategy",
            "entity",
            "--navigation",
            "--verification-max-elapsed-ms",
            "30000",
        ],
    );
    let text = nav["data"]["text"].as_str().unwrap();
    assert!(
        text.contains("[navigation; PageLink; no asserted relationship]"),
        "{text}"
    );
    assert!(text.contains("north_lab.md") && text.contains("south_lab.md"));
    assert_eq!(nav["data"]["usage"]["rendered_bytes"], text.len());
    assert!(nav["data"]["usage"]["graph_bytes"].as_u64().unwrap() > 0);
    let small = ok(
        f.path(),
        &[
            "context",
            "North Lab",
            "--target",
            "graph",
            "--strategy",
            "entity",
            "--navigation",
            "--max-bytes",
            "100",
            "--max-tokens",
            "25",
            "--verification-max-elapsed-ms",
            "30000",
        ],
    );
    assert!(
        small["data"]["omissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["reason"].as_str().unwrap().starts_with("navigation_"))
    );
    assert_eq!(small["meta"]["partial"], true);
    assert!(small["data"]["text"].as_str().unwrap().len() <= 100);
}

#[test]
fn executable_m1_context_tracks_rename_revalidation_withdrawal_and_cache_rebuild() {
    let f = fixture();
    let root = f.path();
    let source = "source_00000000-0000-7000-8000-000000000005";
    let mirror = "source_00000000-0000-7000-8000-000000000006";
    let evidence = "evidence_00000000-0000-7000-8000-000000000012";
    let graph_args = [
        "context",
        FORWARD,
        "--target",
        "graph",
        "--strategy",
        "relationship",
    ];
    let before = ok(root, &graph_args);
    assert_eq!(before["data"]["bundles"].as_array().unwrap().len(), 1);
    let entity = lwiki::records::parse_note(
        &fs::read(root.join("knowledge/entities/north_lab.md")).unwrap(),
    );
    let entity_id = entity.canonical.as_ref().unwrap().id().as_str();
    let read = ok(root, &["read", "--id", entity_id]);
    let hash = read["data"]["hash"].as_str().unwrap();
    ok(
        root,
        &[
            "page",
            "rename",
            entity_id,
            "--to",
            "knowledge/entities/renamed_lab.md",
            "--if-match",
            hash,
        ],
    );
    assert!(!root.join("knowledge/entities/north_lab.md").exists());
    assert!(root.join("knowledge/entities/renamed_lab.md").is_file());
    assert_eq!(
        ok(root, &graph_args)["data"]["bundles"][0]["assertion"]["record_id"],
        FORWARD
    );

    let input = tempfile::NamedTempFile::new().unwrap();
    let original = fs::read(root.join(format!(
        "sources/{source}/revisions/revision_00000000-0000-7000-8000-000000000008/content.md"
    )))
    .unwrap();
    let mut successor = b"New heading\n".to_vec();
    successor.extend(original);
    fs::write(input.path(), successor).unwrap();
    let refreshed = ok(
        root,
        &[
            "source",
            "refresh",
            source,
            "--file",
            input.path().to_str().unwrap(),
        ],
    );
    let revision = refreshed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    let stale = ok(root, &graph_args);
    assert!(
        stale["data"]["passages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|p| p["citations"].as_array().unwrap())
            .all(|c| c["reference"]["evidence_id"] != evidence)
    );
    let read = ok(root, &["read", "--id", evidence]);
    let hash = read["data"]["hash"].as_str().unwrap();
    let staged = ok(
        root,
        &[
            "evidence",
            "revalidate",
            evidence,
            "--to-revision",
            revision,
            "--if-match",
            hash,
        ],
    );
    assert_eq!(staged["data"]["status"], "prepared");
    let change = staged["data"]["change"]["change_id"].as_str().unwrap();
    ok(root, &["changes", "show", change]);
    ok(root, &["changes", "apply", change]);
    let fresh = ok(root, &graph_args);
    assert!(
        fresh["data"]["passages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|p| p["citations"].as_array().unwrap())
            .any(|c| c["reference"]["source_revision"] == revision)
    );
    ok(
        root,
        &[
            "source",
            "withdraw",
            mirror,
            "--reason",
            "Independent support removed",
        ],
    );
    assert_eq!(
        ok(root, &graph_args)["data"]["bundles"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    ok(
        root,
        &[
            "source",
            "withdraw",
            source,
            "--reason",
            "Last support removed",
        ],
    );
    assert!(
        ok(root, &graph_args)["data"]["bundles"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut historical_args = graph_args.to_vec();
    historical_args.extend(["--scope", "historical"]);
    let historical = ok(root, &historical_args);
    assert!(!historical["data"]["bundles"].as_array().unwrap().is_empty());
    let root_handle = lwiki::vault::VaultRoot::explicit(root).unwrap();
    let vault_fs = lwiki::vault::VaultFs::new(root_handle);
    let vault_id = lwiki::changes::ChangeEngine::new(vault_fs.clone())
        .unwrap()
        .vault_id()
        .clone();
    let canonical_before = lwiki::catalog::scan::scan(&vault_fs, &vault_id).unwrap();
    for name in ["index.sqlite", "index.sqlite-wal", "index.sqlite-shm"] {
        let path = root.join(".wiki/cache").join(name);
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    ok(root, &["index", "rebuild"]);
    assert_eq!(
        lwiki::catalog::scan::scan(&vault_fs, &vault_id).unwrap(),
        canonical_before
    );
    let rebuilt = ok(root, &historical_args);
    assert_eq!(rebuilt["data"]["bundles"], historical["data"]["bundles"]);
    assert_eq!(rebuilt["data"]["passages"], historical["data"]["passages"]);
    ok(root, &["recover"]);
    let doctor = ok(root, &["doctor"]);
    assert_eq!(doctor["data"]["history_check_performed"], false);
    assert!(doctor["data"]["check"].is_null());
    assert!(
        doctor["data"]["unresolved_changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(doctor["data"]["provider_probe_performed"], false);
}

#[test]
fn context_cli_dry_run_is_pure_and_bad_requests_never_claim_verification() {
    let f = fixture();
    let before = tree(f.path());
    let plan = ok(
        f.path(),
        &["--dry-run", "context", "uses", "--target", "combined"],
    );
    assert_eq!(plan["data"]["cache_state_unknown"], true);
    assert!(plan["data"]["context"].is_null());
    assert!(plan["meta"]["freshness"].is_null());
    assert_eq!(tree(f.path()), before);
    assert!(!f.path().join(".wiki/cache/index.sqlite").exists());
    ok(f.path(), &["index", "sync"]);
    let before = tree(f.path());
    ok(
        f.path(),
        &[
            "--dry-run",
            "context",
            "uses",
            "--target",
            "graph",
            "--scope",
            "snapshot",
        ],
    );
    assert_eq!(tree(f.path()), before);
    for flags in [
        vec!["--max-bytes", "16385"],
        vec!["--max-tokens", "4097"],
        vec!["--limit", "51"],
        vec!["--target", "graph", "--depth", "3"],
        vec!["--instruction-bytes", "12001"],
        vec!["--seed", "unknown"],
    ] {
        let mut args = vec!["context", "uses"];
        args.extend(flags);
        let (exit, e) = invoke(f.path(), &args);
        assert_eq!(exit, 2, "{args:?}: {e}");
        assert!(e["meta"]["freshness"].is_null());
    }
    let (exit, e) = invoke(f.path(), &["context", "uses", "--no-sync"]);
    assert_eq!(exit, 4);
    assert_eq!(e["error"]["code"], "FRESHNESS_CONFLICT");
    let (exit, e) = invoke(
        f.path(),
        &["context", "uses", "--verification-max-files", "1"],
    );
    assert_eq!(exit, 7, "{e}");
    assert_eq!(e["error"]["code"], "BUDGET_EXCEEDED");
    assert!(e["meta"]["freshness"].is_null());
}

#[test]
fn context_cli_reservations_and_bundle_omissions_are_visible() {
    let f = fixture();
    let omitted = ok(
        f.path(),
        &[
            "context",
            "uses",
            "--target",
            "graph",
            "--strategy",
            "relationship",
            "--max-bytes",
            "300",
            "--max-tokens",
            "75",
        ],
    );
    assert!(omitted["data"]["bundles"].as_array().unwrap().is_empty());
    assert!(!omitted["data"]["omissions"].as_array().unwrap().is_empty());
    assert_eq!(omitted["meta"]["partial"], true);
    assert!(omitted["data"]["text"].as_str().unwrap().len() <= 300);
    let combined = ok(
        f.path(),
        &[
            "context",
            "uses",
            "--target",
            "combined",
            "--instruction-bytes",
            "1000",
            "--instruction-tokens",
            "250",
            "--output-bytes",
            "1000",
            "--output-tokens",
            "250",
        ],
    );
    let usage = &combined["data"]["usage"];
    assert_eq!(usage["reserved_bytes"], 2000);
    assert_eq!(usage["reserved_tokens"], 500);
    assert!(usage["rendered_bytes"].as_u64().unwrap() <= 10000);
    assert!(usage["estimated_tokens"].as_u64().unwrap() <= 2500);
    assert!(usage["graph_bytes"].as_u64().unwrap() <= 5000);
    assert!(usage["graph_estimated_tokens"].as_u64().unwrap() <= 1250);
}

#[test]
fn context_human_stdout_is_exact_budgeted_authority() {
    let f = fixture();
    use clap::Parser;
    let args = lwiki::cli::Arguments::try_parse_from([
        "lwiki",
        "--wiki",
        f.path().to_str().unwrap(),
        "--offline",
        "context",
        PAGE,
        "--kind",
        "page",
    ])
    .unwrap();
    let (envelope, exit) = lwiki::cli::execute(&args);
    assert_eq!(exit, 0);
    let mut bytes = Vec::new();
    lwiki::cli::present(&envelope, lwiki::cli::OutputFormat::Human, &mut bytes).unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        envelope.data["text"].as_str().unwrap()
    );
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(f.path())
        .args(["--offline", "context", PAGE, "--kind", "page"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.len() <= 12000);
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("North Lab uses South Lab")
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Freshness: verified_snapshot")
    );
}

#[test]
fn empty_and_bounded_human_context_explain_the_result_on_stderr() {
    let f = fixture();
    let run = |query: &str, extra: &[&str]| {
        Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .arg("--wiki")
            .arg(f.path())
            .args(["--offline", "context", query])
            .args(extra)
            .output()
            .unwrap()
    };
    let missing = run("zqxvnonexistent", &[]);
    assert!(missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("No matching context")
    );
    let bounded = run(PAGE, &["--max-bytes", "10"]);
    assert!(bounded.status.success());
    assert!(bounded.stdout.len() <= 10);
    let stderr = String::from_utf8(bounded.stderr).unwrap();
    assert!(
        stderr.contains("No context fits") && stderr.contains("Omitted"),
        "{stderr}"
    );
}

#[test]
fn context_default_retains_surrounding_explanation_and_explicit_excerpt_bounds_win() {
    let f = fixture();
    let text = format!(
        "---\nwiki_schema: \"1\"\nwiki_id: page_00000000-0000-7000-8000-000000000099\nwiki_kind: page\ntitle: Explanation\nwiki_status: reviewed\n---\n# Explanation\n\nwaterproof {}\n",
        "Useful surrounding explanation with UTF-8 café. ".repeat(35)
    );
    std::fs::create_dir_all(f.path().join("knowledge/pages")).unwrap();
    std::fs::write(f.path().join("knowledge/pages/explanation.md"), text).unwrap();
    let run = |extra: &[&str]| {
        let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .arg("--wiki")
            .arg(f.path())
            .args(["--json", "--offline", "context", "waterproof"])
            .args(extra)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let default = run(&[]);
    let text = default["data"]["passages"][0]["text"].as_str().unwrap();
    assert!(text.len() > 240 && text.len() <= 1024);
    let explicit = run(&["--excerpt-bytes", "240"]);
    assert!(
        explicit["data"]["passages"][0]["text"]
            .as_str()
            .unwrap()
            .len()
            <= 240
    );
    assert!(default["data"]["text"].as_str().unwrap().len() <= 12000);
}

#[test]
fn explicit_16k_context_budget_preserves_token_and_reservation_bounds() {
    let f = fixture();
    fs::create_dir_all(f.path().join("knowledge/pages")).unwrap();
    for n in 100..120 {
        fs::write(
            f.path().join(format!("knowledge/pages/budget-{n}.md")),
            format!(
                "---\nwiki_schema: \"1\"\nwiki_id: page_00000000-0000-7000-8000-{n:012}\nwiki_kind: page\ntitle: Budget {n}\nwiki_status: reviewed\n---\n# Budget {n}\n\nbudgetprobe {}\n",
                "Evidence café numbered example. ".repeat(27)
            ),
        )
        .unwrap();
    }
    let run = |bytes: &str, tokens: &str, extra: &[&str]| {
        let mut args = vec![
            "context",
            "budgetprobe",
            "--kind",
            "page",
            "--limit",
            "20",
            "--max-bytes",
            bytes,
            "--max-tokens",
            tokens,
            "--verification-max-elapsed-ms",
            "30000",
        ];
        args.extend_from_slice(extra);
        invoke(f.path(), &args)
    };
    let (exit, large) = run("16384", "4096", &[]);
    assert_eq!(exit, 0, "{large}");
    let text = large["data"]["text"].as_str().unwrap();
    assert!(text.len() > 12000 && text.len() <= 16384, "{}", text.len());
    assert_eq!(large["data"]["usage"]["rendered_bytes"], text.len());
    assert!(large["data"]["usage"]["estimated_tokens"].as_u64().unwrap() <= 4096);

    let (exit, token_limited) = run("16384", "1500", &[]);
    assert_eq!(exit, 0, "{token_limited}");
    assert!(token_limited["data"]["text"].as_str().unwrap().len() <= 6000);
    assert!(
        token_limited["data"]["passages"].as_array().unwrap().len()
            < large["data"]["passages"].as_array().unwrap().len()
    );

    let (exit, reserved) = run(
        "16384",
        "4096",
        &[
            "--instruction-bytes",
            "4000",
            "--instruction-tokens",
            "1000",
        ],
    );
    assert_eq!(exit, 0, "{reserved}");
    assert!(reserved["data"]["text"].as_str().unwrap().len() <= 12384);
    assert_eq!(reserved["data"]["usage"]["reserved_bytes"], 4000);
    for (bytes, tokens) in [("16385", "4096"), ("16384", "4097")] {
        let (exit, rejected) = run(bytes, tokens, &[]);
        assert_ne!(exit, 0);
        assert_eq!(rejected["error"]["code"], "USAGE");
    }
}

#[test]
fn host_selection_cli_prepare_dry_run_and_reply_validation_are_offline() {
    let f = fixture();
    ok(f.path(), &["context", "uses"]);
    let before = tree(f.path());
    let dry = ok(
        f.path(),
        &["--dry-run", "context", "uses", "--prepare-selection"],
    );
    assert_eq!(dry["data"]["dry_run"], true);
    assert_eq!(before, tree(f.path()));
    let prepared = ok(f.path(), &["context", "uses", "--prepare-selection"]);
    assert_eq!(prepared["meta"]["freshness"], "verified_snapshot");
    let packet = &prepared["data"]["selection_packet"];
    let task = packet["selector_input"].as_str().unwrap();
    assert_eq!(task.len() as u64, packet["input_bytes"].as_u64().unwrap());
    assert!(task.len() <= 131072);
    assert!(packet["candidate_count"].as_u64().unwrap() > 0);
    assert!(prepared["data"]["passages"].as_array().unwrap().is_empty());
    let replay = ok(f.path(), &["context", "uses", "--prepare-selection"]);
    assert_eq!(replay["data"]["selection_packet"], *packet);
    let reply_dir = tempfile::tempdir().unwrap();
    let reply_path = reply_dir.path().join("selection.json");
    fs::write(
        &reply_path,
        serde_json::to_vec(&serde_json::json!({
            "packet_fingerprint": packet["fingerprint"], "ordered_ids": []
        }))
        .unwrap(),
    )
    .unwrap();
    let result = ok(
        f.path(),
        &[
            "context",
            "uses",
            "--selection",
            reply_path.to_str().unwrap(),
        ],
    );
    assert!(result["data"]["text"].as_str().unwrap().is_empty());
    assert!(result["data"]["passages"].as_array().unwrap().is_empty());
    fs::write(
        &reply_path,
        serde_json::to_vec(&serde_json::json!({
            "packet_fingerprint": packet["fingerprint"], "ordered_ids": ["not-a-card"]
        }))
        .unwrap(),
    )
    .unwrap();
    let (exit, rejected) = invoke(
        f.path(),
        &[
            "context",
            "uses",
            "--selection",
            reply_path.to_str().unwrap(),
        ],
    );
    assert_ne!(exit, 0);
    assert_eq!(rejected["ok"], false);
    fs::write(&reply_path, vec![b' '; 4097]).unwrap();
    assert_ne!(
        invoke(
            f.path(),
            &[
                "context",
                "uses",
                "--selection",
                reply_path.to_str().unwrap()
            ]
        )
        .0,
        0
    );
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(f.path())
        .args(["--offline", "context", "uses", "--prepare-selection"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), task);
}

#[cfg(unix)]
#[test]
fn host_selection_rejects_fifo_without_waiting_for_a_writer() {
    use std::{
        ffi::CString,
        os::unix::ffi::OsStrExt,
        time::{Duration, Instant},
    };
    let f = fixture();
    let input = tempfile::tempdir().unwrap();
    let fifo = input.path().join("reply.fifo");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut child = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(f.path())
        .args(["--json", "--offline", "context", "uses", "--selection"])
        .arg(fifo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if start.elapsed() > Duration::from_secs(2) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("selection input blocked on a FIFO");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

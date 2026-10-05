//! Public CLI acceptance over a disposable normalized vault. Internals are used
//! only to seed the not-yet-public normalized rebuild layout.
#[path = "../../test_support/paths.rs"]
mod test_paths;
use crate::{
    catalog::{
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        scan, selector,
    },
    domain::{Blake3Hash, ByteSpan, CitationRef, RecordId},
    sources::{
        CaptureRequest, CitationScope, ExtractionInput, SourceOrigin, SourceStore, SourceView,
    },
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

const FIRST: &str = "IndexedSignal vessel stores 17 amber tokens. Unicode café 東京 🦀.\n";
const SECOND: &str = "IndexedSignal vessel stores 29 violet tokens. Unicode café 東京 🦀.\n";
const TITLE: &str = "Updated source title";
fn binary() -> PathBuf {
    let supplied=option_env!("CARGO_BIN_EXE_lwiki").map(OsString::from)
        .or_else(||std::env::var_os("CARGO_BIN_EXE_lwiki"))
        .expect("indexed CLI unit tests require the pinned lwiki binary in CARGO_BIN_EXE_lwiki (unit-test data/env)");
    test_paths::binary(supplied.to_str().expect("test binary path must be UTF-8"))
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    input: PathBuf,
    source: String,
    first: String,
    file_id: String,
}
impl Fixture {
    fn new() -> Self {
        Self::with_notes(&[], true)
    }
    fn with_notes(notes: &[(&str, &[u8])], normalized: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("normalized vault with spaces");
        let outside = temp.path().join("outside working directory");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let inputs = temp.path().join("capture inputs");
        fs::create_dir_all(&inputs).unwrap();
        let input = inputs.join("current text.md");
        fs::write(&input, FIRST).unwrap();
        fs::write(root.join("WIKI.md"),b"---\nwiki_schema: '1'\nwiki_kind: vault\nwiki_id: vault_indexed_cli\ntitle: Indexed CLI fixture\n---\n").unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        let writer = WriterPermit::acquire(handle.root(), Duration::ZERO).unwrap();
        let first = SourceStore::new(handle.clone())
            .plan_capture(CaptureRequest {
                title: "Original source title".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "seed.txt".into(),
                original: FIRST.as_bytes().to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            })
            .unwrap();
        for op in first.draft.unwrap().operations {
            let target = root.join(op.target.as_str());
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, op.proposed.unwrap()).unwrap();
        }
        for (path, bytes) in notes {
            let target = root.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        let vault = RecordId::new("vault_indexed_cli").unwrap();
        let selection = CatalogSelection::new(vault.clone(), 1).unwrap();
        if normalized {
            selector::prepare(&handle, &writer, &selection).unwrap();
            let identity = BuildIdentity {
                selection: selection.clone(),
                origin: None,
                vector_cache_lost: false,
                vector_loss_unknown: false,
            };
            let mut builder =
                NormalizedBuilder::begin(&handle, &writer, identity, BuildLimits::default())
                    .unwrap();
            let input_projection = scan::scan_input(&handle, &vault).unwrap();
            let projection =
                scan::project_normalized_with_sink(&handle, &input_projection, false, &mut builder)
                    .unwrap();
            let completed = builder.finish_normalized(&projection).unwrap();
            selector::publish(
                &handle,
                &writer,
                &completed.identity.selection,
                Duration::ZERO,
            )
            .unwrap();
        }
        drop(writer);
        let fixture = Self {
            _temp: temp,
            root,
            outside,
            input,
            source: first.source_id.to_string(),
            first: first.revision_id.to_string(),
            file_id: selection.file_id,
        };
        if !normalized {
            fixture.cli(&["index", "rebuild"]);
        }
        fixture
    }
    fn cli(&self, args: &[&str]) -> Value {
        self.cli_result(args, true)
    }
    fn cli_error(&self, args: &[&str], code: &str) -> Value {
        let value = self.cli_result(args, false);
        assert_eq!(value["error"]["code"], code, "{value}");
        value
    }
    fn cli_result(&self, args: &[&str], success: bool) -> Value {
        let output = Command::new(binary())
            .current_dir(&self.outside)
            .arg("--wiki")
            .arg(&self.root)
            .args(["--json", "--offline", "--lock-timeout-ms", "200"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.stderr.is_empty(),
            "stderr for {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid CLI JSON for {args:?}: {error}; {}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
        assert_eq!(
            output.status.success(),
            success,
            "CLI result for {args:?}: {result}"
        );
        assert_eq!(result["ok"], success, "{result}");
        assert_eq!(result["meta"]["network_used"], false, "{result}");
        result
    }
    fn refresh(&self, body: &str, title: Option<&str>, mode: Option<&str>) -> Value {
        fs::write(&self.input, body).unwrap();
        let mut args = Vec::new();
        if let Some(mode) = mode {
            args.push(mode);
        }
        args.extend([
            "source",
            "refresh",
            self.source.as_str(),
            "--file",
            self.input.to_str().unwrap(),
        ]);
        if let Some(title) = title {
            args.extend(["--title", title]);
        }
        self.cli(&args)
    }
    fn context(&self, revision: &str, body: &str) -> u64 {
        let value = self.cli(&[
            "context",
            "indexedsignal",
            "--scope",
            "indexed-evidence",
            "--no-sync",
            "--mode",
            "lexical",
            "--source-id",
            &self.source,
        ]);
        assert_eq!(value["meta"]["freshness"], "indexed_evidence");
        assert_eq!(
            value["data"]["verification"]["global_membership_verified"],
            false
        );
        let generation = value["meta"]["index_generation"].as_u64().unwrap();
        assert_eq!(
            value["data"]["verification"]["discovery_generation"],
            generation
        );
        let passages = value["data"]["passages"].as_array().unwrap();
        assert!(!passages.is_empty(), "no source context: {value}");
        assert!(
            value["data"]["text"]
                .as_str()
                .unwrap()
                .contains(body.trim()),
            "context omitted expected current source text: {value}"
        );
        let prefix = format!("sources/{}/revisions/{revision}/", self.source);
        for passage in passages {
            let span: ByteSpan = serde_json::from_value(passage["span"].clone()).unwrap();
            let quote = span.slice(body).unwrap();
            assert_eq!(passage["text"], quote);
            assert_eq!(passage["label"], "captured_source");
            assert!(
                passage["locator"]["path"]
                    .as_str()
                    .unwrap()
                    .starts_with(&prefix),
                "stale revision returned: {passage}"
            );
            let citations = passage["citations"].as_array().unwrap();
            assert_eq!(citations.len(), 1);
            let citation = &citations[0];
            assert_eq!(citation["kind"], "source");
            let reference = &citation["reference"];
            assert_eq!(reference["source_id"], self.source);
            assert_eq!(reference["source_revision"], revision);
            assert_eq!(reference["span"], passage["span"]);
            assert_eq!(
                reference["quote_hash"],
                Blake3Hash::digest(quote.as_bytes()).as_str()
            );
        }
        generation
    }
    fn committed_epoch(&self, value: &Value, expected: u64) {
        assert_eq!(value["data"]["status"], "committed");
        assert_eq!(value["data"]["snapshot"]["generation"], expected);
        assert_eq!(
            value["data"]["snapshot"]["publication"]["file_id"],
            self.file_id
        );
    }
    fn shm(&self) -> PathBuf {
        PathBuf::from(format!(".wiki/cache/catalogs/{}.sqlite-shm", self.file_id))
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    directory: bool,
    bytes: Vec<u8>,
    modified: SystemTime,
}
fn tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, TreeEntry>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(
                !metadata.file_type().is_symlink(),
                "fixture contains unexpected symlink"
            );
            let directory = metadata.is_dir();
            out.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                TreeEntry {
                    directory,
                    bytes: if directory {
                        vec![]
                    } else {
                        fs::read(&path).unwrap()
                    },
                    modified: metadata.modified().unwrap(),
                },
            );
            if directory {
                visit(root, &path, out);
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}
fn canonical_tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    tree(root)
        .into_iter()
        .filter(|(path, _)| !path.starts_with(".wiki") && !path.starts_with("changes"))
        .collect()
}
fn same_read_tree(fixture: &Fixture, before: BTreeMap<PathBuf, TreeEntry>) {
    let mut expected = before;
    let mut after = tree(&fixture.root);
    let shm = fixture.shm();
    // Native read-only WAL opens may update shared-memory coordination. The
    // pair was materialized during seed; all other files/paths/mtimes must match.
    assert!(!expected.remove(&shm).expect("seeded SHM pair").directory);
    assert!(!after.remove(&shm).expect("retained SHM pair").directory);
    assert_eq!(after, expected);
}
#[test]
fn indexed_cli_refresh_noop_title_and_history_are_immediately_cited() {
    let fixture = Fixture::new();
    let initial = fixture.context(&fixture.first, FIRST);
    assert_eq!(initial, 1);
    let changed = fixture.refresh(SECOND, None, None);
    let second = changed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(second, fixture.first);
    fixture.committed_epoch(&changed, initial + 1);
    assert_eq!(fixture.context(&second, SECOND), initial + 1);
    let noop = fixture.refresh(SECOND, None, None);
    assert_eq!(noop["data"]["reused"], true);
    assert!(noop["data"]["change"].is_null());
    assert!(
        noop["data"]["plan"]["operations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.context(&second, SECOND), initial + 1);
    let title = fixture.refresh(SECOND, Some(TITLE), None);
    assert_eq!(title["data"]["allocated_ids"]["revision"], second);
    assert_eq!(
        title["data"]["plan"]["operations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.committed_epoch(&title, initial + 2);
    let source_path = fixture
        .root
        .join(format!("sources/{}/source.md", fixture.source));
    let source = crate::records::parse_note(&fs::read(source_path).unwrap());
    assert_eq!(source.canonical.unwrap().string("title"), Some(TITLE));
    let read = fixture.cli(&["read", "--id", &fixture.source, "--no-sync"]);
    assert_eq!(read["data"]["metadata"]["title"], TITLE);
    assert_eq!(read["meta"]["freshness"], "index_snapshot");
    assert_eq!(read["meta"]["index_generation"], initial + 2);
    assert!(read["meta"]["verified_at"].is_null());
    let content_path = format!("sources/{}/revisions/{second}/content.md", fixture.source);
    let content = fixture.cli(&["read", "--path", &content_path, "--no-sync"]);
    assert_eq!(content["data"]["body"], SECOND);
    assert!(content["data"]["record"].is_null());
    assert_eq!(fixture.context(&second, SECOND), initial + 2);
    let reused = fixture.refresh(FIRST, None, None);
    assert_eq!(reused["data"]["reused"], true);
    assert_eq!(reused["data"]["allocated_ids"]["revision"], fixture.first);
    fixture.committed_epoch(&reused, initial + 3);
    assert_eq!(fixture.context(&fixture.first, FIRST), initial + 3);
}
#[test]
fn indexed_cli_dry_run_preserves_full_vault_bytes_and_timestamps() {
    let fixture = Fixture::new();
    let epoch = fixture.context(&fixture.first, FIRST);
    fs::write(&fixture.input, SECOND).unwrap();
    let before = tree(&fixture.root);
    let preview = fixture.cli(&[
        "--dry-run",
        "source",
        "refresh",
        &fixture.source,
        "--file",
        fixture.input.to_str().unwrap(),
    ]);
    assert!(preview["data"]["change"].is_null());
    assert!(
        preview["data"]["plan"]["operations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(preview["data"]["dry_run"], true);
    assert_eq!(preview["data"]["plan_complete"], false);
    assert!(preview["data"]["reused"].is_null());
    assert_eq!(tree(&fixture.root), before);
    assert_eq!(fixture.context(&fixture.first, FIRST), epoch);
}
#[test]
fn indexed_cli_stage_defers_canonical_publication_and_apply_is_idempotent() {
    let fixture = Fixture::new();
    let epoch = fixture.context(&fixture.first, FIRST);
    let before = canonical_tree(&fixture.root);
    let staged = fixture.refresh(SECOND, None, Some("--stage"));
    assert_eq!(staged["data"]["status"], "prepared");
    assert!(staged["data"]["snapshot"].is_null());
    let second = staged["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let change = staged["data"]["change"]["change_id"].as_str().unwrap();
    assert_eq!(canonical_tree(&fixture.root), before);
    assert!(
        !fixture
            .root
            .join(format!("sources/{}/revisions/{second}", fixture.source))
            .exists()
    );
    assert_eq!(fixture.context(&fixture.first, FIRST), epoch);
    let applied = fixture.cli(&["changes", "apply", change]);
    fixture.committed_epoch(&applied, epoch + 1);
    assert_eq!(fixture.context(&second, SECOND), epoch + 1);
    let committed = canonical_tree(&fixture.root);
    let retry = fixture.cli(&["changes", "apply", change]);
    fixture.committed_epoch(&retry, epoch + 1);
    assert_eq!(canonical_tree(&fixture.root), committed);
    assert_eq!(fixture.context(&second, SECOND), epoch + 1);
}

fn retained_manifest(fixture: &Fixture, change: &str) -> Value {
    let text =
        fs::read_to_string(fixture.root.join(format!("changes/{change}/change.md"))).unwrap();
    let json = text
        .split_once("```lwiki-change-v1\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    serde_json::from_str(json).unwrap()
}

#[test]
fn indexed_cli_apply_dry_run_rejects_missing_and_corrupt_proposed_payloads_without_writes() {
    for missing in [true, false] {
        let fixture = Fixture::new();
        let staged = fixture.refresh(SECOND, None, Some("--stage"));
        let change = staged["data"]["change"]["change_id"].as_str().unwrap();
        let manifest = retained_manifest(&fixture, change);
        let payload = manifest["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|op| op["after_payload"]["path"].as_str())
            .unwrap();
        let path = fixture.root.join(payload);
        if missing {
            fs::remove_file(&path).unwrap();
        } else {
            fs::write(&path, b"tampered proposed bytes\n").unwrap();
        }
        let before = tree(&fixture.root);
        let error = fixture.cli_error(&["--dry-run", "changes", "apply", change], "RECORD_INVALID");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("retained payload"),
            "{error}"
        );
        same_read_tree(&fixture, before);
        assert_eq!(fixture.context(&fixture.first, FIRST), 1);
    }
}

#[test]
fn indexed_cli_replay_refuses_malformed_unknown_and_mixed_proofs_in_both_modes_without_writes() {
    let fixture = Fixture::new();
    let staged = fixture.refresh(SECOND, None, Some("--stage"));
    let change = staged["data"]["change"]["change_id"].as_str().unwrap();
    let path = fixture
        .root
        .join(format!("changes/{change}/validation.json"));
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(original["proof"]["version"], 3);
    for variant in ["malformed", "unknown", "mixed_fields", "legacy_with_delta"] {
        let mut value = original.clone();
        let (bytes, code) = match variant {
            "malformed" => (b"{\"proof\":".to_vec(), "RECORD_INVALID"),
            "unknown" => {
                value["proof"]["version"] = 999.into();
                (serde_json::to_vec(&value).unwrap(), "RECOVERY_REQUIRED")
            }
            "mixed_fields" => {
                value["proof"]["parser_fingerprint"] =
                    serde_json::json!(Blake3Hash::digest(b"legacy"));
                value["checksum"] = serde_json::json!(Blake3Hash::digest(
                    serde_json::to_vec(&value["proof"]).unwrap()
                ));
                (serde_json::to_vec(&value).unwrap(), "RECORD_INVALID")
            }
            "legacy_with_delta" => {
                value["proof"]["version"] = 1.into();
                (serde_json::to_vec(&value).unwrap(), "RECOVERY_REQUIRED")
            }
            _ => unreachable!(),
        };
        fs::write(&path, bytes).unwrap();
        for dry in [true, false] {
            let before = tree(&fixture.root);
            let mut args = vec![];
            if dry {
                args.push("--dry-run");
            }
            args.extend(["changes", "apply", change]);
            fixture.cli_error(&args, code);
            same_read_tree(&fixture, before);
        }
    }
    assert_eq!(fixture.context(&fixture.first, FIRST), 1);
}

#[test]
fn indexed_cli_refresh_missing_required_index_refuses_before_canonical_writes() {
    let fixture = Fixture::new();
    let path = fixture
        .root
        .join(format!(".wiki/cache/catalogs/{}.sqlite", fixture.file_id));
    // Fixture-only corruption. Keep this idle native connection open so closing
    // the tamper handle cannot remove the persistent WAL/SHM pair under test.
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP INDEX semantic_dependents")
        .unwrap();
    let epoch: i64 = connection
        .query_row(
            "SELECT epoch FROM catalog_meta WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    fs::write(&fixture.input, SECOND).unwrap();
    for dry in [true, false] {
        let before = tree(&fixture.root);
        let mut args = vec![];
        if dry {
            args.push("--dry-run");
        }
        args.extend([
            "source",
            "refresh",
            &fixture.source,
            "--file",
            fixture.input.to_str().unwrap(),
        ]);
        if dry {
            let preview = fixture.cli(&args);
            assert_eq!(preview["data"]["plan_complete"], false);
            assert!(preview["data"]["reused"].is_null());
            assert_eq!(tree(&fixture.root), before);
        } else {
            fixture.cli_error(&args, "INDEX_CORRUPT");
            same_read_tree(&fixture, before);
        }
        let after: i64 = connection
            .query_row(
                "SELECT epoch FROM catalog_meta WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, epoch);
    }
    assert!(!fixture.root.join("changes").exists());
}

#[test]
fn indexed_cli_cached_read_matches_legacy_for_notes_and_ranges() {
    let notes: &[(&str, &[u8])] = &[
        ("pages/normal.md", b"---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_cached\ntitle: Cached\n---\n**Markdown** stays raw.\n"),
        ("pages/future.md", b"---\nwiki_schema: '99'\nwiki_kind: page\nwiki_id: page_future\ntitle: Future\n---\nFuture body\n"),
        ("pages/malformed.md", b"---\nwiki_schema: '1'\nwiki_id: page_isolated\nbroken: [\n---\nRaw malformed body\n"),
        ("pages/duplicate-a.md", b"---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_duplicate\ntitle: A\n---\nA\n"),
        ("pages/duplicate-b.md", b"---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_duplicate\ntitle: B\n---\nB\n"),
        ("pages/invalid-utf8.md", b"bad\xffbody"),
        ("pages/bookkeeping.md", b"---\nwiki_schema: '1'\nwiki_kind: extraction\nwiki_id: extraction_invalid\ntitle: Invalid bookkeeping\n---\n**Bookkeeping** body must stay visible.\n"),
        ("pages/plain.md", "\u{feff}# Plain\r\nCafé 東京 🦀\r\n".as_bytes()),
    ];
    let legacy = Fixture::with_notes(notes, false);
    let normalized = Fixture::with_notes(notes, true);
    for (path, _) in notes {
        let args = ["read", "--path", *path, "--no-sync"];
        let expected = legacy.cli(&args);
        let actual = normalized.cli(&args);
        assert_eq!(actual["data"], expected["data"], "{path}");
        assert_eq!(actual["meta"]["freshness"], "index_snapshot");
    }
    for id in ["page_cached", "page_future"] {
        let args = ["read", "--id", id, "--no-sync"];
        assert_eq!(normalized.cli(&args)["data"], legacy.cli(&args)["data"]);
    }
    for (id, code) in [
        ("page_isolated", "RECORD_NOT_FOUND"),
        ("page_missing", "RECORD_NOT_FOUND"),
        ("page_duplicate", "REFERENCE_AMBIGUOUS"),
    ] {
        let args = ["read", "--id", id, "--no-sync"];
        legacy.cli_error(&args, code);
        normalized.cli_error(&args, code);
    }
    let args = [
        "read",
        "--path",
        "pages/plain.md",
        "--no-sync",
        "--max-bytes",
        "14",
    ];
    let actual = normalized.cli(&args);
    assert_eq!(actual["data"], legacy.cli(&args)["data"]);
    assert_eq!(actual["data"]["truncated"], true);
    let continuation = &actual["data"]["continuation"];
    let start = continuation["start"].to_string();
    let end = continuation["end"].to_string();
    let args = [
        "read",
        "--path",
        "pages/plain.md",
        "--no-sync",
        "--start",
        &start,
        "--end",
        &end,
    ];
    assert_eq!(normalized.cli(&args)["data"], legacy.cli(&args)["data"]);
    normalized.cli_error(
        &[
            "read",
            "--path",
            "pages/plain.md",
            "--no-sync",
            "--start",
            "1",
            "--end",
            "2",
        ],
        "USAGE",
    );
}

#[test]
fn indexed_cli_cached_read_is_stale_by_contract_and_never_writes() {
    let fixture = Fixture::new();
    let path = format!(
        "sources/{}/revisions/{}/content.md",
        fixture.source, fixture.first
    );
    let before = tree(&fixture.root);
    for dry_run in [false, true] {
        let mut args = vec!["read", "--path", &path, "--no-sync"];
        if dry_run {
            args.insert(0, "--dry-run");
        }
        let read = fixture.cli(&args);
        assert_eq!(read["data"]["body"], FIRST);
        if dry_run {
            assert!(read["meta"]["freshness"].is_null());
        } else {
            assert_eq!(read["meta"]["freshness"], "index_snapshot");
        }
    }
    same_read_tree(&fixture, before);
    fs::write(fixture.root.join(&path), SECOND).unwrap();
    let read = fixture.cli(&["read", "--path", &path, "--no-sync"]);
    assert_eq!(read["data"]["body"], FIRST);
    let result = fixture.cli_result(
        &[
            "context",
            "IndexedSignal",
            "--scope",
            "indexed-evidence",
            "--no-sync",
            "--mode",
            "lexical",
        ],
        false,
    );
    assert!(
        !result["error"].is_null(),
        "stale selected canonical bytes must not be cited: {result}"
    );
}

const GENERAL_REVIEWED: &[u8] = b"---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_general_a\ntitle: PlanningSignal Handbook\nwiki_status: reviewed\naliases: [PlanningAlias]\ntags: [team, keep]\n---\nPlanningSignal teams review the amber checklist before release.\n";
const GENERAL_SECOND: &[u8] = b"---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_general_b\ntitle: PlanningSignal Followup\nwiki_status: reviewed\ntags: [team]\n---\nPlanningSignal teams archive the violet report after release.\n";
const GENERAL_DRAFT: &[u8] = b"---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_general_draft\ntitle: PlanningSignal Draft\nwiki_status: draft\n---\nPlanningSignal unfinished speculative draft.\n";
fn general_fixture(normalized: bool) -> Fixture {
    Fixture::with_notes(
        &[
            ("pages/a.md", GENERAL_REVIEWED),
            ("pages/b.md", GENERAL_SECOND),
            ("pages/draft.md", GENERAL_DRAFT),
            (
                "plain.md",
                b"# PlanningSignal plain discovery\nUnadopted PlanningSignal document.\n",
            ),
        ],
        normalized,
    )
}

fn authored_page(id: &str, body: &str) -> String {
    format!(
        "---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: {id}\ntitle: WriteSignal Handbook\nwiki_status: reviewed\n---\n{body}\n"
    )
}

#[test]
#[ignore = "requires an authentic v2 fixture prepared by a pinned previous release"]
fn authentic_v2_retained_refresh_resumes_after_sql_commit() {
    use crate::{
        catalog::{
            Catalog, CatalogOptions, PublicationCheckpoint, PublicationFault,
            source_refresh::IndexedRefreshSession,
        },
        changes::{
            ChangeEngine, ChangeStatus, PreparedChange, indexed_refresh::IndexedRefreshPhase,
        },
        domain::{ErrorCode, Result, WikiError},
    };
    // The supplied fixture is only read. All fault injection and replay happens
    // in a bounded disposable copy, including its retained SQLite WAL.
    fn copy_fixture(from: &Path, to: &Path, count: &mut usize, bytes: &mut u64) {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            assert!(!metadata.file_type().is_symlink());
            *count += 1;
            assert!(*count <= 1000, "compatibility fixture entry allowance");
            let target = to.join(entry.file_name());
            if metadata.is_dir() {
                copy_fixture(&entry.path(), &target, count, bytes);
            } else {
                assert!(metadata.is_file());
                *bytes += metadata.len();
                assert!(
                    *bytes <= 32 * 1024 * 1024,
                    "compatibility fixture byte allowance"
                );
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    struct AfterCommit;
    impl PublicationFault for AfterCommit {
        fn check(&self, point: PublicationCheckpoint) -> Result<()> {
            if point == PublicationCheckpoint::AfterCommit {
                Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "authentic v2 interrupted after SQL commit",
                ))
            } else {
                Ok(())
            }
        }
    }
    let preparation = std::env::var_os("LWIKI_V2_COMPAT_PREPARATION")
        .expect("provide the authentic v2 preparation report");
    let encoded = fs::read(preparation).unwrap();
    assert!(encoded.len() <= 64 * 1024);
    let input: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(input["proof_version"], 2);
    assert_eq!(input["delta_version"], 2);
    let source = Path::new(input["vault"].as_str().unwrap());
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("v2 replay vault with spaces");
    copy_fixture(source, &root, &mut 0, &mut 0);
    let change_id = RecordId::new(input["change"].as_str().unwrap()).unwrap();
    let receipt_path = root.join(format!("changes/{change_id}/validation.json"));
    let delta_path = root.join(format!("changes/{change_id}/indexed-delta.json"));
    let receipt_bytes = fs::read(&receipt_path).unwrap();
    let delta_bytes = fs::read(&delta_path).unwrap();
    let wire: Value = serde_json::from_slice(&receipt_bytes).unwrap();
    let change: PreparedChange = serde_json::from_value(wire["proof"]["change"].clone()).unwrap();
    let vault_id: RecordId = serde_json::from_value(wire["proof"]["vault_id"].clone()).unwrap();
    let handle = VaultFs::new(VaultRoot::explicit(&root).unwrap());
    let writer = WriterPermit::acquire(handle.root(), Duration::ZERO).unwrap();
    let engine = ChangeEngine::new(handle.clone()).unwrap();
    let proof = engine.load_indexed_refresh_proof(&change).unwrap().unwrap();
    assert_eq!(proof.version, 2);
    let faulty = Catalog::with_options(
        handle.clone(),
        vault_id.clone(),
        CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(AfterCommit)),
        },
    );
    let mut session = IndexedRefreshSession::resume(&faulty, &writer, proof.clone()).unwrap();
    assert_eq!(session.phase(), IndexedRefreshPhase::AtBase);
    let error = engine
        .apply_indexed_refresh(&writer, &mut session)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    assert_eq!(session.phase(), IndexedRefreshPhase::AlreadyPublished);
    drop(session);
    let catalog = Catalog::new(handle, vault_id);
    let before = canonical_tree(&root);
    let mut resumed = IndexedRefreshSession::resume(&catalog, &writer, proof.clone()).unwrap();
    assert_eq!(resumed.phase(), IndexedRefreshPhase::AlreadyPublished);
    let report = engine.apply_indexed_refresh(&writer, &mut resumed).unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    assert_eq!(report.snapshot, Some(proof.intended));
    drop(resumed);
    assert_eq!(canonical_tree(&root), before);
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
    assert_eq!(fs::read(&delta_path).unwrap(), delta_bytes);
    assert_eq!(
        engine
            .indexed_refresh_terminal_report(&writer, &change)
            .unwrap(),
        Some(report)
    );
}

#[test]
fn indexed_cli_pages_initialize_edit_batch_and_retrieve_without_sync() {
    let fixture = Fixture::new();
    fs::write(&fixture.input, "WriteSignal initial draft.").unwrap();
    let initialized = fixture.cli(&[
        "page",
        "init",
        "--file",
        fixture.input.to_str().unwrap(),
        "--id",
        "page_write_a",
        "--path",
        "pages/custom a.md",
        "--title",
        "WriteSignal Handbook",
    ]);
    fixture.committed_epoch(&initialized, 2);
    let initial = fs::read(fixture.root.join("pages/custom a.md")).unwrap();
    let first = authored_page("page_write_a", "WriteSignal revised amber checklist.");
    fs::write(&fixture.input, &first).unwrap();
    // Omit --path: resolving an existing identity must use the selected index,
    // preserve its custom path, and never scan the whole canonical corpus.
    let updated = fixture.cli(&[
        "page",
        "put",
        "--file",
        fixture.input.to_str().unwrap(),
        "--if-match",
        Blake3Hash::digest(&initial).as_str(),
    ]);
    fixture.committed_epoch(&updated, 3);
    assert!(!fixture.root.join("pages/page_write_a.md").exists());
    let read = fixture.cli(&["read", "--id", "page_write_a"]);
    assert!(
        read["data"]["body"]
            .as_str()
            .unwrap()
            .contains("revised amber")
    );
    let search = fixture.cli(&["search", "WriteSignal"]);
    assert!(
        search["data"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"] == "pages/custom a.md")
    );
    let after = authored_page(
        "page_write_a",
        "WriteSignal final violet checklist. [[page_write_b]]",
    );
    let second = authored_page(
        "page_write_b",
        "WriteSignal second review. [[page_write_a]]",
    );
    let batch = fixture.outside.join("coupled edits.json");
    fs::write(&batch, serde_json::to_vec(&serde_json::json!({
        "title": "Coupled authored edits",
        "pages": [
            {"path":"pages/custom a.md", "markdown":after, "if_match":Blake3Hash::digest(first.as_bytes())},
            {"path":"pages/b.md", "markdown":second}
        ]
    })).unwrap()).unwrap();
    let published = fixture.cli(&["page", "batch", "--file", batch.to_str().unwrap()]);
    fixture.committed_epoch(&published, 4);
    let context = fixture.cli(&["context", "WriteSignal", "--kind", "page"]);
    let text = context["data"]["text"].as_str().unwrap();
    assert!(
        text.contains("final violet") && text.contains("second review"),
        "{context}"
    );
    assert!(!text.contains("revised amber"), "{context}");
    for passage in context["data"]["passages"].as_array().unwrap() {
        assert!(passage["citations"].as_array().unwrap().is_empty());
    }
    assert_eq!(
        fs::read_to_string(fixture.root.join("pages/custom a.md")).unwrap(),
        after
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("pages/b.md")).unwrap(),
        second
    );
}

#[test]
fn indexed_cli_page_stage_dry_run_noop_and_apply_preserve_author_guards() {
    let fixture = general_fixture(true);
    let proposed = authored_page("page_general_a", "WriteSignal staged replacement.");
    fs::write(&fixture.input, &proposed).unwrap();
    let expected = Blake3Hash::digest(GENERAL_REVIEWED).to_string();
    let put = [
        "page",
        "put",
        "--file",
        fixture.input.to_str().unwrap(),
        "--if-match",
        &expected,
    ];
    let before = tree(&fixture.root);
    let mut dry = vec!["--dry-run"];
    dry.extend(put);
    let preview = fixture.cli(&dry);
    assert_eq!(
        preview["data"]["unresolved_target"]["record_id"],
        "page_general_a"
    );
    assert!(preview["data"]["unresolved_target"]["path"].is_null());
    assert_eq!(
        preview["data"]["validation"]["explicit_file_guards_checked"],
        false
    );
    assert_eq!(tree(&fixture.root), before);
    let canonical_before = canonical_tree(&fixture.root);
    let mut stage = vec!["--stage"];
    stage.extend(put);
    let prepared = fixture.cli(&stage);
    assert_eq!(prepared["data"]["status"], "prepared");
    assert_eq!(canonical_tree(&fixture.root), canonical_before);
    let change = prepared["data"]["change"]["change_id"].as_str().unwrap();
    let applied = fixture.cli(&["changes", "apply", change]);
    fixture.committed_epoch(&applied, 2);
    let applied_again = fixture.cli(&["changes", "apply", change]);
    fixture.committed_epoch(&applied_again, 2);
    let canonical_after = canonical_tree(&fixture.root);
    fixture.cli_error(&put, "CONTENT_CONFLICT");
    assert_eq!(canonical_tree(&fixture.root), canonical_after);
    let hash = Blake3Hash::digest(proposed.as_bytes()).to_string();
    let noop = fixture.cli(&[
        "page",
        "put",
        "--file",
        fixture.input.to_str().unwrap(),
        "--if-match",
        &hash,
    ]);
    assert_eq!(noop["data"]["reused"], true);
    assert!(noop["data"]["change"].is_null());
    assert_eq!(canonical_tree(&fixture.root), canonical_after);
}

#[test]
fn indexed_cli_page_previews_preserve_complete_tree_and_mark_unchecked_admission() {
    let fixture = general_fixture(true);
    let input = fixture.input.to_str().unwrap();
    fs::write(&fixture.input, "Preview only body.").unwrap();
    let before = tree(&fixture.root);
    let init = fixture.cli(&[
        "--dry-run",
        "page",
        "init",
        "--file",
        input,
        "--title",
        "Preview only",
        "--id",
        "page_preview_only",
        "--path",
        "pages/preview only.md",
    ]);
    assert_eq!(tree(&fixture.root), before);
    fs::write(
        &fixture.input,
        authored_page("page_general_a", "Preview changed text."),
    )
    .unwrap();
    let guard = Blake3Hash::digest(GENERAL_REVIEWED).to_string();
    let explicit = fixture.cli(&[
        "--dry-run",
        "page",
        "put",
        "--file",
        input,
        "--path",
        "pages/a.md",
        "--if-match",
        &guard,
    ]);
    assert_eq!(tree(&fixture.root), before);
    let batch = fixture.outside.join("preview batch.json");
    fs::write(&batch, serde_json::to_vec(&serde_json::json!({
        "title":"Preview batch", "pages":[{
            "path":"pages/a.md", "markdown":authored_page("page_general_a", "Batch preview."),
            "if_match":guard
        }],
        "read_preconditions":[{"path":"missing.md", "expected":{"state":"hash", "hash":Blake3Hash::digest(b"absent")}}]
    })).unwrap()).unwrap();
    // Explicit read dependencies remain unchecked in previews; no seal or
    // stageability is claimed. The normal batch path checks them at admission.
    let batched = fixture.cli(&[
        "--dry-run",
        "page",
        "batch",
        "--file",
        batch.to_str().unwrap(),
    ]);
    assert_eq!(tree(&fixture.root), before);
    for preview in [init, explicit, batched] {
        assert_eq!(preview["data"]["dry_run"], true);
        assert_eq!(preview["data"]["plan_complete"], false);
        assert_eq!(
            preview["data"]["validation"]["explicit_file_guards_checked"],
            true
        );
        assert_eq!(
            preview["data"]["validation"]["indexed_admission_checked"],
            false
        );
        assert_eq!(
            preview["data"]["validation"]["read_dependencies_checked"],
            false
        );
        assert!(preview["data"]["change"].is_null());
        assert!(preview["data"]["reused"].is_null());
    }
    fixture.cli_error(
        &[
            "--dry-run",
            "page",
            "put",
            "--file",
            input,
            "--path",
            "pages/a.md",
            "--if-match",
            Blake3Hash::digest(b"wrong bytes").as_str(),
        ],
        "CONTENT_CONFLICT",
    );
    assert_eq!(tree(&fixture.root), before);
}

#[test]
fn indexed_cli_staged_page_refuses_an_external_edit_without_overwriting_it() {
    let fixture = general_fixture(true);
    fs::write(
        &fixture.input,
        authored_page("page_general_a", "WriteSignal staged."),
    )
    .unwrap();
    let staged = fixture.cli(&[
        "--stage",
        "page",
        "put",
        "--file",
        fixture.input.to_str().unwrap(),
        "--if-match",
        Blake3Hash::digest(GENERAL_REVIEWED).as_str(),
    ]);
    let change = staged["data"]["change"]["change_id"].as_str().unwrap();
    let external = authored_page("page_general_a", "An intervening author edit.");
    fs::write(fixture.root.join("pages/a.md"), &external).unwrap();
    let before = canonical_tree(&fixture.root);
    fixture.cli_result(&["changes", "apply", change], false);
    assert_eq!(canonical_tree(&fixture.root), before);
    assert_eq!(
        fs::read_to_string(fixture.root.join("pages/a.md")).unwrap(),
        external
    );
}

#[test]
fn indexed_cli_general_search_preserves_authored_results_filters_and_cached_dry_run() {
    let legacy = general_fixture(false);
    let normalized = general_fixture(true);
    for words in [
        vec!["search", "page_general_a", "--no-sync"],
        vec!["search", "PlanningSignal Handbook", "--no-sync"],
        vec!["search", "PlanningAlias", "--no-sync"],
        vec!["search", "PlanningSignal", "--no-sync"],
        vec![
            "search",
            "PlanningSignal",
            "--no-sync",
            "--kind",
            "page",
            "--tag",
            "keep",
        ],
        vec![
            "search",
            "PlanningSignal",
            "--no-sync",
            "--status",
            "reviewed",
            "--path-prefix",
            "pages/",
        ],
        vec!["search", "no_result_signal", "--no-sync"],
    ] {
        let expected = legacy.cli(&words);
        let actual = normalized.cli(&words);
        for field in [
            "hits",
            "candidate_count",
            "omitted_candidates",
            "truncated",
            "verification",
        ] {
            assert_eq!(
                actual["data"][field], expected["data"][field],
                "{words:?}: {field}"
            );
        }
        assert_eq!(actual["meta"]["freshness"], "index_snapshot");
        assert!(actual["meta"]["verified_at"].is_null());
    }
    let before = tree(&normalized.root);
    let dry = normalized.cli(&["--dry-run", "search", "PlanningSignal", "--no-sync"]);
    let actual = normalized.cli(&["search", "PlanningSignal", "--no-sync"]);
    assert_eq!(dry["data"], actual["data"]);
    same_read_tree(&normalized, before);
    fs::write(
        normalized.root.join("pages/a.md"),
        b"external replacement\n",
    )
    .unwrap();
    let stale = normalized.cli(&["search", "page_general_a", "--no-sync"]);
    assert_eq!(stale["data"]["hits"][0]["title"], "PlanningSignal Handbook");
    let plain = normalized.cli(&["search", "page_general_a"]);
    assert_eq!(plain["data"], stale["data"]);
    for words in [
        vec!["search", "PlanningSignal", "--no-sync", "--mode", "literal"],
        vec![
            "search",
            "PlanningSignal",
            "--no-sync",
            "--mode",
            "semantic",
        ],
        vec!["search", "PlanningSignal", "--no-sync", "--mode", "hybrid"],
    ] {
        normalized.cli_error(&words, "CAPABILITY_UNAVAILABLE");
    }
}

#[test]
fn indexed_cli_general_snapshot_context_preserves_reviewed_facets_and_dry_run() {
    let legacy = general_fixture(false);
    let normalized = general_fixture(true);
    let args = [
        "context",
        "PlanningSignal",
        "--scope",
        "snapshot",
        "--target",
        "documents",
        "--no-sync",
        "--kind",
        "page",
    ];
    let expected = legacy.cli(&args);
    let before = tree(&normalized.root);
    let actual = normalized.cli(&args);
    for field in ["text", "passages", "omissions", "truncated", "verification"] {
        assert_eq!(actual["data"][field], expected["data"][field], "{field}");
    }
    let text = actual["data"]["text"].as_str().unwrap();
    assert!(
        text.contains("amber checklist") && text.contains("violet report"),
        "{actual}"
    );
    assert!(!text.contains("unfinished speculative draft"));
    assert_eq!(actual["meta"]["freshness"], "index_snapshot");
    assert!(actual["meta"]["verified_at"].is_null());
    for passage in actual["data"]["passages"].as_array().unwrap() {
        assert!(passage["citations"].as_array().unwrap().is_empty());
    }
    let mut dry_args = vec!["--dry-run"];
    dry_args.extend(args);
    let dry = normalized.cli(&dry_args);
    assert_eq!(dry["data"], actual["data"]);
    same_read_tree(&normalized, before);
}

#[test]
fn indexed_cli_general_search_context_observe_refresh_and_reject_old_cursor() {
    let fixture = general_fixture(true);
    let first = fixture.cli(&["search", "PlanningSignal", "--no-sync", "--limit", "1"]);
    let cursor = first["data"]["next_cursor"]
        .as_str()
        .expect("multiple authored hits")
        .to_owned();
    let second_page = fixture.cli(&[
        "search",
        "PlanningSignal",
        "--no-sync",
        "--limit",
        "1",
        "--cursor",
        &cursor,
    ]);
    assert_ne!(
        first["data"]["hits"][0]["path"],
        second_page["data"]["hits"][0]["path"]
    );
    let changed = fixture.refresh(SECOND, None, None);
    let revision = changed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    let generation = changed["data"]["snapshot"]["generation"].as_u64().unwrap();
    let search = fixture.cli(&[
        "search",
        "IndexedSignal",
        "--no-sync",
        "--source-id",
        &fixture.source,
    ]);
    assert_eq!(search["meta"]["index_generation"], generation);
    assert_eq!(search["data"]["hits"][0]["owner_revision"], revision);
    let context = fixture.cli(&[
        "context",
        "IndexedSignal",
        "--scope",
        "snapshot",
        "--target",
        "documents",
        "--no-sync",
        "--source-id",
        &fixture.source,
    ]);
    assert_eq!(context["meta"]["index_generation"], generation);
    let text = context["data"]["text"].as_str().unwrap();
    assert!(text.contains("29 violet tokens"), "{context}");
    // Snapshot scope retains history. The refreshed revision must be current;
    // the old payload may remain only with its historical label.
    let passages = context["data"]["passages"].as_array().unwrap();
    assert!(passages.iter().any(|passage| {
        passage["eligibility"] == "current"
            && passage["text"]
                .as_str()
                .unwrap()
                .contains("29 violet tokens")
            && passage["locator"]["record"]["record_id"] == revision
    }));
    for passage in passages {
        if passage["text"]
            .as_str()
            .unwrap()
            .contains("17 amber tokens")
        {
            assert_eq!(passage["eligibility"], "historical");
        }
    }
    assert_eq!(fixture.context(revision, SECOND), generation);
    fixture.cli_error(
        &[
            "search",
            "PlanningSignal",
            "--no-sync",
            "--limit",
            "1",
            "--cursor",
            &cursor,
        ],
        "CURSOR_STALE",
    );
}

#[test]
fn indexed_cli_general_search_missing_title_index_refuses_without_mutation() {
    let fixture = general_fixture(true);
    let path = fixture
        .root
        .join(format!(".wiki/cache/catalogs/{}.sqlite", fixture.file_id));
    assert!(path.is_file());
    let connection = rusqlite::Connection::open(path).unwrap();
    selector::configure_wal(&connection).unwrap();
    connection
        .execute_batch("DROP INDEX document_titles")
        .unwrap();
    drop(connection);
    let before = tree(&fixture.root);
    fixture.cli_error(
        &["search", "PlanningSignal", "--no-sync"],
        "CAPABILITY_UNAVAILABLE",
    );
    same_read_tree(&fixture, before);
}

#[test]
fn indexed_cli_general_context_refuses_unsupported_modes_before_provider_setup() {
    let fixture = general_fixture(true);
    let before = tree(&fixture.root);
    for dry in [false, true] {
        for (scope, mode) in [
            ("snapshot", "literal"),
            ("snapshot", "semantic"),
            ("snapshot", "hybrid"),
            ("current", "lexical"),
            ("historical", "lexical"),
        ] {
            let mut args = vec![];
            if dry {
                args.push("--dry-run");
            }
            args.extend([
                "context",
                "PlanningSignal",
                "--scope",
                scope,
                "--mode",
                mode,
            ]);
            if scope == "snapshot" {
                args.push("--no-sync");
            }
            fixture.cli_error(&args, "CAPABILITY_UNAVAILABLE");
        }
    }
    same_read_tree(&fixture, before);
}

impl Fixture {
    fn empty_public() -> Self {
        Self::empty_public_with_layout(false)
    }
    fn empty_public_with_layout(retained: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("public source vault with spaces");
        let outside = temp.path().join("outside working directory");
        fs::create_dir_all(&outside).unwrap();
        let input = outside.join("source input.md");
        fs::write(&input, FIRST).unwrap();
        let output = Command::new(binary())
            .current_dir(&outside)
            .args(["--json", "--offline", "init"])
            .arg(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let mut fixture = Self {
            _temp: temp,
            root,
            outside,
            input,
            source: String::new(),
            first: String::new(),
            file_id: String::new(),
        };
        if retained {
            fn copy_tree(from: &Path, to: &Path) {
                fs::create_dir(to).unwrap();
                for entry in fs::read_dir(from).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_type().unwrap().is_dir() {
                        copy_tree(&entry.path(), &to.join(entry.file_name()));
                    } else {
                        fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
                    }
                }
            }
            copy_tree(
                &fixture.root,
                &fixture.outside.join("backup before retained migration"),
            );
            let plan = fixture.cli(&["storage", "plan"]);
            assert!(plan["data"]["blockers"].as_array().unwrap().is_empty());
            fixture.cli(&[
                "storage",
                "cleanup",
                "--expected-plan",
                plan["data"]["plan_hash"].as_str().unwrap(),
            ]);
            // An empty migrated vault has no retained payload directory yet;
            // activation is authoritative before its first retained write.
            assert!(
                fixture
                    .root
                    .join(".wiki/state/storage/layout.json")
                    .is_file()
            );
        }
        let built = fixture.cli(&["index", "rebuild", "--normalized"]);
        fixture.file_id = built["data"]["report"]["snapshot"]["publication"]["file_id"]
            .as_str()
            .expect("public normalized file identity")
            .to_owned();
        fixture
    }
}

#[test]
fn indexed_cli_public_source_capture_refresh_withdraw_retains_citable_history() {
    let mut fixture = Fixture::empty_public();
    let added = fixture.cli(&[
        "source",
        "add",
        fixture.input.to_str().unwrap(),
        "--title",
        "Public capture",
    ]);
    assert_eq!(added["data"]["status"], "committed");
    assert_eq!(added["data"]["citable"], true);
    fixture.source = added["data"]["allocated_ids"]["source"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.first = added["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(fixture.source.len(), 39);
    assert!(fixture.source.bytes().all(|byte| byte.is_ascii_digit()));
    fixture.context(&fixture.first, FIRST);
    let read = fixture.cli(&["read", "--id", &fixture.source]);
    assert_eq!(read["data"]["record"]["wiki_origin_kind"], "local-file");
    let source_path = format!("sources/{}/source.md", fixture.source);
    let original = fs::read(fixture.root.join(&source_path)).unwrap();
    let old_root = format!("sources/{}/revisions/{}", fixture.source, fixture.first);
    let old_text = fixture.root.join(format!("{old_root}/content.md"));
    let old_original = fixture.root.join(format!("{old_root}/original.bin"));
    let retained_text = fs::read(&old_text).unwrap();
    let retained_original = fs::read(&old_original).unwrap();
    let refreshed = fixture.refresh(SECOND, Some("Updated public capture"), None);
    let revision = refreshed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    fixture.context(revision, SECOND);
    assert_eq!(fs::read(&old_text).unwrap(), retained_text);
    assert_eq!(fs::read(&old_original).unwrap(), retained_original);
    let source_before_withdraw = fs::read(fixture.root.join(&source_path)).unwrap();
    let withdrawn = fixture.cli(&[
        "source",
        "withdraw",
        &fixture.source,
        "--reason",
        "Superseded by author",
    ]);
    assert_eq!(withdrawn["data"]["status"], "committed");
    let source_after_withdraw = fs::read(fixture.root.join(&source_path)).unwrap();
    assert_ne!(source_after_withdraw, source_before_withdraw);
    let old_note = crate::records::parse_note(&original);
    let new_note = crate::records::parse_note(&source_after_withdraw);
    assert_eq!(
        old_note.canonical.unwrap().string("wiki_origin"),
        new_note.canonical.as_ref().unwrap().string("wiki_origin")
    );
    let search = fixture.cli(&["search", "IndexedSignal", "--source-id", &fixture.source]);
    assert!(
        search["data"]["hits"].as_array().unwrap().is_empty(),
        "{search}"
    );
    let context = fixture.cli(&["context", "IndexedSignal", "--source-id", &fixture.source]);
    assert!(
        context["data"]["passages"].as_array().unwrap().is_empty(),
        "{context}"
    );
    let historical = fixture.cli(&[
        "search",
        "IndexedSignal",
        "--source-id",
        &fixture.source,
        "--include-historical",
    ]);
    assert!(
        !historical["data"]["hits"].as_array().unwrap().is_empty(),
        "{historical}"
    );
    for hit in historical["data"]["hits"].as_array().unwrap() {
        assert_ne!(hit["eligibility"], "current", "{hit}");
    }
    let read_history = fixture.cli(&["read", "--path", &format!("{old_root}/content.md")]);
    assert_eq!(read_history["data"]["body"], FIRST);
    let canonical = canonical_tree(&fixture.root);
    let repeated = fixture.cli(&[
        "source",
        "withdraw",
        &fixture.source,
        "--reason",
        "Different later reason",
    ]);
    assert_eq!(repeated["data"]["reused"], true);
    assert_eq!(canonical_tree(&fixture.root), canonical);
    assert_eq!(fs::read(&old_text).unwrap(), retained_text);
    assert_eq!(fs::read(&old_original).unwrap(), retained_original);
    fixture.cli(&["check"]);
}

#[test]
fn indexed_cli_public_source_stage_apply_retry_and_unavailable_text() {
    let fixture = Fixture::empty_public();
    let before = canonical_tree(&fixture.root);
    let staged = fixture.cli(&["--stage", "source", "add", fixture.input.to_str().unwrap()]);
    assert_eq!(staged["data"]["status"], "prepared");
    assert_eq!(canonical_tree(&fixture.root), before);
    let change = staged["data"]["change"]["change_id"].as_str().unwrap();
    let applied = fixture.cli(&["changes", "apply", change]);
    assert_eq!(applied["data"]["status"], "committed");
    let after = canonical_tree(&fixture.root);
    let repeated = fixture.cli(&["changes", "apply", change]);
    assert_eq!(repeated["data"]["snapshot"], applied["data"]["snapshot"]);
    assert_eq!(canonical_tree(&fixture.root), after);
    let empty = fixture.outside.join("empty.txt");
    fs::write(&empty, []).unwrap();
    let added = fixture.cli(&["source", "add", empty.to_str().unwrap()]);
    assert_eq!(added["data"]["citable"], false);
    assert_eq!(added["data"]["extraction_status"], "complete");
    let unsupported = fixture.outside.join("original.pdf");
    fs::write(&unsupported, b"%PDF-unsupported\0").unwrap();
    let added = fixture.cli(&["source", "add", unsupported.to_str().unwrap()]);
    assert_eq!(added["data"]["citable"], false);
    assert_eq!(added["data"]["extraction_status"], "unsupported");
    fixture.cli(&["check"]);
}

#[test]
fn indexed_cli_source_request_previews_preserve_full_tree_and_mark_unknowns() {
    let fixture = Fixture::new();
    for args in [
        vec![
            "--dry-run",
            "source",
            "add",
            fixture.input.to_str().unwrap(),
        ],
        vec![
            "--dry-run",
            "source",
            "refresh",
            &fixture.source,
            "--file",
            fixture.input.to_str().unwrap(),
        ],
        vec![
            "--dry-run",
            "source",
            "withdraw",
            &fixture.source,
            "--reason",
            "Preview reason",
        ],
        vec![
            "--dry-run",
            "--stage",
            "source",
            "add",
            fixture.input.to_str().unwrap(),
        ],
    ] {
        let before = tree(&fixture.root);
        let preview = fixture.cli(&args);
        assert_eq!(tree(&fixture.root), before);
        assert_eq!(preview["data"]["plan_complete"], false);
        assert!(preview["data"]["reused"].is_null());
        assert_eq!(
            preview["data"]["validation"]["indexed_admission_checked"],
            false
        );
        assert_eq!(preview["data"]["validation"]["identities_reserved"], false);
        assert!(preview["data"]["change"].is_null());
    }
    fixture.cli_error(
        &["source", "withdraw", &fixture.source, "--reason", "   "],
        "RECORD_INVALID",
    );
}

#[test]
fn indexed_cli_staged_capture_refuses_externally_occupied_source_parent() {
    let fixture = Fixture::empty_public();
    let staged = fixture.cli(&["--stage", "source", "add", fixture.input.to_str().unwrap()]);
    let source = staged["data"]["allocated_ids"]["source"].as_str().unwrap();
    let change = staged["data"]["change"]["change_id"].as_str().unwrap();
    let occupied = fixture.root.join(format!("sources/{source}"));
    fs::create_dir(&occupied).unwrap();
    let before = canonical_tree(&fixture.root);
    fixture.cli_error(&["changes", "apply", change], "CONTENT_CONFLICT");
    assert_eq!(canonical_tree(&fixture.root), before);
    assert!(occupied.is_dir());
    assert_eq!(fs::read_dir(&occupied).unwrap().count(), 0);
    fixture.cli(&["changes", "abort", change]);
    assert!(occupied.is_dir());
    fixture.cli(&["index", "sync"]);
    fixture.cli(&["check"]);
}

#[test]
fn indexed_cli_whole_cache_loss_rebuild_preserves_capture_history_and_resumes_updates() {
    for migrate in [false, true] {
        let mut fixture = Fixture::empty_public_with_layout(migrate);
        let added = fixture.cli(&["source", "add", fixture.input.to_str().unwrap()]);
        fixture.source = added["data"]["allocated_ids"]["source"]
            .as_str()
            .unwrap()
            .to_owned();
        fixture.first = added["data"]["allocated_ids"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let first_epoch = fixture.context(&fixture.first, FIRST);
        let refresh = fixture.refresh(SECOND, None, None);
        let second = refresh["data"]["allocated_ids"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let epoch = fixture.context(&second, SECOND);
        assert_eq!(epoch, first_epoch + 1);
        let epoch = fixture.context(&second, SECOND);
        let canonical = canonical_tree(&fixture.root);
        let retained = fixture
            .root
            .join(if migrate { ".wiki/retained" } else { "changes" });
        let retained_before = tree(&retained);
        let old_cache = fixture.outside.join("preserved old cache");
        fs::rename(fixture.root.join(".wiki/cache"), &old_cache).unwrap();
        let preview_before = tree(&fixture.root);
        fixture.cli(&["--dry-run", "index", "rebuild", "--normalized"]);
        assert_eq!(tree(&fixture.root), preview_before);
        let rebuilt = fixture.cli(&["index", "rebuild", "--normalized"]);
        let snapshot = &rebuilt["data"]["report"]["snapshot"];
        assert_eq!(snapshot["generation"], epoch + 1);
        assert_ne!(snapshot["publication"]["file_id"], fixture.file_id);
        assert_eq!(canonical_tree(&fixture.root), canonical);
        assert_eq!(tree(&retained), retained_before);
        assert!(
            !fixture
                .root
                .join(".wiki/state/catalog-rebuild.json")
                .exists()
        );
        assert_eq!(
            rebuilt["data"]["maintenance"]["abandoned_rebuild_candidates"],
            serde_json::json!([])
        );
        fixture.file_id = snapshot["publication"]["file_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(fixture.context(&second, SECOND), epoch + 1);
        let historical = format!(
            "sources/{}/revisions/{}/content.md",
            fixture.source, fixture.first
        );
        assert_eq!(
            fixture.cli(&["read", "--path", &historical])["data"]["body"],
            FIRST
        );
        let check = fixture.cli(&["check"]);
        assert_eq!(check["data"]["complete"], true);
        let reused = fixture.refresh(SECOND, None, None);
        assert_eq!(reused["data"]["reused"], true);
        fixture.cli(&[
            "source",
            "withdraw",
            &fixture.source,
            "--reason",
            "Rebuild retained history",
        ]);
        let hits = fixture.cli(&["search", "IndexedSignal", "--source-id", &fixture.source]);
        assert!(hits["data"]["hits"].as_array().unwrap().is_empty());
        fixture.cli(&["recover"]);
        fixture.cli(&["check"]);
        assert!(old_cache.exists());
    }
}

#[test]
fn indexed_cli_rebuild_does_not_adopt_partially_lost_acquisition_gate() {
    let fixture = Fixture::empty_public();
    fs::remove_file(fixture.root.join(".wiki/cache/catalog-acquisition.lock")).unwrap();
    let before = tree(&fixture.root);
    fixture.cli_error(&["index", "rebuild", "--normalized"], "INDEX_CORRUPT");
    let mut after = tree(&fixture.root);
    let mut expected = before;
    // A real maintenance request acquires the writer permit before refusal.
    // Its diagnostic PID may change; no authority, cache or other file may.
    let lock = Path::new(".wiki/state/writer.lock");
    assert!(!after.remove(lock).unwrap().directory);
    assert!(!expected.remove(lock).unwrap().directory);
    assert_eq!(after, expected);
    assert!(
        !fixture
            .root
            .join(".wiki/state/catalog-rebuild.json")
            .exists()
    );
}

fn selection_reply(fixture: &Fixture, prepared: &Value) -> PathBuf {
    let packet = &prepared["data"]["selection_packet"];
    let task: Value = serde_json::from_str(packet["selector_input"].as_str().unwrap()).unwrap();
    assert_eq!(
        task["payload"]["binding"]["request"]["scope"],
        "indexed_documents"
    );
    let cards = task["payload"]["cards"].as_array().unwrap();
    assert!(!cards.is_empty());
    let selected = cards
        .iter()
        .take(20)
        .map(|card| card["id"].clone())
        .collect::<Vec<_>>();
    let reply = fixture.outside.join("host selection reply.json");
    fs::write(
        &reply,
        serde_json::to_vec(
            &serde_json::json!({"packet_fingerprint":packet["fingerprint"],"ordered_ids":selected}),
        )
        .unwrap(),
    )
    .unwrap();
    reply
}

#[test]
fn indexed_cli_selection_mixed_evidence_rechecks_changes_and_uncached_growth() {
    let fixture = Fixture::new();
    let page = authored_page(
        "page_selection",
        "IndexedSignal reviewed companion explains the amber ledger.",
    );
    fs::write(&fixture.input, &page).unwrap();
    fixture.cli(&[
        "page",
        "put",
        "--file",
        fixture.input.to_str().unwrap(),
        "--path",
        "pages/selection.md",
    ]);
    let args = [
        "context",
        "IndexedSignal",
        "--max-bytes",
        "6000",
        "--max-tokens",
        "1500",
        "--prepare-selection",
    ];
    let before = canonical_tree(&fixture.root);
    let prepared = fixture.cli(&args);
    assert_eq!(prepared["meta"]["freshness"], "indexed_evidence");
    assert!(prepared["data"]["text"].as_str().unwrap().is_empty());
    assert_eq!(canonical_tree(&fixture.root), before);
    let reply = selection_reply(&fixture, &prepared);
    let replay = fixture.cli(&args);
    assert_eq!(
        replay["data"]["selection_packet"],
        prepared["data"]["selection_packet"]
    );
    // Newly observed unindexed files are outside this selected proof. The
    // command still authenticates its actual owners, never a partial full audit.
    let unselected = fixture.root.join("unindexed");
    fs::create_dir(&unselected).unwrap();
    for n in 0..100 {
        fs::write(
            unselected.join(format!("unrelated-{n}.md")),
            "unrelated external text",
        )
        .unwrap();
    }
    let selected = fixture.cli(&[
        "context",
        "IndexedSignal",
        "--max-bytes",
        "6000",
        "--max-tokens",
        "1500",
        "--selection",
        reply.to_str().unwrap(),
    ]);
    let text = selected["data"]["text"].as_str().unwrap();
    assert!(
        text.contains("17 amber") && text.contains("reviewed companion"),
        "{selected}"
    );
    assert!(text.len() <= 6000);
    let passages = selected["data"]["passages"].as_array().unwrap();
    assert!(
        passages
            .iter()
            .any(|passage| passage["locator"]["record"]["record_id"] == "page_selection")
    );
    assert!(passages.iter().any(|passage| {
        passage["citations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|citation| {
                citation["kind"] == "source" && citation["reference"]["source_id"] == fixture.source
            })
    }));
    let source_path = fixture
        .root
        .join(format!("sources/{}/source.md", fixture.source));
    let source_bytes = fs::read(&source_path).unwrap();
    let changed = String::from_utf8(source_bytes.clone())
        .unwrap()
        .replace("Original source title", "External source title");
    fs::write(&source_path, changed).unwrap();
    fixture.cli_error(
        &[
            "context",
            "IndexedSignal",
            "--max-bytes",
            "6000",
            "--max-tokens",
            "1500",
            "--selection",
            reply.to_str().unwrap(),
        ],
        "FRESHNESS_CONFLICT",
    );
    fs::write(source_path, source_bytes).unwrap();
    let changed_page = authored_page(
        "page_selection",
        "IndexedSignal revised companion uses violet instead.",
    );
    fs::write(&fixture.input, &changed_page).unwrap();
    fixture.cli(&[
        "page",
        "put",
        "--file",
        fixture.input.to_str().unwrap(),
        "--if-match",
        Blake3Hash::digest(page.as_bytes()).as_str(),
    ]);
    fixture.cli_error(
        &[
            "context",
            "IndexedSignal",
            "--max-bytes",
            "6000",
            "--max-tokens",
            "1500",
            "--selection",
            reply.to_str().unwrap(),
        ],
        "FRESHNESS_CONFLICT",
    );
}

#[test]
fn indexed_cli_selection_staged_source_is_current_until_apply_and_withdrawal_rejects_reply() {
    let fixture = Fixture::new();
    let prepared = fixture.cli(&["context", "IndexedSignal", "--prepare-selection"]);
    let reply = selection_reply(&fixture, &prepared);
    let staged = fixture.refresh(SECOND, None, Some("--stage"));
    let before = canonical_tree(&fixture.root);
    let selected = fixture.cli(&[
        "context",
        "IndexedSignal",
        "--selection",
        reply.to_str().unwrap(),
    ]);
    assert!(
        selected["data"]["text"]
            .as_str()
            .unwrap()
            .contains("17 amber")
    );
    assert_eq!(canonical_tree(&fixture.root), before);
    let change = staged["data"]["change"]["change_id"].as_str().unwrap();
    fixture.cli(&["changes", "apply", change]);
    fixture.cli_error(
        &[
            "context",
            "IndexedSignal",
            "--selection",
            reply.to_str().unwrap(),
        ],
        "FRESHNESS_CONFLICT",
    );
    let refreshed = fixture.cli(&["context", "IndexedSignal", "--prepare-selection"]);
    let reply = selection_reply(&fixture, &refreshed);
    fixture.cli(&[
        "source",
        "withdraw",
        &fixture.source,
        "--reason",
        "Selected source retired",
    ]);
    fixture.cli_error(
        &[
            "context",
            "IndexedSignal",
            "--selection",
            reply.to_str().unwrap(),
        ],
        "FRESHNESS_CONFLICT",
    );
    let empty = fixture.cli(&["context", "IndexedSignal", "--prepare-selection"]);
    assert_eq!(empty["data"]["selection_packet"]["candidate_count"], 0);
    fixture.cli_error(
        &[
            "context",
            "IndexedSignal",
            "--scope",
            "snapshot",
            "--prepare-selection",
        ],
        "USAGE",
    );
    fixture.cli_error(
        &[
            "context",
            "IndexedSignal",
            "--scope",
            "indexed-evidence",
            "--prepare-selection",
        ],
        "USAGE",
    );
}

/// Independent canonical verification checks the citation emitted by the public
/// read command, including its exact returned range rather than a search hit.
fn exact_read_source_citation(
    fixture: &Fixture,
    read: &Value,
    revision: &str,
    original: &str,
    eligibility: &str,
    scope: CitationScope,
) -> CitationRef {
    assert_eq!(read["meta"]["freshness"], "indexed_evidence");
    assert_eq!(read["data"]["source_citation"]["eligibility"], eligibility);
    let citation: CitationRef =
        serde_json::from_value(read["data"]["source_citation"]["citation"].clone()).unwrap();
    let CitationRef::Source(reference) = &citation else {
        panic!("ordinary captured read must return a direct source citation");
    };
    let range: ByteSpan = serde_json::from_value(read["data"]["range"].clone()).unwrap();
    let quote = read["data"]["body"].as_str().unwrap();
    assert_eq!(reference.source_id.as_str(), fixture.source);
    assert_eq!(reference.source_revision.as_str(), revision);
    assert_eq!(reference.span, range);
    assert_eq!(range.slice(original).unwrap(), quote);
    assert_eq!(reference.quote_hash, Blake3Hash::digest(quote.as_bytes()));
    assert_eq!(
        read["data"]["hash"],
        Blake3Hash::digest(original.as_bytes()).as_str()
    );
    let handle = VaultFs::new(VaultRoot::explicit(&fixture.root).unwrap());
    let verified = SourceView::from_fs(&handle)
        .unwrap()
        .verify(&citation, scope)
        .unwrap();
    assert_eq!(verified.quote, quote.as_bytes());
    citation
}

#[test]
fn indexed_cli_read_source_citation_unicode_continuation_and_uncited_notes() {
    let fixture = Fixture::with_notes(&[(
        "pages/read-authored.md",
        b"---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_read_citation\ntitle: Authored read\nwiki_status: reviewed\n---\nExact authored prose.\n",
    )], true);
    let path = format!(
        "sources/{}/revisions/{}/content.md",
        fixture.source, fixture.first
    );
    // The byte allowance ends inside a multibyte codepoint; the public read must
    // finish at the preceding boundary and cite only those returned bytes.
    let cap = (FIRST.find('東').unwrap() + 1).to_string();
    let first = fixture.cli(&["read", "--path", &path, "--max-bytes", &cap]);
    assert_eq!(first["data"]["truncated"], true);
    assert_eq!(first["data"]["body"], &FIRST[..FIRST.find('東').unwrap()]);
    let first_citation = exact_read_source_citation(
        &fixture,
        &first,
        &fixture.first,
        FIRST,
        "current",
        CitationScope::Current,
    );
    let continuation: ByteSpan =
        serde_json::from_value(first["data"]["continuation"].clone()).unwrap();
    let initial_range: ByteSpan = serde_json::from_value(first["data"]["range"].clone()).unwrap();
    assert_eq!(initial_range.start(), 0);
    assert_eq!(initial_range.end(), continuation.start());
    assert_eq!(continuation.end(), FIRST.len() as u64);
    let start = continuation.start().to_string();
    let end = continuation.end().to_string();
    let next = fixture.cli(&["read", "--path", &path, "--start", &start, "--end", &end]);
    assert_eq!(next["data"]["truncated"], false);
    assert!(next["data"]["continuation"].is_null());
    let next_citation = exact_read_source_citation(
        &fixture,
        &next,
        &fixture.first,
        FIRST,
        "current",
        CitationScope::Current,
    );
    let next_range: ByteSpan = serde_json::from_value(next["data"]["range"].clone()).unwrap();
    assert_eq!(initial_range.end(), next_range.start());
    assert_ne!(first_citation, next_citation);
    assert_eq!(
        format!(
            "{}{}",
            first["data"]["body"].as_str().unwrap(),
            next["data"]["body"].as_str().unwrap()
        ),
        FIRST
    );
    let warning = first["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .find(|warning| warning.contains("continue with:"))
        .unwrap();
    let quoted_root = format!(
        "'{}'",
        fixture
            .root
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .replace('\'', "'\\''")
    );
    assert!(
        warning.contains(&format!(
            "lwiki --wiki {quoted_root} read --path '{path}' --start {start} --end {end}"
        )),
        "{warning}"
    );

    let cached = fixture.cli(&["read", "--path", &path, "--no-sync", "--max-bytes", &cap]);
    assert_eq!(cached["data"]["body"], first["data"]["body"]);
    assert!(cached["data"]["source_citation"].is_null());
    let cached_warning = cached["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .find(|warning| warning.contains("continue with:"))
        .unwrap();
    assert!(
        cached_warning.contains(&format!("lwiki --wiki {quoted_root} read --path '{path}'")),
        "{cached_warning}"
    );
    assert!(cached_warning.contains("--no-sync"), "{cached_warning}");
    let authored = fixture.cli(&["read", "--id", "page_read_citation"]);
    assert_eq!(authored["data"]["body"], "Exact authored prose.\n");
    assert!(authored["data"]["source_citation"].is_null());
    let empty = fixture.cli(&["read", "--path", &path, "--start", "0", "--end", "0"]);
    assert_eq!(empty["data"]["body"], "");
    assert!(empty["data"]["source_citation"].is_null());
    let dry = fixture.cli(&["--dry-run", "read", "--path", &path]);
    assert_eq!(dry["data"]["body"], FIRST);
    assert!(dry["data"]["source_citation"].is_null());

    let human = Command::new(binary())
        .current_dir(&fixture.outside)
        .arg("--wiki")
        .arg(&fixture.root)
        .args(["--offline", "read", "--path", &path, "--max-bytes", &cap])
        .output()
        .unwrap();
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    assert_eq!(
        human.stdout,
        first["data"]["body"].as_str().unwrap().as_bytes()
    );
    let stderr = String::from_utf8(human.stderr).unwrap();
    let source_line = stderr
        .lines()
        .find_map(|line| line.strip_prefix("Source citation (current): "))
        .expect("human stderr must identify Current source citation");
    let human_citation: CitationRef = serde_json::from_str(source_line).unwrap();
    assert_eq!(human_citation, first_citation);
    assert!(stderr.contains(&format!("lwiki --wiki {quoted_root} read --path '{path}'")));
}

#[test]
fn indexed_cli_read_source_citation_refresh_withdraw_and_external_edit() {
    let fixture = Fixture::new();
    let old_path = format!(
        "sources/{}/revisions/{}/content.md",
        fixture.source, fixture.first
    );
    let old_read = fixture.cli(&["read", "--path", &old_path]);
    let old_citation = exact_read_source_citation(
        &fixture,
        &old_read,
        &fixture.first,
        FIRST,
        "current",
        CitationScope::Current,
    );
    let old_bytes = fs::read(fixture.root.join(&old_path)).unwrap();
    let refreshed = fixture.refresh(SECOND, Some(TITLE), None);
    let revision = refreshed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    assert_ne!(revision, fixture.first);
    let new_path = format!("sources/{}/revisions/{revision}/content.md", fixture.source);
    let current = fixture.cli(&["read", "--path", &new_path]);
    let current_citation = exact_read_source_citation(
        &fixture,
        &current,
        revision,
        SECOND,
        "current",
        CitationScope::Current,
    );
    let historical = fixture.cli(&["read", "--path", &old_path]);
    assert_eq!(
        exact_read_source_citation(
            &fixture,
            &historical,
            &fixture.first,
            FIRST,
            "historical",
            CitationScope::Historical
        ),
        old_citation
    );
    assert_eq!(fs::read(fixture.root.join(&old_path)).unwrap(), old_bytes);
    let handle = VaultFs::new(VaultRoot::explicit(&fixture.root).unwrap());
    assert!(
        SourceView::from_fs(&handle)
            .unwrap()
            .verify(&old_citation, CitationScope::Current)
            .is_err()
    );

    fixture.cli(&[
        "source",
        "withdraw",
        &fixture.source,
        "--reason",
        "Superseded read fixture",
    ]);
    for (path, revision, body) in [
        (&old_path, fixture.first.as_str(), FIRST),
        (&new_path, revision, SECOND),
    ] {
        let withdrawn = fixture.cli(&["read", "--path", path]);
        exact_read_source_citation(
            &fixture,
            &withdrawn,
            revision,
            body,
            "withdrawn",
            CitationScope::Historical,
        );
    }
    let view = SourceView::from_fs(&handle).unwrap();
    assert!(view.verify(&old_citation, CitationScope::Current).is_err());
    assert!(
        view.verify(&current_citation, CitationScope::Current)
            .is_err()
    );
    assert_eq!(
        view.verify(&old_citation, CitationScope::Historical)
            .unwrap()
            .quote,
        FIRST.as_bytes()
    );
    assert_eq!(
        view.verify(&current_citation, CitationScope::Historical)
            .unwrap()
            .quote,
        SECOND.as_bytes()
    );
    drop(view);

    // Corrupt only this disposable selected content. The public ordinary read
    // must refuse rather than emitting cached bytes with a stale citation.
    let changed = "External bytes differ from the indexed immutable capture.\n";
    fs::write(fixture.root.join(&new_path), changed).unwrap();
    let refused = fixture.cli_error(&["read", "--path", &new_path], "FRESHNESS_CONFLICT");
    assert!(refused["data"].is_null(), "{refused}");
    assert_eq!(
        fs::read(fixture.root.join(&new_path)).unwrap(),
        changed.as_bytes()
    );
    let cached = fixture.cli(&["read", "--path", &new_path, "--no-sync"]);
    assert_eq!(cached["data"]["body"], SECOND);
    assert!(cached["data"]["source_citation"].is_null());
}

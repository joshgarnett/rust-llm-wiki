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
    domain::{Blake3Hash, ByteSpan, RecordId},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
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
        let vault = RecordId::new("vault_indexed_cli").unwrap();
        let selection = CatalogSelection::new(vault.clone(), 1).unwrap();
        selector::prepare(&handle, &writer, &selection).unwrap();
        let identity = BuildIdentity {
            selection: selection.clone(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        let mut builder =
            NormalizedBuilder::begin(&handle, &writer, identity, BuildLimits::default()).unwrap();
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
        drop(writer);
        Self {
            _temp: temp,
            root,
            outside,
            input,
            source: first.source_id.to_string(),
            first: first.revision_id.to_string(),
            file_id: selection.file_id,
        }
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
    // General read remains a separate migration requirement. This refresh slice
    // must expose its explicit fence rather than silently full-scan the vault.
    fixture.cli_error(
        &["read", "--id", &fixture.source, "--no-sync"],
        "CAPABILITY_UNAVAILABLE",
    );
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
        !preview["data"]["plan"]["operations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    same_read_tree(&fixture, before);
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
    assert_eq!(original["proof"]["version"], 2);
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
        fixture.cli_error(&args, "INDEX_CORRUPT");
        same_read_tree(&fixture, before);
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

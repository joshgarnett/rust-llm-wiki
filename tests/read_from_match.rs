//! Native public search -> forward read -> exact continuation, with local fixtures only.
#[path = "../test_support/paths.rs"]
mod test_paths;

use lwiki::{
    app::{OfflineApp, OperationOptions, ReadRequest, RecordSelector},
    domain::{Blake3Hash, ByteSpan, ErrorCode, VaultRelativePath},
    vault::{VaultFs, VaultRoot},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::SystemTime,
};

const PAGE: &str = "pages/authored.md";
const AUTHORED: &str = "a🦀éz\nBody coordinates exclude front matter.\n";

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new(normalized: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("read forward wiki with spaces");
        fs::create_dir_all(root.join("pages")).unwrap();
        fs::write(
            root.join("WIKI.md"),
            "---\nwiki_schema: \"1\"\nwiki_id: Vault.ReadForward\nwiki_kind: vault\ntitle: Forward read fixture\n---\n",
        )
        .unwrap();
        fs::write(
            root.join(PAGE),
            format!("---\nwiki_schema: \"1\"\nwiki_id: Page.Authored\nwiki_kind: page\ntitle: Authored\nwiki_status: reviewed\n---\n{AUTHORED}"),
        )
        .unwrap();
        let f = Self { temp, root };
        if normalized {
            f.ok(&["index", "rebuild", "--normalized"]);
        }
        f
    }
    fn run(&self, args: &[&str], json: bool) -> Output {
        let mut command = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
        command
            .current_dir(self.temp.path())
            .arg("--wiki")
            .arg(&self.root)
            .arg("--offline");
        if json {
            command.arg("--json");
        }
        command.args(args).output().unwrap()
    }
    fn invoke(&self, args: &[&str]) -> (i32, Value) {
        let output = self.run(args, true);
        assert!(output.stderr.is_empty(), "{args:?}: {:?}", output.stderr);
        let value: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {:?}", output.stdout));
        assert_eq!(value["meta"]["network_used"], false);
        (output.status.code().unwrap(), value)
    }
    fn ok(&self, args: &[&str]) -> Value {
        let (code, value) = self.invoke(args);
        assert_eq!(code, 0, "{args:?}: {value}");
        assert_eq!(value["ok"], true);
        value
    }
    fn usage(&self, args: &[&str]) -> Value {
        let (code, value) = self.invoke(args);
        assert_eq!(code, 2, "{args:?}: {value}");
        assert_eq!(value["error"]["code"], "USAGE", "{value}");
        assert!(value["data"]["source_citation"].is_null());
        value
    }
    fn add(&self, body: &str) -> (String, String) {
        let input = self.temp.path().join("capture input.md");
        fs::write(&input, body).unwrap();
        let result = self.ok(&[
            "source",
            "add",
            input.to_str().unwrap(),
            "--media-type",
            "text/markdown",
            "--title",
            "Forward captured source",
        ]);
        (
            result["data"]["allocated_ids"]["source"]
                .as_str()
                .unwrap()
                .to_owned(),
            result["data"]["allocated_ids"]["revision"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
    }
    fn search(&self, query: &str) -> Value {
        self.ok(&[
            "search",
            query,
            "--mode",
            "lexical",
            "--verify-selected",
            "--no-sync",
            "--limit",
            "1",
            "--candidates",
            "80",
            "--excerpt-bytes",
            "240",
        ])
    }
}

fn assert_source_read(
    result: &Value,
    path: &str,
    source: &str,
    revision: &str,
    original: &str,
    eligibility: &str,
) {
    let data = &result["data"];
    assert_eq!(data["path"], path);
    assert_eq!(
        data["hash"],
        Blake3Hash::digest(original.as_bytes()).as_str()
    );
    let span: ByteSpan = serde_json::from_value(data["range"].clone()).unwrap();
    assert!(!span.is_empty());
    assert_eq!(data["body"], span.slice(original).unwrap());
    let citation = &data["source_citation"];
    assert_eq!(citation["eligibility"], eligibility);
    assert_eq!(citation["citation"]["kind"], "source");
    let reference = &citation["citation"]["reference"];
    assert_eq!(reference["source_id"], source);
    assert_eq!(reference["source_revision"], revision);
    assert_eq!(reference["span"], data["range"]);
    assert_eq!(
        reference["quote_hash"],
        Blake3Hash::digest(data["body"].as_str().unwrap().as_bytes()).as_str()
    );
}

#[test]
fn native_verified_match_start_reads_forward_and_reuses_exact_continuation() {
    let f = Fixture::new(true);
    let original = format!(
        "# Captured guide\n\n{}\n\nforwardmatch: The cabinet requires two custodians.\n\n{}\nEnd of captured guide.\n",
        "Ordinary introductory material. ".repeat(180),
        "é🦀 Additional captured explanation. ".repeat(600),
    );
    let (source, revision) = f.add(&original);
    let found = f.search("forwardmatch");
    let hit = &found["data"]["hits"][0];
    assert_eq!(hit["source_id"], source);
    assert_eq!(hit["owner_revision"], revision);
    assert_eq!(hit["eligibility"], "current");
    assert_eq!(hit["excerpt"]["citation"]["kind"], "source");
    let primary = &hit["excerpt"]["citation"]["reference"];
    assert_eq!(primary["source_id"], source);
    assert_eq!(primary["source_revision"], revision);
    assert_eq!(primary["span"], hit["excerpt"]["span"]);
    let primary_span: ByteSpan = serde_json::from_value(primary["span"].clone()).unwrap();
    assert_eq!(
        hit["excerpt"]["text"],
        primary_span.slice(&original).unwrap()
    );
    assert_eq!(
        primary["quote_hash"],
        Blake3Hash::digest(primary_span.slice(&original).unwrap()).as_str()
    );
    let start = primary_span.start();
    assert!(start > 0, "exercise a discovered match beyond the prefix");
    let path = hit["path"].as_str().unwrap();
    assert_eq!(
        hit["locator"]["observed_hash"],
        Blake3Hash::digest(&original).as_str()
    );
    let start_arg = start.to_string();
    let first = f.ok(&[
        "read",
        "--path",
        path,
        "--start",
        &start_arg,
        "--max-bytes",
        "8192",
    ]);
    assert_source_read(&first, path, &source, &revision, &original, "current");
    assert_eq!(first["data"]["range"]["start"], start);
    let first_body = first["data"]["body"].as_str().unwrap();
    assert!(!first_body.is_empty() && first_body.len() <= 8192);
    assert!(first_body.len() > primary_span.len() as usize);
    assert_eq!(first["data"]["truncated"], true);
    assert_eq!(first["meta"]["partial"], true);
    let next = &first["data"]["continuation"];
    assert_eq!(next["start"], first["data"]["range"]["end"]);
    assert_eq!(next["end"], original.len());
    let next_start = next["start"].as_u64().unwrap().to_string();
    let next_end = next["end"].as_u64().unwrap().to_string();
    let second = f.ok(&[
        "read",
        "--path",
        path,
        "--start",
        &next_start,
        "--end",
        &next_end,
        "--max-bytes",
        "8192",
    ]);
    assert_source_read(&second, path, &source, &revision, &original, "current");
    assert_eq!(
        second["data"]["range"]["start"],
        first["data"]["range"]["end"]
    );
    assert_ne!(
        first["data"]["source_citation"]["citation"]["reference"]["quote_hash"],
        second["data"]["source_citation"]["citation"]["reference"]["quote_hash"]
    );
    let second_end = second["data"]["range"]["end"].as_u64().unwrap() as usize;
    assert_eq!(
        format!("{first_body}{}", second["data"]["body"].as_str().unwrap()),
        &original[start as usize..second_end]
    );

    let human = f.run(
        &[
            "read",
            "--path",
            path,
            "--start",
            &start_arg,
            "--max-bytes",
            "8192",
        ],
        false,
    );
    assert!(human.status.success());
    assert_eq!(human.stdout, first_body.as_bytes());
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert!(
        stderr.contains(path) && stderr.contains(&source) && stderr.contains(&revision),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("--start {next_start} --end {next_end}")),
        "{stderr}"
    );
    assert!(
        stderr.contains("--wiki") && stderr.contains(f.root.to_str().unwrap()),
        "{stderr}"
    );
}

#[test]
fn native_forward_read_preserves_utf8_empty_eof_limits_and_strict_explicit_bounds() {
    let f = Fixture::new(true);
    let original = "a🦀éz";
    let (source, revision) = f.add(original);
    let captured_path = format!("sources/{source}/revisions/{revision}/content.md");
    let path = captured_path.as_str();
    for cached in [false, true] {
        let mode = if cached { vec!["--no-sync"] } else { vec![] };
        let mut args = vec!["read", "--path", path, "--start", "1", "--max-bytes", "5"];
        args.extend_from_slice(&mode);
        let result = f.ok(&args);
        assert_eq!(result["data"]["body"], "🦀");
        assert_eq!(result["data"]["range"], json!({"start":1,"end":5}));
        assert_eq!(result["data"]["continuation"], json!({"start":5,"end":8}));
        if cached {
            assert!(result["data"]["source_citation"].is_null());
            assert_eq!(result["meta"]["freshness"], "index_snapshot");
        } else {
            assert_source_read(&result, path, &source, &revision, original, "current");
        }
        for bounds in [vec!["--start", "8"], vec!["--start", "0", "--end", "0"]] {
            let mut args = vec!["read", "--path", path];
            args.extend(bounds);
            args.extend_from_slice(&mode);
            let empty = f.ok(&args);
            assert_eq!(empty["data"]["body"], "");
            assert_eq!(empty["data"]["truncated"], false);
            assert!(empty["data"]["continuation"].is_null());
            assert!(empty["data"]["source_citation"].is_null());
        }
        for invalid in [
            vec!["--start", "9"],
            vec!["--start", "2"],
            vec!["--start", "18446744073709551615"],
            vec!["--start", "1", "--end", "9"],
            vec!["--start", "5", "--end", "1"],
            vec!["--start", "1", "--end", "2"],
            vec!["--end", "8"],
        ] {
            let mut args = vec!["read", "--path", path];
            args.extend(invalid);
            args.extend_from_slice(&mode);
            f.usage(&args);
        }
        for ceiling in ["0", "16777217"] {
            let mut args = vec![
                "read",
                "--path",
                path,
                "--start",
                "1",
                "--max-bytes",
                ceiling,
            ];
            args.extend_from_slice(&mode);
            let (code, invalid) = f.invoke(&args);
            assert_eq!(code, 2);
            assert_eq!(invalid["error"]["code"], "CONFIG_INVALID");
            assert!(invalid["data"].is_null());
        }
        let mut args = vec!["read", "--path", path, "--start", "1", "--max-bytes", "1"];
        args.extend_from_slice(&mode);
        let too_small = f.usage(&args);
        assert_eq!(too_small["error"]["details"]["minimum_max_bytes"], 4);
    }
    let all = f.ok(&["read", "--path", path]);
    let zero = f.ok(&["read", "--path", path, "--start", "0"]);
    let exact = f.ok(&["read", "--path", path, "--start", "0", "--end", "8"]);
    assert_eq!(all["data"], zero["data"]);
    assert_eq!(all["data"], exact["data"]);
    let overflow = f.usage(&["read", "--path", path, "--start", "18446744073709551616"]);
    assert!(overflow["data"]["body"].is_null());
}

#[test]
fn native_forward_read_retains_historical_and_withdrawn_eligibility_after_refresh() {
    let f = Fixture::new(true);
    let old_body =
        "# Historical guide\n\nlifecyclematch: Two custodians sign the retained blue form.\n";
    let new_body =
        "# Refreshed guide\n\nlifecyclematch: Three custodians sign the current amber form.\n";
    let (source, old_revision) = f.add(old_body);
    let old = f.search("lifecyclematch");
    let old_hit = &old["data"]["hits"][0];
    let old_path = old_hit["path"].as_str().unwrap();
    let start = old_hit["excerpt"]["citation"]["reference"]["span"]["start"]
        .as_u64()
        .unwrap()
        .to_string();
    let retained_bytes = fs::read(f.root.join(old_path)).unwrap();
    fs::write(f.temp.path().join("capture input.md"), new_body).unwrap();
    let refreshed = f.ok(&[
        "source",
        "refresh",
        &source,
        "--file",
        f.temp.path().join("capture input.md").to_str().unwrap(),
        "--media-type",
        "text/markdown",
    ]);
    let new_revision = refreshed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    assert_ne!(new_revision, old_revision);
    let history = f.ok(&["read", "--path", old_path, "--start", &start]);
    assert_source_read(
        &history,
        old_path,
        &source,
        &old_revision,
        old_body,
        "historical",
    );
    let current = f.search("lifecyclematch");
    let current_path = current["data"]["hits"][0]["path"].as_str().unwrap();
    let current_read = f.ok(&["read", "--path", current_path, "--start", "0"]);
    assert_source_read(
        &current_read,
        current_path,
        &source,
        new_revision,
        new_body,
        "current",
    );
    f.ok(&[
        "source",
        "withdraw",
        &source,
        "--reason",
        "Disposable lifecycle fixture",
    ]);
    let withdrawn = f.ok(&["read", "--path", current_path, "--start", "0"]);
    assert_source_read(
        &withdrawn,
        current_path,
        &source,
        new_revision,
        new_body,
        "withdrawn",
    );
    let retained = f.ok(&["read", "--path", old_path, "--start", &start]);
    assert_source_read(
        &retained,
        old_path,
        &source,
        &old_revision,
        old_body,
        "withdrawn",
    );
    assert_eq!(fs::read(f.root.join(old_path)).unwrap(), retained_bytes);
    assert!(
        f.search("lifecyclematch")["data"]["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_forward_read_uses_authored_body_coordinates_preserves_legacy_and_refuses_selected_drift()
{
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let explicit = f.ok(&[
            "read",
            "--id",
            "Page.Authored",
            "--start",
            "1",
            "--end",
            &AUTHORED.len().to_string(),
            "--max-bytes",
            "5",
        ]);
        let forward = f.ok(&[
            "read",
            "--id",
            "Page.Authored",
            "--start",
            "1",
            "--max-bytes",
            "5",
        ]);
        assert_eq!(forward["data"], explicit["data"]);
        assert_eq!(forward["data"]["body"], "🦀");
        assert!(forward["data"]["metadata"].is_object());
        assert!(forward["data"]["source_citation"].is_null());
        let cached = f.ok(&[
            "read",
            "--id",
            "Page.Authored",
            "--start",
            "1",
            "--max-bytes",
            "5",
            "--no-sync",
        ]);
        assert_eq!(cached["data"], explicit["data"]);
        assert!(cached["data"]["source_citation"].is_null());
        assert_eq!(cached["meta"]["freshness"], "index_snapshot");
        if !normalized {
            let app = OfflineApp::new(
                VaultFs::new(VaultRoot::explicit(&f.root).unwrap()),
                OperationOptions {
                    offline: true,
                    ..Default::default()
                },
            )
            .unwrap();
            let request = ReadRequest {
                selector: RecordSelector::Path(VaultRelativePath::new(PAGE).unwrap()),
                range: None,
                max_bytes: 5,
            };
            let library = app.read_from(request.clone(), 1).unwrap();
            assert_eq!(library.body, "🦀");
            assert!(library.source_citation.is_none());
            let mut exact_request = request;
            exact_request.range = Some(ByteSpan::new(1, AUTHORED.len() as u64).unwrap());
            assert_eq!(app.read(exact_request.clone()).unwrap().body, library.body);
            assert_eq!(
                app.read_from(exact_request, 1).unwrap_err().code,
                ErrorCode::Usage
            );
            let body = "legacymatch: Retained legacy captured passage.\n";
            f.add(body);
            let found = f.ok(&["search", "legacymatch", "--mode", "lexical"]);
            let path = found["data"]["hits"][0]["path"].as_str().unwrap();
            let exact = f.ok(&[
                "read",
                "--path",
                path,
                "--start",
                "1",
                "--end",
                &body.len().to_string(),
            ]);
            let forward = f.ok(&["read", "--path", path, "--start", "1"]);
            assert_eq!(forward["data"], exact["data"]);
            assert_eq!(forward["meta"]["freshness"], exact["meta"]["freshness"]);
            let cached = f.ok(&["read", "--path", path, "--start", "1", "--no-sync"]);
            assert_eq!(cached["data"]["body"], &body[1..]);
            assert!(cached["data"]["source_citation"].is_null());
        }
    }

    let f = Fixture::new(true);
    let original = "driftmatch: Source owner authentication still matters for a one-byte read.\n";
    let (source, _) = f.add(original);
    let found = f.search("driftmatch");
    let path = found["data"]["hits"][0]["path"].as_str().unwrap();
    let owner = f.ok(&["read", "--id", &source]);
    let owner_path = f.root.join(owner["data"]["path"].as_str().unwrap());
    let metadata = fs::metadata(&owner_path).unwrap();
    let bytes = fs::read_to_string(&owner_path).unwrap();
    let edited = bytes.replacen("Forward captured source", "Altered captured source", 1);
    assert_ne!(edited, bytes);
    assert_eq!(edited.len(), bytes.len());
    fs::write(&owner_path, edited).unwrap();
    fs::File::options()
        .write(true)
        .open(&owner_path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
        .unwrap();
    let (exit, refused) = f.invoke(&["read", "--path", path, "--start", "1", "--max-bytes", "1"]);
    assert_eq!(exit, 4, "{refused}");
    assert_eq!(refused["error"]["code"], "FRESHNESS_CONFLICT");
    assert!(refused["data"]["source_citation"].is_null());
    assert!(refused["data"]["body"].is_null());
    let cached = f.ok(&[
        "read",
        "--path",
        path,
        "--start",
        "1",
        "--max-bytes",
        "1",
        "--no-sync",
    ]);
    assert_eq!(cached["data"]["body"], &original[1..2]);
    assert!(cached["data"]["source_citation"].is_null());
}

fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let bytes = if metadata.is_file() {
                fs::read(&path).unwrap()
            } else {
                vec![]
            };
            out.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (bytes, metadata.modified().unwrap()),
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
fn native_read_start_only_dry_run_plans_unresolved_eof_without_target_or_catalog_access() {
    let f = Fixture::new(true);
    // Invalid publication bytes would refuse a command that opens the catalog.
    // Dry-run must still plan a missing target, without inspecting these bytes.
    fs::write(
        f.root.join(".wiki/cache/catalog-current.json"),
        b"not a catalog pointer",
    )
    .unwrap();
    let before = tree(&f.root);
    for cached in [false, true] {
        let mut args = vec![
            "--dry-run",
            "read",
            "--path",
            "missing/never-created.md",
            "--start",
            "18446744073709551615",
            "--max-bytes",
            "8192",
        ];
        if cached {
            args.push("--no-sync");
        }
        let plan = f.ok(&args);
        assert_eq!(plan["data"]["dry_run"], true);
        assert_eq!(
            plan["data"]["requested_range"],
            json!({"start":u64::MAX,"end":null})
        );
        assert_eq!(plan["data"]["selector"]["path"], "missing/never-created.md");
        assert_eq!(plan["data"]["max_bytes"], 8192);
        assert_eq!(
            plan["data"]["mode"],
            if cached { "cached" } else { "verified" }
        );
        assert!(plan["data"]["body"].is_null());
        assert!(plan["data"]["source_citation"].is_null());
        for field in [
            "target_resolution_performed",
            "utf8_range_validation_performed",
            "verification_performed",
        ] {
            assert_eq!(plan["data"][field], false);
        }
        assert!(plan["meta"]["freshness"].is_null());
        let human = f.run(&args, false);
        assert!(human.status.success(), "{:?}", human);
        let stdout = String::from_utf8(human.stdout).unwrap();
        assert!(
            stdout.contains("18446744073709551615..EOF (unresolved)"),
            "{stdout}"
        );
        assert!(stdout.contains("missing/never-created.md"), "{stdout}");
        assert_eq!(tree(&f.root), before);
        assert!(!f.root.join("missing").exists());
    }
    let all = f.ok(&["--dry-run", "read", "--path", "missing.md"]);
    assert!(all["data"]["requested_range"].is_null());
    let exact = f.ok(&[
        "--dry-run",
        "read",
        "--path",
        "missing.md",
        "--start",
        "1",
        "--end",
        "7",
    ]);
    assert_eq!(exact["data"]["requested_range"], json!({"start":1,"end":7}));
    f.usage(&["--dry-run", "read", "--path", "missing.md", "--end", "7"]);
    f.usage(&[
        "--dry-run",
        "read",
        "--path",
        "missing.md",
        "--start",
        "7",
        "--end",
        "1",
    ]);
    assert_eq!(tree(&f.root), before);
}

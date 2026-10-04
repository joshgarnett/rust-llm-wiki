//! Public normalized discovery -> selected read -> document context workflows.
//! All commands run offline, outside the vault, with a vault path containing spaces.
#[path = "../test_support/paths.rs"]
mod test_paths;
use lwiki::domain::{Blake3Hash, ByteSpan};
use serde_json::Value;
use std::{fs, path::Path, process::Command};

const PAGE: &str = "knowledge/pages/architecture.md";
const AUTHORED: &str = "workflowprobe: The authored cabinet contains 41 green folders.";
const CAPTURE: &str =
    "# Capture\n\nworkflowprobe: The captured cabinet contains 37 blue folders.\n";

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
struct Fixture {
    temp: tempfile::TempDir,
}
impl Fixture {
    fn new(normalized: bool) -> Self {
        let f = Self {
            temp: tempfile::tempdir().unwrap(),
        };
        copy(
            &test_paths::fixture(env!("CARGO_MANIFEST_DIR"), "tests/fixtures/bootstrap/vault"),
            &f.root(),
        );
        let path = f.root().join(PAGE);
        let body = fs::read_to_string(&path).unwrap();
        fs::write(path, format!("{body}\n{AUTHORED}\n")).unwrap();
        if normalized {
            f.ok(&["index", "rebuild", "--normalized"]);
        }
        f
    }
    fn root(&self) -> std::path::PathBuf {
        self.temp.path().join("mixed wiki with spaces")
    }
    fn invoke(&self, args: &[&str]) -> (i32, Value) {
        let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .current_dir(self.temp.path())
            .arg("--wiki")
            .arg(self.root())
            .args(["--json", "--offline"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.stderr.is_empty(), "{args:?}: {:?}", output.stderr);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["meta"]["network_used"], false);
        (output.status.code().unwrap(), value)
    }
    fn ok(&self, args: &[&str]) -> Value {
        let (code, value) = self.invoke(args);
        assert_eq!(code, 0, "{args:?}: {value}");
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
            "Workflow capture",
        ]);
        (
            result["data"]["allocated_ids"]["source"]
                .as_str()
                .unwrap()
                .into(),
            result["data"]["allocated_ids"]["revision"]
                .as_str()
                .unwrap()
                .into(),
        )
    }
}
fn assert_citation(passage: &Value, source: &str, revision: &str, body: &str) {
    assert_eq!(passage["label"], "captured_source");
    let span: ByteSpan = serde_json::from_value(passage["span"].clone()).unwrap();
    let quote = span.slice(body).unwrap();
    assert_eq!(passage["text"], quote);
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

#[test]
fn normalized_document_workflow_plain_mixed_discovery_read_and_context_have_distinct_honest_proofs()
{
    let f = Fixture::new(false);
    let (source, revision) = f.add(CAPTURE);
    f.ok(&["index", "rebuild", "--normalized"]);
    let search = f.ok(&["search", "workflowprobe"]);
    assert_eq!(search["meta"]["freshness"], "index_snapshot");
    assert!(
        search["data"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["locator"]["path"] == PAGE)
    );
    let read = f.ok(&["read", "--path", PAGE]);
    assert_eq!(read["meta"]["freshness"], "indexed_evidence");
    assert!(read["data"]["body"].as_str().unwrap().contains(AUTHORED));
    let result = f.ok(&["context", "workflowprobe"]);
    assert_eq!(result["meta"]["freshness"], "indexed_evidence");
    assert_eq!(
        result["data"]["verification"]["global_membership_verified"],
        false
    );
    assert_eq!(
        result["data"]["verification"]["discovery_generation"],
        search["meta"]["index_generation"]
    );
    let passages = result["data"]["passages"].as_array().unwrap();
    let authored = passages
        .iter()
        .find(|p| p["locator"]["path"] == PAGE)
        .expect("authored passage");
    assert_eq!(authored["label"], "note_text");
    assert_eq!(
        authored["locator"]["observed_hash"],
        Blake3Hash::digest(fs::read(f.root().join(PAGE)).unwrap()).as_str()
    );
    assert!(authored["text"].as_str().unwrap().contains(AUTHORED));
    assert!(authored["citations"].as_array().unwrap().is_empty());
    let captured = passages
        .iter()
        .find(|p| {
            p["label"] == "captured_source" && p["text"].as_str().unwrap().contains("37 blue")
        })
        .expect("capture passage");
    assert_citation(captured, &source, &revision, CAPTURE);
    let explicit = f.ok(&["context", "workflowprobe", "--scope", "indexed-documents"]);
    assert_eq!(explicit["data"]["passages"], result["data"]["passages"]);
    let source_only = f.ok(&["context", "workflowprobe", "--scope", "indexed-evidence"]);
    assert!(
        source_only["data"]["passages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["label"] == "captured_source")
    );
}

fn mutate_same_size_and_mtime(path: &Path, old: &str, new: &str) {
    assert_eq!(old.len(), new.len());
    let metadata = fs::metadata(path).unwrap();
    let bytes = fs::read_to_string(path).unwrap();
    assert!(bytes.contains(old));
    fs::write(path, bytes.replacen(old, new, 1)).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
        .unwrap();
    assert_eq!(fs::metadata(path).unwrap().len(), metadata.len());
    assert_eq!(
        fs::metadata(path).unwrap().modified().unwrap(),
        metadata.modified().unwrap()
    );
}
#[test]
fn normalized_document_workflow_selected_authored_and_support_tamper_refuse_but_cached_reads_remain_explicit()
 {
    for support in [false, true] {
        let f = Fixture::new(true);
        if support {
            mutate_same_size_and_mtime(
                &f.root().join("knowledge/evidence/forward_short.md"),
                "Verified",
                "Modified",
            );
        } else {
            mutate_same_size_and_mtime(&f.root().join(PAGE), "41 green", "42 green");
        }
        for args in [
            vec!["read", "--path", PAGE],
            vec!["context", "workflowprobe", "--scope", "indexed-documents"],
        ] {
            let (code, result) = f.invoke(&args);
            assert_ne!(code, 0, "tamper accepted: {args:?}: {result}");
            assert_eq!(result["error"]["code"], "FRESHNESS_CONFLICT", "{result}");
        }
        let cached = f.ok(&["read", "--path", PAGE, "--no-sync"]);
        assert_eq!(cached["meta"]["freshness"], "index_snapshot");
        assert!(
            cached["data"]["body"]
                .as_str()
                .unwrap()
                .contains("41 green")
        );
        assert_eq!(
            f.ok(&["search", "workflowprobe"])["meta"]["freshness"],
            "index_snapshot"
        );
    }
}

#[test]
fn normalized_document_workflow_unrelated_changes_do_not_turn_selected_verification_into_global_freshness()
 {
    let f = Fixture::new(true);
    let before = f.ok(&["search", "workflowprobe"]);
    fs::write(
        f.root().join("knowledge/pages/unindexed.md"),
        "workflowprobe: new unpublished note\n",
    )
    .unwrap();
    let after = f.ok(&["context", "workflowprobe"]);
    assert_eq!(
        after["meta"]["index_generation"],
        before["meta"]["index_generation"]
    );
    assert_eq!(
        after["data"]["verification"]["global_membership_verified"],
        false
    );
    assert!(
        !after["data"]["text"]
            .as_str()
            .unwrap()
            .contains("new unpublished")
    );
    let (code, current) = f.invoke(&["context", "workflowprobe", "--scope", "current"]);
    assert_ne!(code, 0);
    assert_eq!(current["error"]["code"], "CAPABILITY_UNAVAILABLE");
    assert_eq!(
        f.ok(&[
            "context",
            "workflowprobe",
            "--scope",
            "snapshot",
            "--no-sync"
        ])["meta"]["freshness"],
        "index_snapshot"
    );
}

#[test]
fn normalized_document_workflow_refresh_immediately_discovers_new_revision_and_keeps_old_capture_immutable()
 {
    let f = Fixture::new(false);
    let (source, old_revision) = f.add(CAPTURE);
    f.ok(&["index", "rebuild", "--normalized"]);
    let old_path = f.root().join(format!(
        "sources/{source}/revisions/{old_revision}/content.md"
    ));
    let old_bytes = fs::read(&old_path).unwrap();
    let fresh = "# Capture\n\nrefreshprobe: The captured cabinet now contains 53 amber folders.\n";
    let input = f.temp.path().join("refresh input.md");
    fs::write(&input, fresh).unwrap();
    let refreshed = f.ok(&[
        "source",
        "refresh",
        &source,
        "--file",
        input.to_str().unwrap(),
    ]);
    let revision = refreshed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    assert_ne!(revision, old_revision);
    let search = f.ok(&["search", "refreshprobe"]);
    assert!(!search["data"]["hits"].as_array().unwrap().is_empty());
    let result = f.ok(&["context", "refreshprobe"]);
    let passage = result["data"]["passages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["label"] == "captured_source")
        .unwrap();
    assert_citation(passage, &source, revision, fresh);
    assert_eq!(fs::read(old_path).unwrap(), old_bytes);
}

#[test]
fn normalized_document_workflow_legacy_omitted_scope_remains_current() {
    let f = Fixture::new(false);
    let result = f.ok(&["context", "workflowprobe"]);
    assert_eq!(result["meta"]["freshness"], "verified_snapshot");
    assert!(result["data"]["text"].as_str().unwrap().contains(AUTHORED));
}

#[test]
fn normalized_document_workflow_removed_selected_dependency_refuses_without_implicit_repair() {
    let f = Fixture::new(true);
    fs::remove_file(f.root().join("knowledge/assertions/forward.md")).unwrap();
    for args in [
        vec!["read", "--path", PAGE],
        vec!["context", "workflowprobe", "--scope", "indexed-documents"],
    ] {
        let (code, result) = f.invoke(&args);
        assert_ne!(code, 0, "removed dependency accepted: {result}");
        assert_eq!(result["error"]["code"], "FRESHNESS_CONFLICT");
        assert!(result["meta"]["verified_at"].is_null());
    }
    assert!(
        f.ok(&["read", "--path", PAGE, "--no-sync"])["data"]["body"]
            .as_str()
            .unwrap()
            .contains(AUTHORED)
    );
}

#[test]
fn normalized_document_workflow_selected_document_verification_budget_refuses_without_cached_fallback()
 {
    let f = Fixture::new(true);
    let generation = f.ok(&["search", "workflowprobe"])["meta"]["index_generation"].clone();
    let (code, result) = f.invoke(&[
        "context",
        "workflowprobe",
        "--scope",
        "indexed-documents",
        "--verification-max-bytes",
        "1",
    ]);
    assert_ne!(code, 0);
    assert_eq!(result["error"]["code"], "BUDGET_EXCEEDED");
    assert!(result["meta"]["freshness"].is_null());
    assert!(result["meta"]["verified_at"].is_null());
    assert_eq!(
        f.ok(&["search", "workflowprobe"])["meta"]["index_generation"],
        generation
    );
}

#[test]
fn normalized_document_workflow_large_authored_read_uses_document_budget() {
    let f = Fixture::new(false);
    let path = f.root().join(PAGE);
    let mut raw = fs::read(&path).unwrap();
    raw.extend("Detailed authored explanation. ".repeat(40_000).as_bytes());
    assert!(raw.len() > 1024 * 1024);
    fs::write(&path, &raw).unwrap();
    f.ok(&["index", "rebuild", "--normalized"]);
    let read = f.ok(&["read", "--path", PAGE, "--max-bytes", "4096"]);
    assert_eq!(read["meta"]["freshness"], "indexed_evidence");
    assert_eq!(read["data"]["hash"], Blake3Hash::digest(&raw).as_str());
    assert_eq!(read["data"]["truncated"], true);
    assert!(read["data"]["body"].as_str().unwrap().len() <= 4096);
}

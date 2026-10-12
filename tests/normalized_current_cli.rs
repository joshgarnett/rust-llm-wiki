//! Current normalized CLI discovery; native denied-write evidence is separate.
#[path = "../test_support/paths.rs"]
mod test_paths;

use lwiki::domain::{Blake3Hash, ByteSpan};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

fn page(id: &str, body: &str) -> String {
    format!(
        "---\nwiki_schema: '1'\nwiki_id: {id}\nwiki_kind: page\ntitle: {id}\nwiki_status: reviewed\n---\n{body}\n"
    )
}
fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_current_cli\nwiki_kind: vault\ntitle: Current notes\n---\n").unwrap();
    fs::create_dir(temp.path().join("pages")).unwrap();
    fs::write(
        temp.path().join("pages/old.md"),
        page(
            "page_old",
            "signalquartz: the older unrelated cabinet is blue.",
        ),
    )
    .unwrap();
    fs::write(
        temp.path().join("pages/edited.md"),
        page(
            "page_edited",
            "originalcopper: the safety keys are in the west drawer.",
        ),
    )
    .unwrap();
    ok(temp.path(), &["index", "rebuild", "--normalized"]);
    temp
}
fn invoke(root: &Path, args: &[&str]) -> (i32, Value) {
    let unrelated = tempfile::tempdir().unwrap();
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .current_dir(unrelated.path())
        .args(["--offline", "--json", "--no-user-config", "--wiki"])
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(output.stderr.is_empty(), "{args:?}: {:?}", output.stderr);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["meta"]["network_used"], false);
    (output.status.code().unwrap(), result)
}
fn ok(root: &Path, args: &[&str]) -> Value {
    let (exit, result) = invoke(root, args);
    assert_eq!(exit, 0, "{args:?}: {result}");
    assert_eq!(result["ok"], true);
    result
}
fn tree(root: &Path) -> BTreeMap<PathBuf, (Option<Blake3Hash>, SystemTime)> {
    fn walk(
        root: &Path,
        at: &Path,
        found: &mut BTreeMap<PathBuf, (Option<Blake3Hash>, SystemTime)>,
    ) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            found.insert(
                path.strip_prefix(root).unwrap().into(),
                (
                    if metadata.is_file() {
                        Some(Blake3Hash::digest(fs::read(&path).unwrap()))
                    } else {
                        None
                    },
                    metadata.modified().unwrap(),
                ),
            );
            if metadata.is_dir() {
                walk(root, &path, found);
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}
fn assert_current(result: &Value) {
    assert_eq!(result["meta"]["freshness"], "verified_snapshot", "{result}");
    assert!(result["meta"]["index_generation"].is_null());
    assert_eq!(result["meta"]["read_view"]["kind"], "current_observation");
    assert!(result["meta"]["read_view"]["published_generation"].is_null());
    assert!(
        !result["meta"]["read_view"]["fingerprint"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        result["data"]["health"]["observation"],
        "current_observation"
    );
    assert!(result["data"]["health"]["index_generation"].is_null());
    if let Some(snapshot) = result["data"].get("snapshot") {
        assert!(snapshot["generation"].is_null());
        assert!(snapshot.get("publication").is_none());
        assert!(snapshot["control_manifest"].is_string());
    }
}
fn paths(result: &Value) -> Vec<&str> {
    result["data"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["locator"]["path"].as_str().unwrap())
        .collect()
}

#[test]
fn ordinary_current_discovery_finds_changed_note_despite_old_positive_hit_and_tracks_membership() {
    let temp = fixture();
    let root = temp.path();
    fs::write(
        root.join("pages/edited.md"),
        page(
            "page_edited",
            "signalquartz: the safety keys moved to the east drawer.",
        ),
    )
    .unwrap();
    fs::write(
        root.join("pages/new.md"),
        page(
            "page_new",
            "newzeolite: the spare badge is inside the green box.",
        ),
    )
    .unwrap();
    let before = tree(root);
    for mode in ["literal", "lexical"] {
        let result = ok(root, &["search", "signalquartz", "--mode", mode]);
        assert_current(&result);
        let found = paths(&result);
        assert!(found.contains(&"pages/old.md"));
        assert!(found.contains(&"pages/edited.md"));
    }
    let read = ok(root, &["read", "--id", "page_edited"]);
    assert_current(&read);
    assert!(
        read["data"]["body"]
            .as_str()
            .unwrap()
            .contains("east drawer")
    );
    let context = ok(root, &["context", "signalquartz"]);
    assert_current(&context);
    assert!(
        context["data"]["text"]
            .as_str()
            .unwrap()
            .contains("east drawer")
    );
    let explicit = ok(root, &["context", "newzeolite", "--scope", "current"]);
    assert_current(&explicit);
    assert!(
        explicit["data"]["text"]
            .as_str()
            .unwrap()
            .contains("green box")
    );
    assert_eq!(before, tree(root));

    let cached = ok(root, &["search", "signalquartz", "--no-sync"]);
    assert_eq!(cached["meta"]["freshness"], "index_snapshot");
    assert!(cached["meta"].get("read_view").is_none());
    assert_eq!(paths(&cached), vec!["pages/old.md"]);
    let indexed = ok(
        root,
        &["context", "newzeolite", "--scope", "indexed-documents"],
    );
    assert_eq!(indexed["meta"]["freshness"], "indexed_evidence");
    assert!(indexed["data"]["passages"].as_array().unwrap().is_empty());

    fs::rename(root.join("pages/new.md"), root.join("pages/renamed.md")).unwrap();
    fs::remove_file(root.join("pages/edited.md")).unwrap();
    let changed = tree(root);
    let renamed = ok(root, &["search", "newzeolite"]);
    assert_current(&renamed);
    assert_eq!(paths(&renamed), vec!["pages/renamed.md"]);
    assert_eq!(
        paths(&ok(root, &["search", "signalquartz"])),
        vec!["pages/old.md"]
    );
    assert_eq!(changed, tree(root));
}

#[test]
fn observation_admission_limit_uses_honest_publication_but_explicit_current_refuses() {
    let temp = fixture();
    let root = temp.path();
    fs::write(root.join("pages/oversized.md"), vec![b'x'; 9 * 1024 * 1024]).unwrap();
    let before = tree(root);
    for args in [
        vec!["search", "signalquartz"],
        vec!["context", "signalquartz"],
        vec!["read", "--id", "page_old"],
    ] {
        let result = ok(root, &args);
        assert!(result["meta"]["index_generation"].as_u64().is_some());
        assert!(result["meta"].get("read_view").is_none());
        assert_ne!(result["meta"]["freshness"], "verified_snapshot");
        assert!(
            result["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning
                    .as_str()
                    .unwrap()
                    .contains("may miss new or edited notes"))
        );
    }
    let (exit, result) = invoke(root, &["context", "signalquartz", "--scope", "current"]);
    assert_ne!(exit, 0);
    assert_eq!(result["error"]["code"], "BUDGET_EXCEEDED");
    assert_eq!(before, tree(root));
    fs::remove_dir_all(root.join(".wiki/cache")).unwrap();
    let without_cache = tree(root);
    let (exit, result) = invoke(root, &["search", "signalquartz"]);
    assert_ne!(exit, 0);
    assert!(result["error"]["details"]["current_observation_unavailable"].is_object());
    assert_eq!(without_cache, tree(root));
}

#[test]
fn retained_authority_damage_cannot_fall_back_to_readable_published_results() {
    let temp = fixture();
    let root = temp.path();
    fs::write(
        root.join(".wiki/state/operations.json"),
        b"damaged authority",
    )
    .unwrap();
    let before = tree(root);
    for args in [
        vec!["search", "signalquartz"],
        vec!["context", "signalquartz"],
        vec!["read", "--id", "page_old"],
    ] {
        let (exit, result) = invoke(root, &args);
        assert_ne!(exit, 0);
        assert_eq!(result["error"]["code"], "RECOVERY_REQUIRED");
        assert!(result["meta"]["verified_at"].is_null());
    }
    assert_eq!(before, tree(root));
}

#[test]
fn current_source_search_context_and_read_preserve_exact_citations_after_cache_loss() {
    let temp = fixture();
    let root = temp.path();
    let input = tempfile::NamedTempFile::new().unwrap();
    let text = "citationzircon: the emergency badge is beside the north cabinet.\n";
    fs::write(input.path(), text).unwrap();
    let capture = ok(
        root,
        &[
            "source",
            "add",
            input.path().to_str().unwrap(),
            "--media-type",
            "text/markdown",
            "--title",
            "Emergency badge",
        ],
    );
    let source = capture["data"]["allocated_ids"]["source"].as_str().unwrap();
    let revision = capture["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    let path = format!("sources/{source}/revisions/{revision}/content.md");
    fs::remove_dir_all(root.join(".wiki/cache")).unwrap();
    let before = tree(root);
    let search = ok(root, &["search", "citationzircon"]);
    assert_current(&search);
    let hit = search["data"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["locator"]["path"] == path)
        .unwrap();
    let excerpt = &hit["excerpt"];
    let span: ByteSpan = serde_json::from_value(excerpt["span"].clone()).unwrap();
    let quote = span.slice(text).unwrap();
    assert_eq!(excerpt["text"], quote);
    assert_eq!(
        excerpt["citation"]["reference"]["quote_hash"],
        Blake3Hash::digest(quote.as_bytes()).as_str()
    );
    let context = ok(root, &["context", "citationzircon"]);
    assert_current(&context);
    let passage = context["data"]["passages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|passage| passage["locator"]["path"] == path)
        .unwrap();
    let span: ByteSpan = serde_json::from_value(passage["span"].clone()).unwrap();
    assert_eq!(passage["text"], span.slice(text).unwrap());
    assert_eq!(passage["citations"][0]["reference"]["source_id"], source);
    assert_eq!(
        passage["citations"][0]["reference"]["source_revision"],
        revision
    );
    assert_eq!(
        passage["citations"][0]["reference"]["quote_hash"],
        Blake3Hash::digest(span.slice(text).unwrap().as_bytes()).as_str()
    );
    let read = ok(root, &["read", "--path", &path]);
    assert_current(&read);
    assert_eq!(read["data"]["body"], text);
    assert_eq!(read["data"]["source_citation"]["eligibility"], "current");
    assert_eq!(
        read["data"]["source_citation"]["citation"]["reference"]["quote_hash"],
        Blake3Hash::digest(text.as_bytes()).as_str()
    );
    assert_eq!(before, tree(root));
}

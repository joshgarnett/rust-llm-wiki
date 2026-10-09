//! Actual CLI Page moves preserve one identity, exact author bytes and provenance.
#[path = "../test_support/paths.rs"]
mod test_paths;

use lwiki::domain::Blake3Hash;
use pulldown_cmark::{Event, Parser, Tag};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const BEGIN: &str = "<!-- lwiki:source-citations:v1 begin -->";
const END: &str = "<!-- lwiki:source-citations:v1 end -->";

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Page identity wiki with spaces");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: Vault.PageMove\nwiki_kind: vault\ntitle: Page move identity\n---\n").unwrap();
        Self { temp, root }
    }
    fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let target = self.root.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    fn invoke(&self, args: &[&str]) -> (i32, Value) {
        let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .current_dir(self.temp.path())
            .arg("--wiki")
            .arg(&self.root)
            .args(["--offline", "--json"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.stderr.is_empty(), "{args:?}: {:?}", output.stderr);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["meta"]["network_used"], false);
        (output.status.code().unwrap(), value)
    }
    fn ok(&self, args: &[&str]) -> Value {
        let (exit, value) = self.invoke(args);
        assert_eq!(exit, 0, "{args:?}: {value}");
        assert_eq!(value["ok"], true);
        value
    }
    fn refused(&self, args: &[&str], code: &str) -> Value {
        let (exit, value) = self.invoke(args);
        assert_ne!(exit, 0, "{args:?}: {value}");
        assert_eq!(value["error"]["code"], code, "{args:?}: {value}");
        value
    }
}

fn page(id: &str, extra: &str, body: &str) -> String {
    format!(
        "---\nwiki_schema: \"1\"\nwiki_id: {id}\nwiki_kind: page\ntitle: {id}\nwiki_status: reviewed\n{extra}---\n{body}"
    )
}

#[test]
fn native_sole_page_and_plain_incoming_move_keep_exact_author_and_one_current_identity() {
    let f = Fixture::new();
    let original = page(
        "Page.Guide",
        "# Author envelope comment\nauthor_annotation: Keep this value\n",
        "# Anchor\n\nmoveidentityneedle: café 東京 🦀 author wording.\n",
    );
    let incoming = "Plain author intro. [[pages/guide.md#Anchor|Kept label]]\n[inline](pages/guide.md#Anchor \"Kept title\")\n`[[pages/guide.md]]`\nPlain author ending.\n";
    let untouched = b"Unrelated author material.\n";
    f.write("pages/guide.md", &original);
    f.write("incoming.md", incoming);
    f.write("occupied.md", "An occupied untyped note.\n");
    f.write("unrelated.md", untouched);
    f.ok(&["index", "rebuild", "--normalized"]);
    let before = f.ok(&["read", "--id", "Page.Guide"]);
    let hash = before["data"]["hash"].as_str().unwrap();
    assert_eq!(hash, Blake3Hash::digest(&original).as_str());
    let wrong = Blake3Hash::digest("wrong author bytes");
    f.refused(
        &[
            "page",
            "rename",
            "Page.Guide",
            "--to",
            "handbook/Guide.md",
            "--if-match",
            wrong.as_str(),
        ],
        "CONTENT_CONFLICT",
    );
    f.refused(
        &[
            "page",
            "rename",
            "Page.Guide",
            "--to",
            "occupied.md",
            "--if-match",
            hash,
        ],
        "CONTENT_CONFLICT",
    );
    assert_eq!(
        fs::read(f.root.join("pages/guide.md")).unwrap(),
        original.as_bytes()
    );
    assert_eq!(
        fs::read(f.root.join("incoming.md")).unwrap(),
        incoming.as_bytes()
    );
    assert!(!f.root.join("handbook/Guide.md").exists());
    let noop = f.ok(&[
        "page",
        "rename",
        "Page.Guide",
        "--to",
        "pages/guide.md",
        "--if-match",
        hash,
    ]);
    assert_eq!(noop["data"]["reused"], true);
    assert!(noop["data"]["change"].is_null());
    assert!(noop["data"]["snapshot"].is_null());

    let moved = f.ok(&[
        "page",
        "rename",
        "Page.Guide",
        "--to",
        "handbook/Guide.md",
        "--if-match",
        hash,
    ]);
    assert_eq!(moved["data"]["status"], "committed");
    assert!(!f.root.join("pages/guide.md").exists());
    assert_eq!(
        fs::read(f.root.join("handbook/Guide.md")).unwrap(),
        original.as_bytes()
    );
    assert_eq!(
        fs::read(f.root.join("incoming.md")).unwrap(),
        incoming
            .replace(
                "[[pages/guide.md#Anchor|Kept label]]",
                "[[handbook/Guide.md#Anchor|Kept label]]"
            )
            .replace(
                "[inline](pages/guide.md#Anchor",
                "[inline](handbook/Guide.md#Anchor"
            )
            .as_bytes()
    );
    assert_eq!(fs::read(f.root.join("unrelated.md")).unwrap(), untouched);
    let current = f.ok(&["read", "--id", "Page.Guide"]);
    let destination = f.ok(&["read", "--path", "handbook/Guide.md"]);
    assert_eq!(current["data"], destination["data"]);
    assert_eq!(current["data"]["path"], "handbook/Guide.md");
    assert_eq!(current["data"]["hash"], before["data"]["hash"]);
    assert_eq!(current["data"]["record"], before["data"]["record"]);
    assert_eq!(current["data"]["body"], before["data"]["body"]);
    assert!(current["data"]["source_citation"].is_null());
    f.refused(
        &["read", "--path", "pages/guide.md", "--no-sync"],
        "RECORD_NOT_FOUND",
    );
    let found = f.ok(&[
        "search",
        "moveidentityneedle",
        "--mode",
        "lexical",
        "--kind",
        "page",
        "--verify-selected",
        "--no-sync",
    ]);
    let hits = found["data"]["hits"].as_array().unwrap();
    assert_eq!(
        hits.len(),
        1,
        "one unique current Page, without old-path ownership"
    );
    assert_eq!(hits[0]["path"], "handbook/Guide.md");
    assert_eq!(hits[0]["record_ref"]["record_id"], "Page.Guide");
    assert_eq!(hits[0]["eligibility"], "current");
    let check = f.ok(&["check"]);
    assert_eq!(check["data"]["complete"], true);
    assert_eq!(check["data"]["error_count"], 0);
    assert_eq!(check["data"]["cache_matches_canonical"], true);
}

fn source_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}
fn citation_parts(markdown: &str) -> (&str, &str, &str) {
    assert_eq!(markdown.matches(BEGIN).count(), 1);
    assert_eq!(markdown.matches(END).count(), 1);
    let start = markdown.find(BEGIN).unwrap();
    let end = markdown.find(END).unwrap() + END.len();
    (&markdown[..start], &markdown[start..end], &markdown[end..])
}
fn citation_links(root: &Path, page: &str, block: &str) -> Vec<PathBuf> {
    let base = url::Url::from_file_path(root.join(page)).unwrap();
    let mut links = Vec::new();
    for event in Parser::new(block) {
        if let Event::Start(Tag::Link { dest_url, .. }) = event {
            assert!(!dest_url.starts_with('/'));
            links.push(
                fs::canonicalize(base.join(&dest_url).unwrap().to_file_path().unwrap()).unwrap(),
            );
        }
    }
    links.sort();
    links
}
fn encoded_refs(block: &str) -> Value {
    let encoded = block
        .split("```json\n")
        .nth(1)
        .unwrap()
        .split("\n```")
        .next()
        .unwrap();
    serde_json::from_str(encoded).unwrap()
}

#[test]
fn native_cited_page_move_rebases_only_provenance_and_delete_sync_never_resurrects_identity() {
    let f = Fixture::new();
    f.ok(&["index", "rebuild", "--normalized"]);
    let input = f.temp.path().join("source input.md");
    fs::write(
        &input,
        "citedmoveneedle: The source requires two custodians.\n",
    )
    .unwrap();
    let captured = f.ok(&[
        "source",
        "add",
        input.to_str().unwrap(),
        "--title",
        "Unmoved source",
        "--media-type",
        "text/markdown",
    ]);
    let source = captured["data"]["allocated_ids"]["source"]
        .as_str()
        .unwrap();
    let revision = captured["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    let discovered = f.ok(&[
        "search",
        "citedmoveneedle",
        "--mode",
        "lexical",
        "--verify-selected",
        "--no-sync",
    ]);
    let payload_path = discovered["data"]["hits"][0]["path"].as_str().unwrap();
    let proof = f.ok(&["read", "--path", payload_path]);
    let citation = &proof["data"]["source_citation"]["citation"];
    assert_eq!(citation["kind"], "source");
    assert_eq!(citation["reference"]["source_id"], source);
    assert_eq!(citation["reference"]["source_revision"], revision);
    let refs = f.temp.path().join("returned source refs.json");
    fs::write(
        &refs,
        serde_json::to_vec(&json!({"schema_version":"1","citations":[citation]})).unwrap(),
    )
    .unwrap();
    let author_input = f.temp.path().join("author input.md");
    fs::write(
        &author_input,
        "# Author answer\n\ncitedpageidentity: Keep this exact author wording café.\n",
    )
    .unwrap();
    f.ok(&[
        "page",
        "init",
        "--file",
        author_input.to_str().unwrap(),
        "--title",
        "Cited Page",
        "--id",
        "Page.Cited",
        "--path",
        "pages/cited.md",
        "--source-refs",
        refs.to_str().unwrap(),
    ]);
    let before = f.ok(&["read", "--id", "Page.Cited"]);
    let original = fs::read_to_string(f.root.join("pages/cited.md")).unwrap();
    let (author_before, block_before, trailing_before) = citation_parts(&original);
    let original_links = citation_links(&f.root, "pages/cited.md", block_before);
    assert_eq!(
        original_links.len(),
        3,
        "Source, Revision and captured payload links"
    );
    let owner = f.ok(&["read", "--id", source]);
    let revision_note = f.ok(&["read", "--id", revision]);
    let mut expected = [
        owner["data"]["path"].as_str().unwrap(),
        revision_note["data"]["path"].as_str().unwrap(),
        payload_path,
    ]
    .into_iter()
    .map(|p| fs::canonicalize(f.root.join(p)).unwrap())
    .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(original_links, expected);
    let sources_before = source_files(&f.root.join("sources"));
    f.write("incoming.md", "Plain incoming [[pages/cited.md|Answer]].\n");
    f.ok(&["index", "sync"]);
    let moved = f.ok(&[
        "page",
        "rename",
        "Page.Cited",
        "--to",
        "handbook/deep/Answer.md",
        "--if-match",
        before["data"]["hash"].as_str().unwrap(),
    ]);
    assert_eq!(moved["data"]["status"], "committed");
    let moved_bytes = fs::read(f.root.join("handbook/deep/Answer.md")).unwrap();
    let moved_text = std::str::from_utf8(&moved_bytes).unwrap();
    let (author_after, block_after, trailing_after) = citation_parts(moved_text);
    assert_eq!(author_after, author_before);
    assert_eq!(trailing_after, trailing_before);
    assert_eq!(encoded_refs(block_after), encoded_refs(block_before));
    assert_eq!(
        citation_links(&f.root, "handbook/deep/Answer.md", block_after),
        expected
    );
    assert!(!f.root.join("pages/cited.md").exists());
    assert_eq!(
        fs::read(f.root.join("incoming.md")).unwrap(),
        b"Plain incoming [[handbook/deep/Answer.md|Answer]].\n"
    );
    let current = f.ok(&["read", "--id", "Page.Cited"]);
    assert_eq!(current["data"]["record"], before["data"]["record"]);
    assert_eq!(current["data"]["path"], "handbook/deep/Answer.md");
    let source_after = f.ok(&["read", "--path", payload_path]);
    assert_eq!(source_after["data"], proof["data"]);
    assert_eq!(source_files(&f.root.join("sources")), sources_before);

    // A valid Decision output reference must become dangling on deletion,
    // then recover when the same saved Page identity is restored. Seed this
    // authored record through ordinary external-edit synchronization.
    let dependent = "---\nwiki_schema: \"1\"\nwiki_id: Decision.PageOutput\nwiki_kind: decision\ntitle: Page output decision\nwiki_status: active\nwiki_action: correct\nwiki_input_ids: []\nwiki_output_ids: [Page.Cited]\nwiki_created_at: \"2026-10-09T00:00:00Z\"\n---\nAn authored decision output [[Page.Cited]].\n";
    f.write("decisions/output.md", dependent);
    f.ok(&["index", "sync"]);
    let predecessor = f.ok(&["read", "--id", "Decision.PageOutput"]);
    assert!(
        predecessor["data"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    fs::remove_file(f.root.join("handbook/deep/Answer.md")).unwrap();
    let deleted = f.ok(&["index", "sync"]);
    assert_eq!(
        deleted["data"]["maintenance"]["page_sync"]["pages_deleted"],
        1
    );
    assert!(deleted["data"]["maintenance"]["build"].is_null());
    f.refused(
        &["read", "--id", "Page.Cited", "--no-sync"],
        "RECORD_NOT_FOUND",
    );
    f.refused(
        &["read", "--path", "handbook/deep/Answer.md", "--no-sync"],
        "RECORD_NOT_FOUND",
    );
    let absent = f.ok(&[
        "search",
        "citedpageidentity",
        "--mode",
        "lexical",
        "--kind",
        "page",
        "--status",
        "draft",
        "--no-sync",
    ]);
    assert!(absent["data"]["hits"].as_array().unwrap().is_empty());
    let dangling = f.ok(&["read", "--id", "Decision.PageOutput"]);
    assert_eq!(dangling["data"]["body"], predecessor["data"]["body"]);
    assert!(
        dangling["data"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |d| d["details"]["reason"] == "invalid_reference:wiki_output_ids"
                    && d["details"]["details"]["target"] == "Page.Cited"
            )
    );
    assert_eq!(
        fs::read(f.root.join("decisions/output.md")).unwrap(),
        dependent.as_bytes()
    );

    f.write("handbook/deep/Answer.md", &moved_bytes);
    let restored = f.ok(&["index", "sync"]);
    assert_eq!(
        restored["data"]["maintenance"]["page_sync"]["pages_created"],
        1
    );
    assert!(restored["data"]["maintenance"]["build"].is_null());
    let returned = f.ok(&["read", "--id", "Page.Cited"]);
    assert_eq!(returned["data"], current["data"]);
    let predecessor_restored = f.ok(&["read", "--id", "Decision.PageOutput"]);
    assert_eq!(predecessor_restored["data"], predecessor["data"]);
    assert_eq!(source_files(&f.root.join("sources")), sources_before);
    let check = f.ok(&["check"]);
    assert_eq!(check["data"]["complete"], true);
    assert_eq!(check["data"]["error_count"], 0);
    assert_eq!(check["data"]["cache_matches_canonical"], true);
}

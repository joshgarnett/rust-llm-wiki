#[path = "../test_support/paths.rs"]
mod test_paths;
use lwiki::{domain::*, records::parse_note, vault::VaultRoot};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};
fn invoke(root: &Path, args: &[&str], request: Option<&[u8]>) -> (i32, Value) {
    let mut child = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--wiki")
        .arg(root)
        .args(["--offline", "--json"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(b) = request {
        child.stdin.as_mut().unwrap().write_all(b).unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["meta"]["network_used"], false);
    (out.status.code().unwrap(), v)
}
fn ok(root: &Path, args: &[&str], req: Option<&Value>) -> Value {
    let bytes = req.map(|v| serde_json::to_vec(v).unwrap());
    let (exit, v) = invoke(root, args, bytes.as_deref());
    assert_eq!(exit, 0, "{args:?}: {v}");
    v["data"].clone()
}
fn apply(root: &Path, out: &Value) {
    ok(
        root,
        &[
            "changes",
            "apply",
            out["prepared"]["change_id"].as_str().unwrap(),
        ],
        None,
    );
}
fn notes(root: &Path) -> Vec<(VaultRelativePath, lwiki::records::ParsedNote)> {
    VaultRoot::explicit(root)
        .unwrap()
        .scan_markdown()
        .unwrap()
        .into_iter()
        .map(|p| {
            let n = parse_note(&fs::read(root.join(p.as_str())).unwrap());
            (p, n)
        })
        .collect()
}
fn note(root: &Path, id: &str) -> (VaultRelativePath, lwiki::records::ParsedNote) {
    notes(root)
        .into_iter()
        .find(|(_, n)| n.canonical.as_ref().is_some_and(|r| r.id().as_str() == id))
        .unwrap()
}
fn guards(root: &Path) -> Value {
    json!(
        notes(root)
            .iter()
            .filter_map(|(_, n)| n
                .canonical
                .as_ref()
                .map(|r| json!({"record_id":r.id(),"hash":n.source_hash})))
            .collect::<Vec<_>>()
    )
}
fn review(root: &Path, aid: &str, supers: &[&str]) -> Value {
    let (_, an) = note(root, aid);
    let checks=notes(root).into_iter().filter_map(|(_,n)|{let r=n.canonical.as_ref()?;(r.kind()==RecordKind::Evidence&&r.string("wiki_assertion_id")==Some(aid)&&r.string("wiki_status")==Some("active")).then(||json!({"evidence_id":r.id(),"expected_hash":n.source_hash,"assessment":"supports"}))}).collect::<Vec<_>>();
    json!({"schema":"lwiki.graph-review.v1","decisions":[{"assertion_id":aid,"expected_hash":an.source_hash,"decision":"accept","reason":"Authorized full evidence review","evidence_checks":checks}],"supersedes":supers.iter().map(|id|{let(_,n)=note(root,id);json!({"record_id":id,"hash":n.source_hash})}).collect::<Vec<_>>()})
}
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, p: &Path, map: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for e in fs::read_dir(p).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            map.insert(
                p.strip_prefix(root).unwrap().into(),
                (
                    if m.is_file() {
                        fs::read(&p).unwrap()
                    } else {
                        vec![]
                    },
                    m.modified().unwrap(),
                ),
            );
            if m.is_dir() {
                walk(root, &p, map);
            }
        }
    }
    let mut map = BTreeMap::new();
    walk(root, root, &mut map);
    map
}
#[test]
fn m2_packet_import_apply_resolve_apply_decide_apply_review_apply() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("WIKI.md"),
        "---\nwiki_schema: \"1\"\nwiki_id: Vault.M2\nwiki_kind: vault\ntitle: M2 workflow\n---\n",
    )
    .unwrap();
    let text = "Ada works for Acme.\r\nAda is another homonym.\r\n";
    let (exit, source) = invoke(
        root,
        &["source", "add", "-", "--title", "M2 source"],
        Some(text.as_bytes()),
    );
    assert_eq!(exit, 0, "{source}");
    let sid = source["data"]["allocated_ids"]["source"].as_str().unwrap();
    let exported = ok(root, &["graph", "extract", "--source-id", sid], None);
    let packet = &exported["packet"];
    let second = text.rfind("Ada").unwrap();
    let response = json!({"schema":"lwiki.extraction.v1","packet_id":packet["packet_id"],"packet_fingerprint":packet["packet_fingerprint"],"mentions":[{"id":"m1","window_id":"w1","label":"Ada","type":"person","quote":"Ada","span":{"start":0,"end":3}},{"id":"m2","window_id":"w1","label":"Acme","type":"organization","quote":"Acme"},{"id":"m3","window_id":"w1","label":"Ada","type":"person","quote":"Ada","span":{"start":second,"end":second+3}}],"assertions":[{"id":"a1","subject":"m1","predicate":"works_for","object":{"kind":"mention","mention_id":"m2"},"negated":false,"modality":"asserted","evidence":[{"window_id":"w1","stance":"supports","quote":"Ada works for Acme.\r\n"}]}],"unresolved":[]});
    let imported = ok(root, &["graph", "import", "--file", "-"], Some(&response));
    apply(root, &imported);
    let xid = imported["extraction"]["record"]["record_id"]
        .as_str()
        .unwrap();
    let (_, xn) = note(root, xid);
    let resolve = json!({"schema":"lwiki.graph-resolution.v1","extraction_id":xid,"expected_hash":xn.source_hash,"mappings":[{"operation":"CreateEntity","mention_id":"m1","reason":"First homonym explicitly identified","title":"Ada","entity_type":"person"},{"operation":"CreateEntity","mention_id":"m2","reason":"Organization explicitly identified","title":"Acme","entity_type":"organization"},{"operation":"CreateEntity","mention_id":"m3","reason":"Second homonym kept distinct","title":"Ada","entity_type":"person"}]});
    let resolved = ok(root, &["graph", "resolve", "--file", "-"], Some(&resolve));
    apply(root, &resolved);
    let aid = imported["allocations"]["assertions"]["a1"]
        .as_str()
        .unwrap();
    let first = resolved["allocations"]["entities"]["m1"].as_str().unwrap();
    let second = resolved["allocations"]["entities"]["m3"].as_str().unwrap();
    assert_ne!(first, second);
    let alias = json!({"schema":"lwiki.entity-decisions.v1","decisions":[{"operation":"AddAlias","entity_id":first,"alias":"Explicit first Ada","reason":"Label does not merge homonyms","expected_records":guards(root),"remaps":[]}]});
    let decided = ok(root, &["graph", "decide", "--file", "-"], Some(&alias));
    apply(root, &decided);
    let req = review(root, aid, &[]);
    let schema = ok(root, &["schema", "graph-review"], None);
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&req)
        .unwrap();
    let before = tree(root);
    let dry = ok(
        root,
        &["--dry-run", "graph", "review", "--file", "-"],
        Some(&req),
    );
    assert!(dry["summary"]["accept_assertions"] == 1 || dry["accept_assertions"] == 1);
    assert_eq!(tree(root), before);
    let accepted = ok(root, &["graph", "review", "--file", "-"], Some(&req));
    apply(root, &accepted);
    assert_eq!(
        note(root, aid).1.canonical.unwrap().string("wiki_status"),
        Some("accepted")
    );
    // Actual Review Accept predecessor is superseded under D37; no manual acceptance fixture.
    let merge = json!({"schema":"lwiki.entity-decisions.v1","decisions":[{"operation":"MergeEntities","source_ids":[first],"target_id":second,"reason":"Explicit later homonym identity decision","expected_records":guards(root),"remaps":[{"kind":"Assertion","assertion_id":aid,"field":"subject_id","old_entity_id":first,"target":{"kind":"ExistingEntity","entity_id":second}},{"kind":"Mention","extraction_id":xid,"mention_id":"m1","old_entity_id":first,"target":{"kind":"ExistingEntity","entity_id":second}}]}]});
    let merged = ok(root, &["graph", "decide", "--file", "-"], Some(&merge));
    apply(root, &merged);
    assert_eq!(
        note(root, aid).1.canonical.unwrap().string("wiki_status"),
        Some("proposed")
    );
    let old_decision = accepted["allocations"]["decisions"][aid].as_str().unwrap();
    assert_eq!(
        note(root, old_decision)
            .1
            .canonical
            .unwrap()
            .string("wiki_status"),
        Some("superseded")
    );
    let rereview = ok(
        root,
        &["graph", "review", "--file", "-"],
        Some(&review(root, aid, &[])),
    );
    apply(root, &rereview);
    let retry = ok(root, &["graph", "import", "--file", "-"], Some(&response));
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["allocations"], imported["allocations"]);
    let (ep, en) = notes(root)
        .into_iter()
        .find(|(_, n)| {
            n.canonical.as_ref().is_some_and(|r| {
                r.kind() == RecordKind::Evidence && r.string("wiki_assertion_id") == Some(aid)
            })
        })
        .unwrap();
    let eid = en.canonical.unwrap().id().to_string();
    let (exit, refreshed) = invoke(
        root,
        &["source", "refresh", sid, "--file", "-"],
        Some(format!("{text}\r\nNew source head.\r\n").as_bytes()),
    );
    assert_eq!(exit, 0, "{refreshed}");
    let rev = refreshed["data"]["allocated_ids"]["revision"]
        .as_str()
        .unwrap();
    let hash = Blake3Hash::digest(fs::read(root.join(ep.as_str())).unwrap()).to_string();
    let revalidated = ok(
        root,
        &[
            "evidence",
            "revalidate",
            &eid,
            "--to-revision",
            rev,
            "--if-match",
            &hash,
        ],
        None,
    );
    let change = revalidated["change"]["change_id"]
        .as_str()
        .or_else(|| revalidated["prepared"]["change_id"].as_str())
        .unwrap();
    ok(root, &["changes", "apply", change], None);
    let current_decision = rereview["allocations"]["decisions"][aid].as_str().unwrap();
    let final_review = ok(
        root,
        &["graph", "review", "--file", "-"],
        Some(&review(root, aid, &[current_decision])),
    );
    apply(root, &final_review);
    let before_rebuild = note(root, aid).1.raw;
    let context = ok(
        root,
        &[
            "context",
            aid,
            "--target",
            "graph",
            "--strategy",
            "relationship",
        ],
        None,
    );
    assert!(serde_json::to_string(&context).unwrap().contains(aid));
    fs::remove_dir_all(root.join(".wiki/cache")).unwrap();
    ok(root, &["index", "rebuild"], None);
    assert_eq!(note(root, aid).1.raw, before_rebuild);
    let after = ok(
        root,
        &[
            "context",
            aid,
            "--target",
            "graph",
            "--strategy",
            "relationship",
        ],
        None,
    );
    assert!(serde_json::to_string(&after).unwrap().contains(aid));
    let final_import = ok(root, &["graph", "import", "--file", "-"], Some(&response));
    assert_eq!(final_import["allocations"], imported["allocations"]);
    let historical = ok(root, &["graph", "review", "--file", "-"], Some(&req));
    assert!(historical["reused"].as_bool().unwrap());
    assert_eq!(historical["allocations"], accepted["allocations"]);
}

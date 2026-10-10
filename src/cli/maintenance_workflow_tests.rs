//! Private dispatcher controls: public argv/content, not shipping-binary qualification.
//! CC0 texts copied verbatim from the independent integration007 fixture.
use super::{Arguments, execute};
use crate::{domain::Blake3Hash, maintenance_parallel};
use clap::Parser;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Copy, Debug)]
enum Control {
    Sequential,
    Concurrent,
}
fn invoke(root: &Path, control: Control, tail: &[&str]) -> Value {
    let mut argv = vec![
        "lwiki",
        "--json",
        "--offline",
        "--wiki",
        root.to_str().unwrap(),
    ];
    argv.extend_from_slice(tail);
    let args = Arguments::try_parse_from(&argv).unwrap();
    let run = || Ok(execute(&args));
    let (envelope, exit) = match control {
        Control::Sequential => maintenance_parallel::sequential_scope(run),
        Control::Concurrent => maintenance_parallel::command_scope(run),
    }
    .unwrap();
    let value = serde_json::to_value(envelope).unwrap();
    println!(
        "PUBLIC_WORKFLOW010 {}",
        json!({"control":format!("{control:?}"), "argv":argv, "exit":exit, "envelope":value})
    );
    assert_eq!(value["meta"]["network_used"], false);
    assert_eq!(exit == 0, value["ok"] == true, "{argv:?}: {value}");
    if let Some(m) = value["meta"]["maintenance"].as_object() {
        assert_eq!(m["submitted"], m["completed"], "all command jobs joined");
        assert!(m["max_active"].as_u64().unwrap() <= 16);
        assert!(m["max_in_flight_bytes"].as_u64().unwrap() <= 1024 * 1024 * 1024);
        if matches!(control, Control::Sequential) {
            assert!(m["max_active"].as_u64().unwrap() <= 1);
        }
    }
    value
}
fn ok(root: &Path, control: Control, tail: &[&str]) -> Value {
    let v = invoke(root, control, tail);
    assert_eq!(v["ok"], true, "{tail:?}: {v}");
    v
}
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let m = fs::symlink_metadata(&path).unwrap();
            out.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (
                    if m.is_file() {
                        fs::read(&path).unwrap()
                    } else {
                        vec![]
                    },
                    m.modified().unwrap(),
                ),
            );
            if m.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}
fn preflight_tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    tree(root)
        .into_iter()
        .filter(|(path, _)| {
            // Writer bookkeeping may change. Retained canonical objects and all
            // other .wiki descendants remain in the exact bytes+mtime oracle.
            path != Path::new(".wiki") && !path.starts_with(".wiki/state")
        })
        .collect()
}
fn read(root: &Path, control: Control, path: &str) -> Value {
    let value = ok(
        root,
        control,
        &["read", "--path", path, "--max-bytes", "16384"],
    );
    assert_eq!(value["data"]["truncated"], false);
    assert_eq!(value["data"]["continuation"], Value::Null);
    value
}
fn discover(root: &Path, control: Control, query: &str, kind: &str) -> Value {
    // Captured bodies carry source owners but no canonical record kind.
    // Search the public source namespace without choosing an expected owner.
    let (filter, value) = if kind == "source" {
        ("--path-prefix", "sources/")
    } else {
        ("--kind", kind)
    };
    ok(
        root,
        control,
        &[
            "search",
            query,
            "--mode",
            "literal",
            filter,
            value,
            "--verify-selected",
            "--no-sync",
            "--excerpt-bytes",
            "2048",
        ],
    )
}
fn source_hit(found: &Value, required: &str) -> Value {
    let hits: Vec<_> = found["data"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["excerpt"]["text"].as_str().unwrap().contains(required))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "discovery must supply exact evidence: {found}"
    );
    hits[0].clone()
}
fn exact_hit_read(root: &Path, control: Control, hit: &Value) {
    let start = hit["excerpt"]["span"]["start"]
        .as_u64()
        .unwrap()
        .to_string();
    let end = hit["excerpt"]["span"]["end"].as_u64().unwrap().to_string();
    let v = ok(
        root,
        control,
        &[
            "read",
            "--path",
            hit["path"].as_str().unwrap(),
            "--start",
            &start,
            "--end",
            &end,
            "--max-bytes",
            "16384",
        ],
    );
    assert_eq!(v["data"]["body"], hit["excerpt"]["text"]);
    assert_eq!(v["data"]["truncated"], false);
    assert_eq!(v["data"]["continuation"], Value::Null);
    assert_eq!(v["data"]["range"], hit["excerpt"]["span"]);
    assert_eq!(hit["path"], hit["locator"]["path"]);
    let citation = &hit["excerpt"]["citation"];
    assert_eq!(citation["kind"], "source");
    assert_eq!(citation["reference"]["span"], hit["excerpt"]["span"]);
    assert_eq!(citation["reference"]["source_id"], hit["source_id"]);
    assert_eq!(
        citation["reference"]["source_revision"],
        hit["owner_revision"]
    );
    assert_eq!(v["data"]["source_citation"]["citation"], *citation);
    assert_eq!(v["data"]["source_citation"]["eligibility"], "current");
    assert_eq!(
        citation["reference"]["quote_hash"],
        Blake3Hash::digest(v["data"]["body"].as_str().unwrap().as_bytes()).to_string()
    );
}
fn proposal(record: &Value, body: &str) -> String {
    let mut text = String::from("---\n");
    for (key, value) in record.as_object().unwrap() {
        text.push_str(&format!(
            "{}: {}\n",
            serde_json::to_string(key).unwrap(),
            value
        ));
    }
    text.push_str("---\n");
    text.push_str(body);
    text
}
fn author_text(body: &str) -> String {
    let begin = body
        .find("<!-- lwiki:source-citations:v1 begin -->")
        .unwrap();
    let end_marker = "<!-- lwiki:source-citations:v1 end -->";
    let end = body.find(end_marker).unwrap() + end_marker.len();
    format!("{}{}", &body[..begin], &body[end..])
}
fn normalize_provenance(
    body: &str,
    sources: &[String],
    old_hit: &Value,
    new_hit: &Value,
) -> String {
    let begin = body
        .find("<!-- lwiki:source-citations:v1 begin -->")
        .unwrap();
    let end_marker = "<!-- lwiki:source-citations:v1 end -->";
    let end = body.find(end_marker).unwrap() + end_marker.len();
    let mut block = body[begin..end].to_owned();
    for (i, source) in sources.iter().enumerate() {
        let label = format!("SOURCE_{i}");
        // Exact authenticated role fields and canonical path components only.
        block = block.replace(
            &format!("\"source_id\": \"{source}\""),
            &format!("\"source_id\": \"{label}\""),
        );
        block = block.replace(&format!("sources/{source}/"), &format!("sources/{label}/"));
    }
    for (label, hit) in [("OLD_REVISION", old_hit), ("NEW_REVISION", new_hit)] {
        let revision = hit["excerpt"]["citation"]["reference"]["source_revision"]
            .as_str()
            .unwrap();
        block = block.replace(
            &format!("\"source_revision\": \"{revision}\""),
            &format!("\"source_revision\": \"{label}\""),
        );
        block = block.replace(
            &format!("/revisions/{revision}/"),
            &format!("/revisions/{label}/"),
        );
    }
    // Entire authored prefix/suffix and every other generated byte are retained.
    format!("{}{}{}", &body[..begin], block, &body[end..])
}

#[test]
fn dispatcher_public_cited_refresh_external_page_and_history_controls_both_layouts() {
    for retained in [false, true] {
        let mut oracles = Vec::new();
        for control in [Control::Sequential, Control::Concurrent] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("public workflow vault with spaces");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: Vault.Workflow\nwiki_kind: vault\ntitle: Public workflow\n---\n").unwrap();
            let first = ok(&root, control, &["index", "rebuild", "--normalized"]);
            assert_eq!(first["data"]["report"]["reused"], false);
            if retained {
                let plan = ok(&root, control, &["storage", "plan"]);
                ok(
                    &root,
                    control,
                    &[
                        "storage",
                        "cleanup",
                        "--expected-plan",
                        plan["data"]["plan_hash"].as_str().unwrap(),
                    ],
                );
            }
            let mut sources = Vec::new();
            for (i, text) in [BOOKING, DEPOSIT, DISTRACTOR].into_iter().enumerate() {
                let file = temp.path().join(format!("input {i}.md"));
                fs::write(&file, text).unwrap();
                let added = ok(
                    &root,
                    control,
                    &[
                        "source",
                        "add",
                        file.to_str().unwrap(),
                        "--title",
                        &format!("Fixture {i}"),
                        "--media-type",
                        "text/markdown",
                    ],
                );
                sources.push(
                    added["data"]["allocated_ids"]["source"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
            }
            ok(&root, control, &["index", "rebuild", "--normalized"]);
            let same = ok(&root, control, &["index", "sync"]);
            assert_eq!(same["data"]["report"]["reused"], true);
            let before = tree(&root);
            for args in [
                vec!["--dry-run", "index", "sync"],
                vec!["--dry-run", "index", "rebuild"],
                vec!["--dry-run", "search", "20 credits", "--mode", "literal"],
            ] {
                ok(&root, control, &args);
                assert_eq!(tree(&root), before);
            }
            let old_hit = source_hit(
                &discover(&root, control, "20 credits", "source"),
                "20 credits",
            );
            exact_hit_read(&root, control, &old_hit);
            assert_eq!(old_hit["source_id"], sources[1]);
            let old_payload = read(&root, control, old_hit["path"].as_str().unwrap());
            assert_eq!(old_payload["data"]["body"], DEPOSIT);
            let refs = temp.path().join("refs.json");
            fs::write(&refs, serde_json::to_vec(&json!({"schema_version":"1", "citations":[old_hit["excerpt"]["citation"].clone()]})).unwrap()).unwrap();
            let input = temp.path().join("page input.md");
            fs::write(
                &input,
                format!("# Willow deposit\n\n20 credits.\n{SENTINEL}\noldtoken\n"),
            )
            .unwrap();
            ok(
                &root,
                control,
                &[
                    "page",
                    "init",
                    "--file",
                    input.to_str().unwrap(),
                    "--title",
                    "Willow cited deposit",
                    "--path",
                    "pages/willow.md",
                    "--source-refs",
                    refs.to_str().unwrap(),
                ],
            );
            let authored = read(&root, control, "pages/willow.md");
            assert!(
                authored["data"]["body"].as_str().unwrap().contains(
                    old_hit["excerpt"]["citation"]["reference"]["quote_hash"]
                        .as_str()
                        .unwrap()
                )
            );
            // Two genuine changed members, guarded against the actual public head reads.
            let changed_booking = BOOKING.replace("two working days", "three working days");
            let mut items = Vec::new();
            for (i, bytes) in [changed_booking.as_str(), UPDATED].into_iter().enumerate() {
                let head = ok(
                    &root,
                    control,
                    &["read", "--id", &sources[i], "--max-bytes", "16384"],
                );
                let file = temp.path().join(format!("changed {i}.md"));
                fs::write(&file, bytes).unwrap();
                items.push(json!({"source_id":sources[i], "file":file.file_name().unwrap().to_str().unwrap(),
                    "if_match":head["data"]["hash"], "expected_revision":head["data"]["record"]["wiki_current_revision"],
                    "input_hash":Blake3Hash::digest(bytes.as_bytes()).to_string(), "media_type":"text/markdown"}));
            }
            let request = temp.path().join("batch.json");
            fs::write(
                &request,
                serde_json::to_vec(&json!({"items":items})).unwrap(),
            )
            .unwrap();
            let refreshed = ok(
                &root,
                control,
                &[
                    "source",
                    "refresh-batch",
                    "--file",
                    request.to_str().unwrap(),
                ],
            );
            assert_eq!(refreshed["data"]["items"].as_array().unwrap().len(), 2);
            assert!(
                refreshed["data"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|i| i["no_op"] == false)
            );
            if retained && cfg!(unix) && matches!(control, Control::Concurrent) {
                assert!(
                    refreshed["meta"]["maintenance"]["submitted"]
                        .as_u64()
                        .unwrap_or(0)
                        > 0,
                    "retained dispatcher must execute workers: {refreshed}"
                );
            }
            assert_eq!(
                read(&root, control, "pages/willow.md")["data"]["body"],
                authored["data"]["body"]
            );
            assert!(
                discover(&root, control, "20 credits", "source")["data"]["hits"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            let new_hit = source_hit(
                &discover(&root, control, "35 credits", "source"),
                "35 credits",
            );
            exact_hit_read(&root, control, &new_hit);
            assert_eq!(new_hit["source_id"], sources[1]);
            assert_ne!(
                old_hit["excerpt"]["citation"]["reference"]["source_revision"],
                new_hit["excerpt"]["citation"]["reference"]["source_revision"]
            );
            assert_eq!(
                read(&root, control, old_hit["path"].as_str().unwrap())["data"]["body"],
                DEPOSIT
            );
            // Identical stable failures in both controls. The first fixture has
            // two independent errors; reversing manifest order changes the first.
            let mut bad_hash = items[0].clone();
            bad_hash["input_hash"] = Blake3Hash::digest(b"wrong input hash").to_string().into();
            let mut directory = items[1].clone();
            directory["file"] = ".".into();
            let mut cases = vec![
                (
                    vec![bad_hash.clone(), directory.clone()],
                    "CONTENT_CONFLICT",
                ),
                (vec![directory, bad_hash], "USAGE"),
                (vec![items[0].clone(), items[0].clone()], "USAGE"),
            ];
            #[cfg(unix)]
            {
                let link = temp.path().join("linked.md");
                std::os::unix::fs::symlink(temp.path().join("changed 0.md"), &link).unwrap();
                let mut linked = items[0].clone();
                linked["file"] = "linked.md".into();
                cases.push((vec![linked], "USAGE"));
            }
            let canonical_before = preflight_tree(&root);
            let mut errors = Vec::new();
            for (members, expected) in cases {
                fs::write(
                    &request,
                    serde_json::to_vec(&json!({"items":members})).unwrap(),
                )
                .unwrap();
                let failed = invoke(
                    &root,
                    control,
                    &[
                        "source",
                        "refresh-batch",
                        "--file",
                        request.to_str().unwrap(),
                    ],
                );
                assert_eq!(failed["ok"], false);
                assert_eq!(failed["error"]["code"], expected, "{failed}");
                assert_eq!(preflight_tree(&root), canonical_before);
                errors.push(
                    json!({"code":failed["error"]["code"], "message":failed["error"]["message"]}),
                );
            }
            assert_eq!(
                read(&root, control, new_hit["path"].as_str().unwrap())["data"]["body"],
                UPDATED
            );
            let context = ok(
                &root,
                control,
                &[
                    "context",
                    "35 credits",
                    "--mode",
                    "literal",
                    "--no-sync",
                    "--max-bytes",
                    "6000",
                    "--max-tokens",
                    "1500",
                ],
            );
            let passage = context["data"]["passages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["text"].as_str().unwrap().contains("35 credits"))
                .expect("actual context discovery must include updated evidence");
            let citations = passage["citations"].as_array().unwrap();
            assert!(!citations.is_empty());
            for citation in citations {
                assert_eq!(citation["kind"], "source");
                assert_eq!(citation["reference"]["source_id"], sources[1]);
                assert_eq!(
                    citation["reference"]["source_revision"],
                    new_hit["excerpt"]["citation"]["reference"]["source_revision"]
                );
                let span = &citation["reference"]["span"];
                let start = span["start"].as_u64().unwrap().to_string();
                let end = span["end"].as_u64().unwrap().to_string();
                let returned = ok(
                    &root,
                    control,
                    &[
                        "read",
                        "--path",
                        passage["locator"]["path"].as_str().unwrap(),
                        "--start",
                        &start,
                        "--end",
                        &end,
                        "--max-bytes",
                        "16384",
                    ],
                );
                assert_eq!(returned["data"]["body"], passage["text"]);
                assert_eq!(returned["data"]["range"], *span);
                assert_eq!(returned["data"]["truncated"], false);
                assert_eq!(returned["data"]["continuation"], Value::Null);
                assert_eq!(returned["data"]["source_citation"]["citation"], *citation);
                assert_eq!(
                    citation["reference"]["quote_hash"],
                    Blake3Hash::digest(returned["data"]["body"].as_str().unwrap().as_bytes())
                        .to_string()
                );
            }
            // External author edit: identical bytes count and restored original mtime.
            let page_path = &authored["data"]["path"];
            let actual = crate::vault::VaultRoot::explicit(&root)
                .unwrap()
                .resolve(
                    &crate::domain::VaultRelativePath::new(page_path.as_str().unwrap()).unwrap(),
                )
                .unwrap();
            let mtime = fs::metadata(&actual).unwrap().modified().unwrap();
            let old = fs::read_to_string(&actual).unwrap();
            let edited = old.replace("oldtoken", "newtoken");
            assert_ne!(old, edited);
            assert_eq!(old.len(), edited.len());
            fs::write(&actual, &edited).unwrap();
            fs::File::options()
                .write(true)
                .open(&actual)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(mtime))
                .unwrap();
            assert_eq!(fs::metadata(&actual).unwrap().modified().unwrap(), mtime);
            fs::write(
                &input,
                proposal(
                    &authored["data"]["record"],
                    authored["data"]["body"].as_str().unwrap(),
                ),
            )
            .unwrap();
            let stale = invoke(
                &root,
                control,
                &[
                    "page",
                    "put",
                    "--file",
                    input.to_str().unwrap(),
                    "--path",
                    page_path.as_str().unwrap(),
                    "--if-match",
                    authored["data"]["hash"].as_str().unwrap(),
                ],
            );
            assert_eq!(stale["ok"], false);
            assert_eq!(stale["error"]["code"], "CONTENT_CONFLICT");
            assert_eq!(fs::read_to_string(&actual).unwrap(), edited);
            ok(&root, control, &["index", "sync"]);
            let found = discover(&root, control, "newtoken", "page");
            let hit = found["data"]["hits"]
                .as_array()
                .unwrap()
                .iter()
                .find(|h| h["path"] == page_path.as_str().unwrap())
                .expect("external edit must be discovered without expected-owner read bypass");
            let reread = read(&root, control, hit["path"].as_str().unwrap());
            assert!(
                reread["data"]["body"]
                    .as_str()
                    .unwrap()
                    .contains(SENTINEL.trim())
            );
            assert!(
                reread["data"]["body"]
                    .as_str()
                    .unwrap()
                    .contains("newtoken")
            );
            fs::write(
                &refs,
                serde_json::to_vec(&json!({"schema_version":"1", "citations":citations})).unwrap(),
            )
            .unwrap();
            let reconciled = reread["data"]["body"]
                .as_str()
                .unwrap()
                .replace("20 credits", "35 credits");
            fs::write(&input, proposal(&reread["data"]["record"], &reconciled)).unwrap();
            ok(
                &root,
                control,
                &[
                    "page",
                    "put",
                    "--file",
                    input.to_str().unwrap(),
                    "--path",
                    hit["path"].as_str().unwrap(),
                    "--if-match",
                    reread["data"]["hash"].as_str().unwrap(),
                    "--source-refs",
                    refs.to_str().unwrap(),
                ],
            );
            let final_page = read(&root, control, hit["path"].as_str().unwrap());
            let final_body = final_page["data"]["body"].as_str().unwrap();
            assert_eq!(
                author_text(final_body),
                author_text(&reconciled),
                "all authored bytes outside the generated provenance block survive reconciliation"
            );
            assert!(final_body.contains("35 credits."));
            assert!(!final_body.contains("20 credits."));
            assert!(final_body.contains(SENTINEL.trim()));
            assert!(final_body.contains("newtoken"));
            assert!(final_body.contains(citations[0]["reference"]["quote_hash"].as_str().unwrap()));
            ok(&root, control, &["index", "rebuild", "--normalized"]);
            let rebuilt = source_hit(
                &discover(&root, control, "35 credits", "source"),
                "35 credits",
            );
            assert_eq!(rebuilt, new_hit);
            assert_eq!(
                read(&root, control, old_hit["path"].as_str().unwrap())["data"]["body"],
                DEPOSIT
            );
            // Prospectively exclude allocated IDs, revision IDs, timestamps and metrics only.
            // Preserve complete returned payload/Page text and exact citation byte identity.
            let normalized_body = normalize_provenance(final_body, &sources, &old_hit, &new_hit);
            oracles.push(json!({"old":old_payload["data"]["body"], "current":read(&root, control, new_hit["path"].as_str().unwrap())["data"]["body"],
                "page":normalized_body, "context":passage["text"],
                "old_span":old_hit["excerpt"]["citation"]["reference"]["span"],
                "new_span":new_hit["excerpt"]["citation"]["reference"]["span"],
                "old_hash":old_hit["excerpt"]["citation"]["reference"]["quote_hash"],
                "new_hash":new_hit["excerpt"]["citation"]["reference"]["quote_hash"], "errors":errors}));
        }
        assert_eq!(
            oracles[0], oracles[1],
            "dispatcher controls agree on actual facts, author text and exact citations"
        );
    }
}
const BOOKING: &str = "# Willow workshop kiln booking\n\nThis is a fictional public software-test fixture, not operating guidance.\n\nTo book the Willow workshop kiln, submit the booking form at least two working days before the session.\n\nEvery user must complete Willow's kiln induction before their first session. A certificate from another workshop does not replace this induction.\n";
const DEPOSIT: &str = "# Willow workshop kiln deposit\n\nThis is a fictional public software-test fixture, not operating guidance.\n\nThe Willow workshop kiln booking requires a refundable deposit of 20 credits.\n\nThe deposit is waived only for members presenting both a current volunteer card and a booking approved by the coordinator. Membership alone does not waive it.\n";
const UPDATED: &str = "# Willow workshop kiln deposit\n\nThis is a fictional public software-test fixture, not operating guidance.\n\nThe Willow workshop kiln booking requires a refundable deposit of 35 credits.\n\nThe deposit is waived only for members presenting both a current volunteer card and a booking approved by the coordinator. Membership alone does not waive it.\n";
const DISTRACTOR: &str = "# Cedar workshop kiln booking and deposit\n\nThis is a fictional public software-test fixture, not operating guidance.\n\nCedar workshop kiln booking is available on the same day and requires no induction. Its refundable deposit is 5 credits and every member receives a waiver.\n\nThese rules apply only at Cedar workshop. They do not apply to Willow workshop kiln booking.\n";
const SENTINEL: &str = "\n## Author note\n\nKeep the café volunteer rota separate from the kiln booking requirements.\n\n<!-- integration007-author-sentinel: preserve-東京 -->\n";

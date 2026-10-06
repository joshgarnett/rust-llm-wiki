//! Runtime tests execute the maintained recipe; no host install or live provider.
#[path = "../test_support/paths.rs"]
mod test_paths;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};

fn temporary() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

fn invoke(
    cwd: &Path,
    wiki: Option<&Path>,
    args: &[String],
    input: Option<&Value>,
) -> (i32, Value, String) {
    let mut cmd = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
    cmd.current_dir(cwd).args(["--offline", "--json"]);
    if let Some(wiki) = wiki {
        cmd.arg("--wiki").arg(wiki);
    }
    let mut child = cmd
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        let bytes = match input {
            Value::String(s) => s.as_bytes().to_vec(),
            v => serde_json::to_vec(v).unwrap(),
        };
        child.stdin.as_mut().unwrap().write_all(&bytes).unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let data = if out.stdout.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&out.stdout).unwrap()
    };
    (
        out.status.code().unwrap(),
        data,
        String::from_utf8(out.stderr).unwrap(),
    )
}
fn run(cwd: &Path, wiki: Option<&Path>, args: &[&str]) -> Value {
    let (exit, v, stderr) = invoke(
        cwd,
        wiki,
        &args.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
        None,
    );
    assert_eq!(exit, 0, "{args:?}: {v} {stderr}");
    assert!(stderr.is_empty());
    assert_eq!(v["meta"]["network_used"], false);
    v
}
fn export(cwd: &Path, target: &str, output: &Path, dry: bool) -> (i32, Value, String) {
    let mut args = vec![
        "skill".into(),
        "export".into(),
        "--target".into(),
        target.into(),
        "--output".into(),
        output.to_str().unwrap().into(),
    ];
    if dry {
        args.push("--dry-run".into());
    }
    invoke(cwd, None, &args, None)
}
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn walk(root: &Path, dir: &Path, map: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for e in fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            let m = fs::symlink_metadata(&path).unwrap();
            map.insert(
                path.strip_prefix(root).unwrap().into(),
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
                walk(root, &path, map);
            }
        }
    }
    let mut map = BTreeMap::new();
    walk(root, root, &mut map);
    map
}
fn assert_conflict(cwd: &Path, output: &Path, target: &str) {
    let (exit, v, _) = export(cwd, target, output, false);
    assert_eq!(exit, 4, "{v}");
    assert_eq!(v["error"]["code"], "CONTENT_CONFLICT");
}
#[test]
fn skill_exports_correct_single_discovery_layout() {
    let temp = temporary();
    for (target, layout) in [
        ("codex", ".agents/skills/llm-wiki"),
        ("cursor", ".agents/skills/llm-wiki"),
        ("claude-code", ".claude/skills/llm-wiki"),
    ] {
        let out = temp.path().join(target);
        let before = tree(temp.path());
        let (exit, dry, _) = export(temp.path(), target, &out, true);
        assert_eq!(exit, 0, "{dry}");
        assert_eq!(dry["data"]["complete"], false);
        assert_eq!(tree(temp.path()), before);
        let (exit, v, _) = export(temp.path(), target, &out, false);
        assert_eq!(exit, 0, "{v}");
        assert_eq!(v["data"]["complete"], true);
        let root = out.join(layout);
        let manifest: Value =
            serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["target"], target);
        let files = manifest["files"].as_object().unwrap();
        assert_eq!(files.len(), 6);
        for (path, hash) in files {
            assert_eq!(
                blake3::hash(&fs::read(root.join(path)).unwrap())
                    .to_hex()
                    .as_str(),
                hash.as_str().unwrap()
            );
        }
        let before = tree(temp.path());
        let (exit, reuse, _) = export(temp.path(), target, &out, false);
        assert_eq!(exit, 0, "{reuse}");
        assert_eq!(reuse["data"]["reused"], true);
        assert_eq!(tree(temp.path()), before);
        let (exit, _, _) = export(temp.path(), target, &out, true);
        assert_eq!(exit, 0);
        assert_eq!(tree(temp.path()), before);
        for other in [
            ".agents/skills/llm-wiki",
            ".claude/skills/llm-wiki",
            ".cursor/skills/llm-wiki",
            ".codex/skills/llm-wiki",
        ] {
            if other != layout {
                assert!(!out.join(other).exists());
            }
        }
        fs::write(root.join("extra"), b"user content").unwrap();
        assert_conflict(temp.path(), &out, target);
        fs::remove_file(root.join("extra")).unwrap();
        fs::write(root.join("SKILL.md"), b"author edits").unwrap();
        assert_conflict(temp.path(), &out, target);
    }
    let partial = temp.path().join("partial");
    fs::create_dir_all(partial.join(".agents/skills/llm-wiki/references")).unwrap();
    assert_conflict(temp.path(), &partial, "codex");
    let duplicate = temp.path().join("duplicate");
    fs::create_dir_all(duplicate.join(".cursor/skills/llm-wiki")).unwrap();
    assert_conflict(temp.path(), &duplicate, "cursor");
    let (exit, v, _) = export(
        temp.path(),
        "unsupported",
        &temp.path().join("invalid"),
        false,
    );
    assert_eq!(exit, 2, "{v}");
    assert!(!temp.path().join("invalid").exists());
}
#[test]
fn skill_refuses_host_instruction_overwrite() {
    let temp = temporary();
    let root = temp.path();
    fs::create_dir(root.join(".cursor")).unwrap();
    for p in ["AGENTS.md", "CLAUDE.md", ".cursor/rules", "settings.json"] {
        fs::write(root.join(p), format!("Preserve {p}")).unwrap();
    }
    let before = tree(root);
    let (exit, v, _) = export(root, "codex", root, false);
    assert_eq!(exit, 0, "{v}");
    for (p, data) in before {
        assert_eq!(tree(root).get(&p), Some(&data));
    }
    let exported = root.join(".agents/skills/llm-wiki");
    fs::remove_file(exported.join("manifest.json")).unwrap();
    let partial = tree(root);
    assert_conflict(root, root, "codex");
    assert_eq!(tree(root), partial);
    let args = vec![
        "--stage".into(),
        "skill".into(),
        "export".into(),
        "--target".into(),
        "cursor".into(),
        "--output".into(),
        root.join("stage").to_str().unwrap().into(),
    ];
    let (exit, v, _) = invoke(root, None, &args, None);
    assert_eq!(exit, 2, "{v}");
    assert!(!root.join("stage").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let output = root.join("unsafe");
        fs::create_dir(&output).unwrap();
        symlink(root, output.join(".agents")).unwrap();
        assert_conflict(root, &output, "codex");
        let linkroot = root.join("linked-output");
        symlink(root, &linkroot).unwrap();
        assert_conflict(root, &linkroot, "codex");

        let aliases = root.join("aliases");
        let (exit, v, _) = export(root, "codex", &aliases, false);
        assert_eq!(exit, 0, "{v}");
        let package = aliases.join(".agents/skills/llm-wiki");
        let actual = package.join("agents/openai.yaml");
        let alias = package.join("agents\\openai.yaml");
        fs::rename(&actual, &alias).unwrap();
        let before = tree(&aliases);
        assert_conflict(root, &aliases, "codex");
        assert_eq!(tree(&aliases), before);
        fs::copy(&alias, &actual).unwrap();
        assert_conflict(root, &aliases, "codex");
    }
}
#[test]
fn skill_capabilities_do_not_advertise_missing_commands() {
    let temp = temporary();
    let root = temp.path();
    let capabilities = run(root, None, &["capabilities"]);
    let (exit, v, _) = export(root, "codex", root, false);
    assert_eq!(exit, 0, "{v}");
    let package = root.join(".agents/skills/llm-wiki");
    let manifest: Value =
        serde_json::from_slice(&fs::read(package.join("manifest.json")).unwrap()).unwrap();
    let maintained: Value =
        serde_json::from_str(include_str!("fixtures/p14/package-manifest.json")).unwrap();
    assert_eq!(
        manifest, maintained,
        "refresh maintained release reference and fixture from the implemented exporter"
    );
    for key in ["commands", "schemas", "version"] {
        assert_eq!(manifest[key], capabilities["data"][key]);
    }
    let reference = fs::read_to_string(package.join("references/commands.md")).unwrap();
    assert_eq!(
        reference,
        include_str!("../skills/llm-wiki/references/commands.md")
    );
    for name in capabilities["data"]["commands"].as_array().unwrap() {
        let name = name.as_str().unwrap();
        assert!(reference.contains(&format!("## {name}\n")));
        let mut args: Vec<_> = name.split_whitespace().map(str::to_string).collect();
        args.push("--help".into());
        let out = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{name}");
    }
    for schema in capabilities["data"]["schemas"].as_array().unwrap() {
        let v = run(root, None, &["schema", schema.as_str().unwrap()]);
        assert!(v["data"].is_object());
    }
    assert!(
        !capabilities["data"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str() == Some("lwiki mcp"))
    );
    for required in ["research run", "embeddings sync"] {
        assert!(
            capabilities["data"]["commands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|command| command.as_str() == Some(required))
        );
    }
    let version = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .arg("--version")
        .output()
        .unwrap();
    assert!(
        String::from_utf8(version.stdout)
            .unwrap()
            .contains(manifest["version"].as_str().unwrap())
    );
}
fn substitute(v: &Value, bindings: &BTreeMap<String, String>) -> Value {
    match v {
        Value::String(s) => {
            let mut s = s.clone();
            for (k, val) in bindings {
                s = s.replace(&format!("${{{k}}}"), val);
            }
            assert!(!s.contains("${"), "unbound example: {s}");
            Value::String(s)
        }
        Value::Array(a) => Value::Array(a.iter().map(|v| substitute(v, bindings)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), substitute(v, bindings)))
                .collect(),
        ),
        _ => v.clone(),
    }
}
#[test]
fn skill_examples_execute_against_release_binary() {
    execute_maintained_examples("steps");
}
#[test]
fn skill_cited_page_examples_preserve_verified_refs_and_author_edits() {
    execute_maintained_examples("cited_page_steps");
}
fn execute_maintained_examples(recipe: &str) {
    let temp = temporary();
    let wiki = temp.path().join("wiki with spaces");
    fs::create_dir(&wiki).unwrap();
    let root = wiki.as_path();
    fs::write(root.join("WIKI.md"),"---\nwiki_schema: \"1\"\nwiki_id: Vault.Portable\nwiki_kind: vault\ntitle: Portable fixture\n---\n").unwrap();
    let export_temp = temporary();
    let (exit, exported, _) = export(root, "codex", export_temp.path(), false);
    assert_eq!(exit, 0, "{exported}");
    let examples: Value = serde_json::from_slice(
        &fs::read(
            export_temp
                .path()
                .join(".agents/skills/llm-wiki/references/examples.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut bindings = BTreeMap::new();
    let mut imported = Value::Null;
    let mut author_snapshot = None;
    let mut draft_record = Value::Null;
    for step in examples[recipe].as_array().unwrap() {
        // Simulate an author's external edit only in this disposable fixture.
        if let Some(edit) = step.get("fixture_edit") {
            let path = root.join(edit["path"].as_str().unwrap());
            assert!(matches!(
                edit["path"].as_str().unwrap(),
                "pages/portable.md" | "pages/cited-brief.md"
            ));
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(edit["append"].as_str().unwrap().as_bytes())
                .unwrap();
            author_snapshot = Some(fs::read(path).unwrap());
        }
        let args = substitute(&step["args"], &bindings)
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        let input = step.get("stdin").map(|s| substitute(s, &bindings));
        let (exit, v, stderr) = invoke(root, Some(root), &args, input.as_ref());
        let name = step["name"].as_str().unwrap();
        assert!(stderr.is_empty(), "{name}: {stderr}");
        if let Some(error) = step["expect_error"].as_str() {
            assert_ne!(exit, 0, "{name}: {v}");
            assert_eq!(v["error"]["code"], error, "{name}: {v}");
        } else {
            assert_eq!(exit, 0, "{name}: {v}");
            assert_eq!(v["ok"], true);
        }
        assert_eq!(v["meta"]["network_used"], false);
        if let Some(m) = step["bindings"].as_object() {
            for (k, p) in m {
                let s = v
                    .pointer(p.as_str().unwrap())
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("{name} missing binding {k}: {v}"));
                bindings.insert(k.clone(), s.into());
            }
        }
        if let Some(m) = step["serialized_bindings"].as_object() {
            for (k, pointer) in m {
                let value = v.pointer(pointer.as_str().unwrap()).unwrap();
                bindings.insert(k.clone(), serde_json::to_string(value).unwrap());
            }
        }
        if let Some(m) = step["selector_bindings"].as_object() {
            let task: Value = serde_json::from_str(
                v["data"]["selection_packet"]["selector_input"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            for (k, pointer) in m {
                bindings.insert(
                    k.clone(),
                    task.pointer(pointer.as_str().unwrap())
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .into(),
                );
            }
        }
        match step["check"].as_str().unwrap_or("") {
            "cited_selector" => {
                let task: Value = serde_json::from_str(
                    v["data"]["selection_packet"]["selector_input"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(task["packet_fingerprint"], bindings["cited_fingerprint"]);
                assert!(!task["payload"]["cards"].as_array().unwrap().is_empty());
            }
            "cited_packed" => {
                assert_eq!(v["meta"]["freshness"], "indexed_evidence");
                assert!(v["data"]["usage"]["rendered_bytes"].as_u64().unwrap() <= 6000);
                assert!(v["data"]["omissions"].as_array().unwrap().is_empty());
                let passages = v["data"]["passages"].as_array().unwrap();
                assert!(!passages.is_empty());
                let handle =
                    lwiki::vault::VaultFs::new(lwiki::vault::VaultRoot::explicit(root).unwrap());
                let view = lwiki::sources::SourceView::from_fs(&handle).unwrap();
                for passage in passages {
                    assert_eq!(passage["eligibility"], "current");
                    for citation in passage["citations"].as_array().unwrap() {
                        let reference: lwiki::domain::CitationRef =
                            serde_json::from_value(citation.clone()).unwrap();
                        let verified = view
                            .verify(&reference, lwiki::sources::CitationScope::Current)
                            .unwrap();
                        assert_eq!(verified.quote, passage["text"].as_str().unwrap().as_bytes());
                        assert_eq!(citation["reference"]["source_id"], bindings["cited_source"]);
                        assert_eq!(
                            citation["reference"]["source_revision"],
                            bindings["cited_revision"]
                        );
                    }
                }
                assert!(!passages[0]["citations"].as_array().unwrap().is_empty());
                let packed_text = v["data"]["text"].as_str().unwrap();
                assert!(packed_text.contains("a charged battery"));
                assert!(packed_text.contains("closing the cover before pressing START"));
                assert!(
                    v["data"]["text"]
                        .as_str()
                        .unwrap()
                        .contains(passages[0]["text"].as_str().unwrap())
                );
            }
            "cited_draft_read" | "cited_reconcile" => {
                assert_eq!(v["meta"]["partial"], false);
                assert_eq!(v["data"]["truncated"], false);
                assert_eq!(v["data"]["record"]["wiki_status"], "draft");
                assert_eq!(v["data"]["record"]["wiki_id"], bindings["cited_page_id"]);
                assert_eq!(v["data"]["path"], "pages/cited-brief.md");
                assert_eq!(
                    v["data"]["hash"],
                    format!(
                        "blake3:{}",
                        blake3::hash(&fs::read(root.join("pages/cited-brief.md")).unwrap())
                            .to_hex()
                    )
                );
                let record = v["data"]["record"].as_object().unwrap();
                let body = v["data"]["body"].as_str().unwrap();
                if step["check"] == "cited_reconcile" {
                    assert_eq!(v["data"]["record"], draft_record);
                    assert!(body.contains("Author observation: keep the spare battery nearby."));
                    assert_ne!(
                        bindings["cited_reconciled_hash"],
                        bindings["cited_page_hash"]
                    );
                } else {
                    draft_record = v["data"]["record"].clone();
                }
                // Serialize every observed metadata field; read.body is not a full file.
                let mut proposal = String::from("---\n");
                for (field, value) in record {
                    proposal.push_str(&format!(
                        "{field}: {}\n",
                        serde_json::to_string(value).unwrap()
                    ));
                }
                proposal.push_str("---\n");
                proposal.push_str(body);
                proposal.push_str(
                    "\nClarification: this brief describes only the verified revision snapshot.\n",
                );
                bindings.insert("cited_full_proposal".into(), proposal);
            }
            "cited_search" => {
                assert!(
                    v["data"]["hits"].as_array().unwrap().iter().any(|hit| {
                        hit["locator"]["record"]["record_id"] == bindings["cited_page_id"]
                            && hit["path"] == bindings["cited_page_path"]
                            && hit["authored_status"] == "draft"
                    }),
                    "draft title not discoverable: {v}"
                );
            }
            "cited_author_preserved" => {
                assert_eq!(exit, 4);
                assert_eq!(
                    fs::read(root.join("pages/cited-brief.md")).unwrap(),
                    *author_snapshot.as_ref().unwrap()
                );
            }
            "cited_author_preserved_after_sync" => {
                assert_eq!(
                    fs::read(root.join("pages/cited-brief.md")).unwrap(),
                    *author_snapshot.as_ref().unwrap()
                );
            }
            "cited_final" => {
                assert_eq!(v["data"]["record"], draft_record);
                let body = v["data"]["body"].as_str().unwrap();
                assert!(body.contains("Author observation: keep the spare battery nearby."));
                assert!(body.contains(
                    "Clarification: this brief describes only the verified revision snapshot."
                ));
                assert!(
                    body.contains("The returned evidence does not document a warranty duration.")
                );
                let encoded = body
                    .split("```json\n")
                    .nth(1)
                    .unwrap()
                    .split("\n```")
                    .next()
                    .unwrap();
                let references: Value = serde_json::from_str(encoded).unwrap();
                assert_eq!(
                    references,
                    serde_json::from_str::<Value>(&bindings["cited_refs"]).unwrap()
                );
                assert!(body.contains(&format!("[Source](../{})", bindings["cited_source_note"])));
                assert!(body.contains(&format!(
                    "[immutable revision](../{})",
                    bindings["cited_revision_note"]
                )));
                assert!(
                    !v["data"]["record"]
                        .as_object()
                        .unwrap()
                        .contains_key("wiki_depends_on_ids")
                );
            }
            "capabilities" => {
                let modes = v["data"]["search_modes"].as_array().unwrap();
                for mode in ["literal", "lexical", "semantic", "hybrid"] {
                    assert!(modes.contains(&json!(mode)), "{name}: missing {mode}");
                }
            }
            "research_collect" | "research_answer" => {
                assert_eq!(v["data"]["persisted"], true);
                assert_eq!(v["data"]["ready_to_import"], true);
                assert_eq!(v["data"]["external_tool_usage"], "unobserved");
                assert_eq!(v["data"]["packet"]["scope"]["offline"], true);
                let expected = if step["check"] == "research_collect" {
                    "collect_sources"
                } else {
                    "answer"
                };
                assert_eq!(v["data"]["packet"]["stage"], expected);
            }
            "research_complete" => {
                assert_eq!(v["data"]["status"], "completed");
                assert_eq!(v["data"]["ready_to_import"], false);
                assert_eq!(v["data"]["freshness"], "retained");
                assert_eq!(v["data"]["report"]["claims"][0]["assessment"], "unassessed");
            }
            "research_report" => {
                assert_eq!(
                    v["data"]["claims"][0]["citations"][0]["reference"]["source_revision"],
                    bindings["revision"]
                );
                assert_eq!(v["data"]["partial"], false);
            }
            "repeat_revision" => {
                assert_eq!(v["data"]["reused"], true);
                assert_eq!(
                    v["data"]["allocated_ids"]["revision"].as_str().unwrap(),
                    bindings["revision"]
                );
            }
            "literal_citation" => {
                let hits = v["data"]["hits"].as_array().unwrap();
                assert!(!hits.is_empty());
                assert!(hits.iter().any(|h| !h["excerpt"]["citation"].is_null()));
            }
            "lexical_hits" => assert!(!v["data"]["hits"].as_array().unwrap().is_empty()),
            "homonyms" => assert_ne!(bindings["first_ada"], bindings["second_ada"]),
            "evidence_quote" => {
                assert!(
                    v["data"]["body"]
                        .as_str()
                        .unwrap()
                        .contains("Ada works for Acme.")
                );
                assert!(v.to_string().contains(&bindings["revision"]));
            }
            "counterevidence_quote" => {
                assert!(
                    v["data"]["body"]
                        .as_str()
                        .unwrap()
                        .contains("The first Ada does not work for Acme.")
                );
                assert!(v.to_string().contains(&bindings["revision"]));
            }
            "relationship" => {
                assert_eq!(v["data"]["assertions"].as_array().unwrap().len(), 1);
                assert_eq!(v["data"]["assertions"][0]["predicate"], "works_for");
                assert!(!v["data"]["assertions"][0]["support"][0]["citation"].is_null());
                assert_eq!(v["data"]["assertions"][0]["disputed"], true);
                assert_eq!(
                    v["data"]["assertions"][0]["contradictions"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1
                );
            }
            "context_citation" => {
                assert!(!v["data"]["bundles"].as_array().unwrap().is_empty());
                assert!(v["data"]["usage"]["rendered_bytes"].as_u64().unwrap() <= 12000);
                assert!(v.to_string().contains(&bindings["evidence"]));
            }
            "budget_partial" => {
                assert_eq!(v["meta"]["partial"], true);
                assert_eq!(v["meta"]["freshness"], "verified_snapshot");
                assert_eq!(v["data"]["text"], "");
                assert!(v["data"]["passages"].as_array().unwrap().is_empty());
                assert!(v["data"]["bundles"].as_array().unwrap().is_empty());
                assert!(
                    v["data"]["omissions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|o| { o["reason"] == "required_bundle_or_passage_does_not_fit" })
                );
            }
            "semantic_unavailable" => {
                assert_eq!(exit, 6);
                assert_eq!(v["command"], "search");
                assert_eq!(v["error"]["code"], "OFFLINE_UNAVAILABLE");
            }
            "repeat_import" => {
                assert_eq!(v["data"]["reused"], true);
                assert_eq!(v["data"]["allocations"], imported["allocations"]);
            }
            "heading_identity" => {
                assert!(
                    v["data"]["body"]
                        .as_str()
                        .unwrap()
                        .contains("Second heading")
                );
                assert!(v.to_string().contains("Page.Portable"));
            }
            "author_preserved" => assert!(
                fs::read_to_string(root.join("pages/portable.md"))
                    .unwrap()
                    .contains("Human-added observation.")
            ),
            "withdrawn_support" => assert!(v["data"]["assertions"].as_array().unwrap().is_empty()),
            "" => {}
            c => panic!("unimplemented maintained example assertion {c}"),
        }
        if name == "import" {
            imported = v["data"].clone();
        }
    }
}

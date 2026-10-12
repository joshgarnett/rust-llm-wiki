//! Public ordinary lookup retains result limits, exact evidence and useful cards.
#[path = "../test_support/paths.rs"]
mod test_paths;

use lwiki::{
    domain::{Blake3Hash, ByteSpan, CitationRef, SourceSpanRef},
    retrieval::{QueryPlan, SearchLimits, SearchMode},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultFs, VaultRoot},
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path, process::Command};

fn invoke(root: &Path, json_output: bool, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
    command
        .args(["--offline", "--no-user-config", "--wiki"])
        .arg(root);
    if json_output {
        command.arg("--json");
    }
    command.args(args).output().unwrap()
}

fn ok(root: &Path, args: &[&str]) -> Value {
    let output = invoke(root, true, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_kind: vault\nwiki_id: Vault.SearchPresentation\ntitle: My notes\n---\n").unwrap();
    fs::create_dir(temp.path().join("pages")).unwrap();
    temp
}

fn capture(root: &Path, title: &str, text: &str) -> CitationRef {
    let store = SourceStore::new(VaultFs::new(VaultRoot::explicit(root).unwrap()));
    let plan = store
        .plan_capture(CaptureRequest {
            title: title.into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: format!("/fixture/{}.txt", Blake3Hash::digest(text)),
            original: text.as_bytes().to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: Some("text/plain".into()),
        })
        .unwrap();
    for operation in plan.draft.unwrap().operations {
        let target = root.join(operation.target.as_str());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, operation.proposed.unwrap()).unwrap();
    }
    CitationRef::Source(SourceSpanRef {
        source_id: plan.source_id,
        source_revision: plan.revision_id,
        span: ByteSpan::new(0, text.len() as u64).unwrap(),
        quote_hash: Blake3Hash::digest(text),
    })
}

fn rebuild(root: &Path, normalized: bool) {
    if normalized {
        ok(root, &["index", "rebuild", "--normalized"]);
    } else {
        ok(root, &["index", "rebuild"]);
    }
}

#[test]
fn empty_metadata_is_excluded_before_limits_and_explicit_inspection_is_preserved() {
    for normalized in [false, true] {
        let temp = fixture();
        let root = temp.path();
        let originals = [
            "Harbour decision: the trial starts on 18 November in the east annex.\n",
            "Harbour access: Jo may collect the key only after Mara records the inspection.\n",
            "Harbour packing: pack two sealed blue survey cases and six sheets.\n",
        ];
        let refs = originals
            .iter()
            .map(|text| capture(root, "Harbour", text))
            .collect::<Vec<_>>();
        fs::write(root.join("pages/harbour.md"), "---\nwiki_schema: '1'\nwiki_id: Page.Harbour\nwiki_kind: page\ntitle: Harbour\nwiki_status: draft\n---\nHarbour summary: postpone if the battery inspection fails before 08:45.\n").unwrap();
        rebuild(root, normalized);
        for mode in [None, Some("lexical"), Some("literal")] {
            let mut args = vec!["search", "Harbour", "--limit", "2", "--candidates", "4"];
            if let Some(mode) = mode {
                args.extend(["--mode", mode]);
            }
            let first = ok(root, &args);
            assert_eq!(
                first["data"]["hits"].as_array().unwrap().len(),
                2,
                "{first}"
            );
            let cursor = first["data"]["next_cursor"].as_str().unwrap().to_owned();
            args.extend(["--cursor", &cursor]);
            let second = ok(root, &args);
            assert_eq!(
                second["data"]["hits"].as_array().unwrap().len(),
                2,
                "{second}"
            );
            assert!(second["data"]["next_cursor"].is_null());
            let mut paths = BTreeSet::new();
            for result in [&first, &second] {
                for hit in result["data"]["hits"].as_array().unwrap() {
                    assert!(paths.insert(hit["path"].as_str().unwrap().to_owned()));
                    assert!(
                        hit["kind"] != "source" && hit["kind"] != "revision",
                        "{hit}"
                    );
                    assert!(!hit["excerpt"]["text"].as_str().unwrap().is_empty());
                    if hit["source_id"].is_string() {
                        let index = refs.iter().position(|reference| matches!(reference,CitationRef::Source(reference) if reference.source_id.as_str()==hit["source_id"].as_str().unwrap())).unwrap();
                        let citation: CitationRef =
                            serde_json::from_value(hit["excerpt"]["citation"].clone()).unwrap();
                        let CitationRef::Source(reference) = citation else {
                            panic!("exact source citation required")
                        };
                        assert_eq!(
                            reference.span.slice(originals[index]).unwrap(),
                            hit["excerpt"]["text"].as_str().unwrap()
                        );
                        assert_eq!(
                            reference.quote_hash,
                            Blake3Hash::digest(hit["excerpt"]["text"].as_str().unwrap())
                        );
                    }
                }
            }
            assert_eq!(paths.len(), 4);
        }
        for kind in ["source", "revision"] {
            let found = ok(
                root,
                &["search", "Harbour", "--mode", "lexical", "--kind", kind],
            );
            assert_eq!(
                found["data"]["hits"].as_array().unwrap().len(),
                3,
                "{found}"
            );
        }
        let CitationRef::Source(reference) = &refs[0] else {
            unreachable!()
        };
        for id in [&reference.source_id, &reference.source_revision] {
            let found = ok(root, &["search", id.as_str(), "--mode", "lexical"]);
            assert_eq!(
                found["data"]["hits"][0]["record_ref"]["record_id"],
                id.as_str()
            );
        }
        let human = invoke(root, false, &["search", "Harbour", "--mode", "lexical"]);
        assert!(human.status.success());
        let stdout = String::from_utf8(human.stdout).unwrap();
        assert!(stdout.contains("Harbour\nCaptured source (current)"));
        assert!(stdout.contains("Harbour\nPage: pages/harbour.md (draft; current)"));
        assert!(stdout.contains("source read ") && !stdout.contains("/revisions/"));
        assert!(!stdout.contains("Quote hash") && !stdout.contains("Source: "));
    }
}

#[test]
fn grouping_preserves_conditions_and_exact_refs_on_current_and_cached_views() {
    for normalized in [false, true] {
        let temp = fixture();
        let root = temp.path();
        let text = "Harbour trial: start on 18 November in the east annex. Postpone if the battery inspection fails before 08:45.\n";
        let reference = capture(root, "Harbour decision", text);
        rebuild(root, normalized);
        let inputs = tempfile::tempdir().unwrap();
        let body = inputs.path().join("body.md");
        let refs = inputs.path().join("refs.json");
        fs::write(&body, text).unwrap();
        fs::write(
            &refs,
            serde_json::to_vec(&json!({"schema_version":"1","citations":[reference]})).unwrap(),
        )
        .unwrap();
        ok(
            root,
            &[
                "page",
                "init",
                "--file",
                body.to_str().unwrap(),
                "--title",
                "Harbour visit",
                "--path",
                "pages/visit.md",
                "--source-refs",
                refs.to_str().unwrap(),
            ],
        );
        for cached in [false, true] {
            let mut args = vec![
                "search",
                "Harbour",
                "--mode",
                "lexical",
                "--excerpt-bytes",
                "240",
            ];
            if cached {
                args.push("--no-sync");
            }
            let result = ok(root, &args);
            let page = result["data"]["hits"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hit| hit["kind"] == "page")
                .unwrap();
            assert_eq!(page["declared_source_refs"], json!([reference]));
            let source = result["data"]["hits"]
                .as_array()
                .unwrap()
                .iter()
                .find(|hit| hit["owner_revision"].is_string())
                .unwrap();
            if cached {
                assert!(source["excerpt"]["citation"].is_null());
            } else {
                assert_eq!(
                    source["excerpt"]["citation"],
                    serde_json::to_value(&reference).unwrap()
                );
            }
            let human = invoke(root, false, &args);
            assert!(human.status.success());
            let stdout = String::from_utf8(human.stdout).unwrap();
            assert!(
                stdout.contains(
                    "The same displayed text appears in captured source: Harbour decision"
                ),
                "{stdout}"
            );
            assert!(stdout.contains("Postpone if the battery inspection fails before 08:45"));
            assert!(stdout.contains("source read "));
            if cached {
                assert!(stdout.contains("note freshness has not been checked"));
            }
        }
        fs::write(
            &body,
            "Harbour trial: start on 18 November in the east annex.\n",
        )
        .unwrap();
        ok(
            root,
            &[
                "page",
                "init",
                "--file",
                body.to_str().unwrap(),
                "--title",
                "Harbour short summary",
                "--path",
                "pages/short.md",
                "--source-refs",
                refs.to_str().unwrap(),
            ],
        );
        fs::remove_file(root.join("pages/visit.md")).unwrap();
        let human = invoke(root, false, &["search", "Harbour", "--mode", "lexical"]);
        assert!(human.status.success());
        let stdout = String::from_utf8(human.stdout).unwrap();
        assert!(
            !stdout.contains("The same displayed text appears"),
            "{stdout}"
        );
        assert!(stdout.contains("Harbour decision\nCaptured source"));
        assert!(stdout.contains("Postpone if the battery inspection fails before 08:45"));
    }
}

#[test]
fn older_candidate_policy_cursor_requires_an_actionable_restart() {
    let temp = fixture();
    let root = temp.path();
    for index in 0..3 {
        fs::write(root.join(format!("pages/{index}.md")),format!("---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: Page.{index}\ntitle: Harbour {index}\nwiki_status: draft\n---\nHarbour fact {index}.\n")).unwrap();
    }
    rebuild(root, false);
    let first = ok(
        root,
        &[
            "search",
            "Harbour",
            "--mode",
            "literal",
            "--limit",
            "1",
            "--candidates",
            "3",
        ],
    );
    let cursor = first["data"]["next_cursor"].as_str().unwrap();
    let bytes = cursor
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect::<Vec<_>>();
    let mut old: Value = serde_json::from_slice(&bytes).unwrap();
    // A cursor that binds the previous policy cannot page the new candidate set.
    let plan = QueryPlan {
        mode: SearchMode::Literal,
        limits: SearchLimits {
            hits: 1,
            candidates: 3,
            excerpt_bytes: 240,
        },
        ..Default::default()
    };
    old["query"] = json!(Blake3Hash::digest(
        serde_json::to_vec(&(
            "lwiki-query-v1",
            "Harbour",
            plan.mode,
            &plan.filters,
            &plan.limits
        ))
        .unwrap()
    ));
    let old = serde_json::to_vec(&old)
        .unwrap()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let output = invoke(
        root,
        true,
        &[
            "search",
            "Harbour",
            "--mode",
            "literal",
            "--limit",
            "1",
            "--candidates",
            "3",
            "--cursor",
            &old,
        ],
    );
    assert_eq!(output.status.code(), Some(4));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "CURSOR_STALE");
    assert!(
        result["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("without --cursor")
    );
}

use super::*;
use clap::Parser;
use std::collections::BTreeMap;

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_search_adapter\nwiki_kind: vault\ntitle: Search adapter\n---\n").unwrap();
    temp
}

fn files(root: &Path) -> BTreeMap<std::path::PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut result = BTreeMap::new();
    let mut dirs = vec![root.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                result.insert(
                    path.clone(),
                    (
                        std::fs::read(&path).unwrap(),
                        path.metadata().unwrap().modified().unwrap(),
                    ),
                );
            }
        }
    }
    result
}

#[test]
fn verification_flag_is_search_only_and_compatible_with_no_sync() {
    let args = Arguments::try_parse_from([
        "lwiki",
        "search",
        "needle",
        "--verify-selected",
        "--no-sync",
    ])
    .unwrap();
    let Command::Search(command) = args.command else {
        panic!("search required")
    };
    assert!(command.verify_selected && command.search.no_sync);
    assert!(
        Arguments::try_parse_from(["lwiki", "context", "needle", "--verify-selected"]).is_err()
    );
}

#[test]
fn verified_preview_never_resolves_corrupt_cache_and_preserves_files() {
    let fixture = fixture();
    let cache = fixture.path().join(".wiki/cache");
    std::fs::create_dir_all(&cache).unwrap();
    for name in [
        "catalog-current.json",
        "catalog-v2-active.json",
        "catalog.sqlite",
        "catalog.sqlite-wal",
        "catalog.sqlite-shm",
    ] {
        std::fs::write(cache.join(name), b"invalid index: must not open").unwrap();
    }
    let before = files(fixture.path());
    let args = Arguments::try_parse_from([
        "lwiki",
        "--wiki",
        fixture.path().to_str().unwrap(),
        "--offline",
        "--dry-run",
        "search",
        "needle",
        "--verify-selected",
        "--no-sync",
    ])
    .unwrap();
    let (output, code) = execute(&args);
    assert_eq!(code, 0, "{output:?}");
    assert_eq!(output.data["verification_performed"], false);
    assert!(output.data["hits"].is_null());
    assert!(output.meta.freshness.is_none() && output.meta.verified_at.is_none());
    assert!(!output.meta.network_used);
    assert_eq!(before, files(fixture.path()));
}

#[test]
fn verification_refuses_legacy_and_unsupported_modes_without_remote_setup() {
    let fixture = fixture();
    for mode in ["lexical", "literal", "semantic", "hybrid"] {
        let args = Arguments::try_parse_from([
            "lwiki",
            "--wiki",
            fixture.path().to_str().unwrap(),
            "--offline",
            "search",
            "needle",
            "--verify-selected",
            "--mode",
            mode,
        ])
        .unwrap();
        let (output, _) = execute(&args);
        assert!(!output.ok && output.data.is_null());
        assert_eq!(output.error.unwrap().code, "CAPABILITY_UNAVAILABLE");
        assert!(!output.meta.network_used);
    }
}

#[test]
fn ordinary_lexical_search_selects_the_body_subject_in_both_layouts() {
    for normalized in [false, true] {
        let fixture = fixture();
        for (name, id, title, body) in [
            (
                "activity.md",
                "page_activity",
                "Station log 31415",
                "The station was closed. Activity 31415 was filed.\n",
            ),
            (
                "dispatch.md",
                "page_dispatch",
                "Dispatch record",
                "station 31415 uses the cobalt route.\n",
            ),
        ] {
            std::fs::write(
                fixture.path().join(name),
                format!(
                    "---\nwiki_schema: '1'\nwiki_id: {id}\nwiki_kind: page\nwiki_status: draft\ntitle: {title}\n---\n{body}"
                ),
            )
            .unwrap();
        }
        let wiki = fixture.path().to_str().unwrap();
        let mut rebuild = vec!["lwiki", "--wiki", wiki, "--offline", "index", "rebuild"];
        if normalized {
            rebuild.push("--normalized");
        }
        let (output, code) = execute(&Arguments::try_parse_from(rebuild).unwrap());
        assert_eq!(code, 0, "layout {normalized}: {output:?}");

        for (query, mode, expected) in [
            ("station 31415", "lexical", "dispatch.md"),
            ("station 31415", "literal", "dispatch.md"),
            ("Station log 31415", "lexical", "activity.md"),
        ] {
            let args = Arguments::try_parse_from([
                "lwiki",
                "--wiki",
                wiki,
                "--offline",
                "search",
                query,
                "--mode",
                mode,
            ])
            .unwrap();
            let (output, code) = execute(&args);
            assert_eq!(code, 0, "layout {normalized}, {query}: {output:?}");
            assert_eq!(output.data["hits"][0]["path"], expected);
            assert!(!output.meta.network_used);
        }
    }
}

#[test]
fn verified_ordinary_search_reads_the_selected_subject_with_a_source_citation() {
    use crate::sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore};
    use crate::vault::{VaultFs, VaultRoot};

    let fixture = fixture();
    let store = SourceStore::new(VaultFs::new(VaultRoot::explicit(fixture.path()).unwrap()));
    let target_body = "station 31415 uses the cobalt route.\n";
    let mut target_source = None;
    for (title, body) in [
        (
            "station 31415 station 31415 station 31415",
            "station 31432 was assigned activity 31415.\n",
        ),
        ("Dispatch record", target_body),
    ] {
        let capture = store
            .plan_capture(CaptureRequest {
                title: title.into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: format!("{title}.md"),
                original: body.as_bytes().to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/markdown".into()),
            })
            .unwrap();
        if body == target_body {
            target_source = Some(capture.source_id.clone());
        }
        for operation in capture.draft.unwrap().operations {
            let path = fixture.path().join(operation.target.as_str());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, operation.proposed.unwrap()).unwrap();
        }
    }
    let wiki = fixture.path().to_str().unwrap();
    let args = Arguments::try_parse_from([
        "lwiki",
        "--wiki",
        wiki,
        "--offline",
        "index",
        "rebuild",
        "--normalized",
    ])
    .unwrap();
    let (output, code) = execute(&args);
    assert_eq!(code, 0, "{output:?}");
    let args = Arguments::try_parse_from([
        "lwiki",
        "--wiki",
        wiki,
        "--offline",
        "search",
        "station 31415",
        "--verify-selected",
    ])
    .unwrap();
    let (output, code) = execute(&args);
    assert_eq!(code, 0, "{output:?}");
    let selected = output.data["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| !hit["owner_revision"].is_null())
        .unwrap();
    assert_eq!(selected["source_id"], target_source.unwrap().as_str());
    assert_eq!(selected["excerpt"]["label"], "captured_source");
    assert_eq!(selected["excerpt"]["citation"]["kind"], "source");
    let args = Arguments::try_parse_from([
        "lwiki",
        "--wiki",
        wiki,
        "--offline",
        "read",
        "--path",
        selected["path"].as_str().unwrap(),
        "--start",
        "0",
        "--max-bytes",
        "131072",
    ])
    .unwrap();
    let (read, code) = execute(&args);
    assert_eq!(code, 0, "{read:?}");
    assert_eq!(read.data["body"], target_body);
    assert_eq!(read.data["source_citation"]["eligibility"], "current");
    assert_eq!(
        read.data["source_citation"]["citation"]["reference"]["source_id"],
        selected["source_id"]
    );
    assert_eq!(read.data["truncated"], false);
    assert!(read.data["continuation"].is_null());
    assert!(!read.meta.network_used);
}

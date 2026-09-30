//! The maintained host recipe drives the built CLI against an isolated vault.
use std::process::Command;
#[path = "../test_support/paths.rs"]
mod test_paths;

#[test]
fn host_handoff_preserves_citations_and_author_edits_through_cleanup() {
    let binary = test_paths::binary(env!("CARGO_BIN_EXE_lwiki"));
    let script = test_paths::fixture(env!("CARGO_MANIFEST_DIR"), "scripts/shared_wiki_recipe.py");
    let output = Command::new("python3")
        .arg(script)
        .arg("--binary")
        .arg(binary)
        .output()
        .expect("Python 3 is required by the documented recipe");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let metrics: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(metrics["later_recommendation_offset"].as_u64().unwrap() > 4096);
    assert!(metrics["cleanup_deleted_files"].as_u64().unwrap() > 0);
    assert_eq!(metrics["backup_complete"], true);
}

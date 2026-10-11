use super::*;
use crate::{
    app::{OperationOptions, source_relation_inputs::tests::Fixture},
    sources::selected::verify_captured_source,
};
use std::time::Duration;
fn until() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn fixture() -> (Fixture, OfflineApp) {
    let f = Fixture::new(false);
    for (path, bytes) in &f.captured {
        let actual = f.fs.root().resolve(path).unwrap();
        std::fs::create_dir_all(actual.parent().unwrap()).unwrap();
        std::fs::write(actual, bytes).unwrap();
    }
    let app = OfflineApp::new(f.fs.clone(), OperationOptions::default()).unwrap();
    app.index_rebuild_normalized().unwrap();
    (f, app)
}
#[test]
fn selected_capture_authenticates_all_original_bytes_without_unrelated_body_reads() {
    let (f, app) = fixture();
    let unrelated = f.fs.root().path().join("unrelated-content.md");
    let file = std::fs::File::create(&unrelated).unwrap();
    file.set_len(2 * 1024 * 1024).unwrap();
    let catalog = app.catalog();
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let expected = f.authenticate().unwrap().selected_binding;
    let mut remaining = TOTAL_CAPTURE_BYTES;
    let (binding, captured) = capture_source(
        &f.fs,
        &reader,
        &f.vault,
        &expected.source_id,
        &mut remaining,
        until(),
    )
    .unwrap();
    assert_eq!(binding, expected);
    assert_eq!(captured, f.captured);
    assert_eq!(
        TOTAL_CAPTURE_BYTES - remaining,
        captured.values().map(Vec::len).sum::<usize>()
    );
    verify_captured_source(&f.fs, &binding, &captured).unwrap();
    reader.verify_operations(&catalog).unwrap();
    assert_eq!(std::fs::metadata(unrelated).unwrap().len(), 2 * 1024 * 1024);
}
#[test]
fn duplicate_missing_oversize_and_expired_inputs_refuse_without_partial_result() {
    let (f, app) = fixture();
    let binding = f.authenticate().unwrap().selected_binding;
    assert!(
        unique_ids(
            &[binding.source_id.clone(), binding.source_id.clone()],
            1024,
            until()
        )
        .is_err()
    );
    assert!(unique_ids(&vec![binding.source_id.clone(); 4097], 1024, until()).is_err());
    assert!(unique_ids(&[binding.source_id.clone()], 1024, Instant::now()).is_err());
    let catalog = app.catalog();
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut remaining = TOTAL_CAPTURE_BYTES;
    assert!(
        capture_source(
            &f.fs,
            &reader,
            &f.vault,
            &RecordId::new("fictional_missing").unwrap(),
            &mut remaining,
            until()
        )
        .is_err()
    );
    assert_eq!(remaining, TOTAL_CAPTURE_BYTES);
    let original = f
        .captured
        .keys()
        .find(|p| p.as_str().ends_with("original.bin"))
        .unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(f.fs.root().resolve(original).unwrap())
        .unwrap();
    file.set_len(MAX_CAPTURED_BYTES as u64 + 1).unwrap();
    assert!(
        capture_source(
            &f.fs,
            &reader,
            &f.vault,
            &binding.source_id,
            &mut remaining,
            until()
        )
        .is_err()
    );
}
#[test]
fn stale_catalog_source_and_final_guard_change_are_refused() {
    let (f, app) = fixture();
    let binding = f.authenticate().unwrap().selected_binding;
    let catalog = app.catalog();
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut remaining = TOTAL_CAPTURE_BYTES;
    let (_, captured) = capture_source(
        &f.fs,
        &reader,
        &f.vault,
        &binding.source_id,
        &mut remaining,
        until(),
    )
    .unwrap();
    let guards = captured
        .iter()
        .map(|(p, b)| (p.clone(), (Blake3Hash::digest(b), b.len())))
        .collect();
    let mut changed = captured[&binding.source_path].clone();
    // Syntactically unchanged control envelope, distinct exact bytes.
    changed.push(b'\n');
    std::fs::write(f.fs.root().resolve(&binding.source_path).unwrap(), changed).unwrap();
    assert!(verify_guards(&f.fs, guards, &mut remaining, until()).is_err());
    assert_eq!(
        capture_source(
            &f.fs,
            &reader,
            &f.vault,
            &binding.source_id,
            &mut remaining,
            until()
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn cumulative_allowance_is_enforced_before_large_capture_allocation() {
    let (f, _) = fixture();
    let content = f.content.clone();
    let mut remaining = 1;
    assert!(read(&f.fs, &content, MAX_CONTENT_BYTES, &mut remaining, until()).is_err());
    assert_eq!(remaining, 1);
}

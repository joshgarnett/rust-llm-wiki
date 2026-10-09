use super::*;
use crate::vault::VaultRoot;
use std::{io::Write, time::Duration};

fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn limits() -> MaintenanceLimits {
    MaintenanceLimits {
        max_files: 64,
        max_path_steps: 100_000,
        max_manifest_bytes: 1024 * 1024,
        max_file_bytes: 1024 * 1024,
        max_retained_note_bytes: 4 * 1024 * 1024,
        max_io_bytes: 32 * 1024 * 1024,
        max_elapsed: Duration::from_secs(30),
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    fs: VaultFs,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"),"---\nwiki_schema: '1'\nwiki_id: vault_maintenance\nwiki_kind: vault\ntitle: Maintenance\n---\n").unwrap();
        fs::write(temp.path().join("page.md"), "# Plain authored note\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        Self { temp, fs }
    }
    fn input(&self) -> MaintenanceInput {
        MaintenanceInput::capture(
            &self.fs,
            &RecordId::new("vault_maintenance").unwrap(),
            limits(),
        )
        .unwrap()
    }
    fn write(&self, relative: &str, bytes: &[u8]) {
        let target = self.temp.path().join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
}

#[test]
fn capture_retains_scanned_notes_once_and_excludes_revision_payloads() {
    let f = Fixture::new();
    f.write(
        "sources/source_a/revisions/revision_a/revision.md",
        b"# Revision metadata\n",
    );
    f.write(
        "sources/source_a/revisions/revision_a/content.md",
        &vec![b'x'; 256 * 1024],
    );
    f.write(
        "sources/source_a/revisions/revision_a/original.bin",
        &vec![b'y'; 256 * 1024],
    );
    let mut ceiling = limits();
    ceiling.max_file_bytes = 4096;
    let input =
        MaintenanceInput::capture(&f.fs, &RecordId::new("vault_maintenance").unwrap(), ceiling)
            .unwrap();
    assert_eq!(input.notes().len(), 3);
    assert_eq!(input.scanned.len(), 3);
    assert!(
        !input
            .notes()
            .contains_key(&path("sources/source_a/revisions/revision_a/content.md"))
    );
    assert_eq!(
        input.usage().files,
        3 + input.mutable.borrow().layout_observed.len()
    );
    assert_eq!(input.usage().observed_assets, 0);
    assert!(input.usage().io_bytes < 4096);
    assert_eq!(
        input.usage().retained_note_bytes as u64,
        input.usage().io_bytes - input.usage().layout_io_bytes
    );
    let shared = input.notes().shared();
    assert!(std::ptr::eq(
        input.notes().get(&path("page.md")).unwrap(),
        shared.get(&path("page.md")).unwrap()
    ));
    assert_eq!(input.vault_id().as_str(), "vault_maintenance");
    assert_eq!(input.fs().root().path(), f.fs.root().path());
    let before = input.usage();
    input.final_recheck().unwrap();
    assert_eq!(
        input.usage().io_bytes - input.usage().layout_io_bytes,
        2 * (before.io_bytes - before.layout_io_bytes)
    );
    assert!(input.usage().layout_io_bytes > before.layout_io_bytes);
    assert!(input.usage().path_steps > before.path_steps);
    assert!(input.usage().manifest_bytes > before.manifest_bytes);
    assert_eq!(
        input.usage().retained_note_bytes,
        before.retained_note_bytes
    );
}

#[test]
fn lazy_first_observation_allows_stable_semantic_corruption_but_repeat_change_poisons() {
    let f = Fixture::new();
    let input = f.input();
    let asset = path("payload.bin");
    f.write(
        "payload.bin",
        b"stable bytes may disagree with revision metadata",
    );
    let original = input.read_observed(&asset, 1024).unwrap();
    assert_eq!(
        input.state_observed(&asset).unwrap(),
        ExpectedState::Hash(Blake3Hash::digest(&original))
    );
    input.require_clean().unwrap();
    assert_eq!(input.usage().observed_assets, 1);
    assert_eq!(
        input.usage().files,
        3 + input.mutable.borrow().layout_observed.len()
    );
    f.write("payload.bin", b"changed bytes");
    // A caller may convert this error to Invalid; restoration cannot clear it.
    let swallowed = input.read_observed(&asset, 1024).unwrap_err();
    assert_eq!(swallowed.code, ErrorCode::ContentConflict);
    f.write("payload.bin", &original);
    assert_eq!(input.require_clean().unwrap_err(), swallowed);
    assert_eq!(input.final_recheck().unwrap_err(), swallowed);
    assert_eq!(input.read_observed(&asset, 1024).unwrap_err(), swallowed);
}

#[test]
fn stable_missing_assets_are_observed_absences_without_poison() {
    let f = Fixture::new();
    let input = f.input();
    let missing = path("missing.bin");
    assert_eq!(
        input.state_observed(&missing).unwrap(),
        ExpectedState::Absent
    );
    assert_eq!(
        input.read_observed(&missing, 1024).unwrap_err().code,
        ErrorCode::SourceIntegrity
    );
    input.require_clean().unwrap();
    input.final_recheck().unwrap();
    assert_eq!(input.usage().observed_assets, 1);
    f.write("missing.bin", b"appeared");
    assert_eq!(
        input.state_observed(&missing).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    fs::remove_file(f.temp.path().join("missing.bin")).unwrap();
    assert_eq!(
        input.final_recheck().unwrap_err().code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn layout_marker_observation_cannot_be_overwritten_by_later_note_capture() {
    let f = Fixture::new();
    let input = f.input();
    let marker = fs::read(f.temp.path().join("WIKI.md")).unwrap();
    f.write(
        "WIKI.md",
        &[marker.as_slice(), b"changed after layout validation\n"].concat(),
    );
    assert_eq!(
        input.capture_notes().err().unwrap().code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn retained_layout_is_observed_once_and_rechecked_without_per_asset_plan_reads() {
    use crate::{
        storage::{self, StorageOptions},
        vault::WriterPermit,
    };
    let f = Fixture::new();
    let writer = WriterPermit::acquire(f.fs.root(), Duration::from_secs(1)).unwrap();
    storage::cleanup(&f.fs, &writer, &StorageOptions::default()).unwrap();
    for i in 0..20 {
        f.write(
            &format!(".wiki/retained/packets/packet_{i}.md"),
            b"# Retained packet\n",
        );
    }
    let input = f.input();
    let plan_path = input
        .mutable
        .borrow()
        .layout_observed
        .keys()
        .find(|path| path.as_str().ends_with(".plan.json"))
        .unwrap()
        .clone();
    let plan_bytes = fs::read(f.temp.path().join(plan_path.as_str())).unwrap();
    assert!(input.usage().layout_io_bytes >= plan_bytes.len() as u64);
    assert!(
        input
            .notes()
            .contains_key(&path("knowledge/extractions/packets/packet_0.md"))
    );
    let layout_bytes = input.usage().layout_io_bytes;
    for _ in 0..5 {
        for i in 0..20 {
            input
                .read_observed(
                    &path(&format!("knowledge/extractions/packets/packet_{i}.md")),
                    1024,
                )
                .unwrap();
        }
    }
    assert_eq!(
        input.usage().layout_io_bytes,
        layout_bytes,
        "mapped asset reads must not decode migration receipts again"
    );
    input.final_recheck().unwrap();
    assert!(input.usage().layout_io_bytes >= layout_bytes + plan_bytes.len() as u64);
    fs::remove_file(f.temp.path().join(plan_path.as_str())).unwrap();
    let error = input.final_recheck().unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    f.write(plan_path.as_str(), &plan_bytes);
    assert_eq!(input.require_clean().unwrap_err(), error);
}

#[test]
fn named_asset_budgets_fail_before_payload_allocation_and_remain_sticky() {
    let f = Fixture::new();
    let input = f.input();
    f.write("large.bin", &vec![b'x'; 8192]);
    let before = input.usage().io_bytes;
    let error = input.read_observed(&path("large.bin"), 1024).unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(
        input.usage().io_bytes,
        before,
        "metadata refusal must precede payload reads"
    );
    f.write("large.bin", b"x");
    assert_eq!(
        input.read_observed(&path("large.bin"), 1024).unwrap_err(),
        error
    );
    assert_eq!(input.final_recheck().unwrap_err(), error);

    let f = Fixture::new();
    let total = f.input().usage().io_bytes;
    let mut bound = limits();
    bound.max_io_bytes = total + 1;
    let input =
        MaintenanceInput::capture(&f.fs, &RecordId::new("vault_maintenance").unwrap(), bound)
            .unwrap();
    f.write("extra.bin", b"1234");
    assert_eq!(
        input.state_observed(&path("extra.bin")).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(input.usage().io_bytes, total);
}

#[test]
fn manifest_file_note_and_path_work_admission_have_finite_early_limits() {
    let f = Fixture::new();
    for bound in [
        MaintenanceLimits {
            max_files: 1,
            ..limits()
        },
        MaintenanceLimits {
            max_manifest_bytes: 1,
            ..limits()
        },
        MaintenanceLimits {
            max_path_steps: 1,
            ..limits()
        },
        MaintenanceLimits {
            max_file_bytes: 1,
            ..limits()
        },
        MaintenanceLimits {
            max_retained_note_bytes: 1,
            ..limits()
        },
        MaintenanceLimits {
            max_io_bytes: 1,
            ..limits()
        },
    ] {
        assert_eq!(
            MaintenanceInput::capture(&f.fs, &RecordId::new("vault_maintenance").unwrap(), bound)
                .err()
                .unwrap()
                .code,
            ErrorCode::BudgetExceeded
        );
    }
    for bound in [
        MaintenanceLimits {
            max_files: 0,
            ..limits()
        },
        MaintenanceLimits {
            max_elapsed: Duration::ZERO,
            ..limits()
        },
        MaintenanceLimits {
            max_file_bytes: 64 * 1024 * 1024 + 1,
            ..limits()
        },
    ] {
        assert_eq!(
            MaintenanceInput::capture(&f.fs, &RecordId::new("vault_maintenance").unwrap(), bound)
                .err()
                .unwrap()
                .code,
            ErrorCode::Usage
        );
    }
    let mut bound = limits();
    bound.max_files = f.input().usage().files;
    let input =
        MaintenanceInput::capture(&f.fs, &RecordId::new("vault_maintenance").unwrap(), bound)
            .unwrap();
    f.write("asset.bin", b"x");
    let before = input.usage().io_bytes;
    assert_eq!(
        input
            .read_observed(&path("asset.bin"), 1024)
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(input.usage().io_bytes, before);
}

#[test]
fn final_recheck_detects_scanned_membership_note_and_auxiliary_changes() {
    for change in ["addition", "deletion", "edit", "auxiliary"] {
        let f = Fixture::new();
        let input = f.input();
        if change == "auxiliary" {
            f.write("asset.bin", b"before");
            input.state_observed(&path("asset.bin")).unwrap();
        }
        match change {
            "addition" => f.write("new.md", b"new note"),
            "deletion" => fs::remove_file(f.temp.path().join("page.md")).unwrap(),
            "edit" => f.write("page.md", b"changed note"),
            "auxiliary" => f.write("asset.bin", b"after"),
            _ => unreachable!(),
        }
        assert_eq!(
            input.final_recheck().unwrap_err().code,
            ErrorCode::ContentConflict,
            "{change}"
        );
    }
}

#[test]
fn non_utf8_scanned_notes_keep_actual_hash_bytes_and_parse_diagnostics() {
    let f = Fixture::new();
    f.write("invalid.md", &[0xff, 0x00, b'x']);
    let input = f.input();
    let note = input.notes().get(&path("invalid.md")).unwrap();
    assert_eq!(note.raw, [0xff, 0x00, b'x']);
    assert_eq!(note.source_hash, Blake3Hash::digest(&note.raw));
    assert!(!note.diagnostics.is_empty());
    assert!(note.canonical.is_none());
    input.final_recheck().unwrap();
    f.write("invalid.md", b"valid now");
    assert_eq!(
        input
            .read_observed(&path("invalid.md"), 1024)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn deadline_failure_and_unsafe_named_paths_do_not_clear_after_restoration() {
    let f = Fixture::new();
    let mut input = f.input();
    input.started = Instant::now().checked_sub(Duration::from_secs(60)).unwrap();
    assert_eq!(
        input.require_clean().unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    input.started = Instant::now();
    assert_eq!(
        input.require_clean().unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    #[cfg(unix)]
    {
        let f = Fixture::new();
        let input = f.input();
        f.write("real.bin", b"safe");
        std::os::unix::fs::symlink("real.bin", f.temp.path().join("alias.bin")).unwrap();
        let error = input.read_observed(&path("alias.bin"), 1024).unwrap_err();
        fs::remove_file(f.temp.path().join("alias.bin")).unwrap();
        f.write("alias.bin", b"safe");
        assert_eq!(input.require_clean().unwrap_err(), error);
        assert_eq!(
            input.read_observed(&path("alias.bin"), 1024).unwrap_err(),
            error
        );
    }
}

#[test]
fn parallel_recheck_matches_serial_bytes_and_all_usage_counters() {
    let f = Fixture::new();
    for i in 0..13 {
        f.write(&format!("note_{i:02}.md"), &vec![b'x'; 70_000 + i]);
        f.write(&format!("asset_{i:02}.bin"), &vec![b'y'; 2000 + i]);
    }
    let serial = f.input();
    let parallel = f.input();
    for i in 0..13 {
        let asset = path(&format!("asset_{i:02}.bin"));
        assert_eq!(
            serial.state_observed(&asset).unwrap(),
            parallel.state_observed(&asset).unwrap()
        );
    }
    let missing = path("missing.bin");
    serial.state_observed(&missing).unwrap();
    parallel.state_observed(&missing).unwrap();
    serial.final_recheck_mode(false).unwrap();
    parallel.final_recheck().unwrap();
    assert_eq!(
        serde_json::to_value(serial.usage()).unwrap(),
        serde_json::to_value(parallel.usage()).unwrap()
    );
    assert_eq!(
        serial.notes().get(&path("note_00.md")).unwrap().raw,
        parallel.notes().get(&path("note_00.md")).unwrap().raw
    );
    assert_eq!(
        serial.mutable.borrow().observed[&missing].expected,
        ExpectedState::Absent
    );
}

#[test]
fn conservative_parallel_reservations_fall_back_at_exact_cumulative_budget() {
    let f = Fixture::new();
    for i in 0..20 {
        f.write(&format!("empty_{i:02}.md"), b"");
    }
    let serial = f.input();
    // Exercise the hash slice at its exact allowance. Raw authority reads run
    // after this slice and are covered by the complete-recheck equivalence test.
    let paths = serial.census(false).unwrap();
    serial.recheck_hashes(paths, false).unwrap();
    let exact = serial.usage().io_bytes;
    let mut candidate = f.input();
    candidate.limits.max_io_bytes = exact;
    let paths = candidate.census(false).unwrap();
    candidate.recheck_hashes(paths, true).unwrap();
    assert_eq!(
        serde_json::to_value(candidate.usage()).unwrap(),
        serde_json::to_value(serial.usage()).unwrap()
    );
    candidate.require_clean().unwrap();
}

#[test]
fn parallel_recheck_has_four_worker_streams_and_no_larger_window() {
    use std::sync::{Barrier, Mutex, atomic::AtomicUsize};
    let f = Fixture::new();
    for i in 0..10 {
        f.write(&format!("file_{i:02}.md"), &vec![b'x'; 130_000]);
    }
    let mut input = f.input();
    let barrier = Arc::new(Barrier::new(RECHECK_WORKERS));
    let seen = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let first = Arc::new(AtomicUsize::new(0));
    input.read_hook = Some(Arc::new({
        let active = active.clone();
        let maximum = maximum.clone();
        move |path, boundary| {
            if boundary == ReadBoundary::BeforeChunk && seen.lock().unwrap().insert(path.clone()) {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(count, Ordering::SeqCst);
                if first.fetch_add(1, Ordering::SeqCst) < RECHECK_WORKERS {
                    barrier.wait();
                }
            } else if boundary == ReadBoundary::Completed {
                active.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }));
    input.final_recheck().unwrap();
    assert_eq!(maximum.load(Ordering::SeqCst), RECHECK_WORKERS);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

fn rewrite_preserving_mtime(target: &std::path::Path, bytes: &[u8]) {
    let modified = fs::metadata(target).unwrap().modified().unwrap();
    fs::write(target, bytes).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(target)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
}

#[test]
fn worker_boundary_rechecks_full_hash_after_same_size_restored_mtime_edit() {
    use std::sync::atomic::AtomicBool;
    let f = Fixture::new();
    let original = vec![b'a'; 200_000];
    f.write("raced.md", &original);
    let mut input = f.input();
    let target = f.temp.path().join("raced.md");
    let once = AtomicBool::new(false);
    input.read_hook = Some(Arc::new(move |path, boundary| {
        if path.as_str() == "raced.md"
            && boundary == ReadBoundary::BeforeChunk
            && !once.swap(true, Ordering::SeqCst)
        {
            rewrite_preserving_mtime(&target, &vec![b'b'; 200_000]);
        }
    }));
    let before = input.usage().io_bytes;
    let error = input.final_recheck().unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(error.message.contains("raced.md"));
    assert!(input.usage().io_bytes - before >= original.len() as u64);
    f.write("raced.md", &original);
    assert_eq!(input.require_clean().unwrap_err(), error);
    assert_eq!(input.final_recheck().unwrap_err(), error);
}

#[test]
fn worker_growth_detection_and_panics_reconcile_all_consumed_bytes() {
    use std::sync::atomic::AtomicBool;
    for panic in [false, true] {
        let f = Fixture::new();
        f.write("raced.md", &vec![b'x'; 130_000]);
        let mut input = f.input();
        let target = f.temp.path().join("raced.md");
        let once = AtomicBool::new(false);
        let consumed = Arc::new(AtomicU64::new(0));
        input.read_hook = Some(Arc::new({
            let consumed = consumed.clone();
            move |path, boundary| {
                if boundary == ReadBoundary::AfterChunk && path.as_str() == "raced.md" {
                    consumed.fetch_add(1, Ordering::SeqCst);
                    if panic {
                        panic!("controlled stream panic after accounted read");
                    }
                }
                if boundary == ReadBoundary::BeforeChunk
                    && path.as_str() == "raced.md"
                    && !once.swap(true, Ordering::SeqCst)
                    && !panic
                {
                    fs::OpenOptions::new()
                        .append(true)
                        .open(&target)
                        .unwrap()
                        .write_all(b"growth")
                        .unwrap();
                }
            }
        }));
        let before = input.usage().io_bytes;
        let other_bytes: u64 = input
            .scanned
            .iter()
            .filter(|(p, _)| p.as_str() != "raced.md")
            .map(|(_, file)| file.bytes)
            .sum();
        let error = input.final_recheck().unwrap_err();
        assert_eq!(
            error.code,
            if panic {
                ErrorCode::Internal
            } else {
                ErrorCode::ContentConflict
            }
        );
        assert_eq!(
            input.usage().io_bytes - before,
            other_bytes + if panic { 64 * 1024 } else { 130_001 }
        );
        assert!(consumed.load(Ordering::SeqCst) > 0);
        assert_eq!(input.require_clean().unwrap_err(), error);
    }
}

#[test]
fn out_of_order_worker_failures_report_earliest_admitted_path_and_drain() {
    use std::sync::{Condvar, Mutex, atomic::AtomicBool};
    let f = Fixture::new();
    f.write("a.md", &vec![b'a'; 4096]);
    f.write("b.md", &vec![b'b'; 4096]);
    let mut input = f.input();
    let root = f.temp.path().to_path_buf();
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let a_once = AtomicBool::new(false);
    let b_once = AtomicBool::new(false);
    input.read_hook = Some(Arc::new(move |path, boundary| {
        if path.as_str() == "b.md" && boundary == ReadBoundary::Completed {
            *done.0.lock().unwrap() = true;
            done.1.notify_all();
        }
        if boundary != ReadBoundary::BeforeChunk {
            return;
        }
        if path.as_str() == "a.md" && !a_once.swap(true, Ordering::SeqCst) {
            rewrite_preserving_mtime(&root.join("a.md"), &vec![b'c'; 4096]);
            let (guard, wait) = done
                .1
                .wait_timeout_while(done.0.lock().unwrap(), Duration::from_secs(5), |complete| {
                    !*complete
                })
                .unwrap();
            assert!(
                *guard && !wait.timed_out(),
                "later failure must complete first"
            );
        }
        if path.as_str() == "b.md" && !b_once.swap(true, Ordering::SeqCst) {
            rewrite_preserving_mtime(&root.join("b.md"), &vec![b'd'; 4096]);
        }
    }));
    let before = input.usage().io_bytes;
    let admitted_bytes: u64 = input.scanned.values().map(|file| file.bytes).sum();
    let error = input.final_recheck().unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(error.message.contains("a.md"));
    assert_eq!(input.usage().io_bytes - before, admitted_bytes);
    assert_eq!(input.require_clean().unwrap_err(), error);
}

#[test]
fn worker_deadline_uses_original_clock_and_permanently_poisons_input() {
    use std::sync::atomic::AtomicBool;
    let f = Fixture::new();
    let mut input = f.input();
    input.started = Instant::now();
    input.limits.max_elapsed = Duration::from_millis(100);
    let once = AtomicBool::new(false);
    input.read_hook = Some(Arc::new(move |_, boundary| {
        if boundary == ReadBoundary::BeforeChunk && !once.swap(true, Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(150));
        }
    }));
    let error = input.final_recheck().unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    input.started = Instant::now();
    input.limits.max_elapsed = Duration::from_secs(30);
    assert_eq!(input.require_clean().unwrap_err(), error);
    assert_eq!(input.final_recheck().unwrap_err(), error);
}

#[cfg(unix)]
#[test]
fn admitted_descriptor_rejects_parent_and_leaf_replacement() {
    use std::sync::atomic::AtomicBool;
    for parent in [false, true] {
        let f = Fixture::new();
        f.write("sub/raced.md", b"unchanged file bytes");
        let mut input = f.input();
        let root = f.temp.path().to_path_buf();
        let once = AtomicBool::new(false);
        input.read_hook = Some(Arc::new(move |path, boundary| {
            if path.as_str() != "sub/raced.md"
                || boundary != ReadBoundary::BeforeChunk
                || once.swap(true, Ordering::SeqCst)
            {
                return;
            }
            if parent {
                fs::rename(root.join("sub"), root.join("kept")).unwrap();
                fs::create_dir(root.join("sub")).unwrap();
                fs::write(root.join("sub/raced.md"), b"unchanged file bytes").unwrap();
            } else {
                fs::rename(root.join("sub/raced.md"), root.join("sub/kept.bin")).unwrap();
                std::os::unix::fs::symlink("kept.bin", root.join("sub/raced.md")).unwrap();
            }
        }));
        let error = input.final_recheck().unwrap_err();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        let preserved = if parent {
            "kept/raced.md"
        } else {
            "sub/kept.bin"
        };
        assert_eq!(
            fs::read(f.temp.path().join(preserved)).unwrap(),
            b"unchanged file bytes"
        );
        assert_eq!(input.require_clean().unwrap_err(), error);
    }
}

#[cfg(unix)]
#[test]
fn special_leaf_swap_at_actual_open_boundary_cannot_block_admission() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::atomic::AtomicBool};
    let f = Fixture::new();
    let mut input = f.input();
    let target = f.temp.path().join("page.md");
    let once = AtomicBool::new(false);
    input.read_hook = Some(Arc::new(move |path, boundary| {
        if path.as_str() != "page.md"
            || boundary != ReadBoundary::BeforeOpen
            || once.swap(true, Ordering::SeqCst)
        {
            return;
        }
        fs::remove_file(&target).unwrap();
        let native = CString::new(target.as_os_str().as_bytes()).unwrap();
        // A blocking read-only open of this FIFO would hang without a writer.
        assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
    }));
    let before = input.usage().io_bytes;
    let error = input.final_recheck().unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(input.usage().io_bytes - before <= input.scanned[&path("WIKI.md")].bytes);
    assert_eq!(input.require_clean().unwrap_err(), error);
}

#[test]
fn raw_layout_authority_is_rehashed_serially_after_worker_streams() {
    use std::sync::atomic::AtomicBool;
    let f = Fixture::new();
    let mut input = f.input();
    let target = f.temp.path().join("WIKI.md");
    let original = fs::read(&target).unwrap();
    let original_for_hook = original.clone();
    let once = AtomicBool::new(false);
    input.read_hook = Some(Arc::new(move |path, boundary| {
        if path.as_str() == "WIKI.md"
            && boundary == ReadBoundary::BeforeOpen
            && once.swap(true, Ordering::SeqCst)
        {
            let mut changed = original_for_hook.clone();
            changed[0] ^= 1;
            rewrite_preserving_mtime(&target, &changed);
        }
    }));
    let error = input.final_recheck().unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(error.message.contains("storage layout"));
    f.write("WIKI.md", &original);
    assert_eq!(input.require_clean().unwrap_err(), error);
}

#[test]
fn auxiliary_worker_mutation_keeps_observation_authority_and_poison() {
    use std::sync::atomic::AtomicBool;
    let f = Fixture::new();
    f.write("payload.bin", b"original auxiliary bytes");
    let mut input = f.input();
    let asset = path("payload.bin");
    let original = input.state_observed(&asset).unwrap();
    let target = f.temp.path().join("payload.bin");
    let once = AtomicBool::new(false);
    input.read_hook = Some(Arc::new(move |path, boundary| {
        if path.as_str() == "payload.bin"
            && boundary == ReadBoundary::BeforeChunk
            && !once.swap(true, Ordering::SeqCst)
        {
            rewrite_preserving_mtime(&target, b"modified auxiliary bytes");
        }
    }));
    let error = input.final_recheck().unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(error.message.contains("payload.bin"));
    assert_eq!(input.mutable.borrow().observed[&asset].expected, original);
    f.write("payload.bin", b"original auxiliary bytes");
    assert_eq!(input.require_clean().unwrap_err(), error);
}

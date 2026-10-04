use super::*;
use crate::vault::VaultRoot;
use std::time::Duration;

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

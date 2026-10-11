//! Small independent namespace adversaries, using the existing pre-open hook.
use super::*;
use crate::vault::{ExpectedState, VaultFs, WriterPermit};
use std::{cell::Cell, fs, time::Duration};
thread_local! { static OPEN_FAILURE: Cell<Option<usize>> = const { Cell::new(None) }; }
pub(super) fn fail_open() -> bool {
    OPEN_FAILURE.with(|value| match value.get() {
        None => false,
        Some(0) => {
            value.set(None);
            true
        }
        Some(left) => {
            value.set(Some(left - 1));
            false
        }
    })
}
#[derive(Default)]
struct Ownership {
    known: std::collections::BTreeSet<RawFd>,
    peak: usize,
}
thread_local! { static OWNERSHIP: std::cell::RefCell<Option<Ownership>> = const { std::cell::RefCell::new(None) }; }
pub(super) fn record_open(fd: RawFd) {
    OWNERSHIP.with(|cell| {
        if let Some(value) = cell.borrow_mut().as_mut() {
            value.known.insert(fd);
            value
                .known
                .retain(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0);
            value.peak = value.peak.max(value.known.len());
        }
    });
}
fn start_ownership() {
    OWNERSHIP.with(|cell| {
        assert!(cell.borrow().is_none());
        *cell.borrow_mut() = Some(Ownership::default());
    });
}
fn end_ownership() -> (usize, usize) {
    OWNERSHIP.with(|cell| {
        let value = cell.borrow_mut().take().unwrap();
        (
            value.peak,
            value
                .known
                .iter()
                .filter(|fd| unsafe { libc::fcntl(**fd, libc::F_GETFD) } >= 0)
                .count(),
        )
    })
}
struct Exhaustion;
impl Drop for Exhaustion {
    fn drop(&mut self) {
        OPEN_FAILURE.with(|value| value.set(None));
    }
}
fn fixture() -> (tempfile::TempDir, VaultFs) {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("vault")).unwrap();
    fs::write(
        temp.path().join("vault/WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(temp.path().join("vault")).unwrap())
        .for_maintenance_execution();
    (temp, vault)
}
fn relative(name: &str) -> VaultRelativePath {
    VaultRelativePath::new(name).unwrap()
}
fn hook(
    path: PathBuf,
    action: impl FnOnce() + Send + 'static,
) -> crate::vault::maintenance_read_tests::BeforeOpenGuard {
    crate::vault::maintenance_read_tests::install_before_open(path, action)
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn ancestor_rename_and_replacement_does_not_return_detached_bytes() {
    let (_temp, vault) = fixture();
    let ancestor = vault.root().path().join("notes");
    fs::create_dir(&ancestor).unwrap();
    fs::write(ancestor.join("page.md"), b"original").unwrap();
    let moved = ancestor.with_file_name("detached");
    let edited = ancestor.clone();
    let _hook = hook(ancestor.join("page.md"), move || {
        fs::rename(&edited, moved).unwrap();
        fs::create_dir(&edited).unwrap();
        fs::write(edited.join("page.md"), b"original").unwrap();
    });
    assert_eq!(
        vault
            .read_before(&relative("notes/page.md"))
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(fs::read(ancestor.join("page.md")).unwrap(), b"original");
    assert_eq!(
        fs::read(vault.root().path().join("detached/page.md")).unwrap(),
        b"original"
    );
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn ancestor_symlink_swap_refuses_and_preserves_external_leaf() {
    let (_temp, vault) = fixture();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("page.md"), b"external").unwrap();
    let ancestor = vault.root().path().join("notes");
    fs::create_dir(&ancestor).unwrap();
    fs::write(ancestor.join("page.md"), b"original").unwrap();
    let edited = ancestor.clone();
    let moved = ancestor.with_file_name("detached");
    let outside_path = outside.path().to_owned();
    let _hook = hook(ancestor.join("page.md"), move || {
        fs::rename(&edited, moved).unwrap();
        std::os::unix::fs::symlink(outside_path, edited).unwrap();
    });
    assert!(vault.read_before(&relative("notes/page.md")).is_err());
    assert_eq!(
        fs::read(outside.path().join("page.md")).unwrap(),
        b"external"
    );
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn nested_marker_inserted_after_inspection_is_observed_on_fresh_walk() {
    let (_temp, vault) = fixture();
    let ancestor = vault.root().path().join("notes");
    fs::create_dir(&ancestor).unwrap();
    fs::write(ancestor.join("page.md"), b"original").unwrap();
    let marker = ancestor.join("WIKI.md");
    let _hook = hook(ancestor.join("page.md"), move || {
        fs::write(marker, b"nested vault").unwrap()
    });
    assert!(vault.read_before(&relative("notes/page.md")).is_err());
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn absent_ancestor_becoming_symlink_during_callbacks_refuses() {
    let (_temp, vault) = fixture();
    let outside = tempfile::tempdir().unwrap();
    let ancestor = vault.root().path().join("missing");
    let mut calls = 0;
    let error = super::Route::resolve(vault.root(), &relative("missing/leaf.md"), &mut || {
        calls += 1;
        if calls == 2 {
            std::os::unix::fs::symlink(outside.path(), &ancestor).unwrap();
        }
        Ok(())
    })
    .err()
    .expect("a symlink introduced during resolution must be rejected");
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(!outside.path().join("leaf.md").exists());
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn root_rebinding_to_same_content_refuses_original_binding() {
    let (temp, vault) = fixture();
    let target = vault.root().path().join("page.md");
    fs::write(&target, b"original").unwrap();
    let root = vault.root().path().to_owned();
    let moved = temp.path().join("detached-root");
    let _hook = hook(target, move || {
        fs::rename(&root, moved).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("WIKI.md"),
            include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
        )
        .unwrap();
        fs::write(root.join("page.md"), b"original").unwrap();
    });
    assert_eq!(
        vault.read_before(&relative("page.md")).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        fs::read(temp.path().join("vault/page.md")).unwrap(),
        b"original"
    );
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn leaf_replaced_after_named_metadata_refuses_even_same_length() {
    let (_temp, vault) = fixture();
    let target = vault.root().path().join("page.md");
    fs::write(&target, b"before").unwrap();
    let edited = target.clone();
    let _hook = hook(target, move || {
        fs::rename(&edited, edited.with_file_name("old.md")).unwrap();
        fs::write(edited, b"after!").unwrap();
    });
    assert_eq!(
        vault.read_before(&relative("page.md")).unwrap_err().code,
        ErrorCode::ContentConflict
    );
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn same_length_in_place_edit_remains_a_byte_guard_conflict() {
    let (_temp, vault) = fixture();
    let permit = WriterPermit::acquire(vault.root(), Duration::from_secs(1)).unwrap();
    let target = vault.root().path().join("page.md");
    fs::write(&target, b"before").unwrap();
    let staged = vault
        .stage(&relative("page.md"), b"proposed", &permit)
        .unwrap();
    let edited = target.clone();
    let _hook = hook(target.clone(), move || {
        fs::write(edited, b"after!").unwrap()
    });
    let expected = ExpectedState::Hash(crate::domain::Blake3Hash::digest(b"before"));
    assert_eq!(
        vault.replace(staged, &expected, &permit).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert_eq!(fs::read(target).unwrap(), b"after!");
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn layout_witness_change_at_read_boundary_is_not_cached() {
    let (_temp, vault) = fixture();
    let ancestor = vault.root().path().join("changes/change");
    fs::create_dir_all(&ancestor).unwrap();
    fs::write(ancestor.join("page.md"), b"original").unwrap();
    let marker = vault.root().path().join("WIKI.md");
    let _hook = hook(ancestor.join("page.md"), move || {
        let text = fs::read_to_string(&marker)
            .unwrap()
            .replace("wiki_schema: \"1\"", "wiki_schema: \"2\"");
        fs::write(marker, text).unwrap();
    });
    assert_eq!(
        vault
            .read_before(&relative("changes/change/page.md"))
            .unwrap_err()
            .code,
        ErrorCode::RecoveryRequired
    );
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn descriptor_exhaustion_fails_closed_and_a_later_read_succeeds() {
    let (_temp, vault) = fixture();
    fs::write(vault.root().path().join("page.md"), b"original").unwrap();
    // Fail at multiple openat boundaries without process-global rlimit changes
    // that could disturb other concurrently running correctness tests.
    for admitted in [0, 1, 3] {
        let _reset = Exhaustion;
        OPEN_FAILURE.with(|value| value.set(Some(admitted)));
        assert!(vault.read_before(&relative("page.md")).is_err());
        OPEN_FAILURE.with(|value| value.set(None));
        assert_eq!(
            vault
                .read_before(&relative("page.md"))
                .unwrap()
                .unwrap()
                .bytes,
            b"original"
        );
    }
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn held_parent_cannot_authorize_replacement_after_parent_move() {
    let (_temp, vault) = fixture();
    let permit = WriterPermit::acquire(vault.root(), Duration::from_secs(1)).unwrap();
    let ancestor = vault.root().path().join("notes");
    fs::create_dir(&ancestor).unwrap();
    let staged = vault
        .stage(&relative("notes/page.md"), b"proposed", &permit)
        .unwrap();
    let old_stage_name = staged.path().file_name().unwrap().to_owned();
    fs::rename(&ancestor, ancestor.with_file_name("detached")).unwrap();
    fs::create_dir(&ancestor).unwrap();
    // Copy identical staged bytes into the replacement namespace: path/hash
    // validation alone would accept this, but the retained parent must not.
    fs::write(ancestor.join(old_stage_name), b"proposed").unwrap();
    assert_eq!(
        vault
            .replace(staged, &ExpectedState::Absent, &permit)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert!(!ancestor.join("page.md").exists());
    assert!(!vault.root().path().join("detached/page.md").exists());
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn ordinary_native_create_replace_append_truncate_delete_workflow_is_supported() {
    let (_temp, vault) = fixture();
    let permit = WriterPermit::acquire(vault.root(), Duration::from_secs(1)).unwrap();
    assert_eq!(
        vault
            .ensure_directory(&relative("notes/deep"), &permit)
            .unwrap(),
        crate::vault::DirectorySync::Supported
    );
    let target = relative("notes/deep/page.md");
    let staged = vault.stage(&target, b"one", &permit).unwrap();
    vault
        .replace(staged, &ExpectedState::Absent, &permit)
        .unwrap();
    let staged = vault.stage(&target, b"two", &permit).unwrap();
    vault
        .replace(
            staged,
            &ExpectedState::Hash(crate::domain::Blake3Hash::digest(b"one")),
            &permit,
        )
        .unwrap();
    assert_eq!(vault.read_before(&target).unwrap().unwrap().bytes, b"two");
    vault
        .append_synced(&relative(".wiki/state/events"), b"1234", &permit)
        .unwrap();
    vault
        .truncate_synced(&relative(".wiki/state/events"), 2, &permit)
        .unwrap();
    assert_eq!(
        vault
            .read_before(&relative(".wiki/state/events"))
            .unwrap()
            .unwrap()
            .bytes,
        b"12"
    );
    vault.sync_target(&target, &permit).unwrap();
    vault
        .delete(
            &target,
            &ExpectedState::Hash(crate::domain::Blake3Hash::digest(b"two")),
            &permit,
        )
        .unwrap();
    assert!(vault.read_before(&target).unwrap().is_none());
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn private_authority_read_refuses_rebound_state_parent() {
    use crate::changes::operation_authority::{self, Presence, Publication};
    let (_temp, vault) = fixture();
    let permit = WriterPermit::acquire(vault.root(), Duration::from_secs(1)).unwrap();
    let vault_id =
        crate::domain::RecordId::new("vault_00000000-0000-7000-8000-00000000001b").unwrap();
    operation_authority::activate(
        &vault,
        &permit,
        &vault_id,
        Publication {
            file_id: "a".repeat(32),
            epoch: 1,
        },
        Presence::LegacyMayBeAbsent,
    )
    .unwrap();
    let state = vault.root().path().join(".wiki/state");
    let bytes = fs::read(state.join("operations.json")).unwrap();
    let edited = state.clone();
    let _hook = hook(state.join("operations.json"), move || {
        fs::rename(&edited, edited.with_file_name("detached-state")).unwrap();
        fs::create_dir(&edited).unwrap();
        fs::write(edited.join("operations.json"), bytes).unwrap();
    });
    assert!(operation_authority::load(&vault, &vault_id, Presence::Required).is_err());
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "solaris",
    target_os = "illumos",
    target_os = "aix",
    target_os = "cygwin",
    target_os = "emscripten"
))]
#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn search_only_ancestor_does_not_require_directory_read_permission() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, vault) = fixture();
    let ancestor = vault.root().path().join("search-only");
    fs::create_dir(&ancestor).unwrap();
    fs::write(ancestor.join("page.md"), b"readable bytes").unwrap();
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o111)).unwrap();
    let observed = vault.read_before(&relative("search-only/page.md"));
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(observed.unwrap().unwrap().bytes, b"readable bytes");
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn relative_native_mutations_remain_inside_held_parent_after_symlink_swap() {
    use crate::vault::{DurableIo, NativeIo};
    use std::io::Write;
    let (_temp, vault) = fixture();
    let ancestor = vault.root().path().join("notes");
    fs::create_dir(&ancestor).unwrap();
    let route = vault
        .root()
        .bind(&relative("notes/stage.tmp"), true)
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("target.md"), b"external").unwrap();
    let detached = ancestor.with_file_name("detached");
    fs::rename(&ancestor, &detached).unwrap();
    std::os::unix::fs::symlink(outside.path(), &ancestor).unwrap();
    let parent = route.parent().unwrap();
    let mut stage = NativeIo
        .create_stage_at(parent, OsStr::new("stage.tmp"))
        .unwrap();
    stage.write_all(b"inside").unwrap();
    NativeIo
        .replace_at(
            parent,
            OsStr::new("stage.tmp"),
            parent,
            OsStr::new("target.md"),
        )
        .unwrap();
    assert_eq!(fs::read(detached.join("target.md")).unwrap(), b"inside");
    assert_eq!(
        fs::read(outside.path().join("target.md")).unwrap(),
        b"external"
    );
    NativeIo.remove_at(parent, OsStr::new("target.md")).unwrap();
    assert!(!detached.join("target.md").exists());
    assert_eq!(
        fs::read(outside.path().join("target.md")).unwrap(),
        b"external"
    );
    // Containment by fd is separate from current named authorization.
    assert!(
        vault
            .root()
            .bind(&relative("notes/target.md"), true)
            .is_err()
    );
}

#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos",
        target_os = "aix",
        target_os = "cygwin",
        target_os = "emscripten"
    ),
    test
)]
fn deep_route_owns_constant_live_descriptors_and_error_cuts_close_all() {
    let (_temp, vault) = fixture();
    let depth = 96;
    let names = (0..depth).map(|_| "d").collect::<Vec<_>>().join("/");
    let directory = vault.root().path().join(&names);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("leaf.md"), b"deep bytes").unwrap();
    let target = relative(&format!("{names}/leaf.md"));
    start_ownership();
    let route = vault.root().bind(&target, true).unwrap();
    let (peak, live) = end_ownership();
    assert!(peak <= 3, "depth-dependent route FD ownership: {peak}");
    assert_eq!(live, 1, "only final parent is handed off");
    drop(route);
    let root_opens = vault.root().path().components().count();
    let all_opens = 2 * (root_opens + depth);
    for cut in [
        0,
        root_opens,
        root_opens + depth - 1,
        root_opens + depth,
        all_opens - 1,
    ] {
        let _reset = Exhaustion;
        OPEN_FAILURE.with(|value| value.set(Some(cut)));
        start_ownership();
        assert!(
            vault.root().bind(&target, true).is_err(),
            "unreached descriptor failure {cut}"
        );
        let (peak, live) = end_ownership();
        assert!(peak <= 3, "error walk FD ownership {peak}");
        assert_eq!(live, 0, "descriptor leak at cut {cut}");
        OPEN_FAILURE.with(|value| value.set(None));
    }
    assert_eq!(
        vault
            .read_regular_bounded(&target, true, 32, "deep-test")
            .unwrap()
            .unwrap(),
        b"deep bytes"
    );
}

#[test]
fn prepared_stage_crosses_execution_scope_in_both_directions() {
    if !super::supported() {
        return;
    }
    for descriptor_created in [false, true] {
        let (_temp, scoped) = fixture();
        let ordinary = VaultFs::new(scoped.root().clone());
        let permit = WriterPermit::acquire(scoped.root(), Duration::from_secs(1)).unwrap();
        let target = relative("page.md");
        let creator = if descriptor_created {
            &scoped
        } else {
            &ordinary
        };
        let publisher = if descriptor_created {
            &ordinary
        } else {
            &scoped
        };
        let staged = creator.stage(&target, b"prepared", &permit).unwrap();
        publisher
            .replace(staged, &ExpectedState::Absent, &permit)
            .unwrap();
        assert_eq!(
            fs::read(scoped.root().path().join("page.md")).unwrap(),
            b"prepared"
        );
    }
}

#[test]
fn scope_crossing_preserves_prepared_parent_swap_refusal() {
    if !super::supported() {
        return;
    }
    for descriptor_created in [false, true] {
        let (_temp, scoped) = fixture();
        let ordinary = VaultFs::new(scoped.root().clone());
        let permit = WriterPermit::acquire(scoped.root(), Duration::from_secs(1)).unwrap();
        let target = relative("notes/page.md");
        let ancestor = scoped.root().path().join("notes");
        fs::create_dir(&ancestor).unwrap();
        let creator = if descriptor_created {
            &scoped
        } else {
            &ordinary
        };
        let publisher = if descriptor_created {
            &ordinary
        } else {
            &scoped
        };
        let staged = creator.stage(&target, b"prepared", &permit).unwrap();
        let leaf = staged.path().file_name().unwrap().to_owned();
        fs::rename(&ancestor, ancestor.with_file_name("detached")).unwrap();
        fs::create_dir(&ancestor).unwrap();
        fs::write(ancestor.join(leaf), b"prepared").unwrap();
        assert_eq!(
            publisher
                .replace(staged, &ExpectedState::Absent, &permit)
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        assert!(!ancestor.join("page.md").exists());
    }
}

thread_local! { static FAIL_PARENT_OBSERVATION: Cell<bool> = const { Cell::new(false) }; }
pub(super) fn fail_parent_observation() -> bool {
    FAIL_PARENT_OBSERVATION.with(|value| value.replace(false))
}
#[test]
fn failed_prepared_parent_observation_cleans_created_stage() {
    if !super::supported() {
        return;
    }
    for scoped_mode in [false, true] {
        let (_temp, scoped) = fixture();
        let ordinary = VaultFs::new(scoped.root().clone());
        let creator = if scoped_mode { &scoped } else { &ordinary };
        let permit = WriterPermit::acquire(scoped.root(), Duration::from_secs(1)).unwrap();
        FAIL_PARENT_OBSERVATION.with(|value| value.set(true));
        assert!(
            creator
                .stage(&relative("page.md"), b"prepared", &permit)
                .is_err()
        );
        assert!(!fs::read_dir(scoped.root().path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".lwiki-stage-")
        }));
        let retry = creator
            .stage(&relative("page.md"), b"prepared", &permit)
            .unwrap();
        creator
            .replace(retry, &ExpectedState::Absent, &permit)
            .unwrap();
    }
}
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}
fn migrated_fixture() -> (tempfile::TempDir, VaultFs) {
    let (temp, scoped) = fixture();
    let ordinary = VaultFs::new(scoped.root().clone());
    let permit = WriterPermit::acquire(scoped.root(), Duration::from_secs(1)).unwrap();
    crate::storage::cleanup(
        &ordinary,
        &permit,
        &crate::storage::StorageOptions::default(),
    )
    .unwrap();
    (temp, scoped)
}
#[test]
fn scoped_logical_mapping_refuses_rebound_activation_ancestor() {
    if !super::supported() {
        return;
    }
    let (_temp, scoped) = migrated_fixture();
    let storage = scoped.root().path().join(".wiki/state/storage");
    let moved = storage.with_file_name("detached-storage");
    let path = storage.clone();
    let _hook = hook(storage.join("layout.json"), move || {
        fs::rename(&path, &moved).unwrap();
        copy_tree(&moved, &path);
    });
    assert_eq!(
        scoped
            .read_before(&relative("changes/example/manifest.json"))
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
}
#[test]
fn scoped_retained_map_and_existing_object_reads_refuse_parent_rebinding() {
    if !super::supported() {
        return;
    }
    for map in [true, false] {
        let (_temp, scoped) = migrated_fixture();
        let permit = WriterPermit::acquire(scoped.root(), Duration::from_secs(1)).unwrap();
        let id = crate::domain::RecordId::new("change_scope").unwrap();
        let target = if map {
            crate::storage::layout::payload_map_path(&id).unwrap()
        } else {
            crate::storage::layout::object_path(&crate::domain::Blake3Hash::digest(b"payload"))
                .unwrap()
        };
        let absolute = scoped.root().path().join(target.as_str());
        fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        fs::write(&absolute, b"payload").unwrap();
        let parent = absolute.parent().unwrap().to_path_buf();
        let moved = parent.with_file_name("detached-payload");
        let _hook = hook(absolute.clone(), move || {
            fs::rename(&parent, &moved).unwrap();
            copy_tree(&moved, &parent);
        });
        let error = if map {
            crate::storage::layout::legacy_payload(
                &scoped,
                &relative(&format!("changes/{}/proposed/page.md", id)),
                32,
            )
            .unwrap_err()
        } else {
            crate::storage::layout::put(&scoped, &permit, &target, b"payload").unwrap_err()
        };
        assert_eq!(error.code, ErrorCode::ContentConflict);
    }
}

#[test]
fn scoped_worker_logical_mapping_rechecks_owner_witness_route() {
    if !super::supported() {
        return;
    }
    let (_temp, scoped) = migrated_fixture();
    let ordinary = VaultFs::new(scoped.root().clone());
    let permit = WriterPermit::acquire(scoped.root(), Duration::from_secs(1)).unwrap();
    crate::maintenance_parallel::command_scope(|| {
        let proof = crate::storage::maintenance_activation::prepare_parallel_activation(
            &permit, &ordinary,
        )?
        .unwrap();
        let _worker = crate::storage::maintenance_activation::enter_worker(Some(proof))?;
        let storage = scoped.root().path().join(".wiki/state/storage");
        let moved = storage.with_file_name("worker-detached-storage");
        let path = storage.clone();
        let _hook = hook(storage.join("layout.json"), move || {
            fs::rename(&path, &moved).unwrap();
            copy_tree(&moved, &path);
        });
        assert_eq!(
            scoped
                .read_before(&relative("changes/example/manifest.json"))
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn scoped_initialization_accepts_empty_namespace_and_refuses_retained_authority() {
    if !super::supported() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let scoped = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap())
        .for_maintenance_execution();
    fs::create_dir_all(temp.path().join(".wiki/state")).unwrap();
    fs::write(temp.path().join(".wiki/state/writer.lock"), b"").unwrap();
    assert!(!crate::storage::layout::active_fs(&scoped).unwrap());
    fs::create_dir_all(temp.path().join(".wiki/retained")).unwrap();
    assert_eq!(
        crate::storage::layout::active_fs(&scoped).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
}

#[test]
fn scoped_legacy_payload_object_lookup_refuses_bound_object_parent_swap() {
    if !super::supported() {
        return;
    }
    let (_temp, scoped) = fixture();
    let ordinary = VaultFs::new(scoped.root().clone());
    let permit = WriterPermit::acquire(scoped.root(), Duration::from_secs(1)).unwrap();
    let engine = crate::changes::ChangeEngine::new(ordinary.clone()).unwrap();
    let prepared = engine
        .prepare(
            &permit,
            crate::changes::ChangeDraft {
                title: "retained reader fixture".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: std::collections::BTreeMap::new(),
                read_preconditions: vec![],
                operations: vec![crate::changes::ExpectedWrite {
                    target: relative("assets/blob.bin"),
                    expected: ExpectedState::Absent,
                    proposed: Some(b"payload".to_vec()),
                    apply_after: vec![],
                }],
            },
        )
        .unwrap()
        .prepared;
    let (manifest, _) = engine.load_manifest_structure(&prepared.change_id).unwrap();
    assert_eq!(
        manifest.version, 1,
        "prepare before migration must retain a legacy payload"
    );
    let reference = manifest.operations[0].after_payload.clone().unwrap();
    assert!(
        reference
            .path
            .as_str()
            .starts_with(&format!("changes/{}/", prepared.change_id))
    );
    // Exercise the ordinary migration to create the authenticated payload map
    // and shared object; a modern object reference cannot test this legacy path.
    crate::storage::cleanup(
        &ordinary,
        &permit,
        &crate::storage::StorageOptions::default(),
    )
    .unwrap();
    let object = scoped.root().path().join(
        crate::storage::layout::object_path(&reference.hash)
            .unwrap()
            .as_str(),
    );
    assert_eq!(
        crate::storage::layout::legacy_payload(&scoped, &reference.path, 32)
            .unwrap()
            .unwrap(),
        b"payload"
    );
    let parent = object.parent().unwrap().to_path_buf();
    let moved = parent.with_file_name("detached-object");
    let _hook = hook(object, move || {
        fs::rename(&parent, &moved).unwrap();
        copy_tree(&moved, &parent);
    });
    assert_eq!(
        crate::storage::layout::legacy_payload(&scoped, &reference.path, 32)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn scoped_directory_creation_keeps_managed_mapping_and_fresh_activation_guards() {
    if !super::supported() {
        return;
    }
    let (_temp, scoped) = migrated_fixture();
    let permit = WriterPermit::acquire(scoped.root(), Duration::from_secs(1)).unwrap();
    scoped
        .ensure_directory(&relative("changes/mapped-directory"), &permit)
        .unwrap();
    assert!(
        scoped
            .root()
            .path()
            .join(".wiki/retained/changes/mapped-directory")
            .is_dir()
    );
    assert!(
        !scoped
            .root()
            .path()
            .join("changes/mapped-directory")
            .exists()
    );
    let storage = scoped.root().path().join(".wiki/state/storage");
    let moved = storage.with_file_name("directory-detached-storage");
    let path = storage.clone();
    let _hook = hook(storage.join("layout.json"), move || {
        fs::rename(&path, &moved).unwrap();
        copy_tree(&moved, &path);
    });
    assert!(
        scoped
            .ensure_directory(&relative("changes/refused-directory"), &permit)
            .is_err()
    );
    assert!(
        !scoped
            .root()
            .path()
            .join(".wiki/retained/changes/refused-directory")
            .exists()
    );
}

#[test]
fn scoped_owner_capture_preserves_missing_marker_initialization_checks() {
    if !super::supported() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let scoped = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap())
        .for_maintenance_execution();
    let permit = WriterPermit::acquire(scoped.root(), Duration::from_secs(1)).unwrap();
    crate::maintenance_parallel::command_scope(|| {
        assert!(
            crate::storage::maintenance_activation::prepare_parallel_activation(&permit, &scoped)?
                .is_none()
        );
        Ok(())
    })
    .unwrap();
    fs::create_dir_all(temp.path().join(".wiki/retained")).unwrap();
    crate::maintenance_parallel::command_scope(|| {
        let error = match crate::storage::maintenance_activation::prepare_parallel_activation(
            &permit, &scoped,
        ) {
            Err(error) => error,
            Ok(_) => panic!("retained authority must refuse marker-less owner capture"),
        };
        assert_eq!(error.code, ErrorCode::RecoveryRequired);
        Ok(())
    })
    .unwrap();
}

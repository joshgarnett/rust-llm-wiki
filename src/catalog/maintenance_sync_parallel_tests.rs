use super::*;
use crate::{catalog::query_diagnostics, maintenance_parallel, vault::VaultRoot};
use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};

type Hook = Arc<dyn Fn(&str, &VaultRelativePath) + Send + Sync>;
static HOOKS: OnceLock<Mutex<BTreeMap<PathBuf, Hook>>> = OnceLock::new();
pub(super) fn hook(phase: &str, fs: &VaultFs, path: &VaultRelativePath) {
    let callback = HOOKS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(fs.root().path())
        .cloned();
    if let Some(callback) = callback {
        callback(phase, path);
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    fs: VaultFs,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_sync_parallel\nwiki_kind: vault\ntitle: Sync\n---\n").unwrap();
        fs::write(temp.path().join("page.md"), "# Authored before\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        Self { temp, fs }
    }
    fn limits() -> MaintenanceLimits {
        MaintenanceLimits {
            max_files: 128,
            max_path_steps: 100_000,
            max_manifest_bytes: 1024 * 1024,
            max_file_bytes: 1024 * 1024,
            max_retained_note_bytes: 4 * 1024 * 1024,
            max_io_bytes: 32 * 1024 * 1024,
            max_elapsed: Duration::from_secs(30),
        }
    }
    fn input(&self) -> MaintenanceInput {
        MaintenanceInput::capture(
            &self.fs,
            &RecordId::new("vault_sync_parallel").unwrap(),
            Self::limits(),
        )
        .unwrap()
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.temp.path().join(name), bytes).unwrap();
    }
    fn hook(&self, hook: impl Fn(&str, &VaultRelativePath) + Send + Sync + 'static) {
        HOOKS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(self.fs.root().path().to_path_buf(), Arc::new(hook));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        HOOKS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(self.fs.root().path());
    }
}
fn path(name: &str) -> VaultRelativePath {
    VaultRelativePath::new(name).unwrap()
}

#[test]
fn sequential_and_concurrent_capture_and_recheck_observe_equal_inputs_and_work() {
    let f = Fixture::new();
    for i in 0..24 {
        f.write(
            &format!("note_{i:02}.md"),
            format!("# Note {i}\n").as_bytes(),
        );
    }
    let sequential = maintenance_parallel::sequential_scope(|| {
        let input = f.input();
        input.final_recheck()?;
        Ok(input)
    })
    .unwrap();
    let concurrent = maintenance_parallel::command_scope(|| {
        let input = f.input();
        input.final_recheck()?;
        Ok(input)
    })
    .unwrap();
    assert_eq!(sequential.scanned.len(), concurrent.scanned.len());
    for (path, file) in &sequential.scanned {
        assert_eq!(file.hash, concurrent.scanned[path].hash);
    }
    assert_eq!(sequential.usage().io_bytes, concurrent.usage().io_bytes);
    assert_eq!(sequential.usage().path_steps, concurrent.usage().path_steps);
    assert_eq!(
        sequential.usage().retained_note_bytes,
        concurrent.usage().retained_note_bytes
    );
}

#[test]
fn final_checkpoint_rehashes_same_size_restored_mtime_author_edit() {
    let f = Fixture::new();
    maintenance_parallel::command_scope(|| {
        let input = f.input();
        let original = fs::metadata(f.temp.path().join("page.md"))
            .unwrap()
            .modified()
            .unwrap();
        f.write("page.md", b"# Authored after!\n");
        fs::File::options()
            .write(true)
            .open(f.temp.path().join("page.md"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(original))
            .unwrap();
        assert_eq!(
            input.final_recheck().unwrap_err().code,
            ErrorCode::ContentConflict
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn ordered_content_failure_charges_successful_higher_index_read_before_return() {
    for sequential in [true, false] {
        let f = Fixture::new();
        f.write("a.bin", b"before");
        f.write("z.bin", b"higher index bytes");
        let run = || {
            let input = f.input();
            let paths = [path("a.bin"), path("z.bin")];
            input.states_observed(&paths)?;
            f.write("a.bin", b"edited");
            let before = input.usage();
            assert_eq!(
                input.states_observed(&paths).unwrap_err().code,
                ErrorCode::ContentConflict
            );
            assert_eq!(
                input.usage().io_bytes - before.io_bytes,
                6 + b"higher index bytes".len() as u64
            );
            assert!(input.usage().path_steps > before.path_steps);
            Ok(())
        };
        if sequential {
            maintenance_parallel::sequential_scope(run).unwrap();
        } else {
            maintenance_parallel::command_scope(run).unwrap();
        }
    }
}

#[test]
fn shared_byte_budget_accepts_skewed_inputs_whose_aggregate_fits() {
    let f = Fixture::new();
    f.write("a.bin", &vec![b'a'; 100_000]);
    f.write("z.bin", b"z");
    maintenance_parallel::command_scope(|| {
        let mut input = f.input();
        input.limits.max_io_bytes = input.usage().io_bytes + 100_001;
        assert_eq!(
            input
                .states_observed(&[path("a.bin"), path("z.bin")])?
                .len(),
            2
        );
        assert_eq!(input.usage().io_bytes, input.limits.max_io_bytes);
        Ok(())
    })
    .unwrap();
}

#[test]
fn shared_budget_exhaustion_is_explicit_and_all_performed_work_is_charged() {
    let f = Fixture::new();
    f.write("a.bin", &vec![b'a'; 10_000]);
    f.write("z.bin", &vec![b'z'; 10_000]);
    maintenance_parallel::command_scope(|| {
        let mut input = f.input();
        let before = input.usage().io_bytes;
        input.limits.max_io_bytes = before + 15_000;
        let error = input
            .states_observed(&[path("a.bin"), path("z.bin")])
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert_eq!(input.usage().io_bytes - before, 10_000);
        assert_eq!(input.require_clean().unwrap_err(), error);
        let mut input = f.input();
        input.limits.max_path_steps = input.usage().path_steps + 1;
        assert_eq!(
            input
                .states_observed(&[path("a.bin"), path("z.bin")])
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(input.usage().path_steps, input.limits.max_path_steps);
        Ok(())
    })
    .unwrap();
}

#[test]
fn worker_forbidden_access_and_panic_are_joined_and_observer_is_merged() {
    let f = Fixture::new();
    f.write("asset.bin", b"payload");
    maintenance_parallel::command_scope(|| {
        let input = f.input();
        f.hook(|phase, _| {
            if phase == "before_observation" {
                query_diagnostics::access("sync-worker-canary");
            }
        });
        let guard = query_diagnostics::forbid_access(&["sync-worker-canary"]);
        assert_eq!(
            input
                .states_observed(&[path("asset.bin")])
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
        assert_eq!(guard.finish(), vec!["sync-worker-canary"]);
        assert!(maintenance_parallel::metrics().panicked >= 1);
        Ok(())
    })
    .unwrap();
}

#[cfg(unix)]
#[test]
fn raced_fifo_open_is_nonblocking_and_refuses_after_reached_metadata() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let f = Fixture::new();
    f.write("asset.bin", b"payload");
    maintenance_parallel::command_scope(|| {
        let input = f.input();
        let target = f.temp.path().join("asset.bin");
        f.hook(move |phase, path| {
            if phase == "before_open" && path.as_str() == "asset.bin" {
                fs::remove_file(&target).unwrap();
                let name = CString::new(target.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
        });
        assert_eq!(
            input
                .states_observed(&[path("asset.bin")])
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        Ok(())
    })
    .unwrap();
}

#[cfg(unix)]
#[test]
fn parent_symlink_replacement_after_bytes_fails_fresh_namespace_guard() {
    let f = Fixture::new();
    fs::create_dir(f.temp.path().join("data")).unwrap();
    f.write("data/asset.bin", b"payload");
    maintenance_parallel::command_scope(|| {
        let input = f.input();
        let root = f.temp.path().to_path_buf();
        f.hook(move |phase, path| {
            if phase == "after_bytes" && path.as_str() == "data/asset.bin" {
                fs::rename(root.join("data"), root.join("renamed")).unwrap();
                std::os::unix::fs::symlink("renamed", root.join("data")).unwrap();
            }
        });
        assert!(input.states_observed(&[path("data/asset.bin")]).is_err());
        Ok(())
    })
    .unwrap();
}

#[cfg(unix)]
#[test]
fn stable_multi_error_selection_agrees_across_sequential_and_concurrent_batches() {
    for sequential in [true, false] {
        let f = Fixture::new();
        fs::create_dir(f.temp.path().join("a.bin")).unwrap();
        f.write("real.bin", b"payload");
        std::os::unix::fs::symlink("real.bin", f.temp.path().join("z.bin")).unwrap();
        let run = || {
            let input = f.input();
            let before = input.usage().path_steps;
            let error = input
                .states_observed(&[path("a.bin"), path("z.bin")])
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::ContentConflict);
            assert_eq!(error.message, "maintenance input is not a regular file");
            assert!(input.usage().path_steps >= before + 3);
            Ok(())
        };
        if sequential {
            maintenance_parallel::sequential_scope(run).unwrap();
        } else {
            maintenance_parallel::command_scope(run).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn edit_after_observation_is_allowed_for_that_checkpoint_and_next_checkpoint_refuses() {
    use std::sync::Condvar;
    let f = Fixture::new();
    f.write("a.bin", b"before");
    f.write("z.bin", b"stable");
    maintenance_parallel::command_scope(|| {
        let input = f.input();
        let paths = [path("a.bin"), path("z.bin")];
        input.states_observed(&paths)?;
        let barrier = Arc::new((Mutex::new(0u8), Condvar::new()));
        let target = f.temp.path().join("a.bin");
        f.hook(move |phase, path| {
            let (lock, changed) = &*barrier;
            if phase == "after_bytes" && path.as_str() == "a.bin" {
                let mut state = lock.lock().unwrap();
                *state = 1;
                changed.notify_all();
                while *state != 2 {
                    let (next, timeout) =
                        changed.wait_timeout(state, Duration::from_secs(5)).unwrap();
                    assert!(
                        !timeout.timed_out(),
                        "second worker did not release held observation"
                    );
                    state = next;
                }
            }
            if phase == "before_observation" && path.as_str() == "z.bin" {
                let mut state = lock.lock().unwrap();
                while *state != 1 {
                    let (next, timeout) =
                        changed.wait_timeout(state, Duration::from_secs(5)).unwrap();
                    assert!(!timeout.timed_out(), "first observation not reached");
                    state = next;
                }
                fs::write(&target, b"edited").unwrap();
                *state = 2;
                changed.notify_all();
            }
        });
        // The observed bytes were old, despite the edit occurring before join.
        input.states_observed(&paths)?;
        HOOKS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .remove(f.fs.root().path());
        assert_eq!(
            input.states_observed(&paths).unwrap_err().code,
            ErrorCode::ContentConflict
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn deadline_expiring_at_reached_worker_boundary_cannot_complete_checkpoint() {
    let f = Fixture::new();
    f.write("asset.bin", b"payload");
    maintenance_parallel::command_scope(|| {
        let mut input = f.input();
        input.limits.max_elapsed = Duration::from_millis(20);
        input.started = Instant::now();
        f.hook(|phase, _| {
            if phase == "before_observation" {
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let before = input.usage().io_bytes;
        assert_eq!(
            input
                .states_observed(&[path("asset.bin")])
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(input.usage().io_bytes, before);
        assert_eq!(
            input.require_clean().unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn path_outlier_selects_whole_batch_owner_before_dispatch_without_refusing_input() {
    for deep in [false, true] {
        let f = Fixture::new();
        f.write("a.bin", b"ordinary");
        let outlier = if deep {
            let relative = format!("{}/asset.bin", vec!["d"; 80].join("/"));
            let target = f.temp.path().join(&relative);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, b"outlier").unwrap();
            path(&relative)
        } else {
            f.write("asset.bin", b"outlier");
            let mut relative = String::with_capacity(10_000);
            relative.push_str("asset.bin");
            VaultRelativePath::new(relative).unwrap()
        };
        maintenance_parallel::command_scope(|| {
            let input = f.input();
            let owner = std::thread::current().id();
            let observations = Arc::new(Mutex::new(Vec::new()));
            let recorded = observations.clone();
            f.hook(move |phase, path| {
                if phase == "before_observation" {
                    recorded
                        .lock()
                        .unwrap()
                        .push((path.clone(), std::thread::current().id()));
                }
            });
            // Preserve the deliberately oversized allocation: String::clone
            // shrinks spare capacity and would remove this fixture's outlier.
            let paths = [path("a.bin"), outlier];
            if !deep {
                assert!(paths[1].owned_capacity() > 8192);
            }
            assert_eq!(input.states_observed(&paths)?.len(), 2);
            let observations = observations.lock().unwrap();
            assert_eq!(observations.len(), 2);
            assert_eq!(observations[0].0, paths[0]);
            assert_eq!(observations[1].0, paths[1]);
            assert!(observations.iter().all(|(_, thread)| *thread == owner));
            Ok(())
        })
        .unwrap();
    }
}

use super::*;
use std::{
    path::Path,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
};

#[test]
fn retained_results_hold_their_reservation_until_dropped() {
    command_scope(|| {
        let overhead = (std::mem::size_of::<Job<i32>>()
            + std::mem::size_of::<mpsc::Receiver<Completion<i32>>>()
            + std::mem::size_of::<Result<i32>>()) as u64
            + job_overhead::<i32>();
        let available = available_bytes();
        let first = run_batch(vec![Job::new(available - overhead, || Ok(7))])?;
        let denied = run_batch(vec![Job::new(1, || Ok(9))]);
        assert!(matches!(denied, Err(error) if error.code == ErrorCode::BudgetExceeded));
        drop(first);
        let admitted = run_batch(vec![Job::new(1, || Ok(9))])?;
        assert_eq!(admitted.into_iter().next().unwrap()?, 9);
        assert_eq!(metrics().max_in_flight_bytes, MAX_IN_FLIGHT_BYTES);
        Ok(())
    })
    .unwrap();
}

#[test]
fn descriptor_admission_refuses_before_running_any_job() {
    let ran = Arc::new(AtomicBool::new(false));
    command_scope(|| {
        let jobs = (0..MAX_JOBS + 1)
            .map(|_| {
                let ran = ran.clone();
                Job::new(1, move || {
                    ran.store(true, Ordering::SeqCst);
                    Ok(())
                })
            })
            .collect();
        let result = run_batch(jobs);
        assert!(matches!(result, Err(error) if error.code == ErrorCode::BudgetExceeded));
        assert!(!ran.load(Ordering::SeqCst));
        assert_eq!(metrics().submitted, 0);
        Ok(())
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn all_sixteen_jobs_join_and_reduce_in_descriptor_order() {
    let barrier = Arc::new(Barrier::new(MAX_JOBS));
    command_scope(|| {
        let jobs = (0..MAX_JOBS)
            .map(|index| {
                let barrier = barrier.clone();
                Job::new(1024, move || {
                    barrier.wait();
                    if index == 3 {
                        panic!("injected worker panic")
                    }
                    if index == 1 {
                        return Err(error("ordered stable failure"));
                    }
                    Ok(index)
                })
            })
            .collect();
        let results = run_batch(jobs)?.into_iter().collect::<Vec<_>>();
        assert_eq!(results.len(), MAX_JOBS);
        assert_eq!(*results[0].as_ref().unwrap(), 0);
        assert_eq!(
            results[1].as_ref().unwrap_err().message,
            "ordered stable failure"
        );
        assert_eq!(
            results[3].as_ref().unwrap_err().message,
            "maintenance worker panicked"
        );
        assert_eq!(*results[15].as_ref().unwrap(), 15);
        let metrics = metrics();
        assert_eq!(metrics.completed, MAX_JOBS as u64);
        assert_eq!(metrics.failed, 2);
        assert_eq!(metrics.panicked, 1);
        assert_eq!(metrics.max_active, MAX_JOBS);
        Ok(())
    })
    .unwrap();
}

#[test]
fn nested_jobs_and_scopes_are_refused() {
    command_scope(|| {
        let results = run_batch(vec![
            Job::new(1024, || {
                let result = run_batch(vec![Job::new(1024, || Ok(()))]);
                assert!(matches!(result, Err(error) if error.message == "nested maintenance jobs are forbidden"));
                Ok(())
            }),
            Job::new(1024, || {
                let result = command_scope(|| Ok(()));
                assert!(matches!(result, Err(error) if error.message == "maintenance command scopes are forbidden inside worker jobs"));
                Ok(())
            }),
        ])?;
        for result in results { result?; }
        Ok(())
    }).unwrap();
}

#[test]
fn worker_reads_and_panicking_forbidden_access_merge_before_return() {
    crate::catalog::query_diagnostics::begin();
    let forbidden =
        crate::catalog::query_diagnostics::forbid_access(&["maintenance-test-forbidden"]);
    crate::vault::paths::profile::begin();
    command_scope(|| {
        let jobs = vec![
            Job::new(1024, || {
                crate::catalog::query_diagnostics::read("test-worker", Path::new("worker-a"), 7);
                let _profile = crate::vault::paths::profile::portable();
                Ok(())
            }),
            Job::new(1024, || {
                crate::catalog::query_diagnostics::read("test-worker", Path::new("worker-b"), 11);
                crate::catalog::query_diagnostics::access("maintenance-test-forbidden");
                Ok(())
            }),
        ];
        let results = run_batch(jobs)?.into_iter().collect::<Vec<_>>();
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        Ok(())
    })
    .unwrap();
    let observation = crate::catalog::query_diagnostics::end();
    assert_eq!(
        observation
            .reads
            .iter()
            .map(|read| (read.path.as_str(), read.bytes))
            .collect::<Vec<_>>(),
        vec![("worker-a", 7), ("worker-b", 11)]
    );
    assert_eq!(forbidden.finish(), vec!["maintenance-test-forbidden"]);
    assert_eq!(crate::vault::paths::profile::finish().portable_calls, 1);
}

#[test]
fn sequential_and_library_fallback_preserve_owner_observers() {
    for explicit_scope in [false, true] {
        crate::catalog::query_diagnostics::begin();
        let run = || {
            let results = run_batch(vec![Job::new(1024, || {
                crate::catalog::query_diagnostics::read("owner", Path::new("fallback"), 5);
                Ok(())
            })])?;
            for result in results {
                result?;
            }
            Ok(())
        };
        if explicit_scope {
            sequential_scope(run).unwrap();
        } else {
            run().unwrap();
        }
        let observation = crate::catalog::query_diagnostics::end();
        assert_eq!(observation.reads.len(), 1);
        assert_eq!(observation.reads[0].bytes, 5);
    }
}

#[test]
fn no_job_and_sequential_commands_never_start_threads() {
    command_scope(|| {
        CURRENT.with(|slot| {
            let slot = slot.borrow();
            assert!(slot.as_ref().unwrap().workers.lock().unwrap().is_empty());
        });
        for result in run_batch_sequential(vec![Job::new(1, || Ok(()))])? {
            result?;
        }
        CURRENT.with(|slot| {
            let slot = slot.borrow();
            assert!(slot.as_ref().unwrap().workers.lock().unwrap().is_empty());
        });
        Ok(())
    })
    .unwrap();
}

#[test]
fn returned_results_have_finished_service_accounting_and_context_teardown() {
    command_scope(|| {
        let batch = run_batch(vec![Job::new(1, || Ok(()))])?;
        assert_eq!(metrics().completed, 1);
        assert!(metrics().worker_service_ns > 0);
        CURRENT.with(|slot| {
            assert_eq!(
                slot.borrow()
                    .as_ref()
                    .unwrap()
                    .counters
                    .active
                    .load(Ordering::Relaxed),
                0
            )
        });
        for result in batch {
            result?;
        }
        Ok(())
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn held_worker_cancellation_error_and_panic_drain_before_writer_release() {
    use crate::{
        jobs::CancellationToken,
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use std::{
        fs,
        sync::{Condvar, Mutex, mpsc},
        time::Duration,
    };
    // The supervisor owns release even if any assertion fails.
    struct Release(Arc<(Mutex<bool>, Condvar)>);
    impl Drop for Release {
        fn drop(&mut self) {
            let (lock, ready) = &*self.0;
            *lock.lock().unwrap() = true;
            ready.notify_all();
        }
    }
    struct DrainingOwner {
        thread: Option<std::thread::JoinHandle<()>>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        cancel: CancellationToken,
    }
    impl Drop for DrainingOwner {
        fn drop(&mut self) {
            self.cancel.cancel();
            let (lock, ready) = &*self.gate;
            *lock.lock().unwrap() = true;
            ready.notify_all();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    for mode in ["cancel", "error", "panic"] {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            include_bytes!("../tests/fixtures/bootstrap/vault/WIKI.md"),
        )
        .unwrap();
        fs::write(temp.path().join("success.bin"), b"success").unwrap();
        fs::write(temp.path().join("failure.bin"), b"failurebody").unwrap();
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let owner_root = root.clone();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Release(gate.clone());
        let worker_gate = gate.clone();
        let cancel = CancellationToken::default();
        let requested = cancel.clone();
        let entered = Arc::new(AtomicBool::new(false));
        let first_entered = entered.clone();
        let sibling_entered = entered.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (failed_tx, failed_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let owner = std::thread::spawn(move || {
            let writer = WriterPermit::acquire(&owner_root, Duration::ZERO).unwrap();
            let fs = VaultFs::new(owner_root);
            let success_root = fs.root().clone();
            let failure_root = fs.root().clone();
            let success_path = crate::domain::VaultRelativePath::new("success.bin").unwrap();
            let failure_path = crate::domain::VaultRelativePath::new("failure.bin").unwrap();
            let reservation = |root: &VaultRoot, path: &crate::domain::VaultRelativePath| {
                crate::storage::maintenance_activation::WORKSPACE_BYTES
                    + crate::vault::paths::parallel_path_workspace(root, path).unwrap()
                    + root.owned_capacity() as u64
                    + path.owned_capacity() as u64
            };
            let success_reservation = reservation(&success_root, &success_path);
            let failure_reservation = reservation(&failure_root, &failure_path);
            crate::catalog::query_diagnostics::begin();
            let result = command_scope(|| {
                let batch = run_batch(vec![
                    Job::new(success_reservation, move || {
                        first_entered.store(true, Ordering::Release);
                        ready_tx.send(()).unwrap();
                        let (lock, changed) = &*worker_gate;
                        let mut released = lock.lock().unwrap();
                        while !*released {
                            let (next, timed) = changed
                                .wait_timeout(released, Duration::from_secs(10))
                                .unwrap();
                            released = next;
                            if timed.timed_out() {
                                return Err(error("held worker release deadline"));
                            }
                        }
                        let observed = crate::vault::fs::hash_regular_raw_bounded(
                            &success_root,
                            &success_path,
                            7,
                            "held-worker",
                        )?
                        .unwrap();
                        assert_eq!(observed.0, 7);
                        Ok(())
                    }),
                    Job::new(failure_reservation, move || {
                        let deadline = std::time::Instant::now() + Duration::from_secs(5);
                        while !sibling_entered.load(Ordering::Acquire) {
                            if std::time::Instant::now() >= deadline {
                                return Err(error("held sibling did not enter"));
                            }
                            std::thread::yield_now();
                        }
                        while mode == "cancel" && !requested.is_cancelled() {
                            if std::time::Instant::now() >= deadline {
                                return Err(error("supervisor did not cancel reached worker"));
                            }
                            std::thread::yield_now();
                        }
                        let observed = crate::vault::fs::hash_regular_raw_bounded(
                            &failure_root,
                            &failure_path,
                            11,
                            "failed-worker",
                        )?
                        .unwrap();
                        assert_eq!(observed.0, 11);
                        failed_tx.send(()).unwrap();
                        if mode == "panic" {
                            panic!("reached failed sibling panic");
                        }
                        Err(crate::domain::WikiError::new(
                            if mode == "cancel" {
                                ErrorCode::Cancelled
                            } else {
                                ErrorCode::Internal
                            },
                            "reached failed sibling",
                        ))
                    }),
                ])?;
                let metrics = metrics();
                assert_eq!(metrics.completed, 2);
                assert_eq!(metrics.failed, 1);
                assert_eq!(metrics.panicked, u64::from(mode == "panic"));
                assert!(metrics.max_active >= 2);
                for result in batch {
                    result?;
                }
                Ok(())
            });
            let observed = crate::catalog::query_diagnostics::end();
            assert_eq!(observed.reads.iter().map(|r| r.bytes).sum::<usize>(), 18);
            assert!(result.is_err());
            drop(writer);
            done_tx.send(()).unwrap();
        });
        let mut owner = DrainingOwner {
            thread: Some(owner),
            gate: gate.clone(),
            cancel: cancel.clone(),
        };
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        cancel.cancel();
        failed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            done_rx.try_recv().is_err(),
            "owner returned while worker held"
        );
        let contender_root = root.clone();
        let denied = std::thread::spawn(move || {
            WriterPermit::acquire(&contender_root, Duration::ZERO)
                .err()
                .map(|e| e.code)
        })
        .join()
        .unwrap();
        assert_eq!(denied, Some(ErrorCode::LockTimeout));
        assert!(
            done_rx.try_recv().is_err(),
            "writer released before drainage"
        );
        drop(release);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        owner.thread.take().unwrap().join().unwrap();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        drop(permit);
    }
}

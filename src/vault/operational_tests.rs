use super::operational::*;
use super::{DirectorySync, DurableIo, ExpectedState, NativeIo, VaultFs, VaultRoot, WriterPermit};
use crate::{domain::*, jobs::AttemptRef};
use std::{
    fs::{self, File},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn fixture() -> (tempfile::TempDir, VaultFs, WriterPermit) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let writer = WriterPermit::acquire(vault.root(), Duration::from_millis(100)).unwrap();
    (temp, vault, writer)
}
fn vault_id() -> RecordId {
    id("vault_00000000-0000-7000-8000-00000000001b")
}
fn attempt() -> AttemptRef {
    AttemptRef {
        run_id: id("run_private"),
        task_key: Blake3Hash::digest(b"task"),
        attempt_id: id("attempt_private"),
        number: 1,
        request_hash: Blake3Hash::digest(b"request"),
    }
}
#[test]
fn operational_private_spool_bounds_permissions_and_guards() {
    let (temp, vault, writer) = fixture();
    let a = attempt();
    let store = RunStore::bootstrap(&vault, &writer, &vault_id(), &a.run_id).unwrap();
    let guard = store.lock(Duration::ZERO, &mut || Ok(())).unwrap();
    assert_eq!(guard.append_journal(0, b"one").unwrap(), 3);
    assert_eq!(
        guard.append_journal(0, b"two").unwrap_err().code,
        ErrorCode::ContentConflict
    );
    guard.append_journal(3, b"tail").unwrap();
    guard.truncate_torn_tail(7, 3).unwrap();
    assert_eq!(guard.read_journal().unwrap().unwrap().bytes, b"one");
    guard.ensure_attempt_dir(&a).unwrap();
    let hash = guard
        .secure_replace(
            RunFile::Spool {
                attempt: &a,
                part: SpoolPart::Body,
            },
            ExpectedState::Absent,
            b"sensitive mock response",
        )
        .unwrap();
    assert_eq!(
        guard.read_spool(&a, SpoolPart::Body, 3).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(
        guard
            .secure_replace(
                RunFile::Spool {
                    attempt: &a,
                    part: SpoolPart::Body
                },
                ExpectedState::Absent,
                b"overwrite"
            )
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        guard.enumerate_spool_ids(1, &mut || Ok(())).unwrap(),
        vec![a.clone()]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = temp.path().join(".wiki/state/requests/attempt_private");
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(directory.join("response.bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let mut wrong = a.clone();
    wrong.run_id = id("another_run");
    assert_eq!(
        guard.ensure_attempt_dir(&wrong).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    guard.remove_spool_file(&a, SpoolPart::Body, &hash).unwrap();
    guard.remove_empty_attempt_dir(&a).unwrap();
    assert!(
        guard
            .enumerate_spool_ids(1, &mut || Ok(()))
            .unwrap()
            .is_empty()
    );
}
#[test]
fn operational_cleanup_interruption_and_missing_authority_preserve_history() {
    let (temp, vault, writer) = fixture();
    let a = attempt();
    let store = RunStore::bootstrap(&vault, &writer, &vault_id(), &a.run_id).unwrap();
    let guard = store.lock(Duration::ZERO, &mut || Ok(())).unwrap();
    guard.ensure_attempt_dir(&a).unwrap();
    // Actual on-disk state immediately after verified owner removal, before rmdir.
    fs::remove_file(
        temp.path()
            .join(".wiki/state/requests/attempt_private/owner.json"),
    )
    .unwrap();
    assert!(
        guard
            .enumerate_spool_ids(1, &mut || Ok(()))
            .unwrap()
            .is_empty()
    );
    guard.remove_empty_attempt_dir(&a).unwrap();
    assert!(
        !temp
            .path()
            .join(".wiki/state/requests/attempt_private")
            .exists()
    );
    guard.remove_empty_attempt_dir(&a).unwrap();
    fs::remove_file(temp.path().join(".wiki/state/jobs/run_private/journal.bin")).unwrap();
    assert!(guard.read_journal().unwrap().is_none());
    assert!(guard.append_journal(0, b"new false history").is_err());
    assert!(
        !temp
            .path()
            .join(".wiki/state/jobs/run_private/journal.bin")
            .exists()
    );
    drop(guard);
    let open = RunStore::open_existing(&vault, &vault_id(), &id("missing_run")).unwrap();
    assert!(open.lock(Duration::ZERO, &mut || Ok(())).is_err());
    assert!(!temp.path().join(".wiki/state/jobs/missing_run").exists());
}
#[test]
fn operational_ownerless_cleanup_refuses_every_retained_payload() {
    for name in [
        "response.bin",
        "metadata.json",
        "semantic-rejection.bin",
        "semantic-rejection.json",
        "http-error.bin",
        "http-error.json",
        ".lwiki-private-interrupted.tmp",
        "unfamiliar.bin",
    ] {
        let (temp, vault, writer) = fixture();
        let a = attempt();
        let store = RunStore::bootstrap(&vault, &writer, &vault_id(), &a.run_id).unwrap();
        let guard = store.lock(Duration::ZERO, &mut || Ok(())).unwrap();
        guard.ensure_attempt_dir(&a).unwrap();
        let directory = temp.path().join(".wiki/state/requests/attempt_private");
        fs::remove_file(directory.join("owner.json")).unwrap();
        fs::write(directory.join(name), b"retained private bytes").unwrap();
        assert_eq!(
            guard.remove_empty_attempt_dir(&a).unwrap_err().code,
            ErrorCode::ContentConflict,
            "{name}"
        );
        assert_eq!(
            fs::read(directory.join(name)).unwrap(),
            b"retained private bytes",
            "{name}"
        );
        assert!(!directory.join("owner.json").exists());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }
}
#[test]
fn operational_ownerless_cleanup_keeps_pending_epoch_and_run_barriers() {
    let (temp, vault, writer) = fixture();
    let a = attempt();
    let store = RunStore::bootstrap(&vault, &writer, &vault_id(), &a.run_id).unwrap();
    let guard = store.lock(Duration::ZERO, &mut || Ok(())).unwrap();
    guard.append_journal(0, b"retained history").unwrap();
    guard.ensure_attempt_dir(&a).unwrap();
    let directory = temp.path().join(".wiki/state/requests/attempt_private");
    fs::remove_file(directory.join("owner.json")).unwrap();
    let mut wrong = a.clone();
    wrong.run_id = id("another_run");
    assert_eq!(
        guard.remove_empty_attempt_dir(&wrong).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert!(directory.is_dir());
    let pending = temp.path().join(".wiki/state/storage/cleanup.json");
    fs::create_dir_all(pending.parent().unwrap()).unwrap();
    // Ordinary mutations refuse the pending marker before decoding its contents.
    fs::write(&pending, b"pending cleanup authority").unwrap();
    assert_eq!(
        guard.remove_empty_attempt_dir(&a).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    assert!(directory.is_dir());
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    assert_eq!(fs::read(&pending).unwrap(), b"pending cleanup authority");
    assert_eq!(
        guard.read_journal().unwrap().unwrap().bytes,
        b"retained history"
    );
    fs::remove_file(pending).unwrap();
    guard.remove_empty_attempt_dir(&a).unwrap();
    assert!(!directory.exists());
}
#[cfg(unix)]
#[test]
fn operational_ownerless_cleanup_keeps_symlink_and_lock_inode_guards() {
    let (temp, vault, writer) = fixture();
    let a = attempt();
    let store = RunStore::bootstrap(&vault, &writer, &vault_id(), &a.run_id).unwrap();
    let guard = store.lock(Duration::ZERO, &mut || Ok(())).unwrap();
    guard.ensure_attempt_dir(&a).unwrap();
    let directory = temp.path().join(".wiki/state/requests/attempt_private");
    fs::remove_file(directory.join("owner.json")).unwrap();
    let retained = directory.with_file_name("retained_empty_directory");
    fs::rename(&directory, &retained).unwrap();
    std::os::unix::fs::symlink(&retained, &directory).unwrap();
    assert!(guard.remove_empty_attempt_dir(&a).is_err());
    assert!(fs::symlink_metadata(&directory).unwrap().is_symlink());
    assert!(retained.is_dir());
    fs::remove_file(&directory).unwrap();
    fs::rename(&retained, &directory).unwrap();
    let lock = temp.path().join(".wiki/state/jobs/run_private/ledger.lock");
    fs::rename(&lock, lock.with_extension("old")).unwrap();
    fs::write(&lock, b"").unwrap();
    assert_eq!(
        guard.remove_empty_attempt_dir(&a).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert!(directory.is_dir());
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
}
#[test]
fn operational_symlink_changed_inode_and_sparse_spool_refuse() {
    let (temp, vault, writer) = fixture();
    let a = attempt();
    let store = RunStore::bootstrap(&vault, &writer, &vault_id(), &a.run_id).unwrap();
    let guard = store.lock(Duration::ZERO, &mut || Ok(())).unwrap();
    guard.ensure_attempt_dir(&a).unwrap();
    let payload = temp
        .path()
        .join(".wiki/state/requests/attempt_private/response.bin");
    let sparse = File::create(&payload).unwrap();
    sparse.set_len(8 * 1024 * 1024 + 1).unwrap();
    assert_eq!(
        guard
            .read_spool(&a, SpoolPart::Body, 8 * 1024 * 1024)
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    drop(sparse);
    fs::remove_file(&payload).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(temp.path().join("WIKI.md"), &payload).unwrap();
        assert!(guard.read_spool(&a, SpoolPart::Body, 1000).is_err());
        fs::remove_file(&payload).unwrap();
        let lock = temp.path().join(".wiki/state/jobs/run_private/ledger.lock");
        fs::rename(&lock, lock.with_extension("old")).unwrap();
        fs::write(&lock, b"").unwrap();
        assert_eq!(
            guard.append_journal(0, b"forbidden").unwrap_err().code,
            ErrorCode::ContentConflict
        );
    }
}
#[test]
fn operational_run_discovery_is_bounded_readonly_and_preserves_incomplete_history() {
    let (temp, vault, writer) = fixture();
    assert!(
        RunStore::discover_existing(&vault, &vault_id(), 4, &mut || Ok(()))
            .unwrap()
            .is_empty()
    );
    assert!(!temp.path().join(".wiki/state/jobs").exists());
    let run = id("run_private");
    RunStore::bootstrap(&vault, &writer, &vault_id(), &run).unwrap();
    let incomplete = temp.path().join(".wiki/state/jobs/run_incomplete");
    fs::create_dir(&incomplete).unwrap();
    let mut visits = 0;
    let discovered = RunStore::discover_existing(&vault, &vault_id(), 4, &mut || {
        visits += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(discovered, vec![id("run_incomplete"), run]);
    assert!(visits >= 3);
    assert!(incomplete.read_dir().unwrap().next().is_none());
    assert_eq!(
        RunStore::discover_existing(&vault, &vault_id(), 1, &mut || Ok(()))
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(
        RunStore::discover_existing(&vault, &id("wrong_vault"), 4, &mut || Ok(()))
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert!(
        RunStore::discover_existing(&vault, &vault_id(), 4, &mut || Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "cancelled discovery"
        )))
        .is_err()
    );
    fs::write(temp.path().join(".wiki/state/jobs/foreign_file"), b"retain").unwrap();
    assert!(RunStore::discover_existing(&vault, &vault_id(), 4, &mut || Ok(())).is_err());
    assert_eq!(
        fs::read(temp.path().join(".wiki/state/jobs/foreign_file")).unwrap(),
        b"retain"
    );
}

struct FaultIo {
    steps: AtomicUsize,
    fail: usize,
}
impl FaultIo {
    fn step(&self) -> std::io::Result<()> {
        let count = self.steps.fetch_add(1, Ordering::SeqCst) + 1;
        if count == self.fail {
            return Err(std::io::Error::other("injected boundary"));
        }
        Ok(())
    }
    fn call<T>(&self, f: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
        self.step()?;
        let result = f();
        self.step()?;
        result
    }
}
impl DurableIo for FaultIo {
    fn create_stage(&self, p: &Path) -> std::io::Result<File> {
        self.call(|| NativeIo.create_stage(p))
    }
    fn create_private_stage(&self, p: &Path) -> std::io::Result<std::fs::File> {
        self.call(|| NativeIo.create_private_stage(p))
    }
    fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
        self.call(|| NativeIo.create_private_directory(p))
    }
    fn open_append(&self, p: &Path) -> std::io::Result<File> {
        self.call(|| NativeIo.open_append(p))
    }
    fn truncate_file(&self, f: &File, l: u64) -> std::io::Result<()> {
        self.call(|| NativeIo.truncate_file(f, l))
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> std::io::Result<()> {
        self.call(|| NativeIo.write_stage(f, b))
    }
    fn sync_file(&self, f: &File) -> std::io::Result<()> {
        self.call(|| NativeIo.sync_file(f))
    }
    fn replace(&self, s: &Path, t: &Path) -> std::io::Result<()> {
        self.call(|| NativeIo.replace(s, t))
    }
    fn remove(&self, p: &Path) -> std::io::Result<()> {
        self.call(|| NativeIo.remove(p))
    }
    fn remove_directory(&self, p: &Path) -> std::io::Result<()> {
        self.call(|| NativeIo.remove_directory(p))
    }
    fn create_directory(&self, p: &Path) -> std::io::Result<()> {
        self.call(|| NativeIo.create_directory(p))
    }
    fn sync_directory(&self, p: &Path) -> std::io::Result<DirectorySync> {
        self.call(|| NativeIo.sync_directory(p))
    }
}
fn workflow(vault: &VaultFs, writer: &WriterPermit) -> Result<()> {
    let a = attempt();
    let store = RunStore::bootstrap(vault, writer, &vault_id(), &a.run_id)?;
    let guard = store.lock(Duration::ZERO, &mut || Ok(()))?;
    guard.append_journal(0, b"frame1")?;
    guard.append_journal(6, b"torn")?;
    guard.truncate_torn_tail(10, 6)?;
    guard.ensure_attempt_dir(&a)?;
    let body = guard.secure_replace(
        RunFile::Spool {
            attempt: &a,
            part: SpoolPart::Body,
        },
        ExpectedState::Absent,
        b"mock body",
    )?;
    let meta = guard.secure_replace(
        RunFile::Spool {
            attempt: &a,
            part: SpoolPart::Metadata,
        },
        ExpectedState::Absent,
        b"{}",
    )?;
    guard.secure_replace(RunFile::Checkpoint, ExpectedState::Absent, b"{}")?;
    guard.remove_spool_file(&a, SpoolPart::Body, &body)?;
    guard.remove_spool_file(&a, SpoolPart::Metadata, &meta)?;
    guard.remove_empty_attempt_dir(&a)
}
#[test]
fn operational_injected_before_after_durable_boundaries_keep_exact_payloads() {
    let (_temp, native, writer) = fixture();
    let counted = Arc::new(FaultIo {
        steps: AtomicUsize::new(0),
        fail: usize::MAX,
    });
    let fs = VaultFs::with_io(native.root().clone(), counted.clone());
    workflow(&fs, &writer).unwrap();
    let boundaries = counted.steps.load(Ordering::SeqCst);
    assert!(boundaries > 60);
    assert_eq!(boundaries % 2, 0);
    for fail in 1..=boundaries {
        let (temp, native, writer) = fixture();
        let injected = Arc::new(FaultIo {
            steps: AtomicUsize::new(0),
            fail,
        });
        let vault = VaultFs::with_io(native.root().clone(), injected);
        let marker = fs::read(temp.path().join("WIKI.md")).unwrap();
        let result = workflow(&vault, &writer);
        if result.is_ok() {
            assert!(
                !temp
                    .path()
                    .join(".wiki/state/requests/attempt_private")
                    .exists(),
                "fault{fail}"
            );
        }
        assert_eq!(fs::read(temp.path().join("WIKI.md")).unwrap(), marker);
        let base = temp.path().join(".wiki/state/jobs/run_private");
        if let Ok(bytes) = fs::read(base.join("journal.bin")) {
            assert!(
                [b"".as_slice(), b"frame1", b"frame1torn"].contains(&bytes.as_slice()),
                "fault{fail}"
            );
        }
        for (name, expected) in [
            ("response.bin", b"mock body".as_slice()),
            ("metadata.json", b"{}"),
        ] {
            if let Ok(bytes) = fs::read(
                temp.path()
                    .join(".wiki/state/requests/attempt_private")
                    .join(name),
            ) {
                assert_eq!(bytes, expected, "fault{fail}");
            }
        }
    }
    eprintln!(
        "operational durable native calls={}, before/after fault points={boundaries}",
        boundaries / 2
    );
}

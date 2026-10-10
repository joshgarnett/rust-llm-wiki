use super::{ExpectedState, VaultFs, VaultRoot, WriterPermit};
use crate::domain::{Blake3Hash, ErrorCode, VaultRelativePath};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Duration,
};

type Hook = Box<dyn FnOnce() + Send>;
static BEFORE_OPEN: OnceLock<Mutex<BTreeMap<PathBuf, Hook>>> = OnceLock::new();
pub(crate) struct BeforeOpenGuard(PathBuf);
impl Drop for BeforeOpenGuard {
    fn drop(&mut self) {
        BEFORE_OPEN
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .remove(&self.0);
    }
}
pub(crate) fn install_before_open(
    path: PathBuf,
    hook: impl FnOnce() + Send + 'static,
) -> BeforeOpenGuard {
    assert!(
        BEFORE_OPEN
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(path.clone(), Box::new(hook))
            .is_none()
    );
    BeforeOpenGuard(path)
}
pub(super) fn before_open(path: &Path) {
    let hook = BEFORE_OPEN
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(path);
    if let Some(hook) = hook {
        hook();
    }
}
fn fixture() -> (tempfile::TempDir, VaultFs) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    (temp, vault)
}

#[test]
fn bounded_guard_preserves_exact_bytes_and_refuses_oversize_before_allocation() {
    let (temp, vault) = fixture();
    let path = VaultRelativePath::new("page.md").unwrap();
    fs::write(temp.path().join("page.md"), "Café\n").unwrap();
    let bytes = super::fs::read_regular_bounded(vault.root(), &path, false, 6, "test-read")
        .unwrap()
        .unwrap();
    assert_eq!(bytes, "Café\n".as_bytes());
    assert_eq!(
        super::fs::read_regular_bounded(vault.root(), &path, false, 5, "test-read")
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(
        vault.read_before(&path).unwrap().unwrap().hash,
        Blake3Hash::digest(&bytes)
    );
}

#[test]
fn streamed_authority_hash_rereads_complete_bytes_and_observes_same_size_edit() {
    let (_temp, vault) = fixture();
    let path = VaultRelativePath::new("authority.json").unwrap();
    let target = vault.root().path().join(path.as_str());
    let bytes = vec![b'a'; 2 * 64 * 1024 + 17];
    fs::write(&target, &bytes).unwrap();
    crate::catalog::query_diagnostics::begin();
    for _ in 0..2 {
        let observed =
            super::fs::hash_regular_raw_bounded(vault.root(), &path, bytes.len(), "authority-test")
                .unwrap()
                .unwrap();
        assert_eq!(
            observed,
            (bytes.len() as u64, *blake3::hash(&bytes).as_bytes())
        );
    }
    let observations = crate::catalog::query_diagnostics::end();
    assert_eq!(observations.reads.len(), 2);
    assert!(
        observations
            .reads
            .iter()
            .all(|read| read.bytes == bytes.len())
    );
    let changed = vec![b'b'; bytes.len()];
    fs::write(&target, &changed).unwrap();
    assert_eq!(
        super::fs::hash_regular_raw_bounded(vault.root(), &path, changed.len(), "authority-test",)
            .unwrap()
            .unwrap(),
        (changed.len() as u64, *blake3::hash(&changed).as_bytes())
    );
    assert_eq!(
        super::fs::hash_regular_raw_bounded(
            vault.root(),
            &path,
            changed.len() - 1,
            "authority-test",
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
}

#[test]
fn generic_owner_read_keeps_existing_large_file_compatibility() {
    use std::io::{Seek, SeekFrom, Write};
    let (_temp, vault) = fixture();
    let path = VaultRelativePath::new("large-author-note.md").unwrap();
    let mut file = fs::File::create(vault.root().path().join(path.as_str())).unwrap();
    let length = 64 * 1024 * 1024 + 1;
    file.set_len(length).unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(b"z").unwrap();
    drop(file);
    let observed = vault.read_before(&path).unwrap().unwrap();
    assert_eq!(observed.bytes.len() as u64, length);
    assert_eq!(observed.bytes.last(), Some(&b'z'));
    assert_eq!(observed.hash, Blake3Hash::digest(&observed.bytes));
}

#[cfg(unix)]
#[test]
fn raced_fifo_after_regular_metadata_refuses_without_waiting_for_a_writer() {
    let (_temp, vault) = fixture();
    let target = vault.root().path().join("page.md");
    fs::write(&target, b"before").unwrap();
    let replaced = target.clone();
    BEFORE_OPEN
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(
            target,
            Box::new(move || {
                fs::remove_file(&replaced).unwrap();
                use std::os::unix::ffi::OsStrExt;
                let name = std::ffi::CString::new(replaced.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }),
        );
    let error = vault
        .read_before(&VaultRelativePath::new("page.md").unwrap())
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
}

#[cfg(unix)]
#[test]
fn raced_symlink_after_regular_metadata_does_not_read_external_bytes() {
    let (_temp, vault) = fixture();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("outside.md");
    fs::write(&secret, b"unrelated external bytes").unwrap();
    let target = vault.root().path().join("page.md");
    fs::write(&target, b"before").unwrap();
    let replaced = target.clone();
    BEFORE_OPEN
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(
            target,
            Box::new(move || {
                fs::remove_file(&replaced).unwrap();
                std::os::unix::fs::symlink(secret, replaced).unwrap();
            }),
        );
    assert!(
        vault
            .read_before(&VaultRelativePath::new("page.md").unwrap())
            .is_err()
    );
}

#[test]
fn prepared_stage_keeps_replacement_authority_on_writer_and_rejects_tampering() {
    let (_temp, vault) = fixture();
    let permit = WriterPermit::acquire(vault.root(), Duration::from_secs(1)).unwrap();
    let path = VaultRelativePath::new("page.md").unwrap();
    for tampered in [b"tampered".as_slice(), b"longer tampered bytes".as_slice()] {
        let stage = vault
            .prepare_stage(&path, &permit)
            .unwrap()
            .stage(b"new bytes")
            .unwrap();
        fs::write(stage.path(), tampered).unwrap();
        assert_eq!(
            vault
                .replace(stage, &ExpectedState::Absent, &permit)
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        assert!(vault.read_before(&path).unwrap().is_none());
    }
}

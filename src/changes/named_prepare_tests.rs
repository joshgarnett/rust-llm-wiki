//! Initial named retention only. Catalog dispatch must classify active/terminal
//! authority before calling this seam; these tests grant no application authority.
use super::{journal, prepare::NamedPreparation, types::*};
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordId, VaultRelativePath},
    sources::{CaptureAllocation, CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    storage::{self, StorageOptions},
    vault::{DirectorySync, DurableIo, ExpectedState, NativeIo, VaultFs, VaultRoot, WriterPermit},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

fn rel(value: impl Into<String>) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn identity() -> NamedChangeIdentity {
    NamedChangeIdentity {
        change_id: id("change_named-retention"),
        created_at: "2024-02-29T09:08:07.1200+00:00".into(),
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    engine: ChangeEngine,
    writer: WriterPermit,
}
impl Fixture {
    fn new(migrated: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::copy(
            crate::test_paths::fixture(
                env!("CARGO_MANIFEST_DIR"),
                "tests/fixtures/bootstrap/vault/WIKI.md",
            ),
            temp.path().join("WIKI.md"),
        )
        .unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        if migrated {
            // Exercise real migration/activation, never a hand-edited schema marker.
            storage::cleanup(&vault, &writer, &StorageOptions::default()).unwrap();
        }
        assert_eq!(storage::layout::active(vault.root()).unwrap(), migrated);
        Self {
            temp,
            engine: ChangeEngine::new(vault).unwrap(),
            writer,
        }
    }
    fn draft(&self, members: usize) -> ChangeDraft {
        let store = SourceStore::new(self.engine.fs().clone());
        let mut combined = ChangeDraft {
            title: "Named capture group".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: vec![],
            operations: vec![],
        };
        for member in 0..members {
            let allocation = CaptureAllocation {
                source_id: id(&format!("source_named-{member}")),
                revision_id: id(&format!("revision_named-{member}")),
                captured_at: "2024-02-29T09:08:07.1200+00:00".into(),
            };
            let plan = store
                .plan_capture_named(
                    CaptureRequest {
                        title: format!("Capture {member}: 茶"),
                        origin_kind: SourceOrigin::LocalFile,
                        origin: format!("/fixture/input-{member}.txt"),
                        original: format!("original bytes {member}\n").into_bytes(),
                        extraction: ExtractionInput::Supplied {
                            extractor: "named-test-v1".into(),
                            fingerprint: Blake3Hash::digest(b"named-test-v1"),
                            content: format!("extracted evidence {member}\n").into_bytes(),
                        },
                        media_type: Some("text/plain".into()),
                    },
                    &allocation,
                )
                .unwrap();
            let draft = plan.draft.unwrap();
            for (name, value) in draft.allocated_ids {
                assert!(
                    combined
                        .allocated_ids
                        .insert(format!("{member}:{name}"), value)
                        .is_none()
                );
            }
            combined.operations.extend(draft.operations);
        }
        combined
    }
    fn seal(&self, members: usize) -> NamedPreparation {
        self.engine
            .seal_named(&self.writer, identity(), self.draft(members))
            .unwrap()
    }
    fn physical(&self, path: &VaultRelativePath) -> PathBuf {
        self.engine.fs().root().resolve(path).unwrap()
    }
    fn reopen(self) -> Self {
        // Drop the actual writer and engine, then reopen without the fault adapter.
        let Self {
            temp,
            engine,
            writer,
        } = self;
        drop(engine);
        drop(writer);
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let writer = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        Self {
            temp,
            engine: ChangeEngine::new(VaultFs::new(root)).unwrap(),
            writer,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory,
    File(Vec<u8>, SystemTime),
    Link(PathBuf),
}
// Test-only preservation oracle on a disposable fixture, not importer discovery.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn visit(root: &Path, directory: &Path, result: &mut BTreeMap<PathBuf, Entry>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if metadata.file_type().is_symlink() {
                result.insert(relative, Entry::Link(fs::read_link(&path).unwrap()));
            } else if metadata.is_dir() {
                result.insert(relative, Entry::Directory);
                visit(root, &path, result);
            } else {
                result.insert(
                    relative,
                    Entry::File(fs::read(&path).unwrap(), metadata.modified().unwrap()),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}
fn retained_paths(fixture: &Fixture, sealed: &NamedPreparation) -> BTreeSet<PathBuf> {
    sealed
        .manifest()
        .operations
        .iter()
        .flat_map(|op| [&op.before_payload, &op.after_payload])
        .flatten()
        .map(|payload| fixture.physical(&payload.path))
        .collect()
}
fn note_path(fixture: &Fixture) -> PathBuf {
    fixture.physical(&rel(format!("changes/{}/change.md", identity().change_id)))
}
fn assert_prepared(fixture: &Fixture, sealed: &NamedPreparation, change: &ChangeInspection) {
    assert_eq!(change.prepared, sealed.prepared());
    assert_eq!(&change.manifest, sealed.manifest());
    assert_eq!(change.status, ChangeStatus::Prepared);
    assert_eq!(change.journal.frames.len(), 1);
    assert!(matches!(
        change.journal.frames[0].event,
        ChangeEvent::Prepared
    ));
    for operation in &sealed.manifest().operations {
        assert!(
            !fixture.physical(&operation.target).exists(),
            "preparation published canonical bytes"
        );
        let payload = operation.after_payload.as_ref().unwrap();
        let bytes = fs::read(fixture.physical(&payload.path)).unwrap();
        assert_eq!(Blake3Hash::digest(&bytes), payload.hash);
        assert_eq!(bytes.len() as u64, payload.byte_len);
    }
}

#[test]
fn named_seal_is_deterministic_read_only_for_one_four_and_eight_members() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated);
        for members in [1, 4, 8] {
            let before = snapshot(fixture.temp.path());
            let left = fixture.seal(members);
            let right = fixture.seal(members);
            assert_eq!(left.manifest(), right.manifest());
            assert_eq!(left.prepared(), right.prepared());
            assert_eq!(left.manifest().created_at, identity().created_at);
            assert_eq!(left.manifest().version, if migrated { 2 } else { 1 });
            assert_eq!(left.manifest().operations.len(), members * 4);
            assert_eq!(left.manifest().allocated_ids.len(), members * 2);
            assert_eq!(
                snapshot(fixture.temp.path()),
                before,
                "seal retained an allocation"
            );
        }
    }
}

#[test]
fn malformed_named_admission_refuses_before_any_retained_allocation() {
    for variant in 0..8 {
        let fixture = Fixture::new(false);
        let mut draft = fixture.draft(1);
        let mut named = identity();
        match variant {
            0 => named.created_at = "not a timestamp".into(),
            1 => draft.operations.push(draft.operations[0].clone()),
            2 => draft.operations[0].target = rel(format!("{}/child", draft.operations[1].target)),
            3 => {
                draft.origin = Some(ChangeOrigin {
                    operation: OriginOperation::GraphImport,
                    packet_id: id("packet_named"),
                    response_hash: Blake3Hash::digest(b"graph"),
                })
            }
            4 => draft.inverse_of = Some(id("change_inverse")),
            5 => draft.operations = (0..33).map(|_| draft.operations[0].clone()).collect(),
            6 => {
                draft.allocated_ids = (0..17)
                    .map(|n| (n.to_string(), id(&format!("allocation_{n}"))))
                    .collect();
            }
            7 => draft.operations[0].expected = ExpectedState::Hash(Blake3Hash::digest(b"foreign")),
            _ => unreachable!(),
        }
        let before = snapshot(fixture.temp.path());
        assert!(
            fixture
                .engine
                .seal_named(&fixture.writer, named, draft)
                .is_err(),
            "variant {variant}"
        );
        assert_eq!(
            snapshot(fixture.temp.path()),
            before,
            "variant {variant} retained files"
        );
    }
}

#[test]
fn altered_frozen_intent_refuses_without_retention() {
    let fixture = Fixture::new(false);
    let sealed = fixture.seal(1);
    for variant in 0..7 {
        let mut expected = sealed.manifest().clone();
        match variant {
            0 => expected.title.push_str(" changed"),
            1 => expected.created_at = "2024-02-29T09:08:08Z".into(),
            2 => expected.change_id = id("change_other"),
            3 => expected.vault_id = id("vault_other"),
            4 => {
                expected.operations[0].after_payload.as_mut().unwrap().hash =
                    Blake3Hash::digest(b"other")
            }
            5 => {
                expected
                    .allocated_ids
                    .insert("extra".into(), id("source_other"));
            }
            6 => expected.version = 2,
            _ => unreachable!(),
        }
        let before = snapshot(fixture.temp.path());
        assert!(
            fixture
                .engine
                .prepare_named(&fixture.writer, &expected, &sealed)
                .is_err()
        );
        assert_eq!(snapshot(fixture.temp.path()), before, "variant {variant}");
    }
}

#[derive(Clone, Copy, Debug)]
enum Cut {
    AfterFile(usize),
    BeforeNote,
    AfterNote,
    TornPrepared,
    CompletePrepared,
    AfterPrefixMkdir,
}
struct RetentionIo {
    cut: Option<Cut>,
    payloads: BTreeSet<PathBuf>,
    note: PathBuf,
    prefix: PathBuf,
    journal: PathBuf,
    files: AtomicUsize,
    journal_open: AtomicBool,
    fired: AtomicBool,
    replaced: Mutex<Vec<PathBuf>>,
}
impl RetentionIo {
    fn new(fixture: &Fixture, sealed: &NamedPreparation, cut: Option<Cut>) -> Self {
        Self {
            cut,
            payloads: retained_paths(fixture, sealed),
            note: note_path(fixture),
            prefix: fixture.physical(&rel(format!("changes/{}", identity().change_id))),
            journal: fixture.physical(&journal::journal_path(&identity().change_id).unwrap()),
            files: AtomicUsize::new(0),
            journal_open: AtomicBool::new(false),
            fired: AtomicBool::new(false),
            replaced: Mutex::new(vec![]),
        }
    }
    fn fail(&self) -> std::io::Result<()> {
        self.fired.store(true, Ordering::SeqCst);
        Err(std::io::Error::other(
            "injected named retention returned error",
        ))
    }
}
impl DurableIo for RetentionIo {
    fn create_stage(&self, p: &Path) -> std::io::Result<File> {
        NativeIo.create_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> std::io::Result<File> {
        NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
        NativeIo.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> std::io::Result<File> {
        if p == self.journal {
            self.journal_open.store(true, Ordering::SeqCst);
        }
        NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> std::io::Result<()> {
        NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, bytes: &[u8]) -> std::io::Result<()> {
        if self.journal_open.load(Ordering::SeqCst) && bytes.starts_with(b"LWJNL001") {
            if matches!(self.cut, Some(Cut::TornPrepared)) {
                NativeIo.write_stage(f, &bytes[..19])?;
                return self.fail();
            }
            if matches!(self.cut, Some(Cut::CompletePrepared)) {
                NativeIo.write_stage(f, bytes)?;
                return self.fail();
            }
        }
        NativeIo.write_stage(f, bytes)
    }
    fn sync_file(&self, f: &File) -> std::io::Result<()> {
        NativeIo.sync_file(f)
    }
    fn replace(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        if to == self.note && matches!(self.cut, Some(Cut::BeforeNote)) {
            return self.fail();
        }
        NativeIo.replace(from, to)?;
        self.replaced.lock().unwrap().push(to.to_owned());
        if self.payloads.contains(to) {
            let number = self.files.fetch_add(1, Ordering::SeqCst) + 1;
            if matches!(self.cut, Some(Cut::AfterFile(n)) if n == number) {
                return self.fail();
            }
        }
        if to == self.note && matches!(self.cut, Some(Cut::AfterNote)) {
            return self.fail();
        }
        Ok(())
    }
    fn remove(&self, p: &Path) -> std::io::Result<()> {
        NativeIo.remove(p)
    }
    fn create_directory(&self, p: &Path) -> std::io::Result<()> {
        NativeIo.create_directory(p)?;
        if p == self.prefix && matches!(self.cut, Some(Cut::AfterPrefixMkdir)) {
            return self.fail();
        }
        Ok(())
    }
    fn sync_directory(&self, p: &Path) -> std::io::Result<DirectorySync> {
        NativeIo.sync_directory(p)
    }
}
fn attach(fixture: &mut Fixture, io: Arc<RetentionIo>) {
    fixture.engine =
        ChangeEngine::new(VaultFs::with_io(fixture.engine.fs().root().clone(), io)).unwrap();
}

#[test]
fn fresh_named_retention_writes_payloads_before_note_and_one_prepared_frame() {
    for migrated in [false, true] {
        let mut fixture = Fixture::new(migrated);
        let sealed = fixture.seal(4);
        let io = Arc::new(RetentionIo::new(&fixture, &sealed, None));
        attach(&mut fixture, io.clone());
        let change = fixture
            .engine
            .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
            .unwrap();
        assert_prepared(&fixture, &sealed, &change);
        let replaced = io.replaced.lock().unwrap();
        let note_index = replaced.iter().position(|path| path == &io.note).unwrap();
        assert!(
            io.payloads.iter().all(|payload| replaced
                .iter()
                .position(|path| path == payload)
                .unwrap()
                < note_index)
        );
        assert_eq!(replaced.iter().filter(|path| *path == &io.note).count(), 1);
    }
}

#[cfg(unix)]
#[test]
fn real_payload_and_note_returned_errors_reopen_to_exact_partial_resume() {
    for migrated in [false, true] {
        for cut in [
            Cut::AfterFile(1),
            Cut::AfterFile(2),
            Cut::BeforeNote,
            Cut::AfterNote,
        ] {
            let mut fixture = Fixture::new(migrated);
            let sealed = fixture.seal(1);
            let io = Arc::new(RetentionIo::new(&fixture, &sealed, Some(cut)));
            attach(&mut fixture, io.clone());
            assert!(
                fixture
                    .engine
                    .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                    .is_err(),
                "{migrated}/{cut:?}"
            );
            assert!(io.fired.load(Ordering::SeqCst));
            let retained_before: BTreeMap<_, _> = retained_paths(&fixture, &sealed)
                .into_iter()
                .filter(|path| path.exists())
                .map(|path| {
                    let value = (
                        fs::read(&path).unwrap(),
                        fs::metadata(&path).unwrap().modified().unwrap(),
                    );
                    (path, value)
                })
                .collect();
            assert!(!retained_before.is_empty());
            fixture = fixture.reopen();
            let resealed = fixture.seal(1);
            assert_eq!(resealed.prepared(), sealed.prepared());
            if migrated && matches!(cut, Cut::BeforeNote) {
                // Global objects do not mark the now-empty Change prefix.
                // Catalog replacement must preserve this ambiguous attempt.
                let before = snapshot(fixture.temp.path());
                let error = fixture
                    .engine
                    .prepare_named(&fixture.writer, sealed.manifest(), &resealed)
                    .err()
                    .unwrap();
                assert_eq!(error.details["named_attempt_state"], "ambiguous_prefix");
                assert_eq!(snapshot(fixture.temp.path()), before);
                continue;
            }
            let change = fixture
                .engine
                .prepare_named(&fixture.writer, sealed.manifest(), &resealed)
                .unwrap();
            assert_prepared(&fixture, &resealed, &change);
            for (path, (bytes, modified)) in retained_before {
                assert_eq!(fs::read(&path).unwrap(), bytes);
                assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn returned_torn_or_complete_initial_prepared_append_reopens_once() {
    for migrated in [false, true] {
        for cut in [Cut::TornPrepared, Cut::CompletePrepared] {
            let mut fixture = Fixture::new(migrated);
            let sealed = fixture.seal(1);
            let io = Arc::new(RetentionIo::new(&fixture, &sealed, Some(cut)));
            attach(&mut fixture, io.clone());
            assert!(
                fixture
                    .engine
                    .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                    .is_err()
            );
            assert!(io.fired.load(Ordering::SeqCst));
            let state = journal::load_journal(
                fixture.engine.fs(),
                sealed.manifest(),
                &sealed.prepared().manifest_hash,
            )
            .unwrap();
            assert_eq!(state.torn_tail, matches!(cut, Cut::TornPrepared));
            assert_eq!(
                state.frames.len(),
                usize::from(matches!(cut, Cut::CompletePrepared))
            );
            fixture = fixture.reopen();
            let change = fixture
                .engine
                .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                .unwrap();
            assert_prepared(&fixture, &sealed, &change);
            assert!(!change.journal.torn_tail);
            let before = snapshot(fixture.temp.path());
            let retry = fixture
                .engine
                .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                .unwrap();
            assert_prepared(&fixture, &sealed, &retry);
            assert_eq!(snapshot(fixture.temp.path()), before);
        }
    }
}

#[cfg(unix)]
#[test]
fn prepared_exact_retry_preserves_all_immutable_bytes_and_mtimes() {
    for migrated in [false, true] {
        let mut fixture = Fixture::new(migrated);
        let sealed = fixture.seal(1);
        fixture
            .engine
            .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
            .unwrap();
        fixture = fixture.reopen();
        let before = snapshot(fixture.temp.path());
        let change = fixture
            .engine
            .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
            .unwrap();
        assert_prepared(&fixture, &sealed, &change);
        assert_eq!(snapshot(fixture.temp.path()), before);
    }
}

#[test]
fn empty_unmarked_and_returned_mkdir_prefixes_are_ambiguous_and_preserved() {
    for variant in 0..4 {
        let mut fixture = Fixture::new(false);
        let sealed = fixture.seal(1);
        let prefix = fixture.physical(&rel(format!("changes/{}", identity().change_id)));
        if variant == 3 {
            let io = Arc::new(RetentionIo::new(
                &fixture,
                &sealed,
                Some(Cut::AfterPrefixMkdir),
            ));
            attach(&mut fixture, io.clone());
            assert!(
                fixture
                    .engine
                    .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                    .is_err()
            );
            assert!(io.fired.load(Ordering::SeqCst));
            fixture = fixture.reopen();
        } else {
            fs::create_dir_all(&prefix).unwrap();
            if variant == 1 {
                fs::write(prefix.join("unfamiliar.txt"), b"do not adopt").unwrap();
            }
            if variant == 2 {
                fs::write(prefix.join(".stage-unmarked"), b"stage bytes").unwrap();
            }
        }
        let before = snapshot(fixture.temp.path());
        let error = fixture
            .engine
            .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert_eq!(error.details["named_attempt_state"], "ambiguous_prefix");
        assert_eq!(error.details["change_id"], identity().change_id.as_str());
        assert_eq!(snapshot(fixture.temp.path()), before, "variant {variant}");
    }
}

#[test]
fn changed_partial_payload_or_note_and_foreign_types_preserve_every_byte() {
    for migrated in [false, true] {
        for variant in 0..4 {
            let mut fixture = Fixture::new(migrated);
            let sealed = fixture.seal(1);
            let io = Arc::new(RetentionIo::new(&fixture, &sealed, Some(Cut::AfterNote)));
            attach(&mut fixture, io);
            assert!(
                fixture
                    .engine
                    .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                    .is_err()
            );
            fixture = fixture.reopen();
            let payload = retained_paths(&fixture, &sealed)
                .into_iter()
                .next()
                .unwrap();
            match variant {
                0 => fs::write(&payload, b"unfamiliar payload").unwrap(),
                1 => fs::write(note_path(&fixture), b"unfamiliar note").unwrap(),
                2 => {
                    fs::remove_file(&payload).unwrap();
                    fs::create_dir(&payload).unwrap();
                }
                3 => {
                    fs::write(
                        note_path(&fixture)
                            .parent()
                            .unwrap()
                            .join("foreign-control.json"),
                        b"foreign",
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            let before = snapshot(fixture.temp.path());
            assert!(
                fixture
                    .engine
                    .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                    .is_err(),
                "variant {variant}"
            );
            assert_eq!(snapshot(fixture.temp.path()), before, "variant {variant}");
        }
    }
}

#[cfg(unix)]
#[test]
fn partial_symlink_and_hardlink_controls_are_refused_and_preserved() {
    use std::os::unix::fs::symlink;
    for migrated in [false, true] {
        for hardlink in [false, true] {
            let mut fixture = Fixture::new(migrated);
            let sealed = fixture.seal(1);
            let io = Arc::new(RetentionIo::new(&fixture, &sealed, Some(Cut::AfterNote)));
            attach(&mut fixture, io);
            assert!(
                fixture
                    .engine
                    .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                    .is_err()
            );
            fixture = fixture.reopen();
            let payload = retained_paths(&fixture, &sealed)
                .into_iter()
                .next()
                .unwrap();
            let foreign = fixture.temp.path().join("foreign-original");
            fs::copy(&payload, &foreign).unwrap();
            fs::remove_file(&payload).unwrap();
            if hardlink {
                fs::hard_link(&foreign, &payload).unwrap();
            } else {
                symlink(&foreign, &payload).unwrap();
            }
            let before = snapshot(fixture.temp.path());
            assert!(
                fixture
                    .engine
                    .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                    .is_err()
            );
            assert_eq!(snapshot(fixture.temp.path()), before);
        }
    }
}

#[cfg(unix)]
#[test]
fn prepared_hardlinked_payload_note_and_journal_are_refused_without_mutation() {
    for migrated in [false, true] {
        for control in 0..3 {
            let fixture = Fixture::new(migrated);
            let sealed = fixture.seal(1);
            fixture
                .engine
                .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                .unwrap();
            let target = match control {
                0 => retained_paths(&fixture, &sealed)
                    .into_iter()
                    .next()
                    .unwrap(),
                1 => note_path(&fixture),
                2 => fixture.physical(&journal::journal_path(&identity().change_id).unwrap()),
                _ => unreachable!(),
            };
            let foreign = fixture.temp.path().join("foreign-prepared-control");
            fs::copy(&target, &foreign).unwrap();
            fs::remove_file(&target).unwrap();
            fs::hard_link(&foreign, &target).unwrap();
            let before = snapshot(fixture.temp.path());
            assert!(
                fixture
                    .engine
                    .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                    .is_err(),
                "{migrated}/control {control}"
            );
            assert_eq!(snapshot(fixture.temp.path()), before);
        }
    }
}

#[test]
fn oversized_foreign_prefix_is_rejected_without_retention() {
    let fixture = Fixture::new(false);
    let sealed = fixture.seal(1);
    let payload = retained_paths(&fixture, &sealed)
        .into_iter()
        .next()
        .unwrap();
    fs::create_dir_all(payload.parent().unwrap()).unwrap();
    // More than 128 entries are unfamiliar. Refusal may encounter an unknown
    // entry first; this does not claim to measure the exact inventory ceiling.
    for operation in &sealed.manifest().operations {
        let payload = operation.after_payload.as_ref().unwrap();
        fs::create_dir_all(fixture.physical(&payload.path).parent().unwrap()).unwrap();
    }
    let prefix = fixture.physical(&rel(format!("changes/{}", identity().change_id)));
    for n in 0..130 {
        fs::write(prefix.join(format!("foreign-{n:03}")), b"preserve").unwrap();
    }
    let before = snapshot(fixture.temp.path());
    assert!(
        fixture
            .engine
            .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
            .is_err()
    );
    assert_eq!(snapshot(fixture.temp.path()), before);
}

#[test]
fn known_named_dispatch_does_not_open_unrelated_invalid_history() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated);
        let unrelated = fixture.physical(&rel("changes/change_unrelated/change.md"));
        fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
        fs::write(&unrelated, b"not a valid manifest or origin").unwrap();
        let unrelated_journal =
            fixture.physical(&journal::journal_path(&id("change_unrelated")).unwrap());
        fs::create_dir_all(unrelated_journal.parent().unwrap()).unwrap();
        fs::write(&unrelated_journal, b"corrupt framing in unrelated history").unwrap();
        let sealed = fixture.seal(1);
        let change = fixture
            .engine
            .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
            .unwrap();
        assert_prepared(&fixture, &sealed, &change);
        assert_eq!(
            fs::read(unrelated).unwrap(),
            b"not a valid manifest or origin"
        );
        assert_eq!(
            fs::read(unrelated_journal).unwrap(),
            b"corrupt framing in unrelated history"
        );
        // This only proves unrelated content was not decoded. Generic portable
        // sibling enumeration remains separate from catalog history discovery.
    }
}

#[test]
fn applying_or_torn_later_journal_requires_dispatch_and_preserves_retention() {
    for variant in 0..3 {
        let fixture = Fixture::new(false);
        let sealed = fixture.seal(1);
        fixture
            .engine
            .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
            .unwrap();
        let prepared = sealed.prepared();
        let journal_path = journal::journal_path(&prepared.change_id).unwrap();
        match variant {
            0 => {
                journal::append_event(
                    fixture.engine.fs(),
                    &fixture.writer,
                    sealed.manifest(),
                    &prepared.manifest_hash,
                    ChangeEvent::Applying,
                )
                .unwrap();
            }
            1 => {
                let frame = JournalFrame {
                    version: 1,
                    sequence: 1,
                    change_id: prepared.change_id.clone(),
                    manifest_hash: prepared.manifest_hash.clone(),
                    event: ChangeEvent::Applying,
                };
                let bytes = journal::encode_frame(&frame).unwrap();
                fixture
                    .engine
                    .fs()
                    .append_synced(&journal_path, &bytes[..19], &fixture.writer)
                    .unwrap();
            }
            2 => {
                // Deliberately corrupt a complete journal with a duplicated frame.
                let bytes = fs::read(fixture.physical(&journal_path)).unwrap();
                fixture
                    .engine
                    .fs()
                    .append_synced(&journal_path, &bytes, &fixture.writer)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = snapshot(fixture.temp.path());
        assert!(
            fixture
                .engine
                .prepare_named(&fixture.writer, sealed.manifest(), &sealed)
                .is_err(),
            "variant {variant}"
        );
        assert_eq!(snapshot(fixture.temp.path()), before, "variant {variant}");
    }
}

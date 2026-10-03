//! Original read authorization survives preparation, interruption and recovery.
use lwiki::{
    changes::*,
    domain::{Blake3Hash, ErrorCode, ReadSnapshot, Result, VaultRelativePath},
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use std::{
    collections::BTreeMap,
    fs,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

fn rel(path: &str) -> VaultRelativePath {
    VaultRelativePath::new(path).unwrap()
}
fn fixture() -> (tempfile::TempDir, VaultRoot, ChangeEngine) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    fs::write(temp.path().join("guard.bin"), [0, 255, 128]).unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    (temp, root, engine)
}
fn draft() -> ChangeDraft {
    ChangeDraft {
        title: "guarded derived write".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![ReadDependency {
            path: rel("guard.bin"),
            expected: ExpectedState::Hash(Blake3Hash::digest([0, 255, 128])),
        }],
        operations: vec![ExpectedWrite {
            target: rel("new.md"),
            expected: ExpectedState::Absent,
            proposed: Some(b"new".to_vec()),
            apply_after: vec![],
        }],
    }
}
#[derive(Default)]
struct Validator {
    edit_guard: bool,
    calls: AtomicUsize,
}
impl GraphValidator for Validator {
    fn validate(&self, fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph> {
        if self.edit_guard && self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            std::fs::write(
                fs.root().path().join("guard.bin"),
                b"changed during validation",
            )
            .unwrap();
        }
        let mut projected: BTreeMap<_, _> = input
            .documents
            .iter()
            .map(|d| (d.path.clone(), d.bytes.clone()))
            .collect();
        for target in &input.overlay {
            match &target.bytes {
                Some(bytes) => {
                    projected.insert(target.path.clone(), bytes.clone());
                }
                None => {
                    projected.remove(&target.path);
                }
            }
        }
        Ok(ValidatedGraph {
            parser_fingerprint: Blake3Hash::digest(b"read-guard-test-v1"),
            control_manifest: Blake3Hash::digest(serde_json::to_vec(&projected).unwrap()),
            dependencies: vec![],
        })
    }
}
#[derive(Default)]
struct Publisher {
    calls: AtomicUsize,
    edit_guard: bool,
}
impl PublicationBackend for Publisher {
    fn check_available(&self) -> Result<()> {
        Ok(())
    }
    fn publish(
        &self,
        fs: &VaultFs,
        permit: &PublicationPermit<'_>,
        _input: &ValidationInput,
    ) -> Result<ReadSnapshot> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.edit_guard {
            std::fs::write(
                fs.root().path().join("guard.bin"),
                b"changed during publication",
            )
            .unwrap();
        }
        Ok(ReadSnapshot::canonical(
            1,
            permit.graph().parser_fingerprint.clone(),
            permit.graph().control_manifest.clone(),
        ))
    }
}

#[test]
fn original_guards_fail_before_preparation_or_activation() {
    for when in ["before_prepare", "after_prepare", "in_validator"] {
        let (_temp, root, engine) = fixture();
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        if when == "before_prepare" {
            fs::write(root.path().join("guard.bin"), b"foreign").unwrap();
            assert_eq!(
                engine.prepare(&permit, draft()).unwrap_err().code,
                ErrorCode::ContentConflict
            );
            assert!(!root.path().join("changes").exists());
            continue;
        }
        let prepared = engine.prepare(&permit, draft()).unwrap().prepared;
        if when == "after_prepare" {
            fs::write(root.path().join("guard.bin"), b"foreign").unwrap();
        }
        let validator = Validator {
            edit_guard: when == "in_validator",
            calls: AtomicUsize::new(0),
        };
        let publisher = Publisher::default();
        assert_eq!(
            engine
                .apply(&permit, &prepared, &validator, &publisher)
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        assert_eq!(
            engine.inspect(&prepared.change_id).unwrap().status,
            ChangeStatus::Prepared
        );
        assert!(!root.path().join("new.md").exists());
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
        // Stale read authorization cannot prevent safe bookkeeping abort.
        fs::remove_file(root.path().join("guard.bin")).unwrap();
        fs::create_dir(root.path().join("guard.bin")).unwrap();
        engine.abort(&permit, &prepared).unwrap();
    }
}

#[test]
fn no_op_read_guard_survives_but_matching_write_guard_normalizes() {
    let (_temp, root, engine) = fixture();
    let mut request = draft();
    request.operations.push(ExpectedWrite {
        target: rel("guard.bin"),
        expected: request.read_preconditions[0].expected.clone(),
        proposed: Some(vec![0, 255, 128]),
        apply_after: vec![],
    });
    let plan = engine.plan(&request).unwrap();
    assert_eq!(plan.operations.len(), 1);
    assert_eq!(plan.read_preconditions.len(), 1);
    request.operations[1].proposed = Some(b"new binary".to_vec());
    let plan = engine.plan(&request).unwrap();
    assert_eq!(plan.operations.len(), 2);
    assert!(plan.read_preconditions.is_empty());
    request.read_preconditions[0].expected = ExpectedState::Absent;
    assert_eq!(
        engine.plan(&request).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        fs::read(root.path().join("guard.bin")).unwrap(),
        vec![0, 255, 128]
    );
}

#[test]
fn absent_guard_and_duplicate_paths_fail_closed() {
    let (_temp, root, engine) = fixture();
    let mut request = draft();
    request.read_preconditions = vec![ReadDependency {
        path: rel("absent.bin"),
        expected: ExpectedState::Absent,
    }];
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let prepared = engine.prepare(&permit, request).unwrap().prepared;
    fs::write(root.path().join("absent.bin"), b"external").unwrap();
    assert_eq!(
        engine
            .apply(
                &permit,
                &prepared,
                &Validator::default(),
                &Publisher::default()
            )
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert!(!root.path().join("new.md").exists());
    let mut duplicated = draft();
    duplicated
        .read_preconditions
        .push(duplicated.read_preconditions[0].clone());
    assert_eq!(
        engine.plan(&duplicated).unwrap_err().code,
        ErrorCode::RecordInvalid
    );
}

#[test]
fn interrupted_apply_never_refreshes_changed_original_read_authorization() {
    let (_temp, root, engine) = fixture();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let inspection = engine.prepare(&permit, draft()).unwrap();
    journal::append_event(
        engine.fs(),
        &permit,
        &inspection.manifest,
        &inspection.prepared.manifest_hash,
        ChangeEvent::Applying,
    )
    .unwrap();
    journal::append_event(
        engine.fs(),
        &permit,
        &inspection.manifest,
        &inspection.prepared.manifest_hash,
        ChangeEvent::Intent { op: 0 },
    )
    .unwrap();
    fs::write(root.path().join("new.md"), b"new").unwrap();
    fs::write(root.path().join("guard.bin"), b"unfamiliar dependency").unwrap();
    let publisher = Publisher::default();
    assert_eq!(
        engine
            .recover(&permit, &Validator::default(), &publisher)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        engine
            .inspect(&inspection.prepared.change_id)
            .unwrap()
            .status,
        ChangeStatus::Conflict
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read(root.path().join("new.md")).unwrap(), b"new");
    assert_eq!(
        fs::read(root.path().join("guard.bin")).unwrap(),
        b"unfamiliar dependency"
    );
}

#[test]
fn terminal_history_and_legacy_manifest_encoding_ignore_later_guard_edits() {
    let (_temp, root, engine) = fixture();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let prepared = engine.prepare(&permit, draft()).unwrap().prepared;
    engine
        .apply(
            &permit,
            &prepared,
            &Validator::default(),
            &Publisher::default(),
        )
        .unwrap();
    fs::remove_file(root.path().join("guard.bin")).unwrap();
    fs::create_dir(root.path().join("guard.bin")).unwrap();
    assert_eq!(
        engine
            .apply(
                &permit,
                &prepared,
                &Validator::default(),
                &Publisher::default()
            )
            .unwrap()
            .status,
        ChangeStatus::Committed
    );
    engine
        .recover(&permit, &Validator::default(), &Publisher::default())
        .unwrap();
    let mut legacy = draft();
    legacy.read_preconditions.clear();
    legacy.operations = vec![ExpectedWrite {
        target: rel("legacy.md"),
        expected: ExpectedState::Absent,
        proposed: Some(b"legacy".to_vec()),
        apply_after: vec![],
    }];
    let inspection = engine.prepare(&permit, legacy).unwrap();
    let value = serde_json::to_value(&inspection.manifest).unwrap();
    assert!(value.get("read_preconditions").is_none());
    let restored: ChangeManifest = serde_json::from_value(value).unwrap();
    assert!(restored.read_preconditions.is_empty());
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/change-v1.json")).unwrap();
    let guard_value =
        serde_json::to_value(engine.inspect(&prepared.change_id).unwrap().manifest).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&guard_value)
        .unwrap();
}

#[test]
fn publication_dependency_edit_denies_completion_and_preserves_bytes() {
    let (_temp, root, engine) = fixture();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let prepared = engine.prepare(&permit, draft()).unwrap().prepared;
    let publisher = Publisher {
        calls: AtomicUsize::new(0),
        edit_guard: true,
    };
    assert_eq!(
        engine
            .apply(&permit, &prepared, &Validator::default(), &publisher)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        engine.inspect(&prepared.change_id).unwrap().status,
        ChangeStatus::Conflict
    );
    assert_eq!(
        fs::read(root.path().join("guard.bin")).unwrap(),
        b"changed during publication"
    );
    assert_eq!(fs::read(root.path().join("new.md")).unwrap(), b"new");
}

#[test]
fn origin_retry_retains_original_guard_without_rebinding_or_allocating() {
    let (_temp, root, engine) = fixture();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let origin = ChangeOrigin {
        operation: OriginOperation::GraphImport,
        packet_id: lwiki::domain::RecordId::new("packet_guard").unwrap(),
        response_hash: Blake3Hash::digest(b"response"),
    };
    let first = engine
        .prepare_or_reuse(
            &permit,
            origin.clone(),
            OriginPolicy::ReuseOrConflict,
            || Ok(draft()),
        )
        .unwrap();
    fs::write(
        root.path().join("guard.bin"),
        b"changed after original proposal",
    )
    .unwrap();
    let reused = engine
        .prepare_or_reuse(&permit, origin, OriginPolicy::ReuseOrConflict, || {
            panic!("retry cannot refresh authorization")
        })
        .unwrap();
    assert!(reused.reused);
    assert_eq!(first.change.prepared, reused.change.prepared);
    assert_eq!(
        first.change.manifest.read_preconditions,
        reused.change.manifest.read_preconditions
    );
    assert_eq!(
        engine
            .apply(
                &permit,
                &reused.change.prepared,
                &Validator::default(),
                &Publisher::default()
            )
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn read_write_ancestor_and_portable_case_collisions_reject_before_allocation() {
    for (read, write) in [("branch/missing.bin", "branch"), ("New.md", "new.md")] {
        let (_temp, root, engine) = fixture();
        let mut request = draft();
        request.read_preconditions = vec![ReadDependency {
            path: rel(read),
            expected: ExpectedState::Absent,
        }];
        request.operations[0].target = rel(write);
        assert_eq!(
            engine.plan(&request).unwrap_err().code,
            ErrorCode::RecordInvalid
        );
        let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        assert_eq!(
            engine.prepare(&permit, request).unwrap_err().code,
            ErrorCode::RecordInvalid
        );
        assert!(!root.path().join("changes").exists());
        assert!(!root.path().join(write).exists());
    }
}

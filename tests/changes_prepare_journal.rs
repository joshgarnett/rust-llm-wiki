use lwiki::{
    changes::{
        journal::{self, decode_journal, encode_frame},
        prepare::manifest_path,
        *,
    },
    domain::{Blake3Hash, ReadSnapshot, RecordId, VaultRelativePath},
    vault::{
        ExpectedState, VaultFs, VaultRoot, WriterPermit,
        fs::{DirectorySync, DurableIo, NativeIo},
    },
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
fn path(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn fixture() -> (tempfile::TempDir, ChangeEngine) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        "---\nwiki_schema: \"1\"\nwiki_id: vault_fixture\nwiki_kind: vault\ntitle: Fixture\n---\n",
    )
    .unwrap();
    let engine =
        ChangeEngine::new(VaultFs::new(VaultRoot::explicit(temp.path()).unwrap())).unwrap();
    (temp, engine)
}
fn permit(engine: &ChangeEngine) -> WriterPermit {
    WriterPermit::acquire(engine.fs().root(), Duration::ZERO).unwrap()
}
fn write(s: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> ExpectedWrite {
    ExpectedWrite {
        target: path(s),
        expected: old.map_or(ExpectedState::Absent, |b| {
            ExpectedState::Hash(Blake3Hash::digest(b))
        }),
        proposed: new.map(Vec::from),
        apply_after: Vec::new(),
    }
}
fn draft(operations: Vec<ExpectedWrite>) -> ChangeDraft {
    ChangeDraft {
        title: "Test \"exact\"\nbytes".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        operations,
    }
}
fn frame(change: &ChangeInspection, sequence: u64, event: ChangeEvent) -> JournalFrame {
    JournalFrame {
        version: 1,
        sequence,
        change_id: change.prepared.change_id.clone(),
        manifest_hash: change.prepared.manifest_hash.clone(),
        event,
    }
}
fn append(
    engine: &ChangeEngine,
    permit: &WriterPermit,
    change: &ChangeInspection,
    event: ChangeEvent,
) -> JournalState {
    journal::append_event(
        engine.fs(),
        permit,
        &change.manifest,
        &change.prepared.manifest_hash,
        event,
    )
    .unwrap()
}

#[test]
fn exact_binary_retention_and_manifest_schema_and_identity() {
    let (temp, engine) = fixture();
    let old = b"\0\xffbefore\r\n";
    let new = b"\xfe\0after\n";
    fs::write(temp.path().join("data.bin"), old).unwrap();
    let change = engine
        .prepare(
            &permit(&engine),
            draft(vec![write("data.bin", Some(old), Some(new))]),
        )
        .unwrap();
    assert_eq!(fs::read(temp.path().join("data.bin")).unwrap(), old);
    let op = &change.manifest.operations[0];
    assert_eq!(
        fs::read(
            temp.path()
                .join(op.before_payload.as_ref().unwrap().path.as_str())
        )
        .unwrap(),
        old
    );
    assert_eq!(
        fs::read(
            temp.path()
                .join(op.after_payload.as_ref().unwrap().path.as_str())
        )
        .unwrap(),
        new
    );
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/change-v1.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&serde_json::to_value(&change.manifest).unwrap()));
    let note = temp
        .path()
        .join(manifest_path(&change.prepared.change_id).unwrap().as_str());
    let bytes = fs::read_to_string(&note).unwrap();
    fs::write(
        note,
        bytes.replace("wiki_status: prepared", "wiki_status: aborted"),
    )
    .unwrap();
    let inspected = engine.inspect(&change.prepared.change_id).unwrap();
    assert_eq!(
        inspected.prepared.manifest_hash,
        change.prepared.manifest_hash
    );
    assert_eq!(inspected.status, ChangeStatus::Prepared);
}

#[test]
fn dry_run_and_all_preconditions_before_retained_writes() {
    let (temp, engine) = fixture();
    engine
        .plan(&draft(vec![write("new.md", None, Some(b"new"))]))
        .unwrap();
    assert!(!temp.path().join(".wiki").exists());
    assert!(!temp.path().join("changes").exists());
    let permit = permit(&engine);
    assert!(
        engine
            .prepare(
                &permit,
                draft(vec![
                    write("a.md", None, Some(b"a")),
                    write("z.md", Some(b"missing"), Some(b"z"))
                ])
            )
            .is_err()
    );
    assert!(!temp.path().join("changes").exists());
    assert!(
        engine
            .prepare(
                &permit,
                draft(vec![
                    write("a.md", None, Some(b"a")),
                    write("a.md", None, Some(b"b"))
                ])
            )
            .is_err()
    );
    assert!(!temp.path().join("changes").exists());
}

#[test]
fn cycles_case_prefix_reserved_and_exact_noops() {
    let (temp, engine) = fixture();
    for pair in [
        ("A.md", "a.md"),
        ("dir", "dir/a.md"),
        ("new/Foo.md", "new/foo.md"),
    ] {
        assert!(
            engine
                .plan(&draft(vec![
                    write(pair.0, None, Some(b"a")),
                    write(pair.1, None, Some(b"b"))
                ]))
                .is_err()
        );
    }
    for target in [
        ".wiki/a.md",
        ".WIKI/a.md",
        "CHANGES/new/change.md",
        "Index.md",
        ".git/a",
        "changes/new/change.md",
        "index.md",
        "dir/index.md",
        "dir/.lwiki-stage-forged.tmp",
    ] {
        assert!(
            engine
                .plan(&draft(vec![write(target, None, Some(b"a"))]))
                .is_err()
        );
    }
    let mut a = write("a.md", None, Some(b"a"));
    let mut b = write("b.md", None, Some(b"b"));
    a.apply_after.push(path("b.md"));
    b.apply_after.push(path("a.md"));
    assert!(engine.plan(&draft(vec![a, b])).is_err());
    fs::write(temp.path().join("same.md"), b"same").unwrap();
    let mut new = write("new.md", None, Some(b"new"));
    new.apply_after.push(path("same.md"));
    let plan = engine
        .plan(&draft(vec![
            new,
            write("same.md", Some(b"same"), Some(b"same")),
        ]))
        .unwrap();
    assert_eq!(plan.operations.len(), 1);
    assert!(plan.operations[0].apply_after.is_empty());
}

#[test]
fn immutable_revision_tree_and_record_kind_independent_of_source_head() {
    let (temp, engine) = fixture();
    fs::create_dir_all(temp.path().join("sources/absent/revisions/old")).unwrap();
    fs::write(
        temp.path()
            .join("sources/absent/revisions/old/original.bin"),
        b"old",
    )
    .unwrap();
    for op in [
        write(
            "sources/absent/revisions/old/original.bin",
            Some(b"old"),
            None,
        ),
        write("sources/absent/revisions/old/new.bin", None, Some(b"new")),
    ] {
        assert!(engine.plan(&draft(vec![op])).is_err());
    }
    let immutable=b"---\nwiki_schema: \"1\"\nwiki_id: packet_test\nwiki_kind: extraction_packet\ntitle: Packet\n---\n";
    fs::write(temp.path().join("outside.md"), immutable).unwrap();
    assert!(
        engine
            .plan(&draft(vec![write(
                "outside.md",
                Some(immutable),
                Some(b"replacement")
            )]))
            .is_err()
    );
    let change = engine
        .prepare(
            &permit(&engine),
            draft(vec![
                write(
                    "sources/absent/revisions/new/original.bin",
                    None,
                    Some(b"original"),
                ),
                write(
                    "sources/absent/revisions/new/content.txt",
                    None,
                    Some(b"text"),
                ),
            ]),
        )
        .unwrap();
    assert!(
        change
            .manifest
            .operations
            .iter()
            .all(|op| op.role == OperationRole::ImmutableAsset)
    );
}

#[test]
fn lost_journal_remains_staged_even_when_targets_new_or_mixed() {
    let (temp, engine) = fixture();
    let change = engine
        .prepare(
            &permit(&engine),
            draft(vec![
                write("a.md", None, Some(b"a")),
                write("b.md", None, Some(b"b")),
            ]),
        )
        .unwrap();
    fs::remove_file(
        temp.path().join(
            journal::journal_path(&change.prepared.change_id)
                .unwrap()
                .as_str(),
        ),
    )
    .unwrap();
    for count in 0..=2 {
        if count > 0 {
            fs::write(
                temp.path().join(if count == 1 { "a.md" } else { "b.md" }),
                if count == 1 { b"a" } else { b"b" },
            )
            .unwrap();
        }
        let inspected = engine.inspect(&change.prepared.change_id).unwrap();
        assert_eq!(inspected.status, ChangeStatus::Prepared);
        assert_eq!(inspected.journal.status, None);
    }
}

#[test]
fn origin_build_once_reuse_all_terminal_states_and_different_response() {
    let (_temp, engine) = fixture();
    let permit = permit(&engine);
    let origin = ChangeOrigin {
        operation: OriginOperation::GraphImport,
        packet_id: RecordId::new("packet_test").unwrap(),
        response_hash: Blake3Hash::digest(b"response"),
    };
    let change = engine
        .prepare_or_reuse(
            &permit,
            origin.clone(),
            OriginPolicy::ReuseOrConflict,
            || Ok(draft(vec![])),
        )
        .unwrap();
    assert!(!change.reused);
    let reused = engine
        .prepare_or_reuse(
            &permit,
            origin.clone(),
            OriginPolicy::ReuseOrConflict,
            || panic!("must not allocate/build on retry"),
        )
        .unwrap();
    assert_eq!(reused.change.prepared, change.change.prepared);
    append(&engine, &permit, &change.change, ChangeEvent::Aborted);
    let reused = engine
        .prepare_or_reuse(
            &permit,
            origin.clone(),
            OriginPolicy::ReuseOrConflict,
            || panic!("aborted remains historical"),
        )
        .unwrap();
    assert_eq!(reused.change.status, ChangeStatus::Aborted);
    let different = ChangeOrigin {
        response_hash: Blake3Hash::digest(b"different"),
        ..origin.clone()
    };
    assert!(
        engine
            .prepare_or_reuse(
                &permit,
                different.clone(),
                OriginPolicy::ReuseOrConflict,
                || panic!("conflict does not build")
            )
            .is_err()
    );
    let second = engine
        .prepare_or_reuse(
            &permit,
            different.clone(),
            OriginPolicy::AllowNewResponse,
            || Ok(draft(vec![])),
        )
        .unwrap();
    append(&engine, &permit, &second.change, ChangeEvent::Applying);
    append(&engine, &permit, &second.change, ChangeEvent::FilesApplied);
    append(
        &engine,
        &permit,
        &second.change,
        ChangeEvent::Indexed {
            snapshot: ReadSnapshot {
                generation: 1,
                parser_fingerprint: Blake3Hash::digest(b"parser"),
                control_manifest: Blake3Hash::digest(b"scan"),
            },
        },
    );
    append(&engine, &permit, &second.change, ChangeEvent::Committed);
    let reused = engine
        .prepare_or_reuse(&permit, different, OriginPolicy::ReuseOrConflict, || {
            panic!("committed does not build")
        })
        .unwrap();
    assert_eq!(reused.change.status, ChangeStatus::Committed);
    assert_eq!(reused.change.prepared, second.change.prepared);
    assert!(
        journal::append_event(
            engine.fs(),
            &permit,
            &second.change.manifest,
            &second.change.prepared.manifest_hash,
            ChangeEvent::Applying
        )
        .is_err()
    );
}

#[test]
fn missing_tampered_escaped_payload_and_duplicate_manifest_keys_fail_closed() {
    let (temp, engine) = fixture();
    let change = engine
        .prepare(
            &permit(&engine),
            draft(vec![write("a.md", None, Some(b"new"))]),
        )
        .unwrap();
    let payload = change.manifest.operations[0]
        .after_payload
        .as_ref()
        .unwrap();
    let asset = temp.path().join(payload.path.as_str());
    fs::write(&asset, b"bad").unwrap();
    assert!(engine.inspect(&change.prepared.change_id).is_err());
    fs::remove_file(&asset).unwrap();
    assert!(engine.inspect(&change.prepared.change_id).is_err());
    fs::write(&asset, b"new").unwrap();
    let note = temp
        .path()
        .join(manifest_path(&change.prepared.change_id).unwrap().as_str());
    let original = fs::read_to_string(&note).unwrap();
    let json = serde_json::to_string(&change.manifest).unwrap();
    for modified in [
        json.replacen("\"version\":1", "\"version\":1,\"version\":1", 1),
        json.replace(payload.path.as_str(), "outside.md"),
        json.replace("\"byte_len\":3", "\"byte_len\":2"),
    ] {
        let hash = Blake3Hash::digest(modified.as_bytes());
        fs::write(
            &note,
            original
                .replace(&json, &modified)
                .replace(change.prepared.manifest_hash.as_str(), hash.as_str()),
        )
        .unwrap();
        assert!(engine.inspect(&change.prepared.change_id).is_err());
    }
    fs::write(&note, original).unwrap();
    assert!(engine.inspect(&change.prepared.change_id).is_ok());
}

#[test]
fn truncated_tail_complete_corruption_sequence_and_topological_events() {
    let (_temp, engine) = fixture();
    let mut a = write("a.md", None, Some(b"a"));
    a.apply_after.push(path("b.md"));
    let change = engine
        .prepare(
            &permit(&engine),
            draft(vec![a, write("b.md", None, Some(b"b"))]),
        )
        .unwrap();
    let first = encode_frame(&frame(&change, 0, ChangeEvent::Prepared)).unwrap();
    let second = encode_frame(&frame(&change, 1, ChangeEvent::Applying)).unwrap();
    for cut in 1..second.len() {
        let bytes = [first.as_slice(), &second[..cut]].concat();
        let state =
            decode_journal(&bytes, &change.manifest, &change.prepared.manifest_hash).unwrap();
        assert!(state.torn_tail);
        assert_eq!(state.safe_offset, first.len() as u64);
        assert_eq!(state.status, Some(ChangeStatus::Prepared));
    }
    for index in [0, 8, 12, 43, 44, second.len() - 1] {
        let mut bad = second.clone();
        bad[index] ^= 1;
        for tail in [vec![], first.clone()] {
            assert!(
                decode_journal(
                    &[first.clone(), bad.clone(), tail].concat(),
                    &change.manifest,
                    &change.prepared.manifest_hash
                )
                .is_err(),
                "corruption at {index}"
            );
        }
    }
    // Altered length with valid header is explicitly oversized, never a torn body.
    let mut oversized = second.clone();
    oversized[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    let hash = blake3::hash(&oversized[..12]);
    oversized[12..44].copy_from_slice(hash.as_bytes());
    assert!(decode_journal(&oversized, &change.manifest, &change.prepared.manifest_hash).is_err());
    for (seq, event) in [
        (2, ChangeEvent::Applying),
        (1, ChangeEvent::Committed),
        (1, ChangeEvent::Done { op: 1 }),
    ] {
        let bad = encode_frame(&frame(&change, seq, event)).unwrap();
        assert!(
            decode_journal(
                &[first.clone(), bad].concat(),
                &change.manifest,
                &change.prepared.manifest_hash
            )
            .is_err()
        );
    }
    let permit = permit(&engine);
    append(&engine, &permit, &change, ChangeEvent::Applying);
    assert!(
        journal::append_event(
            engine.fs(),
            &permit,
            &change.manifest,
            &change.prepared.manifest_hash,
            ChangeEvent::Intent { op: 0 }
        )
        .is_err()
    );
    append(&engine, &permit, &change, ChangeEvent::Intent { op: 1 });
    append(&engine, &permit, &change, ChangeEvent::Done { op: 1 });
    append(&engine, &permit, &change, ChangeEvent::Intent { op: 0 });
    append(&engine, &permit, &change, ChangeEvent::Done { op: 0 });
    append(&engine, &permit, &change, ChangeEvent::FilesApplied);
}

#[test]
fn duplicate_journal_keys_and_torn_tail_repaired_before_append() {
    let (temp, engine) = fixture();
    let permit = permit(&engine);
    let change = engine.prepare(&permit, draft(vec![])).unwrap();
    let good = encode_frame(&frame(&change, 1, ChangeEvent::Applying)).unwrap();
    let body = serde_json::to_string(&frame(&change, 1, ChangeEvent::Applying))
        .unwrap()
        .replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    let mut bad = Vec::from(&good[..12]);
    bad[8..12].copy_from_slice(&(body.len() as u32).to_le_bytes());
    bad.extend_from_slice(blake3::hash(&bad).as_bytes());
    bad.extend_from_slice(body.as_bytes());
    bad.extend_from_slice(blake3::hash(body.as_bytes()).as_bytes());
    let first = encode_frame(&frame(&change, 0, ChangeEvent::Prepared)).unwrap();
    assert!(
        decode_journal(
            &[first.clone(), bad].concat(),
            &change.manifest,
            &change.prepared.manifest_hash
        )
        .is_err()
    );
    let journal_path = temp.path().join(
        journal::journal_path(&change.prepared.change_id)
            .unwrap()
            .as_str(),
    );
    fs::write(&journal_path, [first.clone(), good[..17].to_vec()].concat()).unwrap();
    let state = append(&engine, &permit, &change, ChangeEvent::Applying);
    assert!(!state.torn_tail);
    assert_eq!(state.frames.len(), 2);
    assert_eq!(fs::read(journal_path).unwrap(), [first, good].concat());
}

struct FaultIo {
    at: usize,
    count: AtomicUsize,
    unsupported: bool,
    after: bool,
}
impl FaultIo {
    fn operation<T>(&self, call: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let hit = self.count.fetch_add(1, Ordering::SeqCst) + 1 == self.at;
        if hit && !self.after {
            return Err(io::Error::other("injected before durability call"));
        }
        let result = call();
        if hit && self.after {
            return Err(io::Error::other("injected after durability call"));
        }
        result
    }
}
impl DurableIo for FaultIo {
    fn create_stage(&self, p: &Path) -> io::Result<File> {
        self.operation(|| NativeIo.create_stage(p))
    }
    fn open_append(&self, p: &Path) -> io::Result<File> {
        self.operation(|| NativeIo.open_append(p))
    }
    fn truncate_file(&self, f: &File, n: u64) -> io::Result<()> {
        self.operation(|| NativeIo.truncate_file(f, n))
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> io::Result<()> {
        self.operation(|| NativeIo.write_stage(f, b))
    }
    fn sync_file(&self, f: &File) -> io::Result<()> {
        self.operation(|| NativeIo.sync_file(f))
    }
    fn replace(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.operation(|| NativeIo.replace(a, b))
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        NativeIo.remove(p)
    }
    fn create_directory(&self, p: &Path) -> io::Result<()> {
        self.operation(|| NativeIo.create_directory(p))
    }
    fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
        self.operation(|| {
            if self.unsupported {
                Ok(DirectorySync::Unsupported)
            } else {
                NativeIo.sync_directory(p)
            }
        })
    }
}
#[test]
fn actual_prepare_durable_io_failure_matrix_never_mutates_targets() {
    for after in [false, true] {
        let mut reached_success = false;
        for at in 1..200 {
            let (temp, _) = fixture();
            fs::write(temp.path().join("binary.bin"), b"old\0\xff").unwrap();
            let io = Arc::new(FaultIo {
                at,
                count: AtomicUsize::new(0),
                unsupported: false,
                after,
            });
            let engine = ChangeEngine::new(VaultFs::with_io(
                VaultRoot::explicit(temp.path()).unwrap(),
                io.clone(),
            ))
            .unwrap();
            let result = engine.prepare(
                &permit(&engine),
                draft(vec![
                    write("binary.bin", Some(b"old\0\xff"), Some(b"new\0\xfe")),
                    write("page.md", None, Some(b"page")),
                ]),
            );
            assert_eq!(
                fs::read(temp.path().join("binary.bin")).unwrap(),
                b"old\0\xff"
            );
            assert!(!temp.path().join("page.md").exists());
            if result.is_ok() {
                assert!(io.count.load(Ordering::SeqCst) < at);
                reached_success = true;
                break;
            }
            // Every injected failure returns an error, even after a rename happened.
            assert!(io.count.load(Ordering::SeqCst) >= at);
            let restarted =
                ChangeEngine::new(VaultFs::new(VaultRoot::explicit(temp.path()).unwrap())).unwrap();
            let retained_before = tree_files(temp.path());
            let restarted_permit = permit(&restarted);
            let recovered = restarted
                .recover(&restarted_permit, &NoActivation, &NoActivation)
                .unwrap();
            assert!(recovered.changes.is_empty());
            let orphans = restarted.incomplete_preparations().unwrap();
            for orphan in &orphans {
                assert!(
                    !temp
                        .path()
                        .join(manifest_path(orphan).unwrap().as_str())
                        .exists()
                );
            }
            let builds = AtomicUsize::new(0);
            let origin = ChangeOrigin {
                operation: OriginOperation::GraphImport,
                packet_id: RecordId::new("packet_restart").unwrap(),
                response_hash: Blake3Hash::digest(b"retry response"),
            };
            let retry = restarted
                .prepare_or_reuse(
                    &restarted_permit,
                    origin,
                    OriginPolicy::ReuseOrConflict,
                    || {
                        builds.fetch_add(1, Ordering::SeqCst);
                        Ok(draft(vec![]))
                    },
                )
                .unwrap();
            assert!(!retry.reused);
            assert_eq!(builds.load(Ordering::SeqCst), 1);
            assert_eq!(restarted.incomplete_preparations().unwrap(), orphans);
            for (relative, bytes) in retained_before {
                assert_eq!(fs::read(temp.path().join(relative)).unwrap(), bytes);
            }
        }
        assert!(reached_success);
    }
    let (temp, _) = fixture();
    let engine = ChangeEngine::new(VaultFs::with_io(
        VaultRoot::explicit(temp.path()).unwrap(),
        Arc::new(FaultIo {
            at: usize::MAX,
            count: AtomicUsize::new(0),
            unsupported: true,
            after: false,
        }),
    ))
    .unwrap();
    assert!(engine.prepare(&permit(&engine), draft(vec![])).is_err());
    assert!(!temp.path().join("changes").join("change.md").exists());
}

#[test]
fn source_head_infers_all_new_revision_prerequisites_and_terminal_history_ignores_targets() {
    let (temp, engine) = fixture();
    let source = b"---\nwiki_schema: \"1\"\nwiki_id: source_s\nwiki_kind: source\ntitle: Source\nwiki_status: active\nwiki_origin_kind: local-file\nwiki_origin: synthetic\nwiki_current_revision: revision_new\nwiki_revisions: [revision_new]\n---\n";
    let permit = permit(&engine);
    let change = engine
        .prepare(
            &permit,
            draft(vec![
                write("sources/source_s/head.md", None, Some(source)),
                write(
                    "sources/source_s/revisions/revision_new/original.bin",
                    None,
                    Some(b"bytes"),
                ),
                write(
                    "sources/source_s/revisions/revision_new/content.txt",
                    None,
                    Some(b"text"),
                ),
            ]),
        )
        .unwrap();
    assert_eq!(
        change.manifest.operations[0].target,
        path("sources/source_s/head.md")
    );
    assert_eq!(change.manifest.operations[0].apply_after, vec![1, 2]);
    assert_eq!(
        lwiki::changes::prepare::topological_order(
            &change
                .manifest
                .operations
                .iter()
                .map(|op| op.apply_after.clone())
                .collect::<Vec<_>>()
        )
        .unwrap(),
        vec![1, 2, 0]
    );
    append(&engine, &permit, &change, ChangeEvent::Aborted);
    fs::create_dir_all(temp.path().join("sources/source_s")).unwrap();
    fs::write(
        temp.path().join("sources/source_s/WIKI.md"),
        b"nested vault",
    )
    .unwrap();
    let historical = engine.inspect(&change.prepared.change_id).unwrap();
    assert_eq!(historical.status, ChangeStatus::Aborted);
    assert!(historical.observations.is_empty());
}

#[test]
fn recovery_applying_epoch_resets_completions_and_pending_intent() {
    let (_temp, engine) = fixture();
    let permit = permit(&engine);
    let mut a = write("a.md", None, Some(b"a"));
    a.apply_after.push(path("b.md"));
    let change = engine
        .prepare(&permit, draft(vec![a, write("b.md", None, Some(b"b"))]))
        .unwrap();
    append(&engine, &permit, &change, ChangeEvent::Applying);
    append(&engine, &permit, &change, ChangeEvent::Intent { op: 1 });
    append(&engine, &permit, &change, ChangeEvent::Applying);
    assert!(
        journal::append_event(
            engine.fs(),
            &permit,
            &change.manifest,
            &change.prepared.manifest_hash,
            ChangeEvent::Done { op: 1 }
        )
        .is_err()
    );
    for phase in 0..3 {
        assert!(
            journal::append_event(
                engine.fs(),
                &permit,
                &change.manifest,
                &change.prepared.manifest_hash,
                ChangeEvent::Intent { op: 0 }
            )
            .is_err()
        );
        for op in [1, 0] {
            append(&engine, &permit, &change, ChangeEvent::Intent { op });
            append(&engine, &permit, &change, ChangeEvent::Done { op });
        }
        append(&engine, &permit, &change, ChangeEvent::FilesApplied);
        if phase > 0 {
            for generation in 1..=2 {
                append(
                    &engine,
                    &permit,
                    &change,
                    ChangeEvent::Indexed {
                        snapshot: ReadSnapshot {
                            generation,
                            parser_fingerprint: Blake3Hash::digest(b"parser"),
                            control_manifest: Blake3Hash::digest(b"scan"),
                        },
                    },
                );
            }
        }
        if phase < 2 {
            append(&engine, &permit, &change, ChangeEvent::Applying);
        }
    }
    append(&engine, &permit, &change, ChangeEvent::Committed);
    assert!(
        journal::append_event(
            engine.fs(),
            &permit,
            &change.manifest,
            &change.prepared.manifest_hash,
            ChangeEvent::Applying
        )
        .is_err()
    );
    let conflict = engine.prepare(&permit, draft(vec![])).unwrap();
    append(
        &engine,
        &permit,
        &conflict,
        ChangeEvent::Conflict {
            phase: "preflight".into(),
            observations: vec![],
        },
    );
    assert!(
        journal::append_event(
            engine.fs(),
            &permit,
            &conflict.manifest,
            &conflict.prepared.manifest_hash,
            ChangeEvent::Applying
        )
        .is_err()
    );
}

#[test]
fn tail_repair_truncate_and_sync_failures_stop_append() {
    for after in [false, true] {
        for at in [1, 2] {
            let (temp, engine) = fixture();
            let change = engine.prepare(&permit(&engine), draft(vec![])).unwrap();
            let first = encode_frame(&frame(&change, 0, ChangeEvent::Prepared)).unwrap();
            let torn = encode_frame(&frame(&change, 1, ChangeEvent::Applying)).unwrap();
            let path = temp.path().join(
                journal::journal_path(&change.prepared.change_id)
                    .unwrap()
                    .as_str(),
            );
            fs::write(&path, [first.clone(), torn[..15].to_vec()].concat()).unwrap();
            let faulty = ChangeEngine::new(VaultFs::with_io(
                VaultRoot::explicit(temp.path()).unwrap(),
                Arc::new(FaultIo {
                    at,
                    count: AtomicUsize::new(0),
                    unsupported: false,
                    after,
                }),
            ))
            .unwrap();
            assert!(
                journal::append_event(
                    faulty.fs(),
                    &permit(&faulty),
                    &change.manifest,
                    &change.prepared.manifest_hash,
                    ChangeEvent::Applying
                )
                .is_err()
            );
            let state = decode_journal(
                &fs::read(&path).unwrap(),
                &change.manifest,
                &change.prepared.manifest_hash,
            )
            .unwrap();
            assert_eq!(state.status, Some(ChangeStatus::Prepared));
            assert_eq!(state.frames.len(), 1);
        }
    }
}

#[test]
fn journal_rejects_unknown_nested_snapshot_fields_and_wrong_binding() {
    let (_temp, engine) = fixture();
    let permit = permit(&engine);
    let change = engine.prepare(&permit, draft(vec![])).unwrap();
    let events = [
        ChangeEvent::Prepared,
        ChangeEvent::Applying,
        ChangeEvent::FilesApplied,
    ];
    let mut valid = Vec::new();
    for (sequence, event) in events.into_iter().enumerate() {
        valid.extend(encode_frame(&frame(&change, sequence as u64, event)).unwrap());
    }
    let indexed = frame(
        &change,
        3,
        ChangeEvent::Indexed {
            snapshot: ReadSnapshot {
                generation: 1,
                parser_fingerprint: Blake3Hash::digest(b"parser"),
                control_manifest: Blake3Hash::digest(b"scan"),
            },
        },
    );
    let mut value = serde_json::to_value(&indexed).unwrap();
    value["event"]["snapshot"]["unexpected"] = true.into();
    let body = serde_json::to_vec(&value).unwrap();
    let good = encode_frame(&indexed).unwrap();
    let mut bad = good[..12].to_vec();
    bad[8..12].copy_from_slice(&(body.len() as u32).to_le_bytes());
    let hash = blake3::hash(&bad);
    bad.extend_from_slice(hash.as_bytes());
    bad.extend_from_slice(&body);
    bad.extend_from_slice(blake3::hash(&body).as_bytes());
    assert!(
        decode_journal(
            &[valid, bad].concat(),
            &change.manifest,
            &change.prepared.manifest_hash
        )
        .is_err()
    );
    for wrong in 0..3 {
        let mut frame = frame(&change, 0, ChangeEvent::Prepared);
        match wrong {
            0 => frame.version = 2,
            1 => frame.change_id = RecordId::new("change_wrong").unwrap(),
            _ => frame.manifest_hash = Blake3Hash::digest(b"wrong"),
        }
        assert!(
            decode_journal(
                &encode_frame(&frame).unwrap(),
                &change.manifest,
                &change.prepared.manifest_hash
            )
            .is_err()
        );
    }
}

#[test]
fn vault_identity_and_permit_binding_cannot_be_changed() {
    let (temp, engine) = fixture();
    let (_other, other) = fixture();
    assert!(engine.prepare(&permit(&other), draft(vec![])).is_err());
    let marker = temp.path().join("WIKI.md");
    let original = fs::read_to_string(&marker).unwrap();
    fs::write(marker, original.replace("vault_fixture", "vault_changed")).unwrap();
    assert!(engine.plan(&draft(vec![])).is_err());
    assert!(engine.prepare(&permit(&engine), draft(vec![])).is_err());
}

#[test]
fn verified_terminal_receipt_survives_journal_loss_and_origin_retry() {
    let (temp, engine) = fixture();
    let permit = permit(&engine);
    let origin = ChangeOrigin {
        operation: OriginOperation::GraphImport,
        packet_id: RecordId::new("packet_receipt").unwrap(),
        response_hash: Blake3Hash::digest(b"response"),
    };
    let change = engine
        .prepare_or_reuse(
            &permit,
            origin.clone(),
            OriginPolicy::ReuseOrConflict,
            || Ok(draft(vec![write("future/page.md", None, Some(b"page"))])),
        )
        .unwrap()
        .change;
    let aborted = engine.abort(&permit, &change.prepared).unwrap();
    assert_eq!(aborted.status, ChangeStatus::Aborted);
    fs::remove_file(
        temp.path().join(
            journal::journal_path(&change.prepared.change_id)
                .unwrap()
                .as_str(),
        ),
    )
    .unwrap();
    fs::create_dir_all(temp.path().join("future")).unwrap();
    fs::write(temp.path().join("future/WIKI.md"), b"nested vault now").unwrap();
    let reused = engine
        .prepare_or_reuse(
            &permit,
            origin.clone(),
            OriginPolicy::ReuseOrConflict,
            || panic!("verified aborted receipt must reuse"),
        )
        .unwrap();
    assert_eq!(reused.change.status, ChangeStatus::Aborted);
    assert!(reused.change.observations.is_empty());
    assert_eq!(reused.change.journal.status, None);
    assert_eq!(reused.change.note_status, "aborted");
    let receipt = temp.path().join(format!(
        "changes/{}/outcome.json",
        change.prepared.change_id
    ));
    let bytes = fs::read_to_string(&receipt).unwrap();
    fs::write(
        receipt,
        bytes.replace("\"status\":\"aborted\"", "\"status\":\"committed\""),
    )
    .unwrap();
    assert!(
        engine
            .prepare_or_reuse(&permit, origin, OriginPolicy::ReuseOrConflict, || panic!(
                "corrupt receipt must not build"
            ))
            .is_err()
    );
}

struct NoActivation;
impl GraphValidator for NoActivation {
    fn validate(&self, _: &VaultFs, _: &ValidationInput) -> lwiki::domain::Result<ValidatedGraph> {
        panic!("staged/orphan recovery must not validate or activate")
    }
}
impl PublicationBackend for NoActivation {
    fn check_available(&self) -> lwiki::domain::Result<()> {
        panic!("staged/orphan recovery must not require a publisher")
    }
    fn publish(
        &self,
        _: &VaultFs,
        _: &PublicationPermit<'_>,
        _: &ValidationInput,
    ) -> lwiki::domain::Result<ReadSnapshot> {
        panic!("staged/orphan recovery must not publish")
    }
}
fn tree_files(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, files: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(base, &entry.path(), files)
            } else {
                files.insert(
                    entry.path().strip_prefix(base).unwrap().to_owned(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}
#[test]
fn incomplete_preparations_preserve_payloads_but_missing_manifest_with_evidence_refuses() {
    for artifact in [
        "journal",
        "outcome.json",
        "validation.json",
        "revision-trees.json",
    ] {
        let (temp, engine) = fixture();
        let permit = permit(&engine);
        let id = RecordId::new("change_missing").unwrap();
        fs::create_dir_all(temp.path().join("changes/change_missing/proposed")).unwrap();
        fs::write(
            temp.path().join("changes/change_missing/proposed/00000.md"),
            b"retained orphan",
        )
        .unwrap();
        assert_eq!(engine.incomplete_preparations().unwrap(), vec![id.clone()]);
        let recovered = engine
            .recover(&permit, &NoActivation, &NoActivation)
            .unwrap();
        assert!(recovered.staged.is_empty());
        let evidence = if artifact == "journal" {
            journal::journal_path(&id).unwrap()
        } else {
            path(&format!("changes/{id}/{artifact}"))
        };
        fs::create_dir_all(temp.path().join(evidence.as_str()).parent().unwrap()).unwrap();
        fs::write(temp.path().join(evidence.as_str()), b"").unwrap();
        assert_eq!(
            engine.incomplete_preparations().unwrap_err().code,
            lwiki::domain::ErrorCode::RecoveryRequired
        );
        assert_eq!(
            engine
                .recover(&permit, &NoActivation, &NoActivation)
                .unwrap_err()
                .code,
            lwiki::domain::ErrorCode::RecoveryRequired
        );
        let origin = ChangeOrigin {
            operation: OriginOperation::GraphImport,
            packet_id: RecordId::new("packet_test").unwrap(),
            response_hash: Blake3Hash::digest(b"response"),
        };
        assert_eq!(
            engine
                .prepare_or_reuse(&permit, origin, OriginPolicy::ReuseOrConflict, || panic!(
                    "missing manifest evidence must not build"
                ))
                .unwrap_err()
                .code,
            lwiki::domain::ErrorCode::RecoveryRequired
        );
        assert_eq!(
            fs::read(temp.path().join("changes/change_missing/proposed/00000.md")).unwrap(),
            b"retained orphan"
        );
    }
    let (temp, engine) = fixture();
    let permit = permit(&engine);
    fs::create_dir_all(temp.path().join("changes/change_malformed")).unwrap();
    fs::write(
        temp.path().join("changes/change_malformed/change.md"),
        b"malformed existing note",
    )
    .unwrap();
    assert!(engine.incomplete_preparations().unwrap().is_empty());
    assert!(
        engine
            .recover(&permit, &NoActivation, &NoActivation)
            .is_err()
    );
    #[cfg(unix)]
    {
        fs::remove_file(temp.path().join("changes/change_malformed/change.md")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("receipt"), b"").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("receipt"),
            temp.path().join("changes/change_malformed/outcome.json"),
        )
        .unwrap();
        assert!(engine.incomplete_preparations().is_err());
    }
}

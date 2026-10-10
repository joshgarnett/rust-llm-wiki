//! Metadata inspection uses disposable fixtures and mock application backends.
use super::*;
use crate::{
    storage::{self, StorageOptions},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::{collections::BTreeMap, fs, path::PathBuf, time::Duration};

fn rel(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}

struct Fixture {
    _temp: tempfile::TempDir,
    engine: ChangeEngine,
    writer: WriterPermit,
    staged: ChangeInspection,
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
            storage::cleanup(&vault, &writer, &StorageOptions::default()).unwrap();
        }
        assert_eq!(storage::layout::active(vault.root()).unwrap(), migrated);
        fs::write(temp.path().join("alpha.md"), b"old alpha").unwrap();
        fs::write(temp.path().join("guard.md"), b"read guard").unwrap();
        let engine = ChangeEngine::new(vault).unwrap();
        let staged = engine
            .prepare(
                &writer,
                ChangeDraft {
                    title: "Metadata fixture: 茶".into(),
                    origin: None,
                    inverse_of: None,
                    allocated_ids: BTreeMap::new(),
                    read_preconditions: vec![ReadDependency {
                        path: rel("guard.md"),
                        expected: ExpectedState::Hash(Blake3Hash::digest(b"read guard")),
                    }],
                    operations: vec![
                        ExpectedWrite {
                            target: rel("alpha.md"),
                            expected: ExpectedState::Hash(Blake3Hash::digest(b"old alpha")),
                            proposed: Some(b"new alpha".to_vec()),
                            apply_after: vec![],
                        },
                        ExpectedWrite {
                            target: rel("asset.bin"),
                            expected: ExpectedState::Absent,
                            proposed: Some(vec![0, 255, 1, 128]),
                            apply_after: vec![rel("alpha.md")],
                        },
                    ],
                },
            )
            .unwrap();
        Self {
            _temp: temp,
            engine,
            writer,
            staged,
        }
    }
    fn path(&self, relative: &VaultRelativePath) -> PathBuf {
        self.engine.fs().root().resolve(relative).unwrap()
    }
    fn inspect(&self) -> ChangeMetadataInspection {
        self.engine
            .inspect_metadata(&self.staged.prepared.change_id)
            .unwrap()
    }
    fn note_path(&self) -> PathBuf {
        self.path(&manifest_path(&self.staged.prepared.change_id).unwrap())
    }
    fn journal_path(&self) -> PathBuf {
        self.path(&journal::journal_path(&self.staged.prepared.change_id).unwrap())
    }
    fn set_note_status(&self, from: &str, to: &str) {
        let text = String::from_utf8(fs::read(self.note_path()).unwrap()).unwrap();
        let from = format!("wiki_status: {from}\n");
        assert_eq!(text.matches(&from).count(), 1);
        fs::write(
            self.note_path(),
            text.replacen(&from, &format!("wiki_status: {to}\n"), 1),
        )
        .unwrap();
    }
    fn commit(&self) {
        assert_eq!(
            self.engine
                .apply(&self.writer, &self.staged.prepared, &Validator, &Publisher)
                .unwrap()
                .status,
            ChangeStatus::Committed,
        );
    }
}

struct Validator;
impl GraphValidator for Validator {
    fn validate(&self, _: &VaultFs, _: &ValidationInput) -> Result<ValidatedGraph> {
        Ok(ValidatedGraph {
            parser_fingerprint: Blake3Hash::digest(b"metadata fixture parser"),
            control_manifest: Blake3Hash::digest(b"metadata fixture canonical proof"),
            dependencies: vec![],
        })
    }
}
struct Publisher;
impl PublicationBackend for Publisher {
    fn check_available(&self) -> Result<()> {
        Ok(())
    }
    fn publish(
        &self,
        _: &VaultFs,
        permit: &PublicationPermit<'_>,
        _: &ValidationInput,
    ) -> Result<ReadSnapshot> {
        Ok(ReadSnapshot::canonical(
            1,
            permit.graph().parser_fingerprint.clone(),
            permit.graph().control_manifest.clone(),
        ))
    }
}

#[test]
fn staged_payload_damage_does_not_prevent_metadata_but_strict_inspection_refuses() {
    for migrated in [false, true] {
        for remove in [false, true] {
            for before in [false, true] {
                let fixture = Fixture::new(migrated);
                let operation = &fixture.staged.manifest.operations[0];
                let payload = if before {
                    operation.before_payload.as_ref().unwrap()
                } else {
                    operation.after_payload.as_ref().unwrap()
                };
                if remove {
                    fs::remove_file(fixture.path(&payload.path)).unwrap();
                } else {
                    // Preserve the declared length so strict inspection must check the hash.
                    fs::write(
                        fixture.path(&payload.path),
                        vec![b'x'; payload.byte_len as usize],
                    )
                    .unwrap();
                }
                let metadata = fixture.inspect();
                assert_eq!(metadata.prepared, fixture.staged.prepared);
                assert_eq!(metadata.manifest, fixture.staged.manifest);
                assert_eq!(metadata.status, ChangeStatus::Prepared);
                assert_eq!(metadata.note_status, "prepared");
                assert!(
                    fixture
                        .engine
                        .inspect(&metadata.prepared.change_id)
                        .is_err()
                );
                assert!(
                    fixture
                        .engine
                        .inspect_history(&metadata.prepared.change_id)
                        .is_err()
                );
            }
        }
    }
}

#[test]
fn missing_current_targets_and_read_guards_leave_metadata_unchanged() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated);
        fs::remove_file(fixture.path(&rel("alpha.md"))).unwrap();
        fs::remove_file(fixture.path(&rel("guard.md"))).unwrap();
        let metadata = fixture.inspect();
        assert_eq!(metadata.manifest, fixture.staged.manifest);
        assert_eq!(metadata.status, ChangeStatus::Prepared);
        let strict = fixture
            .engine
            .inspect(&metadata.prepared.change_id)
            .unwrap();
        assert_eq!(strict.observations[0].observed, ExpectedState::Absent);
        assert_ne!(
            strict.observations[0].observed,
            strict.observations[0].before
        );
    }
}

#[test]
fn missing_journal_defaults_to_prepared_and_editable_note_is_diagnostic() {
    let fixture = Fixture::new(false);
    fixture.set_note_status("prepared", "committed");
    fs::remove_file(fixture.journal_path()).unwrap();
    let metadata = fixture.inspect();
    assert_eq!(metadata.status, ChangeStatus::Prepared);
    assert_eq!(metadata.note_status, "committed");
}

#[test]
fn unresolved_journal_status_overrides_editable_note() {
    let fixture = Fixture::new(false);
    journal::append_event(
        fixture.engine.fs(),
        &fixture.writer,
        &fixture.staged.manifest,
        &fixture.staged.prepared.manifest_hash,
        ChangeEvent::Applying,
    )
    .unwrap();
    fixture.set_note_status("prepared", "aborted");
    let metadata = fixture.inspect();
    assert_eq!(metadata.status, ChangeStatus::Applying);
    assert_eq!(metadata.note_status, "aborted");
}

#[test]
fn terminal_receipt_overrides_prepared_prefix_and_stale_note_without_payloads() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated);
        let prepared_prefix = fs::read(fixture.journal_path()).unwrap();
        fixture.commit();
        fixture.set_note_status("committed", "prepared");
        fs::write(fixture.journal_path(), prepared_prefix).unwrap();
        for payload in fixture
            .staged
            .manifest
            .operations
            .iter()
            .flat_map(|op| [&op.before_payload, &op.after_payload])
            .flatten()
        {
            fs::remove_file(fixture.path(&payload.path)).unwrap();
        }
        fs::remove_file(fixture.path(&rel("alpha.md"))).unwrap();
        let metadata = fixture.inspect();
        assert_eq!(metadata.status, ChangeStatus::Committed);
        assert_eq!(metadata.note_status, "prepared");
        assert_eq!(metadata.manifest, fixture.staged.manifest);
        assert!(
            fixture
                .engine
                .inspect(&metadata.prepared.change_id)
                .is_err()
        );
    }
}

#[test]
fn metadata_inspection_refuses_missing_identity_or_changed_vault_binding() {
    for remove in [false, true] {
        let fixture = Fixture::new(false);
        let missing = RecordId::new("change_missing-metadata").unwrap();
        assert!(fixture.engine.inspect_metadata(&missing).is_err());
        let marker = fixture.path(&rel("WIKI.md"));
        if remove {
            fs::remove_file(marker).unwrap();
        } else {
            let text = String::from_utf8(fs::read(&marker).unwrap()).unwrap();
            let note = parse_note(text.as_bytes());
            let original_id = note.canonical.as_ref().unwrap().id().as_str();
            fs::write(
                marker,
                text.replacen(original_id, "vault_changed-metadata", 1),
            )
            .unwrap();
        }
        assert!(
            fixture
                .engine
                .inspect_metadata(&fixture.staged.prepared.change_id)
                .is_err()
        );
    }
}

#[test]
fn malformed_manifest_envelope_and_structural_bindings_refuse() {
    for damage in [
        "envelope",
        "identity",
        "hash",
        "reference",
        "dependencies",
        "vault",
    ] {
        let fixture = Fixture::new(false);
        let bytes = match damage {
            "envelope" => b"not a change note".to_vec(),
            "identity" => String::from_utf8(fs::read(fixture.note_path()).unwrap())
                .unwrap()
                .replacen(
                    &format!("wiki_id: {}", fixture.staged.prepared.change_id),
                    "wiki_id: change_wrong-envelope",
                    1,
                )
                .into_bytes(),
            "hash" => String::from_utf8(fs::read(fixture.note_path()).unwrap())
                .unwrap()
                .replacen("Metadata fixture", "Tampered fixture", 2)
                .into_bytes(),
            _ => {
                let mut manifest = fixture.staged.manifest.clone();
                match damage {
                    "reference" => {
                        manifest.operations[0].after_payload.as_mut().unwrap().hash =
                            Blake3Hash::digest(b"unbound reference");
                    }
                    "dependencies" => manifest.operations[0].apply_after = vec![0],
                    "vault" => manifest.vault_id = RecordId::new("vault_wrong-binding").unwrap(),
                    _ => unreachable!(),
                }
                let hash = Blake3Hash::digest(serde_json::to_vec(&manifest).unwrap());
                super::super::prepare::render_prepared_note(&manifest, &hash).unwrap()
            }
        };
        fs::write(fixture.note_path(), bytes).unwrap();
        assert!(
            fixture
                .engine
                .inspect_metadata(&fixture.staged.prepared.change_id)
                .is_err(),
            "accepted malformed {damage} metadata",
        );
    }
}

#[test]
fn corrupt_or_misbound_journal_refuses() {
    for misbind in [false, true] {
        let fixture = Fixture::new(false);
        let bytes = if misbind {
            let mut frame = fixture.staged.journal.frames[0].clone();
            frame.manifest_hash = Blake3Hash::digest(b"different manifest");
            journal::encode_frame(&frame).unwrap()
        } else {
            let mut bytes = fs::read(fixture.journal_path()).unwrap();
            bytes[0] ^= 1;
            bytes
        };
        fs::write(fixture.journal_path(), bytes).unwrap();
        assert!(
            fixture
                .engine
                .inspect_metadata(&fixture.staged.prepared.change_id)
                .is_err()
        );
    }
}

#[test]
fn corrupt_terminal_proof_refuses_even_with_valid_manifest_and_journal() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated);
        fixture.commit();
        assert_eq!(fixture.inspect().status, ChangeStatus::Committed);
        let outcome = rel(&format!(
            "changes/{}/outcome.json",
            fixture.staged.prepared.change_id,
        ));
        let path = fixture.path(&outcome);
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        receipt["proof"]["status"] = serde_json::json!("aborted");
        fs::write(path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(
            fixture
                .engine
                .inspect_metadata(&fixture.staged.prepared.change_id)
                .is_err()
        );
    }
}

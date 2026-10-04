//! Disposable fixtures and mock publication/lookup test engine ownership mechanics.
use super::*;
use crate::{
    domain::ReadSnapshot,
    vault::{VaultFs, VaultRoot},
};
use std::{cell::Cell, time::Duration};

const TREE: &str = "sources/source_s/revisions/revision_r";
fn relative(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn fixture() -> (tempfile::TempDir, ChangeEngine, WriterPermit) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_ownership\nwiki_kind: vault\ntitle: Ownership fixture\n---\n").unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root)).unwrap();
    (temp, engine, permit)
}
fn draft(tree: &str) -> ChangeDraft {
    ChangeDraft {
        title: "Create immutable fixture".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![ExpectedWrite {
            target: relative(&format!("{tree}/original.bin")),
            expected: ExpectedState::Absent,
            proposed: Some(b"immutable".to_vec()),
            apply_after: vec![],
        }],
    }
}
struct Validator;
impl GraphValidator for Validator {
    fn validate(&self, _: &VaultFs, _: &ValidationInput) -> Result<ValidatedGraph> {
        Ok(ValidatedGraph {
            parser_fingerprint: Blake3Hash::digest(b"fixture parser"),
            control_manifest: Blake3Hash::digest(b"fixture canonical proof"),
            dependencies: vec![],
        })
    }
}
struct Publisher {
    stop: bool,
}
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
        if self.stop {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "fixture stop at files-applied",
            ));
        }
        Ok(ReadSnapshot::canonical(
            1,
            permit.graph().parser_fingerprint.clone(),
            permit.graph().control_manifest.clone(),
        ))
    }
}
struct Lookup {
    vault: RecordId,
    snapshot: ReadSnapshot,
    owners: BTreeMap<RevisionTreeKey, PreparedChange>,
    ready: bool,
    probes: Cell<usize>,
}
impl Lookup {
    fn new(owners: Vec<RevisionOwnerRow>) -> Self {
        Self {
            vault: RecordId::new("vault_ownership").unwrap(),
            snapshot: ReadSnapshot::published(
                1,
                Blake3Hash::digest(b"parser"),
                "0123456789abcdef0123456789abcdef".into(),
                Blake3Hash::digest(b"publication"),
            )
            .unwrap(),
            owners: owners
                .into_iter()
                .map(|row| (row.key, row.change))
                .collect(),
            ready: true,
            probes: Cell::new(0),
        }
    }
}
impl RevisionOwnershipLookup for Lookup {
    fn vault_id(&self) -> &RecordId {
        &self.vault
    }
    fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }
    fn require_ready(&self) -> Result<()> {
        if self.ready {
            Ok(())
        } else {
            Err(super::super::apply::recovery_error(
                "fixture registry unready",
            ))
        }
    }
    fn revision_owner(&self, key: &RevisionTreeKey) -> Result<Option<PreparedChange>> {
        self.require_ready()?;
        self.probes.set(self.probes.get() + 1);
        Ok(self.owners.get(key).cloned())
    }
}
fn activate(
    engine: &ChangeEngine,
    permit: &WriterPermit,
    change: &PreparedChange,
    lookup: &Lookup,
) -> operation_authority::Authority {
    let publication = operation_authority::Publication {
        file_id: lookup.snapshot.publication().unwrap().file_id.clone(),
        epoch: 1,
    };
    let idle = operation_authority::activate(
        &engine.fs,
        permit,
        &engine.vault_id,
        publication.clone(),
        operation_authority::Presence::LegacyMayBeAbsent,
    )
    .unwrap();
    operation_authority::begin(
        &engine.fs,
        permit,
        &idle,
        change.clone(),
        operation_authority::Publication {
            epoch: 2,
            ..publication
        },
    )
    .unwrap()
}
fn baseline(engine: &ChangeEngine, change: &PreparedChange) {
    fs::write(
        engine
            .fs
            .root()
            .path()
            .join(format!("changes/{}/validation.json", change.change_id)),
        b"original fixture baseline",
    )
    .unwrap();
}
fn reconstruct(
    engine: &ChangeEngine,
    permit: &WriterPermit,
    current: Option<&PreparedChange>,
) -> Result<Vec<RevisionOwnerRow>> {
    let mut result = BTreeMap::new();
    engine.reconstruct_revision_owners(permit, current, &mut || Ok(()), &mut |row| {
        if result
            .get(&row.key)
            .is_some_and(|owner| owner != &row.change)
        {
            return Err(conflict("fixture duplicate owner"));
        }
        result.insert(row.key, row.change);
        Ok(())
    })?;
    Ok(result
        .into_iter()
        .map(|(key, change)| RevisionOwnerRow { key, change })
        .collect())
}

#[test]
fn committed_owner_survives_deleted_tree_and_missing_obsolete_payloads() {
    let (_temp, engine, permit) = fixture();
    let prior = engine.prepare(&permit, draft(TREE)).unwrap();
    engine
        .apply(
            &permit,
            &prior.prepared,
            &Validator,
            &Publisher { stop: false },
        )
        .unwrap();
    fs::remove_dir_all(engine.fs.root().path().join(TREE)).unwrap();
    for operation in &prior.manifest.operations {
        if let Some(payload) = &operation.after_payload {
            fs::remove_file(engine.fs.root().resolve(&payload.path).unwrap()).unwrap();
        }
    }
    fs::remove_file(engine.fs.root().path().join(format!(
        "changes/{}/validation.json",
        prior.prepared.change_id
    )))
    .unwrap();
    let owners = reconstruct(&engine, &permit, None).unwrap();
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].change, prior.prepared);
    let current = engine.prepare(&permit, draft(TREE)).unwrap().prepared;
    baseline(&engine, &current);
    let lookup = Lookup::new(owners);
    activate(&engine, &permit, &current, &lookup);
    let guard = engine
        .indexed_revision_guard(&permit, &current, &lookup)
        .unwrap();
    assert_eq!(
        guard.preflight().unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert_eq!(lookup.probes.get(), 1);
    assert!(!engine.fs.root().path().join(TREE).exists());
}

#[test]
fn indexed_guard_reuses_receipt_and_does_not_enumerate_unrelated_history() {
    let (_temp, engine, permit) = fixture();
    let current = engine.prepare(&permit, draft(TREE)).unwrap().prepared;
    baseline(&engine, &current);
    let lookup = Lookup::new(vec![]);
    activate(&engine, &permit, &current, &lookup);
    // The legacy classifier rejects this object. Bounded current lookups must
    // never inspect unrelated history to discover competing owners.
    fs::write(
        engine.fs.root().path().join("changes/unrelated_poison"),
        b"not a change directory",
    )
    .unwrap();
    let guard = engine
        .indexed_revision_guard(&permit, &current, &lookup)
        .unwrap();
    guard.preflight().unwrap();
    guard.preflight().unwrap();
    guard.verify(false).unwrap();
    assert!(guard.complete_owner_rows().is_err());
    assert_eq!(lookup.probes.get(), 4);
    assert!(reconstruct(&engine, &permit, None).is_err());
    let receipt = engine
        .fs
        .root()
        .path()
        .join(format!("changes/{}/revision-trees.json", current.change_id));
    fs::write(&receipt, b"corrupt receipt").unwrap();
    assert!(guard.verify(false).is_err());
}

#[test]
fn guard_requires_ready_exact_active_change_and_starting_publication_even_without_roots() {
    let (_temp, engine, permit) = fixture();
    let current = engine
        .prepare(
            &permit,
            ChangeDraft {
                operations: vec![],
                ..draft(TREE)
            },
        )
        .unwrap()
        .prepared;
    let mut lookup = Lookup::new(vec![]);
    assert!(
        engine
            .indexed_revision_guard(&permit, &current, &lookup)
            .is_err()
    );
    let active = activate(&engine, &permit, &current, &lookup);
    lookup.ready = false;
    assert!(
        engine
            .indexed_revision_guard(&permit, &current, &lookup)
            .is_err()
    );
    lookup.ready = true;
    lookup.snapshot.generation = 2; // intended is not a starting-snapshot proof
    assert!(
        engine
            .indexed_revision_guard(&permit, &current, &lookup)
            .is_err()
    );
    lookup.snapshot.generation = 1;
    lookup.vault = RecordId::new("vault_other").unwrap();
    assert!(
        engine
            .indexed_revision_guard(&permit, &current, &lookup)
            .is_err()
    );
    lookup.vault = engine.vault_id.clone();
    let other = PreparedChange {
        change_id: current.change_id.clone(),
        manifest_hash: Blake3Hash::digest(b"changed manifest"),
    };
    assert!(
        engine
            .indexed_revision_guard(&permit, &other, &lookup)
            .is_err()
    );
    let guard = engine
        .indexed_revision_guard(&permit, &current, &lookup)
        .unwrap();
    guard.preflight().unwrap();
    operation_authority::cancel(&engine.fs, &permit, &active, &current).unwrap();
    assert!(guard.verify(false).is_err());
    assert_eq!(lookup.probes.get(), 0);
}

#[test]
fn reconstruction_only_accepts_exact_inflight_current_receipt_and_complete_membership() {
    let (_temp, engine, permit) = fixture();
    let current = engine.prepare(&permit, draft(TREE)).unwrap().prepared;
    assert!(reconstruct(&engine, &permit, Some(&current)).is_err());
    assert!(
        engine
            .apply(&permit, &current, &Validator, &Publisher { stop: true })
            .is_err()
    );
    assert_eq!(
        engine.inspect(&current.change_id).unwrap().status,
        ChangeStatus::FilesApplied
    );
    assert!(reconstruct(&engine, &permit, None).is_err());
    let owners = reconstruct(&engine, &permit, Some(&current)).unwrap();
    assert_eq!(owners.len(), 1);
    fs::write(
        engine.fs.root().path().join(format!("{TREE}/extra.bin")),
        b"unplanned",
    )
    .unwrap();
    assert!(reconstruct(&engine, &permit, Some(&current)).is_err());
    fs::remove_file(engine.fs.root().path().join(format!("{TREE}/extra.bin"))).unwrap();
    fs::remove_file(
        engine
            .fs
            .root()
            .path()
            .join(format!("changes/{}/validation.json", current.change_id)),
    )
    .unwrap();
    assert!(reconstruct(&engine, &permit, Some(&current)).is_err());
}

#[test]
fn current_without_roots_requires_baseline_but_no_nonexistent_tree_receipt() {
    let (_temp, engine, permit) = fixture();
    let current = engine
        .prepare(
            &permit,
            ChangeDraft {
                operations: vec![],
                ..draft(TREE)
            },
        )
        .unwrap()
        .prepared;
    assert!(
        engine
            .apply(&permit, &current, &Validator, &Publisher { stop: true })
            .is_err()
    );
    assert_eq!(
        engine.inspect(&current.change_id).unwrap().status,
        ChangeStatus::FilesApplied
    );
    assert!(
        !engine
            .fs
            .root()
            .path()
            .join(format!("changes/{}/revision-trees.json", current.change_id))
            .exists()
    );
    assert!(
        reconstruct(&engine, &permit, Some(&current))
            .unwrap()
            .is_empty()
    );
    fs::remove_file(
        engine
            .fs
            .root()
            .path()
            .join(format!("changes/{}/validation.json", current.change_id)),
    )
    .unwrap();
    assert!(reconstruct(&engine, &permit, Some(&current)).is_err());
}

#[test]
fn aborted_owner_is_not_reconstructed_and_namespace_aliases_share_keys() {
    let (_temp, engine, permit) = fixture();
    let aborted = engine.prepare(&permit, draft(TREE)).unwrap();
    outcome::finish(
        &engine,
        &permit,
        &aborted.manifest,
        &aborted.prepared.manifest_hash,
        ChangeStatus::Aborted,
    )
    .unwrap();
    assert!(reconstruct(&engine, &permit, None).unwrap().is_empty());
    let a = tree_path(&relative("Sources/source_s/REVISIONS/revision_r/file.bin"))
        .unwrap()
        .unwrap();
    let b = tree_path(&relative("sources/source_s/revisions/revision_r/other.bin"))
        .unwrap()
        .unwrap();
    assert_eq!(indexed_key(&a), indexed_key(&b));
    let different = tree_path(&relative("sources/Source_s/revisions/revision_r/file.bin"))
        .unwrap()
        .unwrap();
    assert_ne!(indexed_key(&a), indexed_key(&different));
}

#[test]
fn complete_owner_rows_requires_original_receipt_and_complete_files_applied_tree() {
    let (_temp, engine, permit) = fixture();
    let current = engine.prepare(&permit, draft(TREE)).unwrap().prepared;
    assert!(
        engine
            .apply(&permit, &current, &Validator, &Publisher { stop: true })
            .is_err()
    );
    let lookup = Lookup::new(vec![]);
    activate(&engine, &permit, &current, &lookup);
    let guard = engine
        .indexed_revision_guard(&permit, &current, &lookup)
        .unwrap();
    guard.preflight().unwrap();
    let rows = guard.complete_owner_rows().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].change, current);
    assert_eq!(
        rows[0].key,
        RevisionTreeKey {
            source_component: "source_s".into(),
            revision_component: "revision_r".into()
        }
    );
    let member = engine.fs.root().path().join(format!("{TREE}/original.bin"));
    fs::write(&member, b"foreign bytes").unwrap();
    assert_eq!(
        guard.complete_owner_rows().unwrap_err().code,
        ErrorCode::ContentConflict
    );
    fs::write(&member, b"immutable").unwrap();
    fs::write(
        engine
            .fs
            .root()
            .path()
            .join(format!("changes/{}/validation.json", current.change_id)),
        b"altered baseline",
    )
    .unwrap();
    assert!(guard.complete_owner_rows().is_err());
}

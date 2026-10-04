//! Cross-component historical ownership reconstruction and selected lookup.
//!
//! These are real engine-committed immutable asset trees. Their legacy raw path
//! components deliberately have no canonical source/head pointing at the tree,
//! so external deletion does not introduce a broken captured-source fixture.
use super::{
    Catalog, CatalogGraphValidator,
    file_types::{BuildIdentity, CatalogSelection},
    normalized_build::{BuildLimits, CompletedCatalog, NormalizedBuilder},
    query_types::QueryReadLimits,
    scan, selector,
};
use crate::{
    changes::{
        ChangeDraft, ChangeEngine, ChangeStatus, ExpectedWrite, PreparedChange, PublicationBackend,
        PublicationPermit, RevisionOwnershipLookup, RevisionTreeKey, ValidationInput,
    },
    domain::{ErrorCode, ReadSnapshot, RecordId, Result, VaultRelativePath, WikiError},
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};

const SOURCE: &str = "source legacy";
const REVISION: &str = "Revision.Retained";

fn key() -> RevisionTreeKey {
    RevisionTreeKey {
        source_component: SOURCE.into(),
        revision_component: REVISION.into(),
    }
}

fn tree() -> String {
    format!("sources/{SOURCE}/revisions/{REVISION}")
}

fn asset_draft() -> ChangeDraft {
    ChangeDraft {
        title: "Retained legacy revision ownership fixture".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: [
            ("original.bin", b"immutable original\x00\xff".as_slice()),
            ("content.md", b"Immutable legacy capture text.\n".as_slice()),
        ]
        .into_iter()
        .map(|(name, bytes)| ExpectedWrite {
            target: VaultRelativePath::new(format!("{}/{name}", tree())).unwrap(),
            expected: ExpectedState::Absent,
            proposed: Some(bytes.to_vec()),
            apply_after: vec![],
        })
        .collect(),
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    writer: WriterPermit,
    vault: RecordId,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            b"---\nwiki_schema: \"1\"\nwiki_id: vault_ownership_cross\nwiki_kind: vault\ntitle: Disposable ownership cross-component fixture\n---\n",
        )
        .unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
        Self {
            _temp: temp,
            fs,
            writer,
            vault: RecordId::new("vault_ownership_cross").unwrap(),
        }
    }

    fn engine(&self) -> ChangeEngine {
        ChangeEngine::new(self.fs.clone()).unwrap()
    }

    fn catalog(&self) -> Catalog {
        Catalog::new(self.fs.clone(), self.vault.clone())
    }

    fn prepare(&self) -> PreparedChange {
        self.engine()
            .prepare(&self.writer, asset_draft())
            .unwrap()
            .prepared
    }

    fn commit(&self) -> PreparedChange {
        let change = self.prepare();
        let report = self
            .engine()
            .apply(
                &self.writer,
                &change,
                &CatalogGraphValidator,
                &self.catalog(),
            )
            .unwrap();
        assert_eq!(report.status, ChangeStatus::Committed);
        assert_eq!(report.change, change);
        assert!(self.fs.root().path().join(tree()).is_dir());
        assert!(
            self.fs
                .root()
                .path()
                .join(format!("changes/{}/revision-trees.json", change.change_id))
                .is_file()
        );
        change
    }

    fn build(&self) -> Result<CompletedCatalog> {
        self.build_with_limits(BuildLimits::default())
    }

    fn build_with_limits(&self, limits: BuildLimits) -> Result<CompletedCatalog> {
        let identity = BuildIdentity {
            selection: CatalogSelection::new(self.vault.clone(), 1)?,
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&self.fs, &self.writer, &identity.selection)?;
        let mut builder = NormalizedBuilder::begin(&self.fs, &self.writer, identity, limits)?;
        let input = scan::scan_input(&self.fs, &self.vault)?;
        let projection = scan::project_with_sink(&self.fs, &input, false, &mut builder)?;
        builder.finish(&projection)
    }

    fn select(&self, candidate: &CompletedCatalog) {
        selector::publish(
            &self.fs,
            &self.writer,
            &candidate.identity.selection,
            Duration::ZERO,
        )
        .unwrap();
    }
}

#[test]
fn deleted_committed_tree_keeps_exact_historical_owner_after_rebuild_and_selection() {
    let fixture = Fixture::new();
    let committed = fixture.commit();
    fs::remove_dir_all(fixture.fs.root().path().join(tree())).unwrap();
    let completed = fixture.build().unwrap();
    assert_eq!(completed.stats.revision_owners, 1);
    fixture.select(&completed);

    // A selected lookup must not replay unrelated history. Add malformed entries
    // only AFTER reconstruction/activation; they remain outside this read scope.
    for index in 0..64 {
        let directory = fixture
            .fs
            .root()
            .path()
            .join(format!("changes/unrelated-malformed-{index}"));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("change.md"), b"not a changeset").unwrap();
    }
    let reader = fixture
        .catalog()
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    reader.require_ready().unwrap();
    assert!(
        RevisionOwnershipLookup::snapshot(&reader)
            .publication()
            .is_some()
    );
    assert_eq!(reader.revision_owner(&key()).unwrap(), Some(committed));
    assert!(
        reader
            .revision_owner(&RevisionTreeKey {
                source_component: SOURCE.to_uppercase(),
                revision_component: REVISION.into(),
            })
            .unwrap()
            .is_none()
    );
    assert!(
        reader
            .revision_owner(&RevisionTreeKey {
                source_component: SOURCE.into(),
                revision_component: REVISION.to_lowercase(),
            })
            .unwrap()
            .is_none()
    );
    assert!(reader.usage().rows <= 8);
    assert!(!fixture.fs.root().path().join(tree()).exists());
}

#[test]
fn genuinely_aborted_tree_has_no_owner_in_a_ready_selected_registry() {
    let fixture = Fixture::new();
    let change = fixture.prepare();
    assert_eq!(
        fixture
            .engine()
            .abort(&fixture.writer, &change)
            .unwrap()
            .status,
        ChangeStatus::Aborted
    );
    assert!(!fixture.fs.root().path().join(tree()).exists());
    let completed = fixture.build().unwrap();
    assert_eq!(completed.stats.revision_owners, 0);
    fixture.select(&completed);
    let reader = fixture
        .catalog()
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    reader.require_ready().unwrap();
    assert!(reader.revision_owner(&key()).unwrap().is_none());
}

#[test]
fn history_budget_covers_classification_and_zero_owner_receipts() {
    let fixture = Fixture::new();
    for _ in 0..4 {
        let change = fixture.prepare();
        fixture.engine().abort(&fixture.writer, &change).unwrap();
    }
    for max_history_steps in [4, 9] {
        let error = fixture
            .build_with_limits(BuildLimits {
                max_history_steps,
                ..BuildLimits::default()
            })
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert!(error.message.contains("history work ceiling"));
    }
    let completed = fixture.build().unwrap();
    assert_eq!(completed.stats.revision_owners, 0);
    assert!(completed.stats.history_steps > 9);
    fixture.select(&completed);
    fixture
        .catalog()
        .query_snapshot(QueryReadLimits::default())
        .unwrap()
        .require_ready()
        .unwrap();
}

fn copy_fixture_directory(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        assert!(!kind.is_symlink());
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_fixture_directory(&entry.path(), &target);
        } else {
            assert!(kind.is_file());
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn independently_committed_conflicting_history_refuses_a_rebuilt_candidate() {
    let first = Fixture::new();
    let first_owner = first.commit();
    let second = Fixture::new();
    let second_owner = second.commit();
    assert_ne!(first_owner, second_owner);
    // Simulate a divergent vault history merge using two authentic terminal
    // histories for the same vault/key, never fabricated receipts or SQL rows.
    let relative = format!("changes/{}", second_owner.change_id);
    copy_fixture_directory(
        &second.fs.root().path().join(&relative),
        &first.fs.root().path().join(&relative),
    );
    fs::remove_dir_all(first.fs.root().path().join(tree())).unwrap();
    let error = first.build().err().unwrap();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(error.message.contains("claim") || error.message.contains("owner"));
    assert!(
        !first
            .fs
            .root()
            .path()
            .join(".wiki/cache/catalog-current.json")
            .exists()
    );
}

struct InterruptedPublication;
impl PublicationBackend for InterruptedPublication {
    fn check_available(&self) -> Result<()> {
        Ok(())
    }
    fn publish(
        &self,
        _fs: &VaultFs,
        _permit: &PublicationPermit<'_>,
        _input: &ValidationInput,
    ) -> Result<ReadSnapshot> {
        Err(WikiError::new(
            ErrorCode::Internal,
            "injected ownership fixture publication interruption",
        ))
    }
}

#[test]
fn unresolved_files_applied_history_refuses_a_rebuilt_candidate() {
    let fixture = Fixture::new();
    let change = fixture.prepare();
    let error = fixture
        .engine()
        .apply(
            &fixture.writer,
            &change,
            &CatalogGraphValidator,
            &InterruptedPublication,
        )
        .unwrap_err();
    assert!(error.message.contains("injected ownership fixture"));
    assert_eq!(
        fixture.engine().inspect(&change.change_id).unwrap().status,
        ChangeStatus::FilesApplied
    );
    let error = fixture.build().err().unwrap();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    assert!(
        !fixture
            .fs
            .root()
            .path()
            .join(".wiki/cache/catalog-current.json")
            .exists()
    );
}

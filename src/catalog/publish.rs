//! Held-writer publication: ordinary rows precede the atomic FTS/pointer switch.
use super::{scan, sql, types::*};
use crate::{
    changes::{ChangeEngine, ChangeStatus, PublicationBackend, PublicationPermit, ValidationInput},
    domain::{ErrorCode, ReadSnapshot, RecordId, Result, VaultRelativePath, WikiError},
    vault::{VaultFs, WriterPermit},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

impl Catalog {
    pub(crate) fn operation_state(
        &self,
    ) -> Result<Option<crate::changes::operation_authority::Authority>> {
        use crate::changes::operation_authority::{self, Presence};
        let presence = if super::selector::has_activation_evidence(&self.fs)? {
            Presence::Required
        } else {
            Presence::LegacyMayBeAbsent
        };
        operation_authority::load(&self.fs, &self.vault_id, presence)
    }
    pub(crate) fn operation_state_bounded(
        &self,
        budget: &scan::ReadScanBudget,
    ) -> Result<Option<crate::changes::operation_authority::Authority>> {
        use crate::changes::operation_authority::{self, Presence};
        let presence = if super::selector::has_activation_evidence_bounded(&self.fs, budget)? {
            Presence::Required
        } else {
            Presence::LegacyMayBeAbsent
        };
        operation_authority::load_bounded(&self.fs, &self.vault_id, presence, budget)
    }
    pub(crate) fn require_legacy_catalog_bounded(
        &self,
        budget: &scan::ReadScanBudget,
    ) -> Result<()> {
        if self.operation_state_bounded(budget)?.is_some() {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "this command is not yet available with the normalized catalog",
            ));
        }
        Ok(())
    }
    /// Until each legacy path is migrated, never read or publish the predecessor
    /// database behind an activated normalized catalog.
    pub(crate) fn require_legacy_catalog(&self) -> Result<()> {
        if self.operation_state()?.is_some() {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "this command is not yet available with the normalized catalog",
            ));
        }
        Ok(())
    }
    pub(crate) fn guard_query(&self) -> Result<()> {
        let engine = ChangeEngine::new(self.fs.clone())?;
        if engine.vault_id() != &self.vault_id {
            return Err(WikiError::invalid("catalog vault identity changed"));
        }
        if self.operation_state()?.is_some() {
            // Selected evidence is authenticated independently of unrelated
            // updates. The reader enforces the floor observed at acquisition.
            Ok(())
        } else {
            self.guard_current(None)
        }
    }
    pub fn new(fs: VaultFs, vault_id: RecordId) -> Self {
        Self {
            fs,
            vault_id,
            options: CatalogOptions::default(),
        }
    }
    pub fn with_options(fs: VaultFs, vault_id: RecordId, options: CatalogOptions) -> Self {
        Self {
            fs,
            vault_id,
            options,
        }
    }
    pub fn fs(&self) -> &VaultFs {
        &self.fs
    }
    pub fn vault_id(&self) -> &RecordId {
        &self.vault_id
    }
    pub(crate) fn cache_path(&self) -> Result<std::path::PathBuf> {
        self.fs
            .root()
            .resolve(&VaultRelativePath::new(".wiki/cache/index.sqlite")?)
    }
    pub fn check_available(&self) -> Result<()> {
        self.require_legacy_catalog()?;
        if self.options.busy_timeout_ms > 30_000 {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "catalog busy timeout exceeds 30 seconds",
            ));
        }
        sql::capability()?;
        let path = self.cache_path()?;
        if path.exists() {
            let c = sql::open(&path, self.options.busy_timeout_ms, false)?;
            sql::validate(&c)?;
        }
        Ok(())
    }
    pub(crate) fn guard_current(&self, exempt: Option<&PublicationPermit<'_>>) -> Result<()> {
        self.guard_current_checked(exempt, None)
    }
    pub(crate) fn guard_current_bounded(
        &self,
        exempt: Option<&PublicationPermit<'_>>,
        budget: &scan::ReadScanBudget,
    ) -> Result<()> {
        self.guard_current_checked(exempt, Some(budget))
    }
    fn guard_current_checked(
        &self,
        exempt: Option<&PublicationPermit<'_>>,
        budget: Option<&scan::ReadScanBudget>,
    ) -> Result<()> {
        let input = budget
            .map(|budget| scan::BoundedReadInput::new(&self.fs, budget))
            .transpose()?;
        let mut reader = |path: &VaultRelativePath, maximum| match &input {
            Some(input) => input.read_managed(path, maximum),
            None => crate::changes::prepare::read_bounded(&self.fs, path, maximum),
        };
        let engine = ChangeEngine::new_with_reader(self.fs.clone(), &mut reader)?;
        if engine.vault_id() != &self.vault_id {
            return Err(WikiError::invalid("catalog vault identity changed"));
        }
        let authority = match budget {
            Some(budget) => self.operation_state_bounded(budget)?,
            None => self.operation_state()?,
        };
        if let Some(authority) = authority {
            if exempt.is_some() {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "legacy publication cannot update an activated normalized catalog",
                ));
            }
            return authority.require_publication(authority.publication());
        }
        let ids = match (&input, budget) {
            (Some(input), Some(budget)) => engine.change_ids_with_reader(
                &input.resolve(&VaultRelativePath::new("changes")?)?,
                &mut || budget.charge_observation(0, 0, 1),
                &mut reader,
            )?,
            _ => engine.change_ids()?,
        };
        for id in ids {
            let i = engine.inspect_history_with_reader(&id, &mut reader)?;
            if let Some(permit) = exempt.filter(|p| p.change().change_id == id) {
                if i.prepared != *permit.change()
                    || !matches!(i.status, ChangeStatus::FilesApplied | ChangeStatus::Indexed)
                    || i.observations.iter().any(|o| o.observed != o.after)
                {
                    return Err(WikiError::new(
                        ErrorCode::RecoveryRequired,
                        "publication permit is not bound to fully applied change",
                    ));
                }
                continue;
            }
            if matches!(
                i.status,
                ChangeStatus::Applying
                    | ChangeStatus::FilesApplied
                    | ChangeStatus::Indexed
                    | ChangeStatus::Conflict
            ) || i.journal.status.is_none()
                && i.observations.iter().any(|o| o.observed != o.before)
            {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "unresolved apply requires recovery before current catalog publication",
                ));
            }
        }
        Ok(())
    }
    pub fn sync(&self, writer: &WriterPermit) -> Result<SyncReport> {
        self.index(writer, false)
    }
    /// Standalone automatic cache preparation publishes an ordinary durable
    /// legacy generation under the caller's read allowance. Canonical Change
    /// transactions continue using the ordinary publication backend without
    /// this optional read deadline, so entered transactions can finish.
    pub(crate) fn sync_bounded(
        &self,
        writer: &WriterPermit,
        budget: &scan::ReadScanBudget,
    ) -> Result<ReaderSnapshot> {
        writer.require_root(self.fs.root())?;
        self.require_legacy_catalog_bounded(budget)?;
        self.guard_current_bounded(None, budget)?;
        sql::capability_bounded(budget)?;
        let mut c = self.writer_connection_bounded(writer, budget)?;
        for attempt in 0..2 {
            let p = scan::scan_with_budget(&self.fs, &self.vault_id, budget)?;
            super::snapshot::require_projection_size(&p)?;
            match self.publish_projection_checked(&mut c, writer, None, &p, false, Some(budget)) {
                Ok(_) => {
                    self.guard_current_bounded(None, budget)?;
                    let mut reader = self.snapshot_from_connection(c, Some(budget))?;
                    if reader.projection != p {
                        return Err(WikiError::new(
                            ErrorCode::FreshnessConflict,
                            "published automatic projection changed before acquisition",
                        ));
                    }
                    reader.verification = SnapshotVerification::VerifiedSnapshot {
                        verified_at: time::OffsetDateTime::now_utc()
                            .format(&time::format_description::well_known::Rfc3339)
                            .map_err(|_| WikiError::invalid("snapshot UTC encoding"))?,
                    };
                    return Ok(reader);
                }
                Err(error) if error.code == ErrorCode::FreshnessConflict && attempt == 0 => {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!()
    }
    pub fn rebuild(&self, writer: &WriterPermit) -> Result<SyncReport> {
        self.index(writer, true)
    }
    fn index(&self, writer: &WriterPermit, rebuild: bool) -> Result<SyncReport> {
        writer.require_root(self.fs.root())?;
        self.require_legacy_catalog()?;
        self.guard_current(None)?;
        sql::capability()?;
        if !rebuild {
            self.check_available()?;
        }
        let absent = !self.cache_path()?.exists();
        let mut c = self.writer_connection(writer)?;
        let migrating = rebuild && !absent && sql::version(&c)? != sql::SQL_VERSION;
        for attempt in 0..2 {
            let p = scan::scan(&self.fs, &self.vault_id)?;
            let result = if migrating {
                self.migrate_projection(&mut c, writer, &p)
                    .map(|snapshot| (snapshot, false))
            } else {
                self.publish_projection(&mut c, writer, None, &p, rebuild)
            };
            match result {
                Ok((snapshot, reused)) => {
                    let (vector_cache_lost, vector_loss_unknown) = sql::cache_loss_notices(&c)?;
                    return Ok(SyncReport {
                        snapshot,
                        reused,
                        vector_cache_lost,
                        vector_loss_unknown,
                    });
                }
                Err(e) if e.code == ErrorCode::FreshnessConflict && attempt == 0 => continue,
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }
    fn migrate_projection(
        &self,
        c: &mut Connection,
        writer: &WriterPermit,
        p: &CatalogProjection,
    ) -> Result<ReadSnapshot> {
        writer.require_root(self.fs.root())?;
        self.recheck(p, None)?;
        c.pragma_update(None, "foreign_keys", false)
            .map_err(sql::sql_error)?;
        let result = (|| {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql::sql_error)?;
            let lost = sql::reset(&tx)?;
            tx.execute(
                "UPDATE index_meta SET vector_cache_lost=?1,vector_loss_unknown=1",
                [lost],
            )
            .map_err(sql::sql_error)?;
            sql::insert_projection(&tx, 1, p)?;
            self.checkpoint(PublicationCheckpoint::AfterOrdinaryRows)?;
            sql::replace_documents_fts(&tx, 1, p)?;
            self.checkpoint(PublicationCheckpoint::AfterDocumentsFts)?;
            sql::replace_graph_fts(&tx, 1, p)?;
            self.checkpoint(PublicationCheckpoint::AfterGraphFts)?;
            tx.execute("UPDATE generations SET state='complete' WHERE gen=1", [])
                .map_err(sql::sql_error)?;
            tx.execute("UPDATE index_meta SET published_gen=1", [])
                .map_err(sql::sql_error)?;
            self.checkpoint(PublicationCheckpoint::AfterPointer)?;
            self.checkpoint(PublicationCheckpoint::MigrationBeforeCommit)?;
            self.recheck(p, None)?;
            tx.commit().map_err(sql::sql_error)
        })();
        c.pragma_update(None, "foreign_keys", true)
            .map_err(sql::sql_error)?;
        result?;
        self.checkpoint(PublicationCheckpoint::AfterCommit)?;
        Ok(ReadSnapshot::canonical(
            1,
            p.parser_fingerprint.clone(),
            p.control_manifest.clone(),
        ))
    }
    fn writer_connection(&self, writer: &WriterPermit) -> Result<Connection> {
        writer.require_root(self.fs.root())?;
        self.require_legacy_catalog()?;
        self.fs
            .ensure_directory(&VaultRelativePath::new(".wiki/cache")?, writer)?;
        self.fs
            .root()
            .validate_portable_paths(&[VaultRelativePath::new(".wiki/cache/index.sqlite")?])?;
        let mut c = sql::open(&self.cache_path()?, self.options.busy_timeout_ms, true)?;
        let count:i64=c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0)).map_err(sql::sql_error)?;
        if count == 0 && sql::version(&c)? == 0 {
            sql::initialize(&mut c)?;
        }
        Ok(c)
    }
    fn writer_connection_bounded(
        &self,
        writer: &WriterPermit,
        budget: &scan::ReadScanBudget,
    ) -> Result<Connection> {
        writer.require_root(self.fs.root())?;
        self.require_legacy_catalog_bounded(budget)?;
        self.fs.ensure_directory_bounded(
            &VaultRelativePath::new(".wiki/cache")?,
            writer,
            budget,
        )?;
        let input = scan::BoundedReadInput::new(&self.fs, budget)?;
        let path = input.resolve(&VaultRelativePath::new(".wiki/cache/index.sqlite")?)?;
        input.validate_paths(&[VaultRelativePath::new(".wiki/cache/index.sqlite")?])?;
        let mut c = sql::open_bounded(&path, self.options.busy_timeout_ms, true, budget)?;
        sql::clamp_busy(&c, self.options.busy_timeout_ms, budget)?;
        let count: i64 = c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |row| row.get(0)).map_err(sql::sql_error)?;
        if count == 0 && sql::version(&c)? == 0 {
            sql::clamp_busy(&c, self.options.busy_timeout_ms, budget)?;
            sql::initialize(&mut c)?;
        } else {
            // An old schema is an explicit rebuild decision, never an
            // automatic reset or a claim of normalized activation authority.
            sql::clamp_busy(&c, self.options.busy_timeout_ms, budget)?;
            sql::validate(&c)?;
        }
        budget.check()?;
        Ok(c)
    }
    fn checkpoint(&self, point: PublicationCheckpoint) -> Result<()> {
        if let Some(f) = &self.options.fault {
            f.check(point)?;
        }
        Ok(())
    }
    fn publish_projection(
        &self,
        c: &mut Connection,
        writer: &WriterPermit,
        exempt: Option<&PublicationPermit<'_>>,
        p: &CatalogProjection,
        force: bool,
    ) -> Result<(ReadSnapshot, bool)> {
        self.publish_projection_checked(c, writer, exempt, p, force, None)
    }
    #[allow(clippy::too_many_arguments)]
    fn publish_projection_checked(
        &self,
        c: &mut Connection,
        writer: &WriterPermit,
        exempt: Option<&PublicationPermit<'_>>,
        p: &CatalogProjection,
        force: bool,
        budget: Option<&scan::ReadScanBudget>,
    ) -> Result<(ReadSnapshot, bool)> {
        writer.require_root(self.fs.root())?;
        self.guard_current_checked(exempt, budget)?;
        if p.vault_id != self.vault_id {
            return Err(WikiError::invalid("projection belongs to another vault"));
        }
        self.clamp_publication_wait(c, budget)?;
        sql::validate(c)?;
        let snapshot_for = |generation: u64| {
            ReadSnapshot::canonical(
                generation,
                p.parser_fingerprint.clone(),
                p.control_manifest.clone(),
            )
        };
        let existing:Option<(i64,String)>=c.query_row("SELECT g.gen,g.projection_json FROM generations g JOIN index_meta m ON g.gen=m.published_gen WHERE g.state='complete'",[],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql::sql_error)?;
        if let Some((generation, serialized)) = existing {
            let old: CatalogProjection = serde_json::from_str(&serialized)
                .map_err(|e| WikiError::new(ErrorCode::IndexCorrupt, e.to_string()))?;
            if !force && old == *p {
                self.recheck_checked(p, exempt, budget)?;
                super::integrity::validate_projection(c, generation, &old)?;
                // A failed attempt may leave building rows even after canonical
                // bytes revert to the already published view.
                self.clamp_publication_wait(c, budget)?;
                let tx = c
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(sql::sql_error)?;
                sql::prune_generations(&tx, generation)?;
                self.clamp_publication_wait(&tx, budget)?;
                tx.commit().map_err(sql::sql_error)?;
                return Ok((
                    snapshot_for(u64::try_from(generation).map_err(|_| {
                        WikiError::new(ErrorCode::IndexCorrupt, "negative generation")
                    })?),
                    true,
                ));
            }
        }
        let max: i64 = c
            .query_row("SELECT coalesce(max(gen),0) FROM generations", [], |r| {
                r.get(0)
            })
            .map_err(sql::sql_error)?;
        let generation = max.checked_add(1).ok_or_else(|| {
            WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "catalog generation exhausted",
            )
        })?;
        {
            self.clamp_publication_wait(c, budget)?;
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql::sql_error)?;
            sql::insert_projection(&tx, generation, p)?;
            self.clamp_publication_wait(&tx, budget)?;
            tx.commit().map_err(sql::sql_error)?;
        }
        self.checkpoint(PublicationCheckpoint::AfterOrdinaryRows)?;
        self.clamp_publication_wait(c, budget)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql::sql_error)?;
        sql::replace_documents_fts(&tx, generation, p)?;
        self.checkpoint(PublicationCheckpoint::AfterDocumentsFts)?;
        sql::replace_graph_fts(&tx, generation, p)?;
        self.checkpoint(PublicationCheckpoint::AfterGraphFts)?;
        tx.execute(
            "UPDATE generations SET state='complete' WHERE gen=?1",
            [generation],
        )
        .map_err(sql::sql_error)?;
        tx.execute(
            "UPDATE index_meta SET published_gen=?1 WHERE singleton=1",
            [generation],
        )
        .map_err(sql::sql_error)?;
        self.checkpoint(PublicationCheckpoint::AfterPointer)?;
        sql::prune_generations(&tx, generation)?;
        if force {
            sql::rebuild_query_indexes(&tx)?;
        }
        self.recheck_checked(p, exempt, budget)?;
        self.clamp_publication_wait(&tx, budget)?;
        tx.commit().map_err(sql::sql_error)?;
        self.checkpoint(PublicationCheckpoint::AfterCommit)?;
        Ok((
            snapshot_for(
                u64::try_from(generation)
                    .map_err(|_| WikiError::new(ErrorCode::IndexCorrupt, "negative generation"))?,
            ),
            false,
        ))
    }
    fn recheck(&self, p: &CatalogProjection, exempt: Option<&PublicationPermit<'_>>) -> Result<()> {
        self.recheck_checked(p, exempt, None)
    }
    fn clamp_publication_wait(
        &self,
        c: &Connection,
        budget: Option<&scan::ReadScanBudget>,
    ) -> Result<()> {
        if let Some(budget) = budget {
            sql::clamp_busy(c, self.options.busy_timeout_ms, budget)?;
        }
        Ok(())
    }
    fn recheck_checked(
        &self,
        p: &CatalogProjection,
        exempt: Option<&PublicationPermit<'_>>,
        budget: Option<&scan::ReadScanBudget>,
    ) -> Result<()> {
        self.guard_current_checked(exempt, budget)?;
        let unchanged = if let Some(budget) = budget {
            scan::recheck_bounded(&self.fs, p, budget)?;
            self.guard_current_bounded(exempt, budget)?;
            true
        } else {
            scan::scan(&self.fs, &self.vault_id)? == *p
        };
        if !unchanged {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "canonical membership or dependencies changed before publication",
            ));
        }
        Ok(())
    }
}
impl PublicationBackend for Catalog {
    fn check_available(&self) -> Result<()> {
        Catalog::check_available(self)
    }
    fn publish(
        &self,
        fs: &VaultFs,
        permit: &PublicationPermit<'_>,
        input: &ValidationInput,
    ) -> Result<ReadSnapshot> {
        permit.writer().require_root(self.fs.root())?;
        if fs.root() != self.fs.root()
            || permit.vault_id() != &self.vault_id
            || input.vault_id != self.vault_id
        {
            return Err(WikiError::invalid("publication root/vault binding differs"));
        }
        self.guard_current(Some(permit))?;
        let p = scan::project(fs, input)?;
        if p.parser_fingerprint != permit.graph().parser_fingerprint
            || p.control_manifest != permit.graph().control_manifest
            || p.dependencies != permit.graph().dependencies
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "publication projection differs from validated graph permit",
            ));
        }
        // This also detects copied IDs/new decisions absent from the engine's original scan.
        self.recheck(&p, Some(permit))?;
        self.check_available()?;
        let mut c = self.writer_connection(permit.writer())?;
        self.publish_projection(&mut c, permit.writer(), Some(permit), &p, false)
            .map(|v| v.0)
    }
}

#[cfg(test)]
mod automatic_bounded_tests {
    use super::*;
    use crate::vault::VaultRoot;
    use std::{
        fs,
        sync::Arc,
        time::{Duration, Instant},
    };

    fn fixture() -> (tempfile::TempDir, Catalog, WriterPermit) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_id: vault_auto_bound\nwiki_kind: vault\ntitle: Bounded automatic fixture\n---\n").unwrap();
        fs::write(temp.path().join("page.md"), b"---\nwiki_schema: '1'\nwiki_id: page_auto_bound\nwiki_kind: page\ntitle: Cobalt notes\nwiki_status: reviewed\n---\nCobalt observations.\n").unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        let catalog = Catalog::new(vault, RecordId::new("vault_auto_bound").unwrap());
        (temp, catalog, writer)
    }
    fn budget() -> scan::ReadScanBudget {
        scan::ReadScanBudget::new(Default::default()).unwrap()
    }

    #[test]
    fn automatic_missing_cache_publishes_real_generation_and_reuses_it() {
        let (temp, catalog, writer) = fixture();
        assert!(!temp.path().join(".wiki/cache/index.sqlite").exists());
        let local = budget();
        let first = catalog.sync_bounded(&writer, &local).unwrap();
        let generation = first.persistent_generation().unwrap();
        assert!(generation > 0);
        assert!(matches!(
            first.verification(),
            SnapshotVerification::VerifiedSnapshot { .. }
        ));
        assert_eq!(first.document_matches("cobalt", 10).unwrap().len(), 1);
        drop(first);
        let repeat = catalog.sync_bounded(&writer, &local).unwrap();
        assert_eq!(repeat.persistent_generation(), Some(generation));
        catalog.recheck_read_snapshot(&repeat, &local).unwrap();
    }

    struct FailAt(PublicationCheckpoint);
    impl PublicationFault for FailAt {
        fn check(&self, point: PublicationCheckpoint) -> Result<()> {
            if point == self.0 {
                return Err(WikiError::new(
                    ErrorCode::Internal,
                    "bounded publication cut",
                ));
            }
            Ok(())
        }
    }
    #[test]
    fn bounded_publication_cuts_keep_predecessor_and_retry_existing_algorithm() {
        for point in [
            PublicationCheckpoint::AfterOrdinaryRows,
            PublicationCheckpoint::AfterPointer,
        ] {
            let (temp, catalog, writer) = fixture();
            let old = catalog
                .sync_bounded(&writer, &budget())
                .unwrap()
                .persistent_generation()
                .unwrap();
            fs::write(temp.path().join("page.md"), b"Changed amber notes.\n").unwrap();
            let interrupted = Catalog::with_options(
                catalog.fs.clone(),
                catalog.vault_id.clone(),
                CatalogOptions {
                    fault: Some(Arc::new(FailAt(point))),
                    ..Default::default()
                },
            );
            assert_eq!(
                interrupted
                    .sync_bounded(&writer, &budget())
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::Internal
            );
            let predecessor = catalog.index_snapshot().unwrap();
            assert_eq!(predecessor.persistent_generation(), Some(old));
            assert_eq!(predecessor.document_matches("cobalt", 10).unwrap().len(), 1);
            drop(predecessor);
            let recovered = catalog.sync_bounded(&writer, &budget()).unwrap();
            assert!(recovered.persistent_generation().unwrap() > old);
            assert_eq!(recovered.document_matches("amber", 10).unwrap().len(), 1);
        }
    }

    struct ChangeBeforeCommit(std::path::PathBuf);
    impl PublicationFault for ChangeBeforeCommit {
        fn check(&self, point: PublicationCheckpoint) -> Result<()> {
            if point == PublicationCheckpoint::AfterPointer {
                fs::write(
                    &self.0,
                    b"External replacement after provisional pointer.\n",
                )
                .unwrap();
            }
            Ok(())
        }
    }
    #[test]
    fn bounded_pointer_recheck_rejects_raced_membership_without_stale_activation() {
        let (temp, catalog, writer) = fixture();
        let old = catalog
            .sync_bounded(&writer, &budget())
            .unwrap()
            .persistent_generation()
            .unwrap();
        fs::write(
            temp.path().join("page.md"),
            b"Candidate amber observations.\n",
        )
        .unwrap();
        let raced = Catalog::with_options(
            catalog.fs.clone(),
            catalog.vault_id.clone(),
            CatalogOptions {
                fault: Some(Arc::new(ChangeBeforeCommit(temp.path().join("page.md")))),
                ..Default::default()
            },
        );
        // The first candidate is invalidated. The ordinary retry observes and
        // publishes the replacement rather than committing the stale candidate.
        assert!(raced.sync_bounded(&writer, &budget()).is_ok());
        let current = catalog.index_snapshot().unwrap();
        assert!(current.persistent_generation().unwrap() > old);
        assert_eq!(current.document_matches("amber", 10).unwrap().len(), 0);
        assert_eq!(
            current.document_matches("replacement", 10).unwrap().len(),
            1
        );
    }

    #[test]
    fn bounded_publication_does_not_reset_an_old_sql_schema() {
        let (temp, catalog, writer) = fixture();
        fs::create_dir_all(temp.path().join(".wiki/cache")).unwrap();
        let db = temp.path().join(".wiki/cache/index.sqlite");
        let old = Connection::open(&db).unwrap();
        old.execute_batch("CREATE TABLE preserved(value); INSERT INTO preserved VALUES ('old'); PRAGMA user_version=7;").unwrap();
        drop(old);
        assert_eq!(
            catalog.sync_bounded(&writer, &budget()).err().unwrap().code,
            ErrorCode::CapabilityUnavailable
        );
        let check = Connection::open(&db).unwrap();
        assert_eq!(sql::version(&check).unwrap(), 7);
        assert_eq!(
            check
                .query_row("SELECT value FROM preserved", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "old"
        );
    }

    #[test]
    fn authority_exhaustion_refuses_before_protected_reads() {
        let (_temp, catalog, _writer) = fixture();
        let local = scan::ReadScanBudget::new(scan::ReadScanLimits {
            max_bytes: 1,
            ..Default::default()
        })
        .unwrap();
        super::super::query_diagnostics::begin();
        let error = catalog.require_legacy_catalog_bounded(&local).unwrap_err();
        let observed = super::super::query_diagnostics::end();
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert!(observed.reads.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn protected_authority_path_failure_remains_an_error() {
        let (temp, catalog, _writer) = fixture();
        fs::create_dir_all(temp.path().join(".wiki/cache")).unwrap();
        fs::write(temp.path().join("foreign.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(
            temp.path().join("foreign.json"),
            temp.path().join(".wiki/cache/catalog-v2-active.json"),
        )
        .unwrap();
        assert!(catalog.require_legacy_catalog_bounded(&budget()).is_err());
    }

    #[test]
    fn canonical_and_retained_history_share_one_file_and_entry_allowance() {
        let (temp, catalog, writer) = fixture();
        fs::create_dir(temp.path().join("changes")).unwrap();
        for n in 0..24 {
            fs::create_dir(temp.path().join(format!("changes/change_bound_{n}"))).unwrap();
        }
        let local = scan::ReadScanBudget::new(scan::ReadScanLimits {
            max_files: 16,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            catalog.sync_bounded(&writer, &local).err().unwrap().code,
            ErrorCode::BudgetExceeded
        );
        assert!(!temp.path().join(".wiki/cache/index.sqlite").exists());
        let local = scan::ReadScanBudget::new(scan::ReadScanLimits {
            max_entries: 16,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            catalog.sync_bounded(&writer, &local).err().unwrap().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn bounded_legacy_publication_refuses_existing_normalized_authority() {
        let (temp, catalog, writer) = fixture();
        use crate::changes::operation_authority::{self, Presence, Publication};
        operation_authority::activate(
            catalog.fs(),
            &writer,
            catalog.vault_id(),
            Publication {
                file_id: "0123456789abcdef0123456789abcdef".into(),
                epoch: 1,
            },
            Presence::LegacyMayBeAbsent,
        )
        .unwrap();
        assert_eq!(
            catalog.sync_bounded(&writer, &budget()).err().unwrap().code,
            ErrorCode::CapabilityUnavailable
        );
        assert!(!temp.path().join(".wiki/cache/index.sqlite").exists());
    }

    #[test]
    fn bounded_sql_lock_wait_obeys_remaining_deadline() {
        let (_temp, catalog, writer) = fixture();
        drop(catalog.sync_bounded(&writer, &budget()).unwrap());
        let blocker = Connection::open(catalog.cache_path().unwrap()).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let configured = Catalog::with_options(
            catalog.fs.clone(),
            catalog.vault_id.clone(),
            CatalogOptions {
                busy_timeout_ms: 30_000,
                ..Default::default()
            },
        );
        let start = Instant::now();
        let local = scan::ReadScanBudget::with_deadline(
            Default::default(),
            start + Duration::from_millis(75),
        )
        .unwrap();
        assert!(configured.sync_bounded(&writer, &local).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        blocker.execute_batch("ROLLBACK").unwrap();
    }
}

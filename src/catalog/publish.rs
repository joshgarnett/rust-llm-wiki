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
        let engine = ChangeEngine::new(self.fs.clone())?;
        if engine.vault_id() != &self.vault_id {
            return Err(WikiError::invalid("catalog vault identity changed"));
        }
        if let Some(authority) = self.operation_state()? {
            if exempt.is_some() {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "legacy publication cannot update an activated normalized catalog",
                ));
            }
            return authority.require_publication(authority.publication());
        }
        for id in engine.change_ids()? {
            let i = engine.inspect_history(&id)?;
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
        writer.require_root(self.fs.root())?;
        self.guard_current(exempt)?;
        if p.vault_id != self.vault_id {
            return Err(WikiError::invalid("projection belongs to another vault"));
        }
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
                self.recheck(p, exempt)?;
                super::integrity::validate_projection(c, generation, &old)?;
                // A failed attempt may leave building rows even after canonical
                // bytes revert to the already published view.
                let tx = c
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(sql::sql_error)?;
                sql::prune_generations(&tx, generation)?;
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
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql::sql_error)?;
            sql::insert_projection(&tx, generation, p)?;
            tx.commit().map_err(sql::sql_error)?;
        }
        self.checkpoint(PublicationCheckpoint::AfterOrdinaryRows)?;
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
        self.recheck(p, exempt)?;
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
        self.guard_current(exempt)?;
        if scan::scan(&self.fs, &self.vault_id)? != *p {
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

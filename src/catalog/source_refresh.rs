//! Retained bounded source-update publication, sharing the canonical change engine.
//!
//! SQL actions are retained before canonical mutation. Recovery recognizes the
//! exact intended header before trying to reopen a superseded starting snapshot.
use super::{
    Catalog, PublicationCheckpoint,
    normalized_delta::CatalogDelta,
    normalized_read,
    query::QuerySnapshot,
    query_types::{QueryCatalog, QueryReadLimits},
    scan, selector, sql,
};
use crate::{
    changes::{
        ChangeEngine, ChangeStatus, PreparedChange, ReadDependency, RevisionOwnerRow,
        RevisionOwnershipLookup,
        indexed_refresh::{IndexedRefreshPhase, IndexedRefreshProof},
        journal,
        operation_authority::{self, ActiveOperation, Authority, Presence, Publication},
        prepare::{MAX_PAYLOAD_BYTES, read_bounded, strict_json},
    },
    domain::{Blake3Hash, ErrorCode, ReadSnapshot, RecordId, Result, VaultRelativePath, WikiError},
    vault::{ExpectedState, WriterPermit},
};
use rusqlite::{Transaction, TransactionBehavior, limits::Limit, params};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::BTreeMap,
    time::{Duration, Instant},
};

const DELTA_VERSION: u32 = 2;
const MAX_DELTA_BYTES: usize = 256 * 1024 * 1024;
const MAX_SELECTED_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedDelta {
    version: u32,
    vault_id: RecordId,
    source_id: RecordId,
    change: PreparedChange,
    base: ReadSnapshot,
    before: Vec<ReadDependency>,
    after: Vec<ReadDependency>,
    rows: CatalogDelta,
}

pub(crate) struct IndexedRefreshSession<'a> {
    catalog: Catalog,
    writer: &'a WriterPermit,
    sql_writer: selector::DeltaWriter<'a>,
    // Only AtBase opens this read transaction. It never supplies finalization authority.
    starting: Option<QuerySnapshot>,
    proof: IndexedRefreshProof,
    delta: RetainedDelta,
    phase: IndexedRefreshPhase,
    selected_bytes_remaining: Cell<usize>,
}

fn recovery(message: &str) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn publication(snapshot: &ReadSnapshot) -> Result<Publication> {
    let binding = snapshot
        .publication()
        .ok_or_else(|| recovery("refresh requires a published epoch"))?;
    Ok(Publication {
        file_id: binding.file_id.clone(),
        epoch: snapshot.generation,
    })
}
fn delta_path(change: &PreparedChange) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("changes/{}/indexed-delta.json", change.change_id))
}
fn intended(
    base: &ReadSnapshot,
    change: &PreparedChange,
    delta_hash: &Blake3Hash,
) -> Result<ReadSnapshot> {
    let binding = base
        .publication()
        .ok_or_else(|| recovery("refresh base is not a published epoch"))?;
    let hash = Blake3Hash::digest(
        serde_json::to_vec(&(
            "lwiki.source-refresh-publication.v1",
            base,
            change,
            delta_hash,
        ))
        .map_err(|error| WikiError::invalid(error.to_string()))?,
    );
    ReadSnapshot::published(
        base.generation
            .checked_add(1)
            .ok_or_else(|| recovery("refresh epoch exhausted"))?,
        base.parser_fingerprint.clone(),
        binding.file_id.clone(),
        hash,
    )
}

impl<'a> IndexedRefreshSession<'a> {
    /// Prepare and retain only a complete semantic projection. The private
    /// capability cannot be constructed from caller-supplied row actions.
    pub(crate) fn prepare_projected(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        projected: super::source_projection::ProjectedSourceRefresh,
    ) -> Result<Self> {
        writer.require_root(catalog.fs.root())?;
        let mut parts = projected.into_parts();
        if parts.delta.version != 2 || !parts.delta.owners.is_empty() {
            return Err(recovery(
                "projected refresh has an invalid publication envelope",
            ));
        }
        parts.delta.validate()?;
        let query = catalog.query_snapshot(QueryReadLimits::default())?;
        if QueryCatalog::snapshot(&query) != &parts.base {
            return Err(recovery(
                "projected refresh base changed before preparation",
            ));
        }
        RevisionOwnershipLookup::require_ready(&query)?;
        parts
            .delta
            .require_layout(QueryCatalog::connection(&query))?;
        drop(query);
        let engine = ChangeEngine::new(catalog.fs.clone())?;
        let change = engine.prepare(writer, parts.draft)?.prepared;
        parts.delta.owners = engine.manifest_revision_owners(&change)?;
        Self::retain_bound(
            catalog,
            writer,
            parts.source_id,
            change,
            parts.base,
            parts.before,
            parts.after,
            parts.delta,
            DELTA_VERSION,
        )
    }

    /// Raw row actions are only a recovery-mechanics fixture constructor.
    /// Their legacy envelope is never accepted by production replay.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn retain(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        source_id: RecordId,
        change: PreparedChange,
        base: ReadSnapshot,
        before: Vec<ReadDependency>,
        after: Vec<ReadDependency>,
        rows: CatalogDelta,
    ) -> Result<Self> {
        Self::retain_bound(
            catalog, writer, source_id, change, base, before, after, rows, 1,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn retain_bound(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        source_id: RecordId,
        change: PreparedChange,
        base: ReadSnapshot,
        before: Vec<ReadDependency>,
        after: Vec<ReadDependency>,
        rows: CatalogDelta,
        version: u32,
    ) -> Result<Self> {
        writer.require_root(catalog.fs.root())?;
        rows.validate()?;
        let delta = RetainedDelta {
            version,
            vault_id: catalog.vault_id.clone(),
            source_id,
            change,
            base,
            before,
            after,
            rows,
        };
        super::normalized_delta::counted(&delta, MAX_DELTA_BYTES)?;
        let bytes =
            serde_json::to_vec(&delta).map_err(|error| WikiError::invalid(error.to_string()))?;
        if bytes.len() > MAX_DELTA_BYTES {
            return Err(budget("retained refresh delta exceeds byte ceiling"));
        }
        let delta_hash = Blake3Hash::digest(&bytes);
        let proof = IndexedRefreshProof {
            version: 2,
            vault_id: delta.vault_id.clone(),
            source_id: delta.source_id.clone(),
            change: delta.change.clone(),
            base: delta.base.clone(),
            intended: intended(&delta.base, &delta.change, &delta_hash)?,
            delta_hash,
            before: delta.before.clone(),
            after: delta.after.clone(),
        };
        // Check the complete envelope and selected catalog before retaining an
        // intent. Nothing here mutates canonical files or advances publication.
        let session = Self::open(catalog, writer, proof, delta)?;
        if session.phase != IndexedRefreshPhase::AtBase {
            return Err(recovery(
                "new refresh plan requires its exact starting publication",
            ));
        }
        let path = delta_path(&session.proof.change)?;
        match read_bounded(&catalog.fs, &path, MAX_DELTA_BYTES)? {
            Some(retained) if retained == bytes => {
                journal::require_sync(catalog.fs.sync_target(&path, writer)?)?;
            }
            Some(_) => {
                return Err(recovery(
                    "refresh delta already exists with different bytes",
                ));
            }
            None => {
                let stage = catalog.fs.stage(&path, &bytes, writer)?;
                journal::require_sync(catalog.fs.replace(
                    stage,
                    &ExpectedState::Absent,
                    writer,
                )?)?;
            }
        }
        session.validate_before_files()?;
        Ok(session)
    }

    pub(crate) fn resume(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        proof: IndexedRefreshProof,
    ) -> Result<Self> {
        let bytes = read_bounded(&catalog.fs, &delta_path(&proof.change)?, MAX_DELTA_BYTES)?
            .ok_or_else(|| recovery("retained refresh delta is missing"))?;
        if Blake3Hash::digest(&bytes) != proof.delta_hash {
            return Err(recovery("retained refresh delta hash changed"));
        }
        let delta = strict_json(&bytes)?;
        Self::open(catalog, writer, proof, delta)
    }

    fn open(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        proof: IndexedRefreshProof,
        delta: RetainedDelta,
    ) -> Result<Self> {
        writer.require_root(catalog.fs.root())?;
        if catalog.options.busy_timeout_ms > 30_000 {
            return Err(recovery("refresh timeout exceeds ceiling"));
        }
        let engine = ChangeEngine::new(catalog.fs.clone())?;
        let (manifest, hash) = engine.load_manifest_structure(&proof.change.change_id)?;
        proof.validate_manifest(&manifest)?;
        if hash != proof.change.manifest_hash
            || (delta.version != DELTA_VERSION && !(cfg!(test) && delta.version == 1))
            || (delta.version == DELTA_VERSION && delta.rows.version != 2)
            || proof.vault_id != catalog.vault_id
            || delta.vault_id != proof.vault_id
            || delta.source_id != proof.source_id
            || delta.change != proof.change
            || delta.base != proof.base
            || delta.before != proof.before
            || delta.after != proof.after
            || proof.base.parser_fingerprint != scan::parser_fingerprint()
            || intended(&proof.base, &proof.change, &proof.delta_hash)? != proof.intended
        {
            return Err(recovery(
                "refresh delta, manifest, or replay version differs from retained proof",
            ));
        }
        delta.rows.validate()?;
        if delta.rows.owners != engine.manifest_revision_owners(&proof.change)? {
            return Err(recovery(
                "refresh delta owners differ from exact manifest roots",
            ));
        }
        let after: BTreeMap<_, _> = proof
            .after
            .iter()
            .map(|dep| (&dep.path, &dep.expected))
            .collect();
        if delta
            .rows
            .dependencies
            .iter()
            .any(|dep| after.get(&dep.path).copied() != Some(&dep.expected))
        {
            return Err(recovery(
                "refresh catalog dependency is outside the selected after state",
            ));
        }
        let timeout = Duration::from_millis(catalog.options.busy_timeout_ms);
        let selection = selector::delta_selection(&catalog.fs, writer, &catalog.vault_id, timeout)?;
        selector::ensure_delta_ready(&catalog.fs, writer, &selection, timeout)?;
        let sql_writer = selector::open_delta(&catalog.fs, writer, &selection, timeout)?;
        configure_delta(sql_writer.connection())?;
        // This index was added with bounded owned-claim replacement. An older
        // internally activated v3 sibling must refuse BEFORE canonical writes,
        // rather than discovering the missing access path during publication.
        sql_writer.connection().prepare(
            "SELECT record_id FROM identity_claims INDEXED BY identity_claim_paths WHERE path=?1 LIMIT 1",
        ).map_err(sql::sql_error)?;
        delta.rows.require_layout(sql_writer.connection())?;
        let header = normalized_read::header(sql_writer.connection(), &selection)?;
        // Inspect exact intended origin FIRST. Never open the obsolete base in
        // the committed-SQL recovery case.
        let phase = if header.snapshot == proof.intended
            && header.origin.as_ref().is_some_and(|origin| {
                origin.change_id == proof.change.change_id
                    && origin.manifest_hash == proof.change.manifest_hash
            }) {
            IndexedRefreshPhase::AlreadyPublished
        } else if header.snapshot == proof.base {
            IndexedRefreshPhase::AtBase
        } else {
            return Err(recovery(
                "refresh catalog is neither exact base nor intended publication",
            ));
        };
        let authority =
            operation_authority::load(&catalog.fs, &catalog.vault_id, Presence::Required)?
                .ok_or_else(|| recovery("refresh authority is absent"))?;
        require_authority(&authority, &proof, phase, false)?;
        let starting = if phase == IndexedRefreshPhase::AtBase {
            let query = catalog.query_snapshot(QueryReadLimits::default())?;
            if QueryCatalog::snapshot(&query) != &proof.base {
                return Err(recovery(
                    "refresh base changed before ownership lookup opened",
                ));
            }
            RevisionOwnershipLookup::require_ready(&query)?;
            Some(query)
        } else {
            None
        };
        let catalog = Catalog::with_options(
            catalog.fs.clone(),
            catalog.vault_id.clone(),
            catalog.options.clone(),
        );
        let session = Self {
            catalog,
            writer,
            sql_writer,
            starting,
            proof,
            delta,
            phase,
            selected_bytes_remaining: Cell::new(MAX_SELECTED_BYTES),
        };
        if phase == IndexedRefreshPhase::AlreadyPublished {
            session.verify_published()?;
        }
        Ok(session)
    }

    pub(crate) fn proof(&self) -> &IndexedRefreshProof {
        &self.proof
    }
    pub(crate) fn phase(&self) -> IndexedRefreshPhase {
        self.phase
    }
    pub(crate) fn starting_ownership_lookup(&self) -> Result<&dyn RevisionOwnershipLookup> {
        self.starting
            .as_ref()
            .map(|query| query as &dyn RevisionOwnershipLookup)
            .ok_or_else(|| recovery("published refresh has no starting ownership capability"))
    }
    fn retained(&self) -> Result<()> {
        let bytes = read_bounded(
            &self.catalog.fs,
            &delta_path(&self.proof.change)?,
            MAX_DELTA_BYTES,
        )?
        .ok_or_else(|| recovery("retained refresh delta disappeared"))?;
        if Blake3Hash::digest(&bytes) != self.proof.delta_hash {
            return Err(recovery("retained refresh delta changed"));
        }
        Ok(())
    }
    fn authority(&self, active: bool) -> Result<()> {
        self.writer.require_root(self.catalog.fs.root())?;
        let authority = operation_authority::load(
            &self.catalog.fs,
            &self.catalog.vault_id,
            Presence::Required,
        )?
        .ok_or_else(|| recovery("refresh authority disappeared"))?;
        require_authority(&authority, &self.proof, self.phase, active)
    }
    pub(crate) fn validate_before_files(&self) -> Result<()> {
        if self.phase != IndexedRefreshPhase::AtBase {
            return Err(recovery("refresh is already published"));
        }
        self.retained()?;
        self.authority(false)?;
        let header =
            normalized_read::header(self.sql_writer.connection(), self.sql_writer.selection())?;
        if header.snapshot != self.proof.base {
            return Err(recovery("refresh base publication changed"));
        }
        // Only exact operation targets may already contain their planned after
        // bytes during replay; unchanged selected facts must still match.
        self.verify_selected(false, true)
    }

    pub(crate) fn verify_selected(&self, after_only: bool, allow_mixed: bool) -> Result<()> {
        if after_only {
            verify_dependencies(
                &self.catalog,
                &self.proof.after,
                None,
                &self.selected_bytes_remaining,
            )
        } else {
            verify_dependencies(
                &self.catalog,
                &self.proof.before,
                allow_mixed.then_some(self.proof.after.as_slice()),
                &self.selected_bytes_remaining,
            )
        }
    }

    pub(crate) fn publish(&mut self, owners: &[RevisionOwnerRow]) -> Result<ReadSnapshot> {
        if self.phase != IndexedRefreshPhase::AtBase {
            return Err(recovery("refresh publication already occurred"));
        }
        self.retained()?;
        self.authority(true)?;
        let engine = ChangeEngine::new(self.catalog.fs.clone())?;
        let (manifest, hash) = engine.load_manifest_structure(&self.proof.change.change_id)?;
        self.proof.validate_manifest(&manifest)?;
        if hash != self.proof.change.manifest_hash
            || owners != self.delta.rows.owners
            || owners != engine.manifest_revision_owners(&self.proof.change)?
        {
            return Err(recovery("refresh publication owner or manifest mismatch"));
        }
        let state = journal::load_journal(&self.catalog.fs, &manifest, &hash)?;
        if !matches!(
            state.status,
            Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed)
        ) {
            return Err(recovery(
                "refresh publication requires durable files-applied intent",
            ));
        }
        self.verify_selected(true, false)?;
        // Recheck complete immutable membership and indexed competing ownership
        // at the publication boundary, even if a caller supplied a matching slice.
        let guard = engine.indexed_revision_guard(
            self.writer,
            &self.proof.change,
            self.starting_ownership_lookup()?,
        )?;
        if guard.complete_owner_rows()? != owners {
            return Err(recovery("refresh owner membership changed"));
        }
        drop(guard);
        let connection = self.sql_writer.connection();
        configure_delta(connection)?;
        let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
            .map_err(sql::sql_error)?;
        let header = normalized_read::header(&transaction, self.sql_writer.selection())?;
        if header.snapshot != self.proof.base {
            return Err(recovery("refresh base changed before transaction"));
        }
        self.authority(true)?;
        self.delta.rows.apply(&transaction)?;
        let after_binding = self
            .proof
            .intended
            .publication()
            .expect("validated published proof");
        let base_binding = self
            .proof
            .base
            .publication()
            .expect("validated published base");
        let changed = transaction.execute(
            "UPDATE catalog_meta SET epoch=?1,publication_hash=?2,origin_change_id=?3,origin_manifest_hash=?4,audit_epoch=NULL,control_hash=NULL,dependency_hash=NULL WHERE singleton=1 AND file_id=?5 AND epoch=?6 AND publication_hash=?7 AND parser_hash=?8 AND state='complete'",
            params![sql::integer(self.proof.intended.generation)?,after_binding.publication_hash.as_str(),
                self.proof.change.change_id.as_str(),self.proof.change.manifest_hash.as_str(),base_binding.file_id,
                sql::integer(self.proof.base.generation)?,base_binding.publication_hash.as_str(),self.proof.base.parser_fingerprint.as_str()],
        ).map_err(sql::sql_error)?;
        if changed != 1 {
            return Err(recovery("refresh publication compare-and-swap failed"));
        }
        if let Some(fault) = &self.catalog.options.fault {
            fault.check(PublicationCheckpoint::AfterPointer)?;
        }
        self.verify_selected(true, false)?;
        self.authority(true)?;
        transaction.commit().map_err(sql::sql_error)?;
        // Set this before a fault can return: this session can never publish twice.
        self.phase = IndexedRefreshPhase::AlreadyPublished;
        self.starting = None;
        if let Some(fault) = &self.catalog.options.fault {
            fault.check(PublicationCheckpoint::AfterCommit)?;
        }
        self.verify_published()
    }

    pub(crate) fn verify_published(&self) -> Result<ReadSnapshot> {
        configure_delta(self.sql_writer.connection())?;
        self.retained()?;
        self.authority(false)?;
        let header =
            normalized_read::header(self.sql_writer.connection(), self.sql_writer.selection())?;
        if header.snapshot != self.proof.intended
            || !header.origin.as_ref().is_some_and(|origin| {
                origin.change_id == self.proof.change.change_id
                    && origin.manifest_hash == self.proof.change.manifest_hash
            })
        {
            return Err(recovery("intended refresh publication or origin changed"));
        }
        let engine = ChangeEngine::new(self.catalog.fs.clone())?;
        let (manifest, hash) = engine.load_manifest_structure(&self.proof.change.change_id)?;
        self.proof.validate_manifest(&manifest)?;
        if hash != self.proof.change.manifest_hash
            || self.delta.rows.owners != engine.manifest_revision_owners(&self.proof.change)?
        {
            return Err(recovery("published refresh manifest ownership changed"));
        }
        let state = journal::load_journal(&self.catalog.fs, &manifest, &hash)?;
        if let Some(terminal) =
            crate::changes::outcome::terminal_report(&self.catalog.fs, &manifest, &hash)?
        {
            if terminal.status != ChangeStatus::Committed
                || terminal.snapshot.as_ref() != Some(&self.proof.intended)
            {
                return Err(recovery(
                    "refresh terminal receipt differs from intended publication",
                ));
            }
            // A valid retained terminal transcript remains proof even when the
            // operational journal is only a prefix. terminal_report checks that
            // exact prefix relationship; the receipt is durable authority.
        } else {
            if !matches!(
                state.status,
                Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed)
            ) {
                return Err(recovery(
                    "published refresh lacks durable apply intent or terminal receipt",
                ));
            }
            if state.status == Some(ChangeStatus::Indexed)
                && state
                    .frames
                    .iter()
                    .rev()
                    .find_map(|frame| match &frame.event {
                        crate::changes::ChangeEvent::Indexed { snapshot } => Some(snapshot),
                        _ => None,
                    })
                    != Some(&self.proof.intended)
            {
                return Err(recovery(
                    "indexed refresh journal names another publication",
                ));
            }
        }
        let ready: bool = self
            .sql_writer
            .connection()
            .query_row(
                "SELECT revision_ownership_version=1 FROM catalog_meta WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(sql::sql_error)?;
        if !ready {
            return Err(recovery("published revision ownership registry is unready"));
        }
        for owner in &self.delta.rows.owners {
            let exists: bool = self.sql_writer.connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM revision_tree_owners WHERE source_component=?1 AND revision_component=?2 AND change_id=?3 AND manifest_hash=?4)",
                params![owner.key.source_component,owner.key.revision_component,owner.change.change_id.as_str(),owner.change.manifest_hash.as_str()], |row| row.get(0),
            ).map_err(sql::sql_error)?;
            if !exists {
                return Err(recovery(
                    "published refresh owner row differs from exact change",
                ));
            }
        }
        self.verify_selected(true, false)?;
        Ok(header.snapshot)
    }
}

fn require_authority(
    authority: &Authority,
    proof: &IndexedRefreshProof,
    phase: IndexedRefreshPhase,
    require_active: bool,
) -> Result<()> {
    let base = publication(&proof.base)?;
    let intended = publication(&proof.intended)?;
    if let Some(active) = authority.active() {
        if active
            != &(ActiveOperation {
                change: proof.change.clone(),
                starting: base.clone(),
                intended,
            })
            || authority.publication() != &base
        {
            return Err(recovery(
                "refresh active authority differs from retained operation",
            ));
        }
    } else if require_active
        || authority.publication()
            != if phase == IndexedRefreshPhase::AtBase {
                &base
            } else {
                &intended
            }
    {
        return Err(recovery(
            "refresh acknowledged authority differs from required publication",
        ));
    }
    Ok(())
}

fn verify_dependencies(
    catalog: &Catalog,
    expected: &[ReadDependency],
    replay_after: Option<&[ReadDependency]>,
    remaining: &Cell<usize>,
) -> Result<()> {
    for (index, dep) in expected.iter().enumerate() {
        let bytes = read_bounded(
            &catalog.fs,
            &dep.path,
            MAX_PAYLOAD_BYTES.min(remaining.get()),
        )?;
        remaining.set(
            remaining
                .get()
                .checked_sub(bytes.as_ref().map_or(0, Vec::len))
                .ok_or_else(|| budget("selected refresh verification exceeds byte ceiling"))?,
        );
        let actual = bytes.map_or(ExpectedState::Absent, |bytes| {
            ExpectedState::Hash(Blake3Hash::digest(bytes))
        });
        let after = replay_after.and_then(|deps| deps.get(index));
        if actual != dep.expected && !after.is_some_and(|dep| actual == dep.expected) {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                format!("selected refresh dependency changed: {}", dep.path),
            ));
        }
    }
    Ok(())
}

fn configure_delta(connection: &rusqlite::Connection) -> Result<()> {
    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, 8 * 1024 * 1024)
        .map_err(sql::sql_error)?;
    connection
        .execute_batch("PRAGMA mmap_size=0; PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;")
        .map_err(sql::sql_error)?;
    let started = Instant::now();
    let mut steps = 0u64;
    connection
        .progress_handler(
            1000,
            Some(move || {
                steps += 1000;
                steps > 10_000_000 || started.elapsed() > Duration::from_secs(30)
            }),
        )
        .map_err(sql::sql_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{VaultFs, VaultRoot};

    #[test]
    fn repeated_selected_verification_shares_one_byte_allowance() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_budget\nwiki_kind: vault\ntitle: Budget fixture\n---\n").unwrap();
        std::fs::write(temp.path().join("selected.md"), b"four").unwrap();
        let catalog = Catalog::new(
            VaultFs::new(VaultRoot::explicit(temp.path()).unwrap()),
            RecordId::new("vault_budget").unwrap(),
        );
        let dependencies = vec![ReadDependency {
            path: VaultRelativePath::new("selected.md").unwrap(),
            expected: ExpectedState::Hash(Blake3Hash::digest(b"four")),
        }];
        let remaining = Cell::new(8);
        verify_dependencies(&catalog, &dependencies, None, &remaining).unwrap();
        assert_eq!(remaining.get(), 4);
        verify_dependencies(&catalog, &dependencies, None, &remaining).unwrap();
        assert_eq!(remaining.get(), 0);
        assert!(verify_dependencies(&catalog, &dependencies, None, &remaining).is_err());
        assert_eq!(remaining.get(), 0);
    }
}

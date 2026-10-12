//! One connection/read transaction pins ordinary and FTS rows to one generation.
use super::{scan, sql, types::*};
use crate::{
    domain::{ErrorCode, ReadSnapshot, RecordKind, Result, WikiError},
    vault::WriterPermit,
};
use rusqlite::params;
use std::time::Instant;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const MAX_READ_PROJECTION_BYTES: i64 = 256 * 1024 * 1024;

struct ProjectionSize(usize);
impl std::io::Write for ProjectionSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|length| *length <= MAX_READ_PROJECTION_BYTES as usize)
            .ok_or_else(|| {
                std::io::Error::other("read projection allocation allowance exceeded")
            })?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn require_projection_size(projection: &CatalogProjection) -> Result<()> {
    serde_json::to_writer(ProjectionSize(0), projection).map_err(|_| {
        WikiError::new(
            ErrorCode::BudgetExceeded,
            "reconstructed catalog projection exceeds the read allocation allowance",
        )
    })
}

impl Catalog {
    /// Rebuild a verified read view in memory, including FTS. This creates no
    /// cache files and permits local research after disposable cache deletion.
    pub fn canonical_snapshot(&self) -> Result<ReaderSnapshot> {
        self.canonical_snapshot_with_budget(&scan::ReadScanBudget::new(
            scan::ReadScanLimits::default(),
        )?)
    }

    fn canonical_snapshot_with_budget(
        &self,
        budget: &scan::ReadScanBudget,
    ) -> Result<ReaderSnapshot> {
        self.require_legacy_catalog_bounded(budget)?;
        self.guard_current_bounded(None, budget)?;
        budget.check()?;
        let projection = scan::scan_with_budget(&self.fs, &self.vault_id, budget)?;
        // SQL publication serializes the projection as one owned string. Count
        // its encoding first so the aggregate allocation remains admitted.
        require_projection_size(&projection)?;
        budget.check()?;
        let mut connection = rusqlite::Connection::open_in_memory().map_err(sql::sql_error)?;
        let deadline = budget.deadline();
        connection
            .progress_handler(1000, Some(move || Instant::now() >= deadline))
            .map_err(sql::sql_error)?;
        sql::configure(&connection, self.options.busy_timeout_ms, true)?;
        sql::initialize(&mut connection)?;
        let transaction = connection.transaction().map_err(sql::sql_error)?;
        sql::insert_projection(&transaction, 1, &projection)?;
        sql::replace_documents_fts(&transaction, 1, &projection)?;
        sql::replace_graph_fts(&transaction, 1, &projection)?;
        transaction
            .execute("UPDATE generations SET state='complete' WHERE gen=1", [])
            .map_err(sql::sql_error)?;
        transaction
            .execute("UPDATE index_meta SET published_gen=1", [])
            .map_err(sql::sql_error)?;
        transaction.commit().map_err(sql::sql_error)?;
        scan::recheck_bounded(&self.fs, &projection, budget)?;
        self.require_legacy_catalog_bounded(budget)?;
        self.guard_current_bounded(None, budget)?;
        connection
            .execute_batch("BEGIN DEFERRED")
            .map_err(sql::sql_error)?;
        // Retrieval has its own bounds; the reconstruction deadline must not
        // become a time bomb attached to a successfully obtained reader.
        connection
            .progress_handler(0, None::<fn() -> bool>)
            .map_err(sql::sql_error)?;
        let snapshot = ReadSnapshot::canonical(
            1,
            projection.parser_fingerprint.clone(),
            projection.control_manifest.clone(),
        );
        Ok(ReaderSnapshot {
            connection,
            snapshot,
            projection,
            verification: SnapshotVerification::VerifiedSnapshot {
                verified_at: OffsetDateTime::now_utc()
                    .format(&Rfc3339)
                    .map_err(|_| WikiError::invalid("snapshot UTC encoding"))?,
            },
        })
    }
    pub fn index_snapshot(&self) -> Result<ReaderSnapshot> {
        self.require_legacy_catalog()?;
        let path = self.cache_path()?;
        if !path.exists() {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "catalog cache is absent",
            ));
        }
        let c = sql::open(&path, self.options.busy_timeout_ms, false)?;
        sql::validate(&c)?;
        self.snapshot_from_connection(c, None)
    }
    pub(crate) fn snapshot_from_connection(
        &self,
        c: rusqlite::Connection,
        budget: Option<&scan::ReadScanBudget>,
    ) -> Result<ReaderSnapshot> {
        if let Some(budget) = budget {
            sql::clamp_busy(&c, self.options.busy_timeout_ms, budget)?;
        }
        c.execute_batch("BEGIN DEFERRED").map_err(sql::sql_error)?;
        // The first read establishes SQLite's snapshot, including both FTS virtual tables.
        let (generation,parser,manifest,serialized):(i64,String,String,String)=c.query_row("SELECT g.gen,g.parser_hash,g.manifest_hash,g.projection_json FROM generations g JOIN index_meta m ON g.gen=m.published_gen WHERE g.state='complete'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|e|if matches!(e,rusqlite::Error::QueryReturnedNoRows){WikiError::new(ErrorCode::OfflineUnavailable,"catalog has no published generation")}else{sql::sql_error(e)})?;
        // An older projection may have classified copied packet windows as
        // ordinary invalid notes. Refuse that snapshot before any query can
        // search its rows; callers with a writer can sync and retry.
        if parser != scan::parser_fingerprint().as_str() {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "catalog parser fingerprint changed; run index sync or rebuild",
            ));
        }
        let projection: CatalogProjection = serde_json::from_str(&serialized)
            .map_err(|e| WikiError::new(ErrorCode::IndexCorrupt, e.to_string()))?;
        let snapshot = ReadSnapshot::canonical(
            u64::try_from(generation)
                .map_err(|_| WikiError::new(ErrorCode::IndexCorrupt, "negative generation"))?,
            crate::domain::Blake3Hash::new(parser)?,
            crate::domain::Blake3Hash::new(manifest)?,
        );
        if projection.vault_id != self.vault_id
            || snapshot.parser_fingerprint != projection.parser_fingerprint
            || snapshot.require_canonical_manifest()? != &projection.control_manifest
        {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "catalog projection binding mismatch",
            ));
        }
        super::integrity::validate_projection(&c, generation, &projection)?;
        if let Some(budget) = budget {
            budget.check()?;
        }
        Ok(ReaderSnapshot {
            connection: c,
            snapshot,
            projection,
            verification: SnapshotVerification::IndexSnapshot,
        })
    }
    pub fn verified_snapshot(&self, writer: Option<&WriterPermit>) -> Result<ReaderSnapshot> {
        self.require_legacy_catalog()?;
        if let Some(w) = writer {
            w.require_root(self.fs.root())?;
        }
        self.guard_current(None)?;
        for attempt in 0..2 {
            match self.index_snapshot() {
                Ok(mut reader) => {
                    let current = scan::scan(&self.fs, &self.vault_id)?;
                    self.guard_current(None)?;
                    if current == reader.projection {
                        reader.verification = SnapshotVerification::VerifiedSnapshot {
                            verified_at: OffsetDateTime::now_utc()
                                .format(&Rfc3339)
                                .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
                        };
                        return Ok(reader);
                    }
                }
                Err(e)
                    if e.code == ErrorCode::OfflineUnavailable
                        && writer.is_some()
                        && attempt == 0 => {}
                Err(e) => return Err(e),
            }
            if attempt == 0
                && let Some(w) = writer
            {
                self.sync(w)?;
                continue;
            }
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "cached canonical membership or dependencies differ from current vault",
            ));
        }
        unreachable!()
    }

    /// Ordinary verified local lookup never acquires a writer or repairs state.
    pub fn read_only_snapshot(&self) -> Result<ReaderSnapshot> {
        self.read_only_snapshot_with_budget(&scan::ReadScanBudget::new(
            scan::ReadScanLimits::default(),
        )?)
    }

    pub(crate) fn read_only_snapshot_with_budget(
        &self,
        budget: &scan::ReadScanBudget,
    ) -> Result<ReaderSnapshot> {
        // Opening a mutable WAL cache can create coordination files even with
        // SQLite's read-only flag. Canonical reconstruction opens no cache path.
        self.canonical_snapshot_with_budget(budget)
    }

    pub(crate) fn recheck_read_snapshot(
        &self,
        reader: &ReaderSnapshot,
        budget: &scan::ReadScanBudget,
    ) -> Result<()> {
        self.require_legacy_catalog_bounded(budget)?;
        self.guard_current_bounded(None, budget)?;
        scan::recheck_bounded(&self.fs, reader.projection(), budget)?;
        self.require_legacy_catalog_bounded(budget)?;
        self.guard_current_bounded(None, budget)
    }
}
impl ReaderSnapshot {
    /// In-memory reconstruction does not publish a persistent generation.
    pub fn persistent_generation(&self) -> Option<u64> {
        self.connection
            .path()
            .filter(|path| !path.is_empty())
            .map(|_| self.snapshot.generation)
    }
    pub fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }
    pub fn verification(&self) -> &SnapshotVerification {
        &self.verification
    }
    pub fn projection(&self) -> &CatalogProjection {
        &self.projection
    }
    pub fn canonical_equivalent(&self, other: &Self) -> bool {
        self.projection == other.projection
    }
    pub(crate) fn connection(&self) -> &rusqlite::Connection {
        &self.connection
    }
    /// Bound FTS expression. Retrieval owns safe plain-query construction and filters.
    pub fn document_matches(
        &self,
        expression: &str,
        limit: usize,
    ) -> Result<Vec<(DocumentRow, f64)>> {
        let mut statement=self.connection.prepare("SELECT d.row_json,bm25(documents_fts,8,6,3,2,1,0,0) AS rank FROM documents_fts JOIN documents d ON d.doc_row=documents_fts.doc_row AND d.gen=documents_fts.gen WHERE documents_fts MATCH ?1 AND documents_fts.gen=?2 ORDER BY rank,coalesce(d.record_id,d.path) LIMIT ?3").map_err(sql::sql_error)?;
        let rows = statement
            .query_map(
                params![
                    expression,
                    sql::integer(self.snapshot.generation)?,
                    sql::integer(
                        u64::try_from(limit)
                            .map_err(|_| WikiError::invalid("candidate limit too large"))?
                    )?
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)),
            )
            .map_err(sql::sql_error)?;
        rows.map(|r| {
            let (serialized, rank) = r.map_err(sql::sql_error)?;
            Ok((
                serde_json::from_str(&serialized)
                    .map_err(|e| WikiError::new(ErrorCode::IndexCorrupt, e.to_string()))?,
                rank,
            ))
        })
        .collect()
    }
    pub fn graph_matches(
        &self,
        expression: &str,
        kind: RecordKind,
        limit: usize,
    ) -> Result<Vec<(GraphRow, f64)>> {
        let weights = match kind {
            RecordKind::Entity => "8,6,0,0,0,1,0,0,0",
            RecordKind::Assertion => "0,0,4,6,2,1,0,0,0",
            _ => {
                return Err(WikiError::invalid(
                    "graph candidate kind must be entity or assertion",
                ));
            }
        };
        let query = format!(
            "SELECT target_id,bm25(graph_fts,{weights}) AS rank FROM graph_fts WHERE graph_fts MATCH ?1 AND gen=?2 AND target_kind=?3 ORDER BY rank,target_id LIMIT ?4"
        );
        let mut statement = self.connection().prepare(&query).map_err(sql::sql_error)?;
        let rows = statement
            .query_map(
                params![
                    expression,
                    sql::integer(self.snapshot.generation)?,
                    kind.to_string(),
                    sql::integer(
                        u64::try_from(limit)
                            .map_err(|_| WikiError::invalid("candidate limit too large"))?
                    )?
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)),
            )
            .map_err(sql::sql_error)?;
        rows.map(|r| {
            let (id, rank) = r.map_err(sql::sql_error)?;
            let row = self
                .projection
                .graph
                .iter()
                .find(|g| g.target_id.as_str() == id)
                .cloned()
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "graph FTS target absent from pinned projection",
                    )
                })?;
            Ok((row, rank))
        })
        .collect()
    }
}
impl Drop for ReaderSnapshot {
    fn drop(&mut self) {
        let _ = self.connection.execute_batch("ROLLBACK");
    }
}

#[cfg(test)]
mod read_only_tests {
    use super::*;
    use crate::{
        domain::{RecordId, VaultRelativePath},
        vault::{VaultFs, VaultRoot},
    };

    fn fixture() -> (tempfile::TempDir, Catalog) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_id: vault_read_only\nwiki_kind: vault\ntitle: Read only\n---\n").unwrap();
        std::fs::write(temp.path().join("page.md"), b"---\nwiki_schema: '1'\nwiki_id: page_read_only\nwiki_kind: page\ntitle: Read only page\nwiki_status: reviewed\n---\nCurrent cobalt notes.\n").unwrap();
        let catalog = Catalog::new(
            VaultFs::new(VaultRoot::explicit(temp.path()).unwrap()),
            RecordId::new("vault_read_only").unwrap(),
        );
        (temp, catalog)
    }

    #[test]
    fn read_only_missing_cache_reconstructs_without_lock_or_publication() {
        let (temp, catalog) = fixture();
        let reader = catalog.read_only_snapshot().unwrap();
        assert!(
            reader
                .projection()
                .documents
                .iter()
                .any(|row| row.path.as_str() == "page.md")
        );
        assert!(matches!(
            reader.verification(),
            SnapshotVerification::VerifiedSnapshot { .. }
        ));
        assert_eq!(reader.persistent_generation(), None);
        assert!(!temp.path().join(".wiki").exists());
    }

    #[test]
    fn read_only_uses_canonical_notes_without_opening_corrupt_derived_cache() {
        let (temp, catalog) = fixture();
        std::fs::create_dir_all(temp.path().join(".wiki/cache")).unwrap();
        std::fs::write(
            temp.path().join(".wiki/cache/index.sqlite"),
            b"not a sqlite database",
        )
        .unwrap();
        let before = std::fs::read(temp.path().join(".wiki/cache/index.sqlite")).unwrap();
        let reader = catalog.read_only_snapshot().unwrap();
        assert!(
            reader
                .projection()
                .documents
                .iter()
                .any(|row| row.path.as_str() == "page.md")
        );
        assert_eq!(
            std::fs::read(temp.path().join(".wiki/cache/index.sqlite")).unwrap(),
            before
        );
        assert!(!temp.path().join(".wiki/cache/index.sqlite-wal").exists());
        assert!(!temp.path().join(".wiki/cache/index.sqlite-shm").exists());
        assert!(!temp.path().join(".wiki/state/writer.lock").exists());
    }

    #[test]
    fn read_only_scan_and_recheck_share_one_byte_allowance() {
        let (_temp, catalog) = fixture();
        let bytes = catalog
            .fs
            .root()
            .scan_markdown()
            .unwrap()
            .iter()
            .map(|path| catalog.fs.read_before(path).unwrap().unwrap().bytes.len())
            .sum::<usize>();
        let budget = scan::ReadScanBudget::new(scan::ReadScanLimits {
            max_bytes: bytes + 1,
            ..Default::default()
        })
        .unwrap();
        let projection = scan::scan_with_budget(catalog.fs(), catalog.vault_id(), &budget).unwrap();
        assert_eq!(
            scan::recheck_bounded(catalog.fs(), &projection, &budget)
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn recheck_rejects_new_identity_claim_without_projecting_again() {
        let (temp, catalog) = fixture();
        let budget = scan::ReadScanBudget::new(Default::default()).unwrap();
        let reader = catalog.read_only_snapshot_with_budget(&budget).unwrap();
        std::fs::write(
            temp.path().join("copied.md"),
            std::fs::read(temp.path().join("page.md")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            catalog
                .recheck_read_snapshot(&reader, &budget)
                .unwrap_err()
                .code,
            ErrorCode::FreshnessConflict
        );
        assert!(!temp.path().join(".wiki").exists());
        assert!(
            reader
                .projection()
                .records
                .contains_key(&RecordId::new("page_read_only").unwrap())
        );
        assert_eq!(
            reader.projection().records[&RecordId::new("page_read_only").unwrap()].path,
            VaultRelativePath::new("page.md").unwrap()
        );
    }
}
